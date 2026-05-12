//! npm-based package manager (global packages).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{
    bootstrap_via_brew_then_system, brew_available, run_pkg_cmd_live, tool_cmd_with_resolver,
};

pub struct NpmManager;

/// Find npm binary, checking PATH and common nvm install locations.
///
/// Not usable with the generic `resolve_tool_with_fallbacks` helper because the
/// nvm path is a wildcard `~/.nvm/versions/node/*/bin/npm` that requires a
/// directory scan rather than a fixed fallback list.
///
/// Honors the `CFGD_NPM_BIN` env-var seam for tests — when set and pointing
/// at a real file, short-circuits the PATH + nvm scan.
pub(super) fn find_npm() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("CFGD_NPM_BIN") {
        let p = PathBuf::from(custom);
        if p.is_file() {
            return Some(p);
        }
    }
    if command_available("npm") {
        return Some(PathBuf::from("npm"));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    find_npm_in_nvm(&home)
}

/// Scan `<home>/.nvm/versions/node/*/bin/npm` and return the first match.
/// Split out so tests can drive the directory scan against a tempdir without
/// mutating `$HOME`.
pub(super) fn find_npm_in_nvm(home: &std::path::Path) -> Option<PathBuf> {
    let nvm_dir = home.join(".nvm/versions/node");
    let entries = std::fs::read_dir(&nvm_dir).ok()?;
    for entry in entries.flatten() {
        let npm_path = entry.path().join("bin/npm");
        if npm_path.exists() {
            return Some(npm_path);
        }
    }
    None
}

pub(super) fn npm_available() -> bool {
    find_npm().is_some()
}

pub(super) fn npm_cmd() -> Command {
    tool_cmd_with_resolver("npm", find_npm)
}

impl PackageManager for NpmManager {
    fn name(&self) -> &str {
        "npm"
    }

    fn is_available(&self) -> bool {
        npm_available()
    }

    fn can_bootstrap(&self) -> bool {
        // Can bootstrap via system package manager or nvm
        brew_available()
            || command_available("apt")
            || command_available("dnf")
            || command_available("curl")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        if bootstrap_via_brew_then_system(printer, "npm", "node", &["nodejs", "npm"])? {
            return Ok(());
        }

        // Fall back to nvm
        if command_available("curl") {
            let result = printer
                .run_with_output(
                    Command::new("bash")
                        .arg("-c")
                        .arg(concat!(
                            "curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash && ",
                            "export NVM_DIR=\"$HOME/.nvm\" && [ -s \"$NVM_DIR/nvm.sh\" ] && . \"$NVM_DIR/nvm.sh\" && ",
                            "nvm install --lts"
                        )),
                    "Installing Node.js via nvm",
                )
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "npm".into(),
                    message: format!("nvm install failed: {}", e),
                })?;
            if result.status.success() {
                return Ok(());
            }
        }

        Err(PackageError::BootstrapFailed {
            manager: "npm".into(),
            message: "no installation method available".into(),
        }
        .into())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = npm_cmd()
            .args(["list", "-g", "--depth=0", "--json"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "npm".into(),
                source: e,
            })?;
        // npm list exits non-zero if there are peer dep issues, but still produces valid JSON
        parse_npm_list_packages(&String::from_utf8_lossy(&output.stdout))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let label = format!("npm install -g {}", packages.join(" "));
        run_pkg_cmd_live(
            printer,
            "npm",
            npm_cmd().arg("install").arg("-g").args(packages),
            &label,
            "install",
        )?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let label = format!("npm uninstall -g {}", packages.join(" "));
        run_pkg_cmd_live(
            printer,
            "npm",
            npm_cmd().arg("uninstall").arg("-g").args(packages),
            &label,
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "npm",
            npm_cmd().args(["update", "-g"]),
            "npm update -g",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // npm view <pkg> version
        let output = npm_cmd()
            .args(["view", package, "version"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "npm".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let version = cfgd_core::stdout_lossy_trimmed(&output);
        if version.is_empty() {
            Ok(None)
        } else {
            Ok(Some(version))
        }
    }

    fn installed_packages_with_versions(&self) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        let output = npm_cmd()
            .args(["list", "-g", "--depth=0", "--json"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "npm".into(),
                source: e,
            })?;
        // npm list exits non-zero on peer dep issues but still produces valid JSON
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "npm".into(),
                message: format!("failed to parse npm list output: {}", e),
            })?;
        Ok(parse_npm_list_versions(&parsed))
    }
}

/// Parse `npm list -g --depth=0 --json` dependencies object into a name-only
/// `HashSet`. Shared between `installed_packages` and tests; the JSON-string
/// boundary is the natural contract since `npm list` exits non-zero on peer
/// dep issues but still produces valid JSON we have to consume.
pub(super) fn parse_npm_list_packages(stdout: &str) -> Result<HashSet<String>> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| PackageError::ListFailed {
            manager: "npm".into(),
            message: format!("failed to parse npm list output: {}", e),
        })?;
    let mut packages = HashSet::new();
    if let Some(deps) = parsed.get("dependencies").and_then(|d| d.as_object()) {
        for key in deps.keys() {
            packages.insert(key.clone());
        }
    }
    Ok(packages)
}

/// Parse `npm list -g --depth=0 --json` dependencies object into PackageInfo.
/// JSON format: `{"dependencies": {"pkg": {"version": "1.2.3"}, ...}}`
pub(super) fn parse_npm_list_versions(
    parsed: &serde_json::Value,
) -> Vec<cfgd_core::providers::PackageInfo> {
    let mut packages = Vec::new();
    if let Some(deps) = parsed.get("dependencies").and_then(|d| d.as_object()) {
        for (name, info) in deps {
            let version = info
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            packages.push(cfgd_core::providers::PackageInfo {
                name: name.clone(),
                version,
            });
        }
    }
    packages
}

#[cfg(test)]
mod tests {
    use cfgd_core::command_available;
    use cfgd_core::providers::PackageManager;

    use super::super::shared::brew_available;
    use super::*;

    #[test]
    fn test_parse_npm_list_versions_basic() {
        let json = serde_json::json!({
            "dependencies": {
                "typescript": {"version": "5.3.3"},
                "eslint": {"version": "8.56.0"},
                "prettier": {"version": "3.2.0"}
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 3);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "typescript" && p.version == "5.3.3")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "eslint" && p.version == "8.56.0")
        );
    }

    #[test]
    fn test_parse_npm_list_versions_no_deps() {
        let json = serde_json::json!({"name": "root"});
        let pkgs = parse_npm_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_parse_npm_list_versions_missing_version() {
        let json = serde_json::json!({
            "dependencies": {
                "some-pkg": {}
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn parse_npm_list_versions_nested_deps_ignored() {
        // Only top-level dependencies are parsed
        let json = serde_json::json!({
            "dependencies": {
                "typescript": {
                    "version": "5.3.3",
                    "dependencies": {
                        "nested-pkg": {"version": "1.0.0"}
                    }
                }
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "typescript");
    }

    #[test]
    fn npm_manager_name() {
        let mgr = NpmManager;
        assert_eq!(mgr.name(), "npm");
    }

    #[test]
    fn parse_npm_list_versions_empty_deps() {
        let json = serde_json::json!({"dependencies": {}});
        let pkgs = parse_npm_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_npm_list_versions_non_string_version() {
        let json = serde_json::json!({
            "dependencies": {
                "pkg": {"version": 123}
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        // version is not a string, so it falls back to "unknown"
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn parse_npm_list_versions_real_world_output() {
        let json = serde_json::json!({
            "version": "10.2.4",
            "name": "lib",
            "dependencies": {
                "corepack": {"version": "0.24.0"},
                "npm": {"version": "10.2.4"},
                "typescript": {"version": "5.3.3"},
                "eslint": {"version": "8.56.0"},
                "prettier": {"version": "3.2.0"}
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 5);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "corepack" && p.version == "0.24.0")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "npm" && p.version == "10.2.4")
        );
    }

    #[test]
    fn parse_npm_list_versions_with_extra_fields() {
        let json = serde_json::json!({
            "version": "1.0.0",
            "dependencies": {
                "express": {
                    "version": "4.18.2",
                    "resolved": "https://registry.npmjs.org/express/-/express-4.18.2.tgz",
                    "overridden": false
                }
            }
        });
        let pkgs = parse_npm_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "express");
        assert_eq!(pkgs[0].version, "4.18.2");
    }

    #[test]
    fn npm_manager_can_bootstrap_checks_cascade() {
        let mgr = NpmManager;
        let can = mgr.can_bootstrap();
        // Should be true if brew, apt, dnf, or curl is available
        let expected = brew_available()
            || command_available("apt")
            || command_available("dnf")
            || command_available("curl");
        assert_eq!(can, expected);
    }

    #[test]
    fn npm_manager_is_available_checks_npm() {
        let mgr = NpmManager;
        let available = mgr.is_available();
        assert_eq!(available, npm_available());
    }

    // --- parse_npm_list_packages ---

    #[test]
    fn parse_npm_list_packages_returns_top_level_dep_names() {
        let stdout = r#"{"dependencies": {"typescript":{"version":"5.3.3"}, "eslint":{"version":"8.56.0"}}}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.contains("typescript"));
        assert!(pkgs.contains("eslint"));
    }

    #[test]
    fn parse_npm_list_packages_no_deps_field_yields_empty() {
        let stdout = r#"{"name":"root","version":"1.0.0"}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_npm_list_packages_empty_deps_object_yields_empty() {
        let stdout = r#"{"dependencies":{}}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_npm_list_packages_ignores_nested_deps() {
        // Only top-level keys are returned; nested dependency trees stay nested.
        let stdout = r#"{"dependencies":{"typescript":{"version":"5.3.3","dependencies":{"nested-pkg":{"version":"1.0.0"}}}}}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert_eq!(pkgs.len(), 1);
        assert!(pkgs.contains("typescript"));
        assert!(
            !pkgs.contains("nested-pkg"),
            "nested deps must not leak into the top-level set"
        );
    }

    #[test]
    fn parse_npm_list_packages_errors_on_invalid_json() {
        let err = parse_npm_list_packages("not-json").expect_err("invalid JSON must error");
        let msg = err.to_string();
        assert!(
            msg.contains("npm") && msg.contains("failed to parse npm list output"),
            "error must include 'npm' and parse-failure context, got: {msg}"
        );
    }

    #[test]
    fn parse_npm_list_packages_dependencies_not_object_yields_empty() {
        // npm sometimes emits "dependencies": [] when there are issues —
        // treat that as no deps rather than panicking.
        let stdout = r#"{"dependencies":[]}"#;
        let pkgs = parse_npm_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    // --- find_npm_in_nvm ---

    #[test]
    fn find_npm_in_nvm_returns_none_when_no_nvm_dir() {
        let home = tempfile::tempdir().unwrap();
        // No .nvm directory exists in the tempdir at all.
        assert!(find_npm_in_nvm(home.path()).is_none());
    }

    #[test]
    fn find_npm_in_nvm_returns_none_when_nvm_dir_empty() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".nvm/versions/node")).unwrap();
        // Directory exists but contains no node version subdirs.
        assert!(find_npm_in_nvm(home.path()).is_none());
    }

    #[test]
    fn find_npm_in_nvm_returns_first_npm_binary_found() {
        let home = tempfile::tempdir().unwrap();
        let v20 = home.path().join(".nvm/versions/node/v20.10.0/bin");
        std::fs::create_dir_all(&v20).unwrap();
        let npm = v20.join("npm");
        std::fs::write(&npm, b"#!/bin/sh\n").unwrap();

        let found = find_npm_in_nvm(home.path()).expect("npm should be found in nvm version dir");
        assert_eq!(
            found, npm,
            "must return the absolute path to the located npm binary"
        );
    }

    #[test]
    fn find_npm_in_nvm_skips_versions_without_bin_npm() {
        let home = tempfile::tempdir().unwrap();
        // v18 has no bin/npm; v20 does. Result must be from v20.
        std::fs::create_dir_all(home.path().join(".nvm/versions/node/v18.0.0")).unwrap();
        let v20bin = home.path().join(".nvm/versions/node/v20.10.0/bin");
        std::fs::create_dir_all(&v20bin).unwrap();
        std::fs::write(v20bin.join("npm"), b"").unwrap();

        let found = find_npm_in_nvm(home.path()).expect("v20 npm should be located");
        assert!(
            found.to_string_lossy().contains("v20.10.0"),
            "must skip the version that lacks bin/npm, got: {}",
            found.display()
        );
    }

    // ---------------------------------------------------------------------
    // PackageManager-impl tests via CFGD_NPM_BIN ToolShim.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    mod npm_shim {
        use super::*;
        use cfgd_core::test_helpers::{ToolShim, test_printer};
        use serial_test::serial;

        fn shim(stdout: &str, stderr: &str, exit: i32) -> ToolShim {
            ToolShim::install("CFGD_NPM_BIN", exit, stdout, stderr)
        }

        #[test]
        #[serial]
        fn npm_install_passes_install_g_with_packages() {
            let s = shim("", "", 0);
            let p = test_printer();
            NpmManager
                .install(&["typescript".into(), "eslint".into()], &p)
                .expect("Ok");
            let argv = s.argv_log();
            assert!(
                argv.contains("install -g typescript eslint"),
                "argv must include install -g + packages: {argv}"
            );
        }

        #[test]
        #[serial]
        fn npm_install_skips_command_when_empty() {
            let s = shim("", "", 0);
            let p = test_printer();
            NpmManager.install(&[], &p).expect("Ok");
            assert_eq!(s.invocation_count(), 0);
        }

        #[test]
        #[serial]
        fn npm_uninstall_passes_uninstall_g_with_packages() {
            let s = shim("", "", 0);
            let p = test_printer();
            NpmManager
                .uninstall(&["typescript".into()], &p)
                .expect("Ok");
            assert!(s.argv_log().contains("uninstall -g typescript"));
        }

        #[test]
        #[serial]
        fn npm_update_runs_update_g() {
            let s = shim("", "", 0);
            let p = test_printer();
            NpmManager.update(&p).expect("Ok");
            assert!(s.argv_log().contains("update -g"));
        }

        #[test]
        #[serial]
        fn npm_available_version_runs_view_and_returns_trimmed_stdout() {
            let _s = shim("5.3.3\n", "", 0);
            let v = NpmManager.available_version("typescript").expect("Ok");
            assert_eq!(v.as_deref(), Some("5.3.3"));
        }

        #[test]
        #[serial]
        fn npm_available_version_passes_view_subcommand_with_package_and_field() {
            let s = shim("1.0.0", "", 0);
            NpmManager.available_version("typescript").expect("Ok");
            let argv = s.argv_log();
            assert!(
                argv.contains("view typescript version"),
                "argv must include view <pkg> version: {argv}"
            );
        }

        #[test]
        #[serial]
        fn npm_available_version_returns_none_on_nonzero_exit() {
            let _s = shim("", "404 not found", 1);
            let v = NpmManager
                .available_version("nonexistent")
                .expect("non-zero → Ok(None) not Err");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn npm_available_version_returns_none_on_empty_stdout() {
            let _s = shim("\n   \n", "", 0);
            let v = NpmManager
                .available_version("weird-pkg")
                .expect("empty stdout → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn npm_installed_packages_parses_npm_list_json() {
            let json = r#"{"dependencies":{"typescript":{"version":"5.3.3"},"eslint":{"version":"8.0.0"}}}"#;
            // npm list exits non-zero on peer dep issues; stdout still valid JSON.
            let _s = shim(json, "peer dep issues", 1);
            let pkgs = NpmManager.installed_packages().expect("Ok");
            assert_eq!(pkgs.len(), 2);
            assert!(pkgs.contains("typescript"));
            assert!(pkgs.contains("eslint"));
        }

        #[test]
        #[serial]
        fn npm_installed_packages_with_versions_includes_versions() {
            let json = r#"{"dependencies":{"typescript":{"version":"5.3.3"}}}"#;
            let _s = shim(json, "", 0);
            let pkgs = NpmManager.installed_packages_with_versions().expect("Ok");
            let ts = pkgs
                .iter()
                .find(|p| p.name == "typescript")
                .expect("typescript present");
            assert_eq!(ts.version, "5.3.3");
        }
    }
}

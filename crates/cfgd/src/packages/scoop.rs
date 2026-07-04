//! Scoop package manager (Windows).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;
use cfgd_core::providers::{PackageInfo, PackageManager};

use super::shared::{
    canonical_ci_pkg_name, parse_version_field, resolve_tool_with_fallbacks, run_pkg_cmd_live,
    run_pkg_query, tool_cmd_with_resolver,
};

pub struct ScoopManager;

/// Build a `Command` for scoop, resolved shim-aware. scoop ships on Windows only as
/// `scoop.ps1`/`scoop.cmd` (never `scoop.exe`), so a bare `Command::new("scoop")`
/// dies with "program not found" even though the tool is on `$PATH`. Routing through
/// `tool_cmd_with_resolver` resolves the full shim path and invokes it via
/// `powershell -File` / `cmd /c` on Windows (a plain PATH lookup on Unix). No
/// fallbacks: scoop always lives in its shims dir on `$PATH`.
fn scoop_cmd() -> Command {
    tool_cmd_with_resolver("scoop", || resolve_tool_with_fallbacks("scoop", &[]))
}

/// Parse the set of installed app names from `scoop export` JSON. The document is
/// `{ "buckets": [...], "apps": [ { "Name": "...", ... }, ... ] }`; a missing/empty
/// `apps` array (or non-JSON input) yields an empty set. Used instead of parsing
/// `scoop list`, whose PowerShell Format-Table renders NO rows when captured with no
/// console width (every non-interactive child process), making it unparseable.
pub(super) fn parse_scoop_export(output: &str) -> HashSet<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return HashSet::new();
    };
    value
        .get("apps")
        .and_then(|apps| apps.as_array())
        .map(|apps| {
            apps.iter()
                .filter_map(|app| app.get("Name").and_then(|n| n.as_str()))
                .map(canonical_ci_pkg_name)
                .collect()
        })
        .unwrap_or_default()
}

/// Installed apps WITH versions for `installed_packages_with_versions`. Unlike
/// [`parse_scoop_export`] this preserves the REGISTERED app-name case for display
/// (the scan/status surface) and carries `scoop export`'s reported `Version`.
fn parse_scoop_export_versions(output: &str) -> Vec<PackageInfo> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    value
        .get("apps")
        .and_then(|apps| apps.as_array())
        .map(|apps| {
            apps.iter()
                .filter_map(|app| {
                    let name = app.get("Name").and_then(|n| n.as_str())?;
                    let version = app
                        .get("Version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    Some(PackageInfo {
                        name: name.to_string(),
                        version: version.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl PackageManager for ScoopManager {
    fn name(&self) -> &str {
        "scoop"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("scoop")
    }

    fn can_bootstrap(&self) -> bool {
        true
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "scoop",
            Command::new("powershell").args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "irm get.scoop.sh | iex",
            ]),
            "Installing Scoop",
            "install",
        )?;
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        // `scoop list` renders a PowerShell Format-Table that produces NO rows when
        // captured with no console width (any non-interactive child process), so it
        // cannot be parsed. `scoop export` emits stable JSON regardless of console —
        // enumerate its `apps[].Name`. Exit code is tolerated via run_pkg_query
        // (scoop exits non-zero on an empty DB, a benign result — not a failure).
        let output = run_pkg_query("scoop", scoop_cmd().arg("export"))?;
        Ok(parse_scoop_export(&String::from_utf8_lossy(&output.stdout)))
    }

    /// scoop app names are matched case-insensitively; canonicalize to lowercase so
    /// a profile entry matches `scoop export`'s reported `Name` for install-diffing,
    /// prune, and tracking (mirrors chocolatey/winget).
    fn package_identity(&self, entry: &str) -> String {
        canonical_ci_pkg_name(entry)
    }

    /// Display surface (scan/status): keep the REGISTERED app-name case and the real
    /// version, rather than the lowercase identity form used for matching.
    fn installed_packages_with_versions(&self) -> Result<Vec<PackageInfo>> {
        let output = run_pkg_query("scoop", scoop_cmd().arg("export"))?;
        Ok(parse_scoop_export_versions(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            run_pkg_cmd_live(
                printer,
                "scoop",
                scoop_cmd().args(["install", pkg]),
                &format!("Installing {}", pkg),
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            run_pkg_cmd_live(
                printer,
                "scoop",
                scoop_cmd().args(["uninstall", pkg]),
                &format!("Uninstalling {}", pkg),
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "scoop",
            scoop_cmd().args(["update", "*"]),
            "Upgrading all scoop packages",
            "install",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // Bounded via run_pkg_query: `scoop info` runs through PowerShell on Windows
        // and must never hang a headless reconcile. A non-zero exit (unknown package)
        // is a benign "no version", not an error.
        let output = run_pkg_query("scoop", scoop_cmd().args(["info", package]))?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_version_field(&stdout))
    }
}

#[cfg(test)]
mod tests {
    use cfgd_core::command_available;
    use cfgd_core::providers::PackageManager;

    use super::*;

    // scoop `export` emits stable JSON regardless of console; parse_scoop_export
    // enumerates `apps[].Name`. (scoop `list` renders a Format-Table that produces
    // no rows when captured with no console width, so it is not used.)
    #[test]
    fn scoop_parse_export_multiple_apps() {
        let output = r#"{"buckets":[{"Name":"main"}],"apps":[
            {"Name":"7zip","Version":"26.02","Source":"main"},
            {"Name":"ripgrep","Version":"14.1.0","Source":"main"},
            {"Name":"fd","Version":"9.0.0","Source":"main"}
        ]}"#;
        let packages = parse_scoop_export(output);
        assert_eq!(packages.len(), 3);
        assert!(packages.contains("7zip"));
        assert!(packages.contains("ripgrep"));
        assert!(packages.contains("fd"));
    }

    #[test]
    fn scoop_parse_export_single_app() {
        let output = r#"{"apps":[{"Name":"git","Version":"2.44.0","Source":"main"}]}"#;
        let packages = parse_scoop_export(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("git"));
    }

    #[test]
    fn scoop_parse_export_canonicalizes_case() {
        // scoop app names are matched case-insensitively; fold to lowercase so a
        // profile entry matches for install-diff + prune.
        let output = r#"{"apps":[{"Name":"NodeJS","Version":"22.0.0","Source":"main"}]}"#;
        let packages = parse_scoop_export(output);
        assert!(packages.contains("nodejs"));
        assert!(!packages.contains("NodeJS"));
    }

    #[test]
    fn scoop_package_identity_folds_case() {
        assert_eq!(ScoopManager.package_identity("NodeJS"), "nodejs");
        assert_eq!(ScoopManager.package_identity("7zip"), "7zip");
    }

    #[test]
    fn scoop_versions_keep_registered_case_and_version() {
        // Display surface keeps the registered app-name case and the exported version.
        let output = r#"{"apps":[{"Name":"NodeJS","Version":"22.0.0","Source":"main"}]}"#;
        let infos = parse_scoop_export_versions(output);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "NodeJS");
        assert_eq!(infos[0].version, "22.0.0");
    }

    #[test]
    fn scoop_parse_export_empty_apps_array() {
        let packages = parse_scoop_export(r#"{"buckets":[{"Name":"main"}],"apps":[]}"#);
        assert!(packages.is_empty());
    }

    #[test]
    fn scoop_parse_export_missing_apps_key() {
        let packages = parse_scoop_export(r#"{"buckets":[{"Name":"main"}]}"#);
        assert!(packages.is_empty());
    }

    #[test]
    fn scoop_parse_export_non_json_is_empty() {
        // A benign empty-DB message or any non-JSON text yields an empty set.
        assert!(parse_scoop_export("There aren't any apps installed.").is_empty());
        assert!(parse_scoop_export("").is_empty());
    }

    #[test]
    fn scoop_parse_export_ignores_apps_without_name() {
        let output = r#"{"apps":[{"Version":"1.0"},{"Name":"fd","Version":"9.0.0"}]}"#;
        let packages = parse_scoop_export(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("fd"));
    }

    #[test]
    fn scoop_manager_name_and_traits() {
        let mgr = ScoopManager;
        assert_eq!(mgr.name(), "scoop");
        assert!(mgr.can_bootstrap());
    }

    #[test]
    #[serial_test::serial]
    fn scoop_manager_is_available_checks_scoop() {
        let mgr = ScoopManager;
        let available = mgr.is_available();
        assert_eq!(available, command_available("scoop"));
    }

    #[test]
    fn scoop_manager_can_bootstrap_true() {
        let mgr = ScoopManager;
        assert!(mgr.can_bootstrap());
    }

    // ---------------------------------------------------------------------------
    // PackageManager trait impls via a fake scoop binary on PATH. scoop_cmd()
    // resolves the tool through command_path (no CFGD_SCOOP_BIN seam), so PATH
    // manipulation routes the invocation through our shim.
    // ---------------------------------------------------------------------------

    #[cfg(unix)]
    mod scoop_shim {
        use super::*;
        use cfgd_core::test_helpers::{install_named_path_shim, test_printer};
        use serial_test::serial;

        // Local wrapper: scoop is invoked by name via PATH, no env-var seam.
        // Delegates to the shared helper so the shim-script body stays in
        // one place across the package crate.
        fn install_scoop_shim(
            exit_code: u8,
            stdout: &str,
            stderr: &str,
        ) -> (tempfile::TempDir, cfgd_core::test_helpers::EnvVarGuard) {
            install_named_path_shim("scoop", exit_code, stdout, stderr)
        }

        #[test]
        #[serial]
        fn scoop_bootstrap_runs_powershell_install_script() {
            // Scoop bootstrap shells out to `powershell` with the irm | iex
            // pipeline. A PATH-shimmed `powershell` swallowing the args and
            // exiting 0 proves the bootstrap path is exercised without
            // requiring real Windows infrastructure.
            let (_bin, _path) = install_named_path_shim("powershell", 0, "", "");
            let p = test_printer();
            ScoopManager.bootstrap(&p).expect("bootstrap Ok via shim");
        }

        #[test]
        #[serial]
        fn scoop_bootstrap_propagates_powershell_failure() {
            let (_bin, _path) =
                install_named_path_shim("powershell", 1, "", "scoop install failed");
            let p = test_printer();
            let err = ScoopManager
                .bootstrap(&p)
                .expect_err("non-zero powershell must error");
            let _ = err.to_string();
        }

        #[test]
        #[serial]
        fn scoop_install_invokes_per_package() {
            let (_bin, _path) = install_scoop_shim(0, "", "");
            let p = test_printer();
            ScoopManager
                .install(&["git".into(), "ripgrep".into()], &p)
                .expect("install Ok");
        }

        #[test]
        #[serial]
        fn scoop_install_propagates_nonzero_exit_as_install_failed() {
            let (_bin, _path) = install_scoop_shim(1, "", "couldn't find git");
            let p = test_printer();
            let err = ScoopManager
                .install(&["git".into()], &p)
                .expect_err("non-zero scoop install must error");
            let msg = err.to_string();
            // Error kind is "install" — surfaces as InstallFailed and carries
            // the manager name in the error.
            assert!(
                msg.contains("scoop"),
                "error must reference manager name, got: {msg}"
            );
        }

        #[test]
        #[serial]
        fn scoop_uninstall_invokes_per_package() {
            let (_bin, _path) = install_scoop_shim(0, "", "");
            let p = test_printer();
            ScoopManager
                .uninstall(&["git".into()], &p)
                .expect("uninstall Ok");
        }

        #[test]
        #[serial]
        fn scoop_uninstall_propagates_nonzero_exit_as_uninstall_failed() {
            let (_bin, _path) = install_scoop_shim(1, "", "no such package");
            let p = test_printer();
            let err = ScoopManager
                .uninstall(&["git".into()], &p)
                .expect_err("non-zero scoop uninstall must error");
            assert!(err.to_string().contains("scoop"));
        }

        #[test]
        #[serial]
        fn scoop_update_runs_update_star() {
            let (_bin, _path) = install_scoop_shim(0, "", "");
            let p = test_printer();
            ScoopManager.update(&p).expect("update Ok");
        }

        #[test]
        #[serial]
        fn scoop_installed_packages_parses_export_json() {
            let stdout =
                r#"{"apps":[{"Name":"git","Version":"2.44.0"},{"Name":"fd","Version":"9.0.0"}]}"#;
            let (_bin, _path) = install_scoop_shim(0, stdout, "");
            let pkgs = ScoopManager.installed_packages().expect("Ok");
            assert!(pkgs.contains("git"));
            assert!(pkgs.contains("fd"));
            assert_eq!(pkgs.len(), 2);
        }

        #[test]
        #[serial]
        fn scoop_installed_packages_empty_when_no_apps() {
            let (_bin, _path) = install_scoop_shim(0, r#"{"apps":[]}"#, "");
            let pkgs = ScoopManager.installed_packages().expect("Ok");
            assert!(pkgs.is_empty());
        }

        #[test]
        #[serial]
        fn scoop_installed_packages_empty_db_exit1_is_empty_not_error() {
            // scoop exits 1 when the DB is empty; installed_packages must treat that
            // as an empty set, not a ListFailed — otherwise every apply on a fresh
            // scoop aborts before it can install anything. (Export prints an empty
            // apps array; some scoop versions emit a non-JSON message — both parse to
            // an empty set.)
            let (_bin, _path) = install_scoop_shim(1, "There aren't any apps installed.\n", "");
            let pkgs = ScoopManager
                .installed_packages()
                .expect("empty-DB exit-1 must be Ok(empty), not Err");
            assert!(pkgs.is_empty());
        }

        #[test]
        #[serial]
        fn scoop_available_version_returns_none_on_nonzero_exit() {
            let (_bin, _path) = install_scoop_shim(1, "", "package not found");
            let v = ScoopManager
                .available_version("nonexistent")
                .expect("non-zero → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn scoop_available_version_extracts_version_field_from_info_output() {
            // `scoop info <pkg>` prints "Version: X.Y.Z" among other fields;
            // parse_version_field plucks that line.
            let info = "Name: git\nVersion: 2.44.0\nDescription: git\n";
            let (_bin, _path) = install_scoop_shim(0, info, "");
            let v = ScoopManager.available_version("git").expect("Ok");
            assert_eq!(v.as_deref(), Some("2.44.0"));
        }
    }
}

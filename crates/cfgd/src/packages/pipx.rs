//! pipx-based package manager.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{
    bootstrap_via_brew_then_system, brew_available, resolve_tool_with_fallbacks, run_pkg_cmd,
    run_pkg_cmd_live, tool_cmd_with_resolver,
};

pub struct PipxManager;

fn pipx_fallbacks() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| pipx_fallbacks_for_home(std::path::Path::new(&h)))
        .unwrap_or_default()
}

/// `pipx_fallbacks` with the `$HOME` directory injected — split out so tests
/// exercise the path-construction contract without mutating process env state.
pub(super) fn pipx_fallbacks_for_home(home: &std::path::Path) -> Vec<PathBuf> {
    vec![home.join(".local/bin/pipx")]
}

pub(super) fn find_pipx() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("pipx", &pipx_fallbacks())
}

pub(super) fn pipx_available() -> bool {
    find_pipx().is_some()
}

pub(super) fn pipx_cmd() -> Command {
    tool_cmd_with_resolver("pipx", find_pipx)
}

impl PackageManager for PipxManager {
    fn name(&self) -> &str {
        "pipx"
    }

    fn is_available(&self) -> bool {
        pipx_available()
    }

    fn can_bootstrap(&self) -> bool {
        // Can bootstrap via system package manager or pip
        brew_available()
            || command_available("apt")
            || command_available("dnf")
            || command_available("pip3")
            || command_available("pip")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        if bootstrap_via_brew_then_system(printer, "pipx", "pipx", &["pipx"])? {
            return Ok(());
        }

        // Fall back to pip
        let pip_cmd = if command_available("pip3") {
            "pip3"
        } else if command_available("pip") {
            "pip"
        } else {
            return Err(PackageError::BootstrapFailed {
                manager: "pipx".into(),
                message: "no installation method available".into(),
            }
            .into());
        };

        let label = format!("Installing pipx via {}", pip_cmd);
        let result = printer
            .run_with_output(
                Command::new(pip_cmd).args(["install", "--user", "pipx"]),
                &label,
            )
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "pipx".into(),
                message: format!("{} install failed: {}", pip_cmd, e),
            })?;
        if !result.status.success() {
            return Err(PackageError::BootstrapFailed {
                manager: "pipx".into(),
                message: format!("{} install --user pipx failed", pip_cmd),
            }
            .into());
        }

        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("pipx", pipx_cmd().args(["list", "--json"]), "list")?;
        parse_pipx_list_packages(&String::from_utf8_lossy(&output.stdout))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            let label = format!("pipx install {}", pkg);
            run_pkg_cmd_live(
                printer,
                "pipx",
                pipx_cmd().args(["install", pkg]),
                &label,
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            let label = format!("pipx uninstall {}", pkg);
            run_pkg_cmd_live(
                printer,
                "pipx",
                pipx_cmd().args(["uninstall", pkg]),
                &label,
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "pipx",
            pipx_cmd().args(["upgrade-all"]),
            "pipx upgrade-all",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // Query PyPI JSON API: https://pypi.org/pypi/<pkg>/json → .info.version
        let url = format!("https://pypi.org/pypi/{}/json", package);
        let output = Command::new("curl")
            .args(["-fsSL", &url])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "pipx".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        parse_pypi_version(&String::from_utf8_lossy(&output.stdout))
    }

    fn installed_packages_with_versions(&self) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        let output = run_pkg_cmd("pipx", pipx_cmd().args(["list", "--json"]), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "pipx".into(),
                message: format!("failed to parse pipx list output: {}", e),
            })?;
        Ok(parse_pipx_list_versions(&parsed))
    }
}

/// Parse `pipx list --json` venvs object into a name-only `HashSet`.
/// Shared with `installed_packages`; the JSON-string boundary is the natural
/// contract since `pipx list --json` is the production input.
pub(super) fn parse_pipx_list_packages(stdout: &str) -> Result<HashSet<String>> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| PackageError::ListFailed {
            manager: "pipx".into(),
            message: format!("failed to parse pipx list output: {}", e),
        })?;
    let mut packages = HashSet::new();
    if let Some(venvs) = parsed.get("venvs").and_then(|v| v.as_object()) {
        for key in venvs.keys() {
            packages.insert(key.clone());
        }
    }
    Ok(packages)
}

/// Parse the PyPI JSON API response for the latest version.
/// Returns `Ok(None)` when `/info/version` is absent or non-string —
/// callers treat that as "version unknown" rather than an error.
pub(super) fn parse_pypi_version(stdout: &str) -> Result<Option<String>> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| PackageError::ListFailed {
            manager: "pipx".into(),
            message: format!("failed to parse PyPI response: {}", e),
        })?;
    Ok(parsed
        .pointer("/info/version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

/// Parse `pipx list --json` venvs object into PackageInfo.
/// JSON format: `{"venvs": {"pkg": {"metadata": {"main_package": {"package_version": "1.2.3"}}}}}`
pub(super) fn parse_pipx_list_versions(
    parsed: &serde_json::Value,
) -> Vec<cfgd_core::providers::PackageInfo> {
    let mut packages = Vec::new();
    if let Some(venvs) = parsed.get("venvs").and_then(|v| v.as_object()) {
        for (name, info) in venvs {
            let version = info
                .pointer("/metadata/main_package/package_version")
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
    fn test_parse_pipx_list_versions_basic() {
        let json = serde_json::json!({
            "venvs": {
                "black": {
                    "metadata": {
                        "main_package": {
                            "package_version": "24.1.1"
                        }
                    }
                },
                "httpie": {
                    "metadata": {
                        "main_package": {
                            "package_version": "3.2.2"
                        }
                    }
                }
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 2);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "black" && p.version == "24.1.1")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "httpie" && p.version == "3.2.2")
        );
    }

    #[test]
    fn test_parse_pipx_list_versions_no_venvs() {
        let json = serde_json::json!({"venvs": {}});
        let pkgs = parse_pipx_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_parse_pipx_list_versions_missing_version_field() {
        let json = serde_json::json!({
            "venvs": {
                "awscli": {
                    "metadata": {
                        "main_package": {}
                    }
                }
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn parse_pipx_list_versions_null_root() {
        let json = serde_json::json!(null);
        let pkgs = parse_pipx_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_pipx_list_versions_missing_metadata() {
        let json = serde_json::json!({
            "venvs": {
                "tool": {}
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "tool");
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn pipx_manager_name() {
        let mgr = PipxManager;
        assert_eq!(mgr.name(), "pipx");
    }

    #[test]
    fn parse_pipx_list_versions_multiple_venvs() {
        let json = serde_json::json!({
            "venvs": {
                "black": {"metadata": {"main_package": {"package_version": "24.1.1"}}},
                "httpie": {"metadata": {"main_package": {"package_version": "3.2.2"}}},
                "ruff": {"metadata": {"main_package": {"package_version": "0.2.0"}}},
                "mypy": {"metadata": {"main_package": {"package_version": "1.8.0"}}}
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 4);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "ruff" && p.version == "0.2.0")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "mypy" && p.version == "1.8.0")
        );
    }

    #[test]
    fn parse_pipx_list_versions_no_venvs_key() {
        let json = serde_json::json!({"pipx_spec_version": "0.1"});
        let pkgs = parse_pipx_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_pipx_list_versions_real_world_output() {
        let json = serde_json::json!({
            "pipx_spec_version": "0.1",
            "venvs": {
                "black": {
                    "metadata": {
                        "main_package": {
                            "package": "black",
                            "package_version": "24.1.1",
                            "pip_args": [],
                            "include_apps": true,
                            "include_dependencies": false
                        },
                        "python_version": "Python 3.12.1"
                    }
                },
                "ruff": {
                    "metadata": {
                        "main_package": {
                            "package": "ruff",
                            "package_version": "0.2.0",
                            "pip_args": [],
                            "include_apps": true,
                            "include_dependencies": false
                        },
                        "python_version": "Python 3.12.1"
                    }
                }
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 2);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "black" && p.version == "24.1.1")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "ruff" && p.version == "0.2.0")
        );
    }

    #[test]
    fn parse_pipx_list_versions_with_injected_packages() {
        let json = serde_json::json!({
            "venvs": {
                "black": {
                    "metadata": {
                        "main_package": {"package_version": "24.1.1"},
                        "injected_packages": {
                            "black[jupyter]": {"package_version": "24.1.1"}
                        }
                    }
                }
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        // Only main_package is extracted, not injected
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "black");
        assert_eq!(pkgs[0].version, "24.1.1");
    }

    #[test]
    fn pipx_manager_can_bootstrap_checks_cascade() {
        let mgr = PipxManager;
        let can = mgr.can_bootstrap();
        let expected = brew_available()
            || command_available("apt")
            || command_available("dnf")
            || command_available("pip3")
            || command_available("pip");
        assert_eq!(can, expected);
    }

    #[test]
    fn pipx_manager_is_available_checks_pipx() {
        let mgr = PipxManager;
        let available = mgr.is_available();
        assert_eq!(available, pipx_available());
    }

    // --- parse_pipx_list_packages ---

    #[test]
    fn parse_pipx_list_packages_returns_venv_names() {
        let stdout = r#"{"venvs":{"black":{},"ruff":{},"httpie":{}}}"#;
        let pkgs = parse_pipx_list_packages(stdout).unwrap();
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.contains("black"));
        assert!(pkgs.contains("ruff"));
        assert!(pkgs.contains("httpie"));
    }

    #[test]
    fn parse_pipx_list_packages_no_venvs_field_yields_empty() {
        let stdout = r#"{"pipx_spec_version":"0.1"}"#;
        let pkgs = parse_pipx_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_pipx_list_packages_empty_venvs_yields_empty() {
        let stdout = r#"{"venvs":{}}"#;
        let pkgs = parse_pipx_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_pipx_list_packages_errors_on_invalid_json() {
        let err = parse_pipx_list_packages("garbage").expect_err("invalid JSON must error");
        let msg = err.to_string();
        assert!(
            msg.contains("pipx") && msg.contains("failed to parse pipx list output"),
            "error must include 'pipx' and parse-failure context, got: {msg}"
        );
    }

    // --- parse_pypi_version ---

    #[test]
    fn parse_pypi_version_extracts_info_version() {
        let stdout = r#"{"info":{"name":"black","version":"24.1.1"}}"#;
        let v = parse_pypi_version(stdout).unwrap();
        assert_eq!(v.as_deref(), Some("24.1.1"));
    }

    #[test]
    fn parse_pypi_version_returns_none_when_field_missing() {
        let stdout = r#"{"info":{"name":"black"}}"#;
        let v = parse_pypi_version(stdout).unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn parse_pypi_version_returns_none_when_value_is_non_string() {
        // Tolerate broken/non-conforming PyPI responses (e.g. integer field).
        let stdout = r#"{"info":{"version":42}}"#;
        let v = parse_pypi_version(stdout).unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn parse_pypi_version_errors_on_invalid_json() {
        let err = parse_pypi_version("not-json").expect_err("invalid JSON must error");
        let msg = err.to_string();
        assert!(
            msg.contains("pipx") && msg.contains("failed to parse PyPI response"),
            "error must attribute to pipx + name PyPI source, got: {msg}"
        );
    }

    // --- pipx_fallbacks_for_home ---

    #[test]
    fn pipx_fallbacks_for_home_contains_local_bin_path() {
        let home = std::path::Path::new("/some/home");
        let fallbacks = pipx_fallbacks_for_home(home);
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(fallbacks[0], home.join(".local/bin/pipx"));
    }

    // ---------------------------------------------------------------------
    // PackageManager-impl tests via CFGD_PIPX_BIN ToolShim. The seam is
    // honored automatically by `tool_cmd_with_resolver` / `find_pipx`.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    mod pipx_shim {
        use super::*;
        use cfgd_core::providers::PackageManager;
        use cfgd_core::test_helpers::{ToolShim, test_printer};
        use serial_test::serial;

        fn shim(stdout: &str, stderr: &str, exit: i32) -> ToolShim {
            ToolShim::install("CFGD_PIPX_BIN", exit, stdout, stderr)
        }

        #[test]
        #[serial]
        fn pipx_install_runs_install_subcommand_per_package() {
            let s = shim("", "", 0);
            let p = test_printer();
            PipxManager
                .install(&["black".into(), "ruff".into()], &p)
                .expect("Ok");
            assert_eq!(s.invocation_count(), 2, "one pipx invocation per pkg");
            let argv = s.argv_log();
            assert!(argv.contains("install black"));
            assert!(argv.contains("install ruff"));
        }

        #[test]
        #[serial]
        fn pipx_uninstall_runs_uninstall_subcommand_per_package() {
            let s = shim("", "", 0);
            let p = test_printer();
            PipxManager.uninstall(&["black".into()], &p).expect("Ok");
            assert!(s.argv_log().contains("uninstall black"));
        }

        #[test]
        #[serial]
        fn pipx_update_runs_upgrade_all_subcommand() {
            let s = shim("", "", 0);
            let p = test_printer();
            PipxManager.update(&p).expect("Ok");
            assert!(s.argv_log().contains("upgrade-all"));
        }

        #[test]
        #[serial]
        fn pipx_installed_packages_parses_venvs_json() {
            // pipx list --json: { "venvs": { "black": { ... }, "ruff": { ... } } }
            let json = r#"{"venvs":{"black":{"metadata":{"main_package":{"package":"black","package_version":"24.1.0"}}},"ruff":{"metadata":{"main_package":{"package":"ruff","package_version":"0.2.1"}}}}}"#;
            let _s = shim(json, "", 0);
            let pkgs = PipxManager.installed_packages().expect("Ok");
            assert_eq!(pkgs.len(), 2);
            assert!(pkgs.contains("black"));
            assert!(pkgs.contains("ruff"));
        }

        #[test]
        #[serial]
        fn pipx_installed_packages_with_versions_extracts_versions() {
            let json = r#"{"venvs":{"black":{"metadata":{"main_package":{"package":"black","package_version":"24.1.0"}}}}}"#;
            let _s = shim(json, "", 0);
            let pkgs = PipxManager.installed_packages_with_versions().expect("Ok");
            let black = pkgs
                .iter()
                .find(|p| p.name == "black")
                .expect("black present");
            assert_eq!(black.version, "24.1.0");
        }
    }
}

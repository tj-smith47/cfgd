//! Cargo-based package manager (`cargo install`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{
    resolve_tool_with_fallbacks, run_pkg_cmd, run_pkg_cmd_live, tool_cmd_with_resolver,
};

pub struct CargoManager;

/// Cargo fallback locations when `cargo` is not on `$PATH`.
fn cargo_fallbacks() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| vec![PathBuf::from(h).join(".cargo/bin/cargo")])
        .unwrap_or_default()
}

pub(super) fn find_cargo() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("cargo", &cargo_fallbacks())
}

pub(super) fn cargo_available() -> bool {
    find_cargo().is_some()
}

pub(super) fn cargo_cmd() -> Command {
    tool_cmd_with_resolver("cargo", find_cargo)
}

impl PackageManager for CargoManager {
    fn name(&self) -> &str {
        "cargo"
    }

    fn is_available(&self) -> bool {
        cargo_available()
    }

    fn can_bootstrap(&self) -> bool {
        command_available("curl")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        let result = printer
            .run_with_output(
                Command::new("bash")
                    .arg("-c")
                    .arg("curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"),
                "Installing Rust via rustup",
            )
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "cargo".into(),
                message: format!("rustup install failed: {}", e),
            })?;
        if !result.status.success() {
            return Err(PackageError::BootstrapFailed {
                manager: "cargo".into(),
                message: "rustup install script failed".into(),
            }
            .into());
        }
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("cargo", cargo_cmd().args(["install", "--list"]), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // `cargo install --list` format: "package_name v1.2.3:" followed by indented binary names
        // We only care about the package names (lines that don't start with whitespace)
        Ok(stdout
            .lines()
            .filter(|l| !l.starts_with(' ') && !l.is_empty())
            .filter_map(|l| l.split_whitespace().next())
            .map(|s| s.to_string())
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            let label = format!("cargo install {}", pkg);
            run_pkg_cmd_live(
                printer,
                "cargo",
                cargo_cmd().args(["install", pkg]),
                &label,
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            let label = format!("cargo uninstall {}", pkg);
            run_pkg_cmd_live(
                printer,
                "cargo",
                cargo_cmd().args(["uninstall", pkg]),
                &label,
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        // cargo install re-installs to update; no separate update command
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // cargo search <pkg> --limit 1 → "package_name = \"version\""
        let output = cargo_cmd()
            .args(["search", package, "--limit", "1"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "cargo".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // First line format: `package_name = "1.2.3"    # description`
        // Only match if the package name exactly matches
        for line in stdout.lines() {
            let parts: Vec<&str> = line.splitn(3, '"').collect();
            if parts.len() >= 2 {
                let name = line.split_whitespace().next().unwrap_or("");
                if name == package {
                    return Ok(Some(parts[1].to_string()));
                }
            }
        }
        Ok(None)
    }

    fn installed_packages_with_versions(&self) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        let output = run_pkg_cmd("cargo", cargo_cmd().args(["install", "--list"]), "list")?;
        Ok(parse_cargo_install_list(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }
}

/// Parse `cargo install --list` output into PackageInfo.
/// Format: non-indented lines are `package_name v1.2.3:`, indented lines are binaries.
pub(super) fn parse_cargo_install_list(stdout: &str) -> Vec<cfgd_core::providers::PackageInfo> {
    stdout
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.is_empty())
        .filter_map(|line| {
            // Format: "package_name v1.2.3:"
            let mut parts = line.splitn(2, ' ');
            let name = parts.next()?.trim();
            let version_raw = parts.next().unwrap_or("").trim().trim_end_matches(':');
            let version = version_raw.strip_prefix('v').unwrap_or(version_raw);
            if name.is_empty() {
                return None;
            }
            Some(cfgd_core::providers::PackageInfo {
                name: name.to_string(),
                version: if version.is_empty() {
                    "unknown".to_string()
                } else {
                    version.to_string()
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use cfgd_core::output::Printer;
    use cfgd_core::providers::PackageManager;

    use super::*;

    #[test]
    fn test_parse_cargo_install_list_basic() {
        let output = "bat v0.24.0:\n    bat\nripgrep v14.1.0:\n    rg\nfd-find v9.0.0:\n    fd\n";
        let pkgs = parse_cargo_install_list(output);
        assert_eq!(pkgs.len(), 3);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "bat" && p.version == "0.24.0")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "ripgrep" && p.version == "14.1.0")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "fd-find" && p.version == "9.0.0")
        );
    }

    #[test]
    fn test_parse_cargo_install_list_strips_v_prefix() {
        let output = "cargo-edit v0.12.2:\n    cargo-add\n    cargo-rm\n";
        let pkgs = parse_cargo_install_list(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "cargo-edit");
        assert_eq!(pkgs[0].version, "0.12.2");
    }

    #[test]
    fn test_parse_cargo_install_list_empty() {
        let pkgs = parse_cargo_install_list("");
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_cargo_install_list_no_version() {
        // A package line without version info
        let output = "some-tool:\n    some-tool\n";
        let pkgs = parse_cargo_install_list(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "some-tool:");
        // This is the actual behavior: "some-tool:" is the first whitespace token
    }

    #[test]
    fn parse_cargo_install_list_skips_indented_lines() {
        // Indented lines are binary names, not packages
        let output = "    binary-name\n";
        let pkgs = parse_cargo_install_list(output);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn cargo_manager_name_and_traits() {
        let mgr = CargoManager;
        assert_eq!(mgr.name(), "cargo");
    }

    #[test]
    fn cargo_manager_update_is_noop() {
        let mgr = CargoManager;
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.update(&printer).unwrap();
    }

    #[test]
    fn parse_cargo_install_list_multiple_binaries() {
        let output = "cargo-edit v0.12.2:\n    cargo-add\n    cargo-rm\n    cargo-upgrade\n    cargo-set-version\n";
        let pkgs = parse_cargo_install_list(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "cargo-edit");
        assert_eq!(pkgs[0].version, "0.12.2");
    }

    #[test]
    fn parse_cargo_install_list_consecutive_packages() {
        let output = "bat v0.24.0:\n    bat\nfd-find v9.0.0:\n    fd\ntokei v12.1.2:\n    tokei\n";
        let pkgs = parse_cargo_install_list(output);
        assert_eq!(pkgs.len(), 3);
        let names: Vec<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"bat"));
        assert!(names.contains(&"fd-find"));
        assert!(names.contains(&"tokei"));
    }

    #[test]
    fn parse_cargo_install_list_real_world_output() {
        // Simulate a real cargo install --list output
        let output = "\
cargo-edit v0.12.2:
    cargo-add
    cargo-rm
    cargo-set-version
    cargo-upgrade
cargo-watch v8.5.2:
    cargo-watch
ripgrep v14.1.0:
    rg
tokei v12.1.2:
    tokei
";
        let pkgs = parse_cargo_install_list(output);
        assert_eq!(pkgs.len(), 4);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "cargo-edit" && p.version == "0.12.2")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "cargo-watch" && p.version == "8.5.2")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "ripgrep" && p.version == "14.1.0")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "tokei" && p.version == "12.1.2")
        );
    }

    #[test]
    fn parse_cargo_install_list_path_installed() {
        // Path-based installs show (path) instead of version
        let output = "my-tool v0.1.0 (/home/user/projects/my-tool):\n    my-tool\nripgrep v14.1.0:\n    rg\n";
        let pkgs = parse_cargo_install_list(output);
        assert_eq!(pkgs.len(), 2);
        // The first entry keeps the full "v0.1.0" after stripping 'v'
        let my_tool = pkgs.iter().find(|p| p.name == "my-tool").unwrap();
        assert_eq!(my_tool.version, "0.1.0 (/home/user/projects/my-tool)");
    }

    #[test]
    fn cargo_manager_can_bootstrap_depends_on_curl() {
        let mgr = CargoManager;
        let can = mgr.can_bootstrap();
        // Should be true if curl is available
        assert_eq!(can, command_available("curl"));
    }

    #[test]
    fn cargo_manager_is_available_checks_cargo() {
        let mgr = CargoManager;
        let available = mgr.is_available();
        assert_eq!(available, cargo_available());
    }

    #[test]
    fn cargo_update_returns_ok() {
        let mgr = CargoManager;
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.update(&printer).unwrap();
    }
}

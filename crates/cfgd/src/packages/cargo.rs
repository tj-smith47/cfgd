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

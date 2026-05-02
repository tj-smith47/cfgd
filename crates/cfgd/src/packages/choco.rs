//! Chocolatey package manager (`choco`).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{run_pkg_cmd, run_pkg_cmd_live};

pub struct ChocolateyManager;

pub(super) fn parse_choco_list(output: &str) -> HashSet<String> {
    let mut packages = HashSet::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("Chocolatey v")
            || line.ends_with("packages installed.")
            || line.ends_with("packages installed.\r")
            || line.ends_with("package installed.")
            || line.ends_with("package installed.\r")
        {
            continue;
        }
        if let Some((name, _version)) = line.split_once(' ') {
            packages.insert(name.to_string());
        }
    }
    packages
}

impl PackageManager for ChocolateyManager {
    fn name(&self) -> &str {
        "chocolatey"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("choco")
    }

    fn can_bootstrap(&self) -> bool {
        true
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "chocolatey",
            Command::new("powershell").args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "Set-ExecutionPolicy Bypass -Scope Process -Force; \
                 [System.Net.ServicePointManager]::SecurityProtocol = \
                 [System.Net.ServicePointManager]::SecurityProtocol -bor 3072; \
                 iex ((New-Object System.Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))",
            ]),
            "Installing Chocolatey",
            "install",
        )?;
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("chocolatey", Command::new("choco").args(["list"]), "list")?;
        Ok(parse_choco_list(&String::from_utf8_lossy(&output.stdout)))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        let mut args = vec!["install", "-y"];
        let pkg_refs: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
        args.extend(pkg_refs);
        run_pkg_cmd_live(
            printer,
            "chocolatey",
            Command::new("choco").args(&args),
            "Installing chocolatey packages",
            "install",
        )?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        let mut args = vec!["uninstall", "-y"];
        let pkg_refs: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
        args.extend(pkg_refs);
        run_pkg_cmd_live(
            printer,
            "chocolatey",
            Command::new("choco").args(&args),
            "Uninstalling chocolatey packages",
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "chocolatey",
            Command::new("choco").args(["upgrade", "all", "-y"]),
            "Upgrading all chocolatey packages",
            "install",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        let output = Command::new("choco")
            .args(["info", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "chocolatey".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_choco_info_version(&stdout))
    }
}

/// Parse version from `choco info <pkg>` output.
/// Looks for "Title: name | VERSION" line.
pub(super) fn parse_choco_info_version(output: &str) -> Option<String> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("Title:")
            && let Some((_name, version)) = rest.rsplit_once('|')
        {
            return Some(version.trim().to_string());
        }
    }
    None
}

//! Scoop package manager (Windows).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{parse_version_field, run_pkg_cmd, run_pkg_cmd_live};

pub struct ScoopManager;

pub(super) fn parse_scoop_list(output: &str) -> HashSet<String> {
    let mut packages = HashSet::new();
    let mut header_passed = false;
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("----") {
            header_passed = true;
            continue;
        }
        if !header_passed || line.is_empty() {
            continue;
        }
        if let Some(name) = line.split_whitespace().next() {
            packages.insert(name.to_string());
        }
    }
    packages
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
        let output = run_pkg_cmd("scoop", Command::new("scoop").arg("list"), "list")?;
        Ok(parse_scoop_list(&String::from_utf8_lossy(&output.stdout)))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            run_pkg_cmd_live(
                printer,
                "scoop",
                Command::new("scoop").args(["install", pkg]),
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
                Command::new("scoop").args(["uninstall", pkg]),
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
            Command::new("scoop").args(["update", "*"]),
            "Upgrading all scoop packages",
            "install",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        let output = Command::new("scoop")
            .args(["info", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "scoop".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_version_field(&stdout))
    }
}

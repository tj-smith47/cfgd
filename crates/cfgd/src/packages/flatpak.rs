//! Flatpak package manager (Linux only).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

#[cfg(target_os = "linux")]
use super::shared::linux_system_manager_available;
use super::shared::{
    bootstrap_via_system_manager, parse_version_field, run_pkg_cmd, run_pkg_cmd_live,
};

pub struct FlatpakManager;

impl PackageManager for FlatpakManager {
    fn name(&self) -> &str {
        "flatpak"
    }

    fn is_available(&self) -> bool {
        command_available("flatpak")
    }

    fn can_bootstrap(&self) -> bool {
        // flatpak is a Linux-only package manager; bootstrappable via apt/dnf/zypper.
        #[cfg(target_os = "linux")]
        {
            linux_system_manager_available()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        bootstrap_via_system_manager(printer, "flatpak", "flatpak")
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd(
            "flatpak",
            Command::new("flatpak").args(["list", "--app", "--columns=application"]),
            "list",
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            let label = format!("flatpak install -y {}", pkg);
            run_pkg_cmd_live(
                printer,
                "flatpak",
                Command::new("flatpak").args(["install", "-y", pkg]),
                &label,
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            let label = format!("flatpak uninstall -y {}", pkg);
            run_pkg_cmd_live(
                printer,
                "flatpak",
                Command::new("flatpak").args(["uninstall", "-y", pkg]),
                &label,
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "flatpak",
            Command::new("flatpak").args(["update", "-y"]),
            "flatpak update -y",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // flatpak remote-info flathub <app-id> → parse "Version:" field
        let output = Command::new("flatpak")
            .args(["remote-info", "flathub", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "flatpak".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_version_field(&stdout))
    }
}

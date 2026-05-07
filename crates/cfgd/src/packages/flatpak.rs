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
        Ok(parse_flatpak_app_list(&String::from_utf8_lossy(
            &output.stdout,
        )))
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

/// Parse `flatpak list --app --columns=application` stdout into a `HashSet`
/// of installed app IDs (one per line, surrounding whitespace stripped,
/// blank lines dropped).
pub(super) fn parse_flatpak_app_list(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use cfgd_core::command_available;
    use cfgd_core::providers::PackageManager;

    #[cfg(target_os = "linux")]
    use super::super::shared::linux_system_manager_available;
    use super::*;

    #[test]
    fn flatpak_manager_name_and_traits() {
        let mgr = FlatpakManager;
        assert_eq!(mgr.name(), "flatpak");
    }

    #[test]
    fn flatpak_manager_can_bootstrap_checks_system_managers() {
        let mgr = FlatpakManager;
        #[cfg(target_os = "linux")]
        assert_eq!(mgr.can_bootstrap(), linux_system_manager_available());
        #[cfg(not(target_os = "linux"))]
        assert!(!mgr.can_bootstrap());
    }

    #[test]
    fn flatpak_manager_is_available_checks_flatpak() {
        let mgr = FlatpakManager;
        let available = mgr.is_available();
        assert_eq!(available, command_available("flatpak"));
    }

    // --- parse_flatpak_app_list ---

    #[test]
    fn parse_flatpak_app_list_collects_app_ids() {
        let stdout = "org.mozilla.firefox\norg.signal.Signal\norg.gimp.GIMP\n";
        let pkgs = parse_flatpak_app_list(stdout);
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.contains("org.mozilla.firefox"));
        assert!(pkgs.contains("org.signal.Signal"));
        assert!(pkgs.contains("org.gimp.GIMP"));
    }

    #[test]
    fn parse_flatpak_app_list_drops_blank_and_whitespace_lines() {
        let stdout = "\n org.mozilla.firefox \n\t  \n\norg.signal.Signal\n";
        let pkgs = parse_flatpak_app_list(stdout);
        assert_eq!(pkgs.len(), 2, "blank/whitespace-only lines must be dropped");
        assert!(
            pkgs.contains("org.mozilla.firefox"),
            "surrounding whitespace must be stripped before insertion"
        );
        assert!(pkgs.contains("org.signal.Signal"));
    }

    #[test]
    fn parse_flatpak_app_list_empty_input_yields_empty_set() {
        assert!(parse_flatpak_app_list("").is_empty());
    }
}

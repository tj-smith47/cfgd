//! Flatpak package manager (Linux only).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

#[cfg(target_os = "linux")]
use super::shared::linux_system_manager_available;
use super::shared::{
    bootstrap_via_system_manager, parse_version_field, resolve_tool_with_fallbacks, run_pkg_cmd,
    run_pkg_cmd_live, tool_cmd_with_resolver,
};

pub struct FlatpakManager;

pub(super) fn find_flatpak() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("flatpak", &[])
}

pub(super) fn flatpak_available() -> bool {
    find_flatpak().is_some()
}

pub(super) fn flatpak_cmd() -> Command {
    tool_cmd_with_resolver("flatpak", find_flatpak)
}

impl PackageManager for FlatpakManager {
    fn name(&self) -> &str {
        "flatpak"
    }

    fn is_available(&self) -> bool {
        flatpak_available()
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
            flatpak_cmd().args(["list", "--app", "--columns=application"]),
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
                flatpak_cmd().args(["install", "-y", pkg]),
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
                flatpak_cmd().args(["uninstall", "-y", pkg]),
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
            flatpak_cmd().args(["update", "-y"]),
            "flatpak update -y",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // flatpak remote-info flathub <app-id> → parse "Version:" field
        let output = flatpak_cmd()
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
    #[serial_test::serial]
    fn flatpak_manager_is_available_checks_flatpak() {
        // Snapshot + clear the seam env var so this assertion mirrors the
        // PATH-only contract. Without this, parallel ToolShim tests setting
        // CFGD_FLATPAK_BIN would race with this assertion.
        let prev = std::env::var_os("CFGD_FLATPAK_BIN");
        // SAFETY: serial.
        unsafe {
            std::env::remove_var("CFGD_FLATPAK_BIN");
        }
        let mgr = FlatpakManager;
        let available = mgr.is_available();
        let expected = command_available("flatpak");
        // SAFETY: serial.
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("CFGD_FLATPAK_BIN", v);
            }
        }
        assert_eq!(available, expected);
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

    // ---------------------------------------------------------------------
    // PackageManager-impl tests via CFGD_FLATPAK_BIN ToolShim.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    mod flatpak_shim {
        use super::*;
        use cfgd_core::providers::PackageManager;
        use cfgd_core::test_helpers::{ToolShim, test_printer_v2};
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_FLATPAK_BIN";

        #[test]
        #[serial]
        fn flatpak_install_runs_install_subcommand_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer_v2();
            FlatpakManager
                .install(
                    &["org.mozilla.firefox".into(), "org.signal.Signal".into()],
                    &p,
                )
                .expect("Ok");
            assert_eq!(s.invocation_count(), 2);
            let argv = s.argv_log();
            assert!(argv.contains("install -y org.mozilla.firefox"));
            assert!(argv.contains("install -y org.signal.Signal"));
        }

        #[test]
        #[serial]
        fn flatpak_uninstall_runs_uninstall_subcommand_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer_v2();
            FlatpakManager
                .uninstall(&["org.mozilla.firefox".into()], &p)
                .expect("Ok");
            assert!(s.argv_log().contains("uninstall -y org.mozilla.firefox"));
        }

        #[test]
        #[serial]
        fn flatpak_update_runs_update_y() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer_v2();
            FlatpakManager.update(&p).expect("Ok");
            assert_eq!(s.invocation_count(), 1);
            assert!(s.argv_log().contains("update -y"), "argv: {}", s.argv_log());
        }

        #[test]
        #[serial]
        fn flatpak_installed_packages_parses_columns_application_output() {
            let stdout = "org.mozilla.firefox\norg.signal.Signal\n";
            let _s = ToolShim::install(SHIM_ENV, 0, stdout, "");
            let pkgs = FlatpakManager.installed_packages().expect("Ok");
            assert_eq!(pkgs.len(), 2);
            assert!(pkgs.contains("org.mozilla.firefox"));
        }

        #[test]
        #[serial]
        fn flatpak_available_version_extracts_version_field_from_remote_info() {
            // remote-info output uses "Version: <X.Y.Z>" lines; parse_version_field
            // is shared and returns the first match.
            let stdout = "Description: Browser\nVersion: 124.0.1\nLicense: MPL\n";
            let s = ToolShim::install(SHIM_ENV, 0, stdout, "");
            let v = FlatpakManager
                .available_version("org.mozilla.firefox")
                .expect("Ok");
            assert_eq!(v.as_deref(), Some("124.0.1"));
            let argv = s.argv_log();
            assert!(
                argv.contains("remote-info flathub org.mozilla.firefox"),
                "argv must include `remote-info flathub <app>`: {argv}"
            );
        }

        #[test]
        #[serial]
        fn flatpak_available_version_returns_none_on_nonzero_exit() {
            let _s = ToolShim::install(SHIM_ENV, 1, "", "no such app on flathub");
            let v = FlatpakManager
                .available_version("nonexistent.app")
                .expect("non-zero → Ok(None)");
            assert_eq!(v, None);
        }
    }
}

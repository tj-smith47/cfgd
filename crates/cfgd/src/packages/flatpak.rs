//! Flatpak package manager (Linux only).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::providers::{BootstrapPlan, PackageContext, PackageManager};

use super::shared::{
    bootstrap_via_system_manager, parse_version_field, resolve_tool_with_fallbacks, run_pkg_cmd,
    run_pkg_cmd_live, tool_cmd_with_resolver,
};
#[cfg(target_os = "linux")]
use super::shared::{detect_system_method, linux_system_manager_available};

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

    fn bootstrap_plan(&self) -> Option<BootstrapPlan> {
        // flatpak is a Linux-only package manager; bootstrappable via apt/dnf/zypper.
        // The client lands on the system PATH, so the plan creates no directory.
        #[cfg(target_os = "linux")]
        {
            linux_system_manager_available().then(|| BootstrapPlan::new(detect_system_method()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    fn bootstrap(&self, cx: &PackageContext<'_>) -> Result<()> {
        bootstrap_via_system_manager(cx, "flatpak", "flatpak")
    }

    fn installed_packages(&self, _cx: &PackageContext<'_>) -> Result<HashSet<String>> {
        let output = run_pkg_cmd(
            "flatpak",
            flatpak_cmd().args(["list", "--app", "--columns=application"]),
            "list",
        )?;
        Ok(parse_flatpak_app_list(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn install(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        for pkg in packages {
            let label = format!("flatpak install -y {}", pkg);
            run_pkg_cmd_live(
                cx,
                "flatpak",
                flatpak_cmd().args(["install", "-y", pkg]),
                &label,
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        for pkg in packages {
            let label = format!("flatpak uninstall -y {}", pkg);
            run_pkg_cmd_live(
                cx,
                "flatpak",
                flatpak_cmd().args(["uninstall", "-y", pkg]),
                &label,
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, cx: &PackageContext<'_>) -> Result<()> {
        // `--appstream` refreshes the remotes' metadata. Without it the same
        // command upgrades every installed app, which no plan asked for.
        run_pkg_cmd_live(
            cx,
            "flatpak",
            flatpak_cmd().args(["update", "--appstream", "-y"]),
            "flatpak update --appstream -y",
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
    fn flatpak_bootstrap_plan_installs_flatpak_from_a_system_manager() {
        let plan = FlatpakManager.bootstrap_plan();
        #[cfg(target_os = "linux")]
        {
            assert_eq!(plan.is_some(), linux_system_manager_available());
            if let Some(plan) = plan {
                // `bootstrap` runs `<system manager> install flatpak`, which puts
                // the client on the system PATH.
                assert_eq!(plan.method, super::super::shared::detect_system_method());
                assert!(plan.requires.is_empty());
                assert!(plan.creates_path_dirs.is_empty());
            }
        }
        #[cfg(not(target_os = "linux"))]
        assert!(plan.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn flatpak_manager_is_available_exactly_when_a_flatpak_binary_resolves() {
        // The seam env var is cleared for the whole test: with it set, this
        // asserts about whichever ToolShim ran last rather than about the
        // PATH probe.
        let _seam = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_FLATPAK_BIN");
        let _path_lock = cfgd_core::test_helpers::path_env_mutation_guard();
        let _dirs = cfgd_core::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let mgr = FlatpakManager;

        {
            let _empty = cfgd_core::test_helpers::EnvVarGuard::set("PATH", "");
            assert!(
                !mgr.is_available(),
                "a host resolving no binaries has no flatpak"
            );
        }

        #[cfg(unix)]
        {
            let _probe = cfgd_core::test_helpers::ProbePath::containing(&["flatpak"]);
            assert!(
                mgr.is_available(),
                "the binary this manager probes for is named `flatpak`"
            );
        }
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
        use cfgd_core::test_helpers::{ToolShim, test_package_context, test_printer, test_state};
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_FLATPAK_BIN";

        #[test]
        #[serial]
        fn flatpak_install_runs_install_subcommand_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            FlatpakManager
                .install(
                    &["org.mozilla.firefox".into(), "org.signal.Signal".into()],
                    &cx,
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
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            FlatpakManager
                .uninstall(&["org.mozilla.firefox".into()], &cx)
                .expect("Ok");
            assert!(s.argv_log().contains("uninstall -y org.mozilla.firefox"));
        }

        #[test]
        #[serial]
        fn flatpak_refresh_updates_appstream_without_upgrading_apps() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            assert!(
                FlatpakManager.has_index(),
                "remote appstream data is an index"
            );
            FlatpakManager.refresh_index(&cx).expect("Ok");
            assert_eq!(s.invocation_count(), 1);
            let argv = s.argv_log();
            assert!(
                argv.contains("update --appstream -y"),
                "a bare `flatpak update -y` upgrades every installed app: {argv}"
            );
        }

        #[test]
        #[serial]
        fn flatpak_installed_packages_parses_columns_application_output() {
            let stdout = "org.mozilla.firefox\norg.signal.Signal\n";
            let _s = ToolShim::install(SHIM_ENV, 0, stdout, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = FlatpakManager.installed_packages(&cx).expect("Ok");
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

//! Snap package manager (Linux only).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

#[cfg(target_os = "linux")]
use super::shared::linux_system_manager_available;
use super::shared::{
    bootstrap_via_system_manager, resolve_tool_with_fallbacks, run_pkg_cmd, run_pkg_cmd_live,
    sudo_cmd_with_seam, tool_cmd_with_resolver,
};

pub struct SnapManager;

pub(super) fn find_snap() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("snap", &[])
}

pub(super) fn snap_available() -> bool {
    find_snap().is_some()
}

pub(super) fn snap_cmd() -> Command {
    tool_cmd_with_resolver("snap", find_snap)
}

impl PackageManager for SnapManager {
    fn name(&self) -> &str {
        "snap"
    }

    fn is_available(&self) -> bool {
        snap_available()
    }

    fn can_bootstrap(&self) -> bool {
        // snap is a Linux-only package manager; bootstrappable via apt/dnf/zypper.
        // On non-Linux platforms it is never available.
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
        bootstrap_via_system_manager(printer, "snapd", "snap")
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("snap", snap_cmd().args(["list"]), "list")?;
        Ok(parse_snap_list(&String::from_utf8_lossy(&output.stdout)))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        // Snap requires individual install commands for --classic flag per package
        for pkg in packages {
            let label = format!("snap install {}", pkg);
            let result = run_pkg_cmd_live(
                printer,
                "snap",
                sudo_cmd_with_seam("snap").arg("install").arg(pkg),
                &label,
                "install",
            );
            if let Err(ref e) = result {
                // If install fails and stderr mentions classic confinement, retry with --classic
                if e.to_string().contains("classic") {
                    let label = format!("snap install --classic {}", pkg);
                    run_pkg_cmd_live(
                        printer,
                        "snap",
                        sudo_cmd_with_seam("snap").args(["install", "--classic", pkg]),
                        &label,
                        "install",
                    )?;
                } else {
                    result?;
                }
            }
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let label = format!("snap remove {}", packages.join(" "));
        run_pkg_cmd_live(
            printer,
            "snap",
            sudo_cmd_with_seam("snap").arg("remove").args(packages),
            &label,
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "snap",
            sudo_cmd_with_seam("snap").arg("refresh"),
            "snap refresh",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // snap info <pkg> → parse "latest/stable:" or first channel line for version
        let output = snap_cmd().args(["info", package]).output().map_err(|e| {
            PackageError::CommandFailed {
                manager: "snap".into(),
                source: e,
            }
        })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_snap_info_version(&stdout))
    }
}

/// Parse `snap list` stdout into a `HashSet` of installed package names.
///
/// `snap list` emits a header row (`Name Version Rev Tracking Publisher Notes`)
/// followed by one row per snap; the first whitespace-separated token in each
/// data row is the snap name. We unconditionally skip the first line — empty
/// installations still emit a header (or an empty stdout, in which case the
/// `skip(1)` no-op is safe).
pub(super) fn parse_snap_list(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next().map(|s| s.to_string()))
        .collect()
}

/// Parse version from `snap info` output.
/// Looks for "latest/stable:" or "stable:" channel lines.
/// Format: "latest/stable: 0.10.2 2024-01-01 (1234) 12MB classic"
pub(super) fn parse_snap_info_version(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("latest/stable:") || trimmed.starts_with("stable:") {
            let parts: Vec<&str> = trimmed.splitn(2, ':').collect();
            if parts.len() == 2 {
                let version = parts[1].split_whitespace().next().unwrap_or("");
                if !version.is_empty() && version != "^" && version != "--" {
                    return Some(version.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use cfgd_core::command_available;
    use cfgd_core::providers::PackageManager;

    #[cfg(target_os = "linux")]
    use super::super::shared::linux_system_manager_available;
    use super::*;

    #[test]
    fn snap_manager_name_and_traits() {
        let mgr = SnapManager;
        assert_eq!(mgr.name(), "snap");
    }

    #[test]
    fn parse_snap_info_version_latest_stable() {
        let output = "\
name:      ripgrep
summary:   Fast recursive search
publisher: BurntSushi
store-url: https://snapcraft.io/ripgrep
license:   MIT
description: |
  ripgrep is a line-oriented search tool.
channels:
  latest/stable:    14.1.0 2024-03-15 (234) 5MB classic
  latest/candidate: 14.1.1 2024-04-01 (240) 5MB classic
  latest/beta:      ↑
  latest/edge:      ↑";
        assert_eq!(parse_snap_info_version(output), Some("14.1.0".to_string()));
    }

    #[test]
    fn parse_snap_info_version_stable_without_latest_prefix() {
        let output = "channels:\n  stable:    2.0.3 2024-01-01 (100) 10MB -\n";
        assert_eq!(parse_snap_info_version(output), Some("2.0.3".to_string()));
    }

    #[test]
    fn parse_snap_info_version_no_stable_channel() {
        let output = "channels:\n  latest/edge: 0.1.0-dev 2024-01-01 (1) 1MB -\n";
        assert_eq!(parse_snap_info_version(output), None);
    }

    #[test]
    fn parse_snap_info_version_caret_placeholder() {
        // "^" means "same as above" — not a real version
        let output = "channels:\n  latest/stable:    ^ 2024-01-01 (1) 1MB -\n";
        assert_eq!(parse_snap_info_version(output), None);
    }

    #[test]
    fn parse_snap_info_version_dash_placeholder() {
        let output = "channels:\n  latest/stable:    -- 2024-01-01\n";
        assert_eq!(parse_snap_info_version(output), None);
    }

    #[test]
    fn parse_snap_info_version_picks_stable_over_candidate() {
        // Real snap info output has multiple channels — must pick stable
        let output = "\
channels:
  latest/candidate: 15.0.0-rc1 2024-04-01 (240) 5MB classic
  latest/stable:    14.1.0 2024-03-15 (234) 5MB classic
  latest/beta:      ↑";
        assert_eq!(
            parse_snap_info_version(output),
            Some("14.1.0".to_string()),
            "should pick stable even when candidate appears first"
        );
    }

    #[test]
    fn parse_snap_info_version_empty_string() {
        assert_eq!(parse_snap_info_version(""), None);
    }

    #[test]
    fn parse_snap_info_version_stable_empty_after_colon() {
        let output = "channels:\n  latest/stable:\n";
        assert_eq!(parse_snap_info_version(output), None);
    }

    #[test]
    fn parse_snap_info_version_complex_version_string() {
        let output = "channels:\n  latest/stable:    0.10.2-alpha.1 2024-01-01 (100) 5MB -\n";
        assert_eq!(
            parse_snap_info_version(output),
            Some("0.10.2-alpha.1".to_string())
        );
    }

    #[test]
    fn parse_snap_info_version_real_world_full() {
        let output = "\
name:      core
summary:   snapd runtime environment
publisher: Canonical**
store-url: https://snapcraft.io/core
contact:   https://github.com/snapcore/snapd
license:   unset
description: |
  The core runtime environment for snapd
snap-id: 99T7MUlRhtI3U0QFgl5mXXESAiSwt776
channels:
  latest/stable:    16-2.61.3 2024-03-01 (17200) 112MB -
  latest/candidate: 16-2.61.4 2024-04-01 (17250) 112MB -
  latest/beta:      ↑
  latest/edge:      16-2.62-dev 2024-04-05 (17260) 112MB -
";
        assert_eq!(
            parse_snap_info_version(output),
            Some("16-2.61.3".to_string())
        );
    }

    #[test]
    fn snap_manager_can_bootstrap_checks_system_managers() {
        let mgr = SnapManager;
        #[cfg(target_os = "linux")]
        assert_eq!(mgr.can_bootstrap(), linux_system_manager_available());
        #[cfg(not(target_os = "linux"))]
        assert!(!mgr.can_bootstrap());
    }

    #[test]
    #[serial_test::serial]
    fn snap_manager_is_available_checks_snap() {
        // Snapshot + clear seam env var so this assertion mirrors the
        // PATH-only contract. Without this, parallel ToolShim tests setting
        // CFGD_SNAP_BIN would race with this assertion.
        let prev = std::env::var_os("CFGD_SNAP_BIN");
        // SAFETY: serial.
        unsafe {
            std::env::remove_var("CFGD_SNAP_BIN");
        }
        let mgr = SnapManager;
        let available = mgr.is_available();
        let expected = command_available("snap");
        // SAFETY: serial.
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("CFGD_SNAP_BIN", v);
            }
        }
        assert_eq!(available, expected);
    }

    // --- parse_snap_list ---

    #[test]
    fn parse_snap_list_skips_header_and_returns_first_token() {
        let stdout = "\
Name      Version  Rev    Tracking       Publisher     Notes
core22    20240124 1100   latest/stable  canonical**   base
ripgrep   14.1.0   234    latest/stable  burntsushi    classic
fd        9.0.0    100    latest/stable  -             -
";
        let pkgs = parse_snap_list(stdout);
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.contains("core22"));
        assert!(pkgs.contains("ripgrep"));
        assert!(pkgs.contains("fd"));
    }

    #[test]
    fn parse_snap_list_empty_input_yields_empty_set() {
        // Empty stdout — `skip(1)` over zero lines yields no elements.
        assert!(parse_snap_list("").is_empty());
    }

    #[test]
    fn parse_snap_list_only_header_yields_empty_set() {
        // An installation with no snaps still emits the header line.
        let stdout = "Name  Version  Rev  Tracking  Publisher  Notes\n";
        assert!(parse_snap_list(stdout).is_empty());
    }

    #[test]
    fn parse_snap_list_drops_blank_data_rows() {
        let stdout = "Name  Version\n\ncore22  20240124  1100\n\n";
        let pkgs = parse_snap_list(stdout);
        assert_eq!(pkgs.len(), 1);
        assert!(pkgs.contains("core22"));
    }

    // ---------------------------------------------------------------------
    // PackageManager-impl tests via CFGD_SNAP_BIN ToolShim. The seam wires
    // through sudo_cmd_with_seam: when CFGD_SNAP_BIN is set, the install /
    // uninstall / update paths skip sudo entirely and invoke the shim
    // directly. Read-only paths (snap_cmd / installed_packages /
    // available_version) honor the seam via tool_cmd_with_resolver.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    mod snap_shim {
        use super::*;
        use cfgd_core::providers::PackageManager;
        use cfgd_core::test_helpers::{ToolShim, test_printer};
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_SNAP_BIN";

        #[test]
        #[serial]
        fn snap_install_runs_install_subcommand_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            SnapManager
                .install(&["ripgrep".into(), "fd".into()], &p)
                .expect("Ok");
            assert_eq!(s.invocation_count(), 2);
            let argv = s.argv_log();
            assert!(
                argv.contains("install ripgrep"),
                "argv must include `install ripgrep`: {argv}"
            );
            assert!(
                argv.contains("install fd"),
                "argv must include `install fd`: {argv}"
            );
        }

        #[test]
        #[serial]
        fn snap_install_retries_with_classic_when_first_attempt_complains_classic() {
            // Shim exits non-zero with stderr containing "classic" → the
            // install branch's `e.to_string().contains("classic")` matches
            // (because run_pkg_cmd_live now surfaces stderr in the error
            // message) and a second attempt is fired with `--classic`. The
            // shim is the same for both attempts, so the second also fails
            // — we only assert that both argvs landed.
            let s = ToolShim::install(
                SHIM_ENV,
                1,
                "",
                "snap \"ripgrep\" requires classic confinement",
            );
            let p = test_printer();
            let _ = SnapManager.install(&["ripgrep".into()], &p);
            assert_eq!(
                s.invocation_count(),
                2,
                "retry must fire on classic-confinement stderr; got argv: {}",
                s.argv_log()
            );
            let argv = s.argv_log();
            assert!(
                argv.contains("install ripgrep"),
                "first attempt argv must be `install ripgrep`: {argv}"
            );
            assert!(
                argv.contains("install --classic ripgrep"),
                "retry argv must be `install --classic ripgrep`: {argv}"
            );
        }

        #[test]
        #[serial]
        fn snap_uninstall_runs_remove_with_all_packages_in_one_invocation() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            SnapManager
                .uninstall(&["ripgrep".into(), "fd".into()], &p)
                .expect("Ok");
            assert_eq!(s.invocation_count(), 1, "snap remove batches all pkgs");
            let argv = s.argv_log();
            assert!(
                argv.contains("remove ripgrep fd"),
                "argv must batch all packages on a single `remove`: {argv}"
            );
        }

        #[test]
        #[serial]
        fn snap_uninstall_is_noop_when_packages_empty() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            SnapManager.uninstall(&[], &p).expect("Ok");
            assert_eq!(s.invocation_count(), 0, "no command spawned for empty");
        }

        #[test]
        #[serial]
        fn snap_update_runs_refresh() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            SnapManager.update(&p).expect("Ok");
            assert_eq!(s.invocation_count(), 1);
            assert!(s.argv_log().contains("refresh"), "argv: {}", s.argv_log());
        }

        #[test]
        #[serial]
        fn snap_installed_packages_parses_list_output() {
            let stdout = "\
Name      Version  Rev   Tracking       Publisher    Notes
core22    20240124 1100  latest/stable  canonical**  base
ripgrep   14.1.0   234   latest/stable  burntsushi   classic
";
            let _s = ToolShim::install(SHIM_ENV, 0, stdout, "");
            let pkgs = SnapManager.installed_packages().expect("Ok");
            assert!(pkgs.contains("core22"));
            assert!(pkgs.contains("ripgrep"));
        }

        #[test]
        #[serial]
        fn snap_available_version_extracts_latest_stable_channel_version() {
            let stdout = "\
name: ripgrep
summary: ripgrep
channels:
  latest/stable:    14.1.0 2024-03-01 (234) 12MB classic
";
            let s = ToolShim::install(SHIM_ENV, 0, stdout, "");
            let v = SnapManager.available_version("ripgrep").expect("Ok");
            assert_eq!(v.as_deref(), Some("14.1.0"));
            assert!(
                s.argv_log().contains("info ripgrep"),
                "argv must include `info <pkg>`: {}",
                s.argv_log()
            );
        }

        #[test]
        #[serial]
        fn snap_available_version_returns_none_on_nonzero_exit() {
            let _s = ToolShim::install(SHIM_ENV, 1, "", "no such snap");
            let v = SnapManager
                .available_version("nonexistent")
                .expect("non-zero → Ok(None)");
            assert_eq!(v, None);
        }
    }
}

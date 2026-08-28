//! Snap package manager (Linux only).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::providers::{BootstrapPlan, PackageManager};

#[cfg(target_os = "linux")]
use super::shared::detect_system_method;
use super::shared::{
    MediatedArms, bootstrap_via_system_manager, resolve_tool_with_fallbacks, run_pkg_cmd,
    run_pkg_cmd_live, sudo_cmd_with_seam, system_manager_arms, tool_cmd_with_resolver,
};

pub struct SnapManager;

/// What a mediator installs to deliver snap. There is no brew arm: snapd is a
/// Linux service, not a formula.
const SNAP_MEDIATED: MediatedArms = system_manager_arms(None, &["snapd"]);

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

    fn tool_version(&self) -> Option<String> {
        super::shared::tool_version_from(snap_cmd().arg("version"))
    }

    fn is_available(&self) -> bool {
        snap_available()
    }

    fn bootstrap_plan_given(&self, delivered: &dyn Fn(&str) -> bool) -> Option<BootstrapPlan> {
        // snap is a Linux-only package manager; bootstrappable via apt/dnf/zypper.
        // On non-Linux platforms it is never available. `snapd` puts the client
        // on the system PATH, so the plan creates no directory of its own.
        #[cfg(target_os = "linux")]
        {
            // `None` rather than a hopeful name when no system manager can run
            // it: the method a plan carries is binding at execution.
            detect_system_method(delivered).map(BootstrapPlan::new)
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    fn bootstrap(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        bootstrap_via_system_manager(cx, SNAP_MEDIATED.system[0], "snap")
    }

    fn mediated_packages(&self, via: &str) -> Option<Vec<String>> {
        SNAP_MEDIATED.packages_for(via)
    }

    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("snap", snap_cmd().args(["list"]), "list")?;
        Ok(parse_snap_list(&String::from_utf8_lossy(&output.stdout)))
    }

    fn install(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        // Snap requires individual install commands for --classic flag per package
        for pkg in packages {
            let label = format!("snap install {}", pkg);
            let result = run_pkg_cmd_live(
                cx,
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
                        cx,
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

    fn uninstall(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let label = format!("snap remove {}", packages.join(" "));
        run_pkg_cmd_live(
            cx,
            "snap",
            sudo_cmd_with_seam("snap").arg("remove").args(packages),
            &label,
            "uninstall",
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
    use cfgd_core::providers::PackageManager;
    use cfgd_core::providers::PackageManagerExt;

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
    fn snap_bootstrap_plan_installs_snapd_from_a_system_manager() {
        // Both the plan detection and the `runnable` probes below assert
        // successful PATH resolutions, so hold the read guard across them —
        // a sibling test empties PATH under the write guard.
        let _path = cfgd_core::test_helpers::path_env_read_guard();
        let plan = SnapManager.bootstrap_plan();
        #[cfg(target_os = "linux")]
        {
            // Ground truth spelled out here rather than read back from the
            // detector: a plan's method is BINDING at execution, so the one
            // thing worth asserting is that this host can actually spawn the
            // manager the plan names — and that a runnable one is never
            // dropped. Probed through the same seams `sudo_cmd_with_seam`
            // spawns from, so a concurrently-installed shim cannot make the
            // two answers disagree.
            let runnable = |tool: &str| {
                cfgd_core::command_available_with_seam(
                    &format!("CFGD_{}_BIN", tool.to_uppercase().replace('-', "_")),
                    tool,
                )
            };
            let arm_of = |method: &str| match method {
                "apt" => Some("apt-get"),
                "dnf" => Some("dnf"),
                "zypper" => Some("zypper"),
                _ => None,
            };
            match plan {
                Some(plan) => {
                    // `bootstrap` runs `<system manager> install snapd`, which
                    // puts the client on the system PATH.
                    let tool = arm_of(&plan.method)
                        .unwrap_or_else(|| panic!("unknown method {}", plan.method));
                    assert!(
                        runnable(tool),
                        "a plan may only name a manager this host can run, got {}",
                        plan.method
                    );
                    assert!(plan.requires.is_empty());
                    assert!(plan.creates_path_dirs.is_empty());
                }
                None => assert!(
                    !["apt-get", "dnf", "zypper"].into_iter().any(runnable),
                    "a runnable system manager must not be answered with no plan"
                ),
            }
        }
        #[cfg(not(target_os = "linux"))]
        assert!(plan.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn snap_manager_is_available_exactly_when_a_snap_binary_resolves() {
        // The seam env var is cleared for the whole test: with it set, this
        // asserts about whichever ToolShim ran last rather than about the
        // PATH probe.
        let _seam = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_SNAP_BIN");
        let _path_lock = cfgd_core::test_helpers::path_env_mutation_guard();
        let _dirs = cfgd_core::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let mgr = SnapManager;

        {
            let _empty = cfgd_core::test_helpers::EnvVarGuard::set("PATH", "");
            assert!(
                !mgr.is_available(),
                "a host resolving no binaries has no snap"
            );
        }

        #[cfg(unix)]
        {
            let _probe = cfgd_core::test_helpers::ProbePath::containing(&["snap"]);
            assert!(
                mgr.is_available(),
                "the binary this manager probes for is named `snap`"
            );
        }
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
        use cfgd_core::test_helpers::{ToolShim, test_package_context, test_printer, test_state};
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_SNAP_BIN";

        #[test]
        #[serial]
        fn snap_install_runs_install_subcommand_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            SnapManager
                .install(&["ripgrep".into(), "fd".into()], &cx)
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
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let _ = SnapManager.install(&["ripgrep".into()], &cx);
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
            let st = test_state();
            let cx = test_package_context(&p, &st);
            SnapManager
                .uninstall(&["ripgrep".into(), "fd".into()], &cx)
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
            let st = test_state();
            let cx = test_package_context(&p, &st);
            SnapManager.uninstall(&[], &cx).expect("Ok");
            assert_eq!(s.invocation_count(), 0, "no command spawned for empty");
        }

        #[test]
        #[serial]
        fn snap_declares_no_index_and_refreshing_upgrades_nothing() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            assert!(!SnapManager.has_index(), "snapd tracks channels itself");
            SnapManager.refresh_index(&cx).expect("Ok");
            assert_eq!(
                s.invocation_count(),
                0,
                "a bare `snap refresh` upgrades every installed snap: {}",
                s.argv_log()
            );
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
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = SnapManager.installed_packages(&cx).expect("Ok");
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

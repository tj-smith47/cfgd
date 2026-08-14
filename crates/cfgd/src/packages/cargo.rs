//! Cargo-based package manager (`cargo install`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::providers::{BootstrapPlan, PackageManager};

use super::shared::{
    bootstrap_via_shell_script, home_relative_dir, resolve_tool_with_fallbacks, run_pkg_cmd,
    run_pkg_cmd_live, tool_cmd_with_resolver,
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

// Single source for the rustup-installed bin dir, so `bootstrap_plan`'s
// declaration and `path_dirs`'s recording can never drift apart.
fn cargo_bin_dir() -> Option<PathBuf> {
    home_relative_dir("~/.cargo/bin")
}

impl PackageManager for CargoManager {
    fn name(&self) -> &str {
        "cargo"
    }

    fn is_available(&self) -> bool {
        cargo_available()
    }

    fn bootstrap_plan(&self) -> Option<BootstrapPlan> {
        Some(
            BootstrapPlan::new("rustup")
                .requiring(["curl"])
                .creating(cargo_bin_dir()),
        )
    }

    // Reads `cargo_bin_dir()` directly rather than the recorded state row: the
    // rustup cascade always lands cargo in the same `~`-relative place, so
    // there is nothing a live probe would learn that the plan's own
    // declaration does not already know.
    fn path_dirs(&self, _cx: &cfgd_core::providers::PackageContext<'_>) -> Vec<String> {
        cargo_bin_dir()
            .into_iter()
            .map(cfgd_core::to_posix_string)
            .collect()
    }

    fn bootstrap(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        bootstrap_via_shell_script(
            cx,
            "cargo",
            "Installing Rust via rustup",
            "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y",
        )
    }

    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("cargo", cargo_cmd().args(["install", "--list"]), "list")?;
        Ok(parse_cargo_install_list_packages(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn install(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        for pkg in packages {
            let label = format!("cargo install {}", pkg);
            run_pkg_cmd_live(
                cx,
                "cargo",
                cargo_cmd().args(["install", pkg]),
                &label,
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        for pkg in packages {
            let label = format!("cargo uninstall {}", pkg);
            run_pkg_cmd_live(
                cx,
                "cargo",
                cargo_cmd().args(["uninstall", pkg]),
                &label,
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn update(&self, _cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
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
        Ok(parse_cargo_search_version(
            &String::from_utf8_lossy(&output.stdout),
            package,
        ))
    }

    fn installed_packages_with_versions(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        let output = run_pkg_cmd("cargo", cargo_cmd().args(["install", "--list"]), "list")?;
        Ok(parse_cargo_install_list(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }
}

/// Parse `cargo install --list` stdout into a name-only `HashSet`.
/// Non-indented lines are `package_name v1.2.3:`; indented lines are binary
/// names (skipped). Empty lines are also skipped.
pub(super) fn parse_cargo_install_list_packages(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
        .collect()
}

/// Parse `cargo search <pkg> --limit 1` stdout for the matching package.
/// Each line looks like `name = "1.2.3"    # description`. Returns the
/// quoted version only when the line's leading token matches `expected`
/// exactly — partial / prefix matches do not count, since `cargo search`
/// can return adjacent crates that share a prefix with the query.
pub(super) fn parse_cargo_search_version(stdout: &str, expected: &str) -> Option<String> {
    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, '"').collect();
        if parts.len() >= 2 {
            let name = line.split_whitespace().next().unwrap_or("");
            if name == expected {
                return Some(parts[1].to_string());
            }
        }
    }
    None
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
        let printer = cfgd_core::test_helpers::test_printer();
        let state = cfgd_core::test_helpers::test_state();
        let cx = cfgd_core::test_helpers::test_package_context(&printer, &state);
        mgr.update(&cx).unwrap();
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
    fn cargo_bootstrap_plan_names_rustup_curl_and_the_cargo_bin_dir() {
        // Both sides read `PATH`; without the guard a concurrent test's
        // `PATH` mutation can land between them and they disagree.
        let _path = cfgd_core::test_helpers::path_env_read_guard();
        let home = tempfile::tempdir().unwrap();
        let plan = cfgd_core::with_test_home(home.path(), || CargoManager.bootstrap_plan());
        // The plan is unconditional: it describes what the cascade needs, and
        // whether this host can carry that out is `feasible_bootstrap_plan`'s
        // question — so the planner can name `curl` as the cause when it
        // cannot.
        assert!(plan.is_some());
        let Some(plan) = plan else { return };
        // What `bootstrap` runs: rustup's install script, fetched with curl,
        // landing cargo (and everything `cargo install` builds) in ~/.cargo/bin.
        assert_eq!(plan.method, "rustup");
        assert_eq!(plan.requires, ["curl"]);
        assert_eq!(
            plan.creates_path_dirs,
            [cfgd_core::to_posix_string(home.path().join(".cargo/bin"))]
        );
    }

    #[test]
    fn cargo_path_dirs_matches_the_bootstrap_plans_declaration() {
        let _path = cfgd_core::test_helpers::path_env_read_guard();
        let home = tempfile::tempdir().unwrap();
        cfgd_core::with_test_home(home.path(), || {
            let plan = CargoManager.bootstrap_plan().unwrap();
            let printer = cfgd_core::test_helpers::test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = cfgd_core::test_helpers::test_package_context(&printer, &state);
            let mgr: Box<dyn PackageManager> = Box::new(CargoManager);
            assert_eq!(mgr.path_dirs(&cx), plan.creates_path_dirs);
        });
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
        let printer = cfgd_core::test_helpers::test_printer();
        let state = cfgd_core::test_helpers::test_state();
        let cx = cfgd_core::test_helpers::test_package_context(&printer, &state);
        mgr.update(&cx).unwrap();
    }

    // --- parse_cargo_install_list_packages ---

    #[test]
    fn parse_cargo_install_list_packages_extracts_top_level_names() {
        let stdout = "bat v0.24.0:\n    bat\nripgrep v14.1.0:\n    rg\nfd-find v9.0.0:\n    fd\n";
        let pkgs = parse_cargo_install_list_packages(stdout);
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.contains("bat"));
        assert!(pkgs.contains("ripgrep"));
        assert!(pkgs.contains("fd-find"));
    }

    #[test]
    fn parse_cargo_install_list_packages_skips_indented_binary_names() {
        // Indented lines are binary names installed by the package — they
        // should never end up in the package set.
        let stdout = "ripgrep v14.1.0:\n    rg\n    rg-extra\n";
        let pkgs = parse_cargo_install_list_packages(stdout);
        assert_eq!(pkgs.len(), 1);
        assert!(pkgs.contains("ripgrep"));
        assert!(!pkgs.contains("rg"));
    }

    #[test]
    fn parse_cargo_install_list_packages_drops_empty_lines() {
        let stdout = "\nbat v0.24.0:\n    bat\n\nripgrep v14.1.0:\n    rg\n\n";
        let pkgs = parse_cargo_install_list_packages(stdout);
        assert_eq!(pkgs.len(), 2);
    }

    #[test]
    fn parse_cargo_install_list_packages_empty_input_yields_empty_set() {
        assert!(parse_cargo_install_list_packages("").is_empty());
    }

    // --- parse_cargo_search_version ---

    #[test]
    fn parse_cargo_search_version_extracts_quoted_version() {
        let stdout = "ripgrep = \"14.1.0\"    # ripgrep recursively searches\n";
        let v = parse_cargo_search_version(stdout, "ripgrep");
        assert_eq!(v.as_deref(), Some("14.1.0"));
    }

    #[test]
    fn parse_cargo_search_version_returns_none_when_name_does_not_match() {
        // cargo search returns adjacent crates; do not return their version.
        let stdout = "ripgrep-all = \"0.10.5\"    # different crate\n";
        let v = parse_cargo_search_version(stdout, "ripgrep");
        assert!(
            v.is_none(),
            "name match must be exact — prefix matches must not pass"
        );
    }

    #[test]
    fn parse_cargo_search_version_returns_none_on_empty_input() {
        assert!(parse_cargo_search_version("", "anything").is_none());
    }

    #[test]
    fn parse_cargo_search_version_returns_none_when_no_quote_pair() {
        // No '"' in the line — splitn(3,'"') returns a single element vec.
        let stdout = "ripgrep some text without quotes\n";
        let v = parse_cargo_search_version(stdout, "ripgrep");
        assert!(v.is_none());
    }

    #[test]
    fn parse_cargo_search_version_picks_first_exact_match() {
        // Multi-line stdout where the second line is the exact match.
        let stdout = "rg = \"0.0.1\"\nripgrep = \"14.1.0\"\n";
        let v = parse_cargo_search_version(stdout, "ripgrep");
        assert_eq!(v.as_deref(), Some("14.1.0"));
    }

    // ---------------------------------------------------------------------
    // PackageManager-impl tests via CFGD_CARGO_BIN ToolShim.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    mod cargo_shim {
        use super::*;
        use cfgd_core::providers::PackageManager;
        use cfgd_core::test_helpers::{ToolShim, test_package_context, test_printer, test_state};
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_CARGO_BIN";

        #[test]
        #[serial]
        fn cargo_install_runs_install_subcommand_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            CargoManager
                .install(&["ripgrep".into(), "fd-find".into()], &cx)
                .expect("Ok");
            assert_eq!(s.invocation_count(), 2);
            let argv = s.argv_log();
            assert!(argv.contains("install ripgrep"));
            assert!(argv.contains("install fd-find"));
        }

        #[test]
        #[serial]
        fn cargo_uninstall_runs_uninstall_subcommand_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            CargoManager
                .uninstall(&["ripgrep".into()], &cx)
                .expect("Ok");
            assert!(s.argv_log().contains("uninstall ripgrep"));
        }

        #[test]
        #[serial]
        fn cargo_update_is_noop_no_command_spawned() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            CargoManager.update(&cx).expect("Ok");
            assert_eq!(
                s.invocation_count(),
                0,
                "cargo update is documented no-op (re-install is the convention)"
            );
        }

        #[test]
        #[serial]
        fn cargo_installed_packages_parses_install_list_output() {
            let stdout = "ripgrep v14.1.0:\n    rg\nfd-find v9.0.0:\n    fd\n";
            let _s = ToolShim::install(SHIM_ENV, 0, stdout, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = CargoManager.installed_packages(&cx).expect("Ok");
            assert_eq!(pkgs.len(), 2);
            assert!(pkgs.contains("ripgrep"));
            assert!(pkgs.contains("fd-find"));
        }

        #[test]
        #[serial]
        fn cargo_available_version_extracts_from_search_first_line() {
            let _s = ToolShim::install(SHIM_ENV, 0, "ripgrep = \"14.1.0\"\n", "");
            let v = CargoManager.available_version("ripgrep").expect("Ok");
            assert_eq!(v.as_deref(), Some("14.1.0"));
        }

        #[test]
        #[serial]
        fn cargo_available_version_passes_search_with_limit_flag() {
            let s = ToolShim::install(SHIM_ENV, 0, "x = \"0.1\"\n", "");
            CargoManager.available_version("ripgrep").expect("Ok");
            assert!(
                s.argv_log().contains("search ripgrep --limit 1"),
                "argv: {}",
                s.argv_log()
            );
        }

        #[test]
        #[serial]
        fn cargo_available_version_returns_none_on_nonzero_exit() {
            let _s = ToolShim::install(SHIM_ENV, 1, "", "registry unreachable");
            let v = CargoManager
                .available_version("anything")
                .expect("non-zero → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn cargo_installed_packages_with_versions_parses_install_list() {
            let stdout = "ripgrep v14.1.0:\n    rg\nfd-find v9.0.0:\n    fd\n";
            let _s = ToolShim::install(SHIM_ENV, 0, stdout, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = CargoManager
                .installed_packages_with_versions(&cx)
                .expect("Ok");
            let rg = pkgs
                .iter()
                .find(|p| p.name == "ripgrep")
                .expect("ripgrep present");
            assert_eq!(rg.version, "14.1.0");
            let fd = pkgs
                .iter()
                .find(|p| p.name == "fd-find")
                .expect("fd-find present");
            assert_eq!(fd.version, "9.0.0");
        }

        use cfgd_core::test_helpers::EnvVarGuard;

        /// Point the seam env-var at a non-existent path so the spawned
        /// `Command` fails with ENOENT, exercising the `CommandFailed` map_err
        /// arm in `available_version` (which calls `.output()` directly).
        #[test]
        #[serial]
        fn cargo_available_version_spawn_failure_maps_to_command_failed() {
            let _g = EnvVarGuard::set(SHIM_ENV, "/nonexistent/cfgd-cargo-shim-does-not-exist");
            let err = CargoManager
                .available_version("ripgrep")
                .expect_err("ENOENT spawn must surface as CommandFailed, not a panic");
            assert!(
                matches!(err, cfgd_core::errors::CfgdError::Package(
                    PackageError::CommandFailed { ref manager, .. }) if manager == "cargo"),
                "spawn failure must be PackageError::CommandFailed{{manager:\"cargo\"}}, got: {err:?}"
            );
        }

        use cfgd_core::test_helpers::install_named_path_shim;

        #[test]
        #[serial]
        fn cargo_bootstrap_runs_sh_rustup_pipeline_ok() {
            let (_bin, _path) = install_named_path_shim("sh", 0, "", "");
            let p = test_printer();
            CargoManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect("bootstrap Ok via shim");
        }

        #[test]
        #[serial]
        fn cargo_bootstrap_propagates_nonzero_exit_as_bootstrap_failed() {
            let (_bin, _path) = install_named_path_shim("sh", 1, "", "rustup unreachable");
            let p = test_printer();
            let err = CargoManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect_err("non-zero sh must error");
            let msg = err.to_string();
            assert!(
                msg.contains("rustup") || msg.contains("bootstrap"),
                "error must surface bootstrap context: {msg}"
            );
        }
    }
}

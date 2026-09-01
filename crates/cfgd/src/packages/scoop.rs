//! Scoop package manager (Windows).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::providers::{BootstrapPlan, PackageContext, PackageInfo, PackageManager};

use super::shared::{
    canonical_ci_pkg_name, home_relative_dir, install_batch_then_per_package, parse_version_field,
    resolve_tool_with_fallbacks, run_pkg_cmd_live, run_pkg_query, tool_cmd_with_resolver,
};

pub struct ScoopManager;

/// Where the Scoop installer puts its shims — `SCOOP` when the user pins a root,
/// otherwise the installer's default under the home directory. Windows-only,
/// because the bootstrap is a PowerShell script that runs nowhere else.
fn scoop_shims_dir() -> Option<std::path::PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    match std::env::var_os("SCOOP") {
        Some(root) => Some(std::path::PathBuf::from(root).join("shims")),
        None => home_relative_dir("~/scoop/shims"),
    }
}

/// Build a `Command` for scoop, resolved shim-aware. scoop ships on Windows only as
/// `scoop.ps1`/`scoop.cmd` (never `scoop.exe`), so a bare `Command::new("scoop")`
/// dies with "program not found" even though the tool is on `$PATH`. Routing through
/// `tool_cmd_with_resolver` resolves the full shim path and invokes it via
/// `powershell -File` / `cmd /c` on Windows (a plain PATH lookup on Unix). No
/// fallbacks: scoop always lives in its shims dir on `$PATH`.
fn scoop_cmd() -> Command {
    tool_cmd_with_resolver("scoop", || resolve_tool_with_fallbacks("scoop", &[]))
}

/// Parse the set of installed app names from `scoop export` JSON. The document is
/// `{ "buckets": [...], "apps": [ { "Name": "...", ... }, ... ] }`; a missing/empty
/// `apps` array (or non-JSON input) yields an empty set. Used instead of parsing
/// `scoop list`, whose PowerShell Format-Table renders NO rows when captured with no
/// console width (every non-interactive child process), making it unparseable.
pub(super) fn parse_scoop_export(output: &str) -> HashSet<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return HashSet::new();
    };
    value
        .get("apps")
        .and_then(|apps| apps.as_array())
        .map(|apps| {
            apps.iter()
                .filter_map(|app| app.get("Name").and_then(|n| n.as_str()))
                .map(canonical_ci_pkg_name)
                .collect()
        })
        .unwrap_or_default()
}

/// Installed apps WITH versions for `installed_packages_with_versions`. Unlike
/// [`parse_scoop_export`] this preserves the REGISTERED app-name case for display
/// (the scan/status surface) and carries `scoop export`'s reported `Version`.
fn parse_scoop_export_versions(output: &str) -> Vec<PackageInfo> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    value
        .get("apps")
        .and_then(|apps| apps.as_array())
        .map(|apps| {
            apps.iter()
                .filter_map(|app| {
                    let name = app.get("Name").and_then(|n| n.as_str())?;
                    let version = app
                        .get("Version")
                        .and_then(|v| v.as_str())
                        .unwrap_or(cfgd_core::providers::UNKNOWN_PACKAGE_VERSION);
                    Some(PackageInfo {
                        name: name.to_string(),
                        version: version.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl PackageManager for ScoopManager {
    fn name(&self) -> &str {
        "scoop"
    }

    fn tool_version(&self) -> Option<String> {
        super::shared::tool_version_from(scoop_cmd().arg("--version"))
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("scoop")
    }

    fn bootstrap_plan_given(&self, _delivered: &dyn Fn(&str) -> bool) -> Option<BootstrapPlan> {
        Some(BootstrapPlan::new("system").creating(scoop_shims_dir()))
    }

    fn path_dirs(&self, _cx: &PackageContext<'_>) -> Vec<String> {
        scoop_shims_dir()
            .into_iter()
            .map(cfgd_core::to_posix_string)
            .collect()
    }

    // bootstrap-arm-ok: get.scoop.sh is scoop's only route
    fn bootstrap(&self, cx: &PackageContext<'_>) -> Result<()> {
        run_pkg_cmd_live(
            cx,
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

    fn installed_packages(&self, _cx: &PackageContext<'_>) -> Result<HashSet<String>> {
        // `scoop list` renders a PowerShell Format-Table that produces NO rows when
        // captured with no console width (any non-interactive child process), so it
        // cannot be parsed. `scoop export` emits stable JSON regardless of console —
        // enumerate its `apps[].Name`. Exit code is tolerated via run_pkg_query
        // (scoop exits non-zero on an empty DB, a benign result — not a failure).
        let output = run_pkg_query("scoop", scoop_cmd().arg("export"))?;
        Ok(parse_scoop_export(&String::from_utf8_lossy(&output.stdout)))
    }

    /// scoop app names are matched case-insensitively; canonicalize to lowercase so
    /// a profile entry matches `scoop export`'s reported `Name` for install-diffing,
    /// prune, and tracking (mirrors chocolatey/winget).
    fn package_identity(&self, entry: &str) -> String {
        canonical_ci_pkg_name(entry)
    }

    /// The versioned listing keeps the REGISTERED app-name case for display;
    /// fold a listed name to the same lowercase identity form the matching
    /// surfaces use.
    fn listed_identity(&self, listed_name: &str) -> String {
        canonical_ci_pkg_name(listed_name)
    }

    /// Display surface (scan/status): keep the REGISTERED app-name case and the real
    /// version, rather than the lowercase identity form used for matching.
    fn installed_packages_with_versions(
        &self,
        _cx: &PackageContext<'_>,
    ) -> Result<Vec<PackageInfo>> {
        let output = run_pkg_query("scoop", scoop_cmd().arg("export"))?;
        Ok(parse_scoop_export_versions(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn install(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        // `scoop install <app>...` takes many apps in one invocation
        // (scoop-install.ps1 iterates its $apps array).
        install_batch_then_per_package(cx, "scoop", packages, |pkgs| {
            let mut cmd = scoop_cmd();
            cmd.arg("install").args(pkgs);
            cmd
        })?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        for pkg in packages {
            run_pkg_cmd_live(
                cx,
                "scoop",
                scoop_cmd().args(["uninstall", pkg]),
                &format!("Uninstalling {}", pkg),
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, cx: &PackageContext<'_>) -> Result<()> {
        // `scoop update` alone refreshes the bucket manifests. The `*` form is
        // an upgrade of every installed app, which no plan asked for.
        run_pkg_cmd_live(
            cx,
            "scoop",
            scoop_cmd().arg("update"),
            "scoop update",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // Bounded via run_pkg_query: `scoop info` runs through PowerShell on Windows
        // and must never hang a headless reconcile. A non-zero exit (unknown package)
        // is a benign "no version", not an error.
        let output = run_pkg_query("scoop", scoop_cmd().args(["info", package]))?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_version_field(&stdout))
    }
}

#[cfg(test)]
mod tests {
    use cfgd_core::command_available;
    use cfgd_core::providers::PackageManager;
    use cfgd_core::providers::PackageManagerExt;

    use super::*;

    // scoop `export` emits stable JSON regardless of console; parse_scoop_export
    // enumerates `apps[].Name`. (scoop `list` renders a Format-Table that produces
    // no rows when captured with no console width, so it is not used.)
    #[test]
    fn scoop_parse_export_multiple_apps() {
        let output = r#"{"buckets":[{"Name":"main"}],"apps":[
            {"Name":"7zip","Version":"26.02","Source":"main"},
            {"Name":"ripgrep","Version":"14.1.0","Source":"main"},
            {"Name":"fd","Version":"9.0.0","Source":"main"}
        ]}"#;
        let packages = parse_scoop_export(output);
        assert_eq!(packages.len(), 3);
        assert!(packages.contains("7zip"));
        assert!(packages.contains("ripgrep"));
        assert!(packages.contains("fd"));
    }

    #[test]
    fn scoop_parse_export_single_app() {
        let output = r#"{"apps":[{"Name":"git","Version":"2.44.0","Source":"main"}]}"#;
        let packages = parse_scoop_export(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("git"));
    }

    #[test]
    fn scoop_parse_export_canonicalizes_case() {
        // scoop app names are matched case-insensitively; fold to lowercase so a
        // profile entry matches for install-diff + prune.
        let output = r#"{"apps":[{"Name":"NodeJS","Version":"22.0.0","Source":"main"}]}"#;
        let packages = parse_scoop_export(output);
        assert!(packages.contains("nodejs"));
        assert!(!packages.contains("NodeJS"));
    }

    #[test]
    fn scoop_package_identity_folds_case() {
        assert_eq!(ScoopManager.package_identity("NodeJS"), "nodejs");
        assert_eq!(ScoopManager.package_identity("7zip"), "7zip");
    }

    #[test]
    fn scoop_versions_keep_registered_case_and_version() {
        // Display surface keeps the registered app-name case and the exported version.
        let output = r#"{"apps":[{"Name":"NodeJS","Version":"22.0.0","Source":"main"}]}"#;
        let infos = parse_scoop_export_versions(output);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "NodeJS");
        assert_eq!(infos[0].version, "22.0.0");
    }

    #[test]
    fn scoop_parse_export_empty_apps_array() {
        let packages = parse_scoop_export(r#"{"buckets":[{"Name":"main"}],"apps":[]}"#);
        assert!(packages.is_empty());
    }

    #[test]
    fn scoop_parse_export_missing_apps_key() {
        let packages = parse_scoop_export(r#"{"buckets":[{"Name":"main"}]}"#);
        assert!(packages.is_empty());
    }

    #[test]
    fn scoop_parse_export_non_json_is_empty() {
        // A benign empty-DB message or any non-JSON text yields an empty set.
        assert!(parse_scoop_export("There aren't any apps installed.").is_empty());
        assert!(parse_scoop_export("").is_empty());
    }

    #[test]
    fn scoop_parse_export_ignores_apps_without_name() {
        let output = r#"{"apps":[{"Version":"1.0"},{"Name":"fd","Version":"9.0.0"}]}"#;
        let packages = parse_scoop_export(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("fd"));
    }

    #[test]
    fn scoop_manager_name_and_traits() {
        let mgr = ScoopManager;
        assert_eq!(mgr.name(), "scoop");
        assert!(mgr.bootstrap_plan().is_some());
    }

    #[test]
    #[serial_test::serial]
    fn scoop_manager_is_available_checks_scoop() {
        // Both sides read `PATH`; without the guard a concurrent test's
        // `PATH` mutation can land between them and they disagree.
        let _path = cfgd_core::test_helpers::path_env_read_guard();
        let mgr = ScoopManager;
        let available = mgr.is_available();
        assert_eq!(available, command_available("scoop"));
    }

    #[test]
    fn scoop_bootstrap_plan_declares_the_shims_dir_on_windows() {
        let home = tempfile::tempdir().unwrap();
        let plan = cfgd_core::with_test_home(home.path(), || ScoopManager.bootstrap_plan())
            .expect("always planned");
        assert_eq!(plan.method, "system");
        assert!(plan.requires.is_empty());
        // `bootstrap` is a PowerShell install script; the shims it creates only
        // exist on the platform that can run it.
        if cfg!(windows) {
            assert!(
                plan.creates_path_dirs.iter().all(|d| d.ends_with("/shims")),
                "{:?}",
                plan.creates_path_dirs
            );
        } else {
            assert!(plan.creates_path_dirs.is_empty());
        }
    }

    #[test]
    fn scoop_path_dirs_matches_the_bootstrap_plans_declaration() {
        let home = tempfile::tempdir().unwrap();
        cfgd_core::with_test_home(home.path(), || {
            let plan = ScoopManager.bootstrap_plan().expect("always planned");
            let printer = cfgd_core::test_helpers::test_printer();
            let state = cfgd_core::test_helpers::test_state();
            let cx = cfgd_core::test_helpers::test_package_context(&printer, &state);
            let mgr: Box<dyn PackageManager> = Box::new(ScoopManager);
            assert_eq!(mgr.path_dirs(&cx), plan.creates_path_dirs);
        });
    }

    // ---------------------------------------------------------------------------
    // PackageManager trait impls via a fake scoop binary. scoop_cmd() honors the
    // CFGD_SCOOP_BIN seam first (tool_cmd_with_resolver), so a ToolShim carries
    // argv logging for spawn-count claims; the PATH-shim tests predate the seam
    // and stay on PATH manipulation.
    // ---------------------------------------------------------------------------

    #[cfg(unix)]
    mod scoop_shim {
        use super::*;
        use cfgd_core::test_helpers::{
            install_named_path_shim, test_package_context, test_printer, test_state,
        };
        use serial_test::serial;

        // Local wrapper: scoop is invoked by name via PATH, no env-var seam.
        // Delegates to the shared helper so the shim-script body stays in
        // one place across the package crate.
        fn install_scoop_shim(
            exit_code: u8,
            stdout: &str,
            stderr: &str,
        ) -> (tempfile::TempDir, cfgd_core::test_helpers::PathShimGuard) {
            install_named_path_shim("scoop", exit_code, stdout, stderr)
        }

        #[test]
        #[serial]
        fn scoop_bootstrap_runs_powershell_install_script() {
            // Scoop bootstrap shells out to `powershell` with the irm | iex
            // pipeline. A PATH-shimmed `powershell` swallowing the args and
            // exiting 0 proves the bootstrap path is exercised without
            // requiring real Windows infrastructure.
            let (_bin, _path) = install_named_path_shim("powershell", 0, "", "");
            let p = test_printer();
            ScoopManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect("bootstrap Ok via shim");
        }

        /// The scoop installer warns on stderr about what the user must do next
        /// (a PATH entry that only a new shell sees). Those notes belong to the
        /// caller's sink, which renders them under the action's status line —
        /// the reason `bootstrap` takes the whole context, not a bare printer.
        #[test]
        #[serial]
        fn scoop_bootstrap_caveats_reach_the_callers_sink() {
            let (_bin, _path) = install_named_path_shim(
                "powershell",
                0,
                "",
                "WARNING: scoop shims are on PATH only in a new shell",
            );
            let p = test_printer();
            let notes = cfgd_core::providers::NoteSink::default();
            ScoopManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context_with_notes(
                    &p, &notes,
                ))
                .expect("bootstrap Ok via shim");
            let drained = notes.take();
            assert_eq!(drained.len(), 1, "expected one caveat, got {drained:?}");
            assert_eq!(drained[0].tag.as_deref(), Some("scoop"));
            assert!(
                drained[0].message.contains("new shell"),
                "got: {}",
                drained[0].message
            );
        }

        #[test]
        #[serial]
        fn scoop_bootstrap_propagates_powershell_failure() {
            let (_bin, _path) =
                install_named_path_shim("powershell", 1, "", "scoop install failed");
            let p = test_printer();
            let err = ScoopManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect_err("non-zero powershell must error");
            let _ = err.to_string();
        }

        #[test]
        #[serial]
        fn scoop_install_batches_all_packages_into_one_spawn() {
            let s = cfgd_core::test_helpers::ToolShim::install("CFGD_SCOOP_BIN", 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            ScoopManager
                .install(&["git".into(), "ripgrep".into(), "fd".into()], &cx)
                .expect("install Ok");
            // Filter to the lines naming this test's own subject: the seam is
            // a process-global env var, so an unfiltered count also measures
            // whatever a parallel test spawned through the same shim.
            let lines = s.argv_lines_naming("ripgrep");
            assert_eq!(
                lines.len(),
                1,
                "three apps must produce ONE spawn: {}",
                s.argv_log()
            );
            assert!(
                lines[0].contains("install git ripgrep fd"),
                "the one spawn must carry every app: {}",
                lines[0]
            );
        }

        #[test]
        #[serial]
        fn scoop_install_batch_failure_falls_back_to_per_package_attribution() {
            let s = cfgd_core::test_helpers::ToolShim::install_failing_on(
                "CFGD_SCOOP_BIN",
                "nope",
                "Couldn't find manifest for 'nope'",
            );
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let err = ScoopManager
                .install(&["git".into(), "nope".into()], &cx)
                .expect_err("the bad app must fail after the retry");
            let msg = err.to_string();
            assert!(
                msg.contains("nope") && msg.contains("Couldn't find manifest"),
                "the error must name the failed app and its cause: {msg}"
            );
            assert!(
                !msg.contains("git ("),
                "the valid app must not be attributed a failure: {msg}"
            );
            // One batch spawn naming both, then one retry per app.
            assert_eq!(
                s.argv_lines_naming("git").len(),
                2,
                "batch + its own retry: {}",
                s.argv_log()
            );
            assert_eq!(
                s.argv_lines_naming("nope").len(),
                2,
                "batch + its own retry: {}",
                s.argv_log()
            );
        }

        #[test]
        #[serial]
        fn scoop_install_propagates_nonzero_exit_as_install_failed() {
            let (_bin, _path) = install_scoop_shim(1, "", "couldn't find git");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let err = ScoopManager
                .install(&["git".into()], &cx)
                .expect_err("non-zero scoop install must error");
            let msg = err.to_string();
            // Error kind is "install" — surfaces as InstallFailed and carries
            // the manager name in the error.
            assert!(
                msg.contains("scoop"),
                "error must reference manager name, got: {msg}"
            );
        }

        #[test]
        #[serial]
        fn scoop_uninstall_invokes_per_package() {
            let (_bin, _path) = install_scoop_shim(0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            ScoopManager
                .uninstall(&["git".into()], &cx)
                .expect("uninstall Ok");
        }

        #[test]
        #[serial]
        fn scoop_uninstall_propagates_nonzero_exit_as_uninstall_failed() {
            let (_bin, _path) = install_scoop_shim(1, "", "no such package");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let err = ScoopManager
                .uninstall(&["git".into()], &cx)
                .expect_err("non-zero scoop uninstall must error");
            assert!(err.to_string().contains("scoop"));
        }

        #[test]
        #[serial]
        fn scoop_refresh_updates_buckets_without_upgrading_apps() {
            let (_bin, _path, log) =
                cfgd_core::test_helpers::install_named_path_shim_logged("scoop", 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            assert!(ScoopManager.has_index(), "scoop buckets are a local index");
            ScoopManager.refresh_index(&cx).expect("refresh Ok");
            // The load-bearing half, and the whole reason this test is named
            // "without upgrading apps": `scoop update` refreshes the bucket
            // manifests, `scoop update *` upgrades every installed app. Both
            // exit 0, so only the argv separates them.
            assert_eq!(
                log.argv_log().trim(),
                "update",
                "a bucket refresh is `scoop update` with no target"
            );
        }

        #[test]
        #[serial]
        fn scoop_installed_packages_parses_export_json() {
            let stdout =
                r#"{"apps":[{"Name":"git","Version":"2.44.0"},{"Name":"fd","Version":"9.0.0"}]}"#;
            let (_bin, _path) = install_scoop_shim(0, stdout, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = ScoopManager.installed_packages(&cx).expect("Ok");
            assert!(pkgs.contains("git"));
            assert!(pkgs.contains("fd"));
            assert_eq!(pkgs.len(), 2);
        }

        #[test]
        #[serial]
        fn scoop_installed_packages_empty_when_no_apps() {
            let (_bin, _path) = install_scoop_shim(0, r#"{"apps":[]}"#, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = ScoopManager.installed_packages(&cx).expect("Ok");
            assert!(pkgs.is_empty());
        }

        #[test]
        #[serial]
        fn scoop_installed_packages_empty_db_exit1_is_empty_not_error() {
            // scoop exits 1 when the DB is empty; installed_packages must treat that
            // as an empty set, not a ListFailed — otherwise every apply on a fresh
            // scoop aborts before it can install anything. (Export prints an empty
            // apps array; some scoop versions emit a non-JSON message — both parse to
            // an empty set.)
            let (_bin, _path) = install_scoop_shim(1, "There aren't any apps installed.\n", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = ScoopManager
                .installed_packages(&cx)
                .expect("empty-DB exit-1 must be Ok(empty), not Err");
            assert!(pkgs.is_empty());
        }

        #[test]
        #[serial]
        fn scoop_available_version_returns_none_on_nonzero_exit() {
            let (_bin, _path) = install_scoop_shim(1, "", "package not found");
            let v = ScoopManager
                .available_version("nonexistent")
                .expect("non-zero → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn scoop_available_version_extracts_version_field_from_info_output() {
            // `scoop info <pkg>` prints "Version: X.Y.Z" among other fields;
            // parse_version_field plucks that line.
            let info = "Name: git\nVersion: 2.44.0\nDescription: git\n";
            let (_bin, _path) = install_scoop_shim(0, info, "");
            let v = ScoopManager.available_version("git").expect("Ok");
            assert_eq!(v.as_deref(), Some("2.44.0"));
        }
    }
}

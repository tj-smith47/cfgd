//! Chocolatey package manager (`choco`).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::providers::{BootstrapPlan, PackageInfo, PackageManager};

use super::shared::{
    canonical_ci_pkg_name, partition_already_installed, run_pkg_cmd, run_pkg_cmd_live,
    run_pkg_query, upgrade_each,
};

pub struct ChocolateyManager;

/// Where the Chocolatey installer puts the shims it creates. `ChocolateyInstall`
/// is set by an existing install; the literal is the installer's own default for
/// the machine-wide install cfgd's bootstrap performs. Windows-only, because the
/// bootstrap is a PowerShell script that runs nowhere else.
fn choco_bin_dir() -> Option<std::path::PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    Some(
        std::env::var_os("ChocolateyInstall")
            .map(|root| std::path::PathBuf::from(root).join("bin"))
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData\chocolatey\bin")),
    )
}

/// Extract `(name, version)` for each real package line of `choco list` output,
/// skipping the `Chocolatey vX` banner and the `N packages installed.` footer. The
/// name is in its REGISTERED case (e.g. `Wget`) — callers canonicalize as needed.
fn choco_list_entries(output: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
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
        if let Some((name, version)) = line.split_once(' ') {
            entries.push((name.to_string(), version.trim().to_string()));
        }
    }
    entries
}

pub(super) fn parse_choco_list(output: &str) -> HashSet<String> {
    choco_list_entries(output)
        .into_iter()
        .map(|(name, _)| canonical_ci_pkg_name(&name))
        .collect()
}

/// Installed packages WITH versions for `installed_packages_with_versions`. Unlike
/// [`parse_choco_list`] this preserves the REGISTERED name case for display (the
/// scan/status surface) and carries the real version.
fn parse_choco_list_versions(output: &str) -> Vec<PackageInfo> {
    choco_list_entries(output)
        .into_iter()
        .map(|(name, version)| PackageInfo {
            name,
            version: if version.is_empty() {
                cfgd_core::providers::UNKNOWN_PACKAGE_VERSION.into()
            } else {
                version
            },
        })
        .collect()
}

impl PackageManager for ChocolateyManager {
    fn name(&self) -> &str {
        "chocolatey"
    }

    fn tool_version(&self) -> Option<String> {
        super::shared::tool_version_from(Command::new("choco").arg("--version"))
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("choco")
    }

    fn bootstrap_plan_given(&self, _delivered: &dyn Fn(&str) -> bool) -> Option<BootstrapPlan> {
        Some(BootstrapPlan::new("system").creating(choco_bin_dir()))
    }

    fn path_dirs(&self, _cx: &cfgd_core::providers::PackageContext<'_>) -> Vec<String> {
        choco_bin_dir()
            .into_iter()
            .map(cfgd_core::to_posix_string)
            .collect()
    }

    // bootstrap-arm-ok: the community install script is chocolatey's only route
    fn bootstrap(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        run_pkg_cmd_live(
            cx,
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

    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("chocolatey", Command::new("choco").args(["list"]), "list")?;
        Ok(parse_choco_list(&String::from_utf8_lossy(&output.stdout)))
    }

    /// chocolatey package ids are case-insensitive; `choco list` echoes them in
    /// their registered case (e.g. `Wget`). Canonicalize to lowercase so a profile
    /// `wget` matches installed `Wget` for install-diffing, prune, and tracking.
    fn package_identity(&self, entry: &str) -> String {
        canonical_ci_pkg_name(entry)
    }

    /// The versioned listing keeps the REGISTERED case for display; fold a
    /// listed name to the same lowercase identity form the matching surfaces
    /// use.
    fn listed_identity(&self, listed_name: &str) -> String {
        canonical_ci_pkg_name(listed_name)
    }

    /// Display surface (scan/status): keep the REGISTERED name case and the real
    /// version, rather than the lowercase identity form used for matching.
    fn installed_packages_with_versions(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<Vec<PackageInfo>> {
        let output = run_pkg_cmd("chocolatey", Command::new("choco").args(["list"]), "list")?;
        Ok(parse_choco_list_versions(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn install(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        let (held, fresh) = partition_already_installed(self, packages, cx);
        if !fresh.is_empty() {
            let mut args = vec!["install", "-y"];
            let pkg_refs: Vec<&str> = fresh.iter().map(|s| s.as_str()).collect();
            args.extend(pkg_refs);
            run_pkg_cmd_live(
                cx,
                "chocolatey",
                Command::new("choco").args(&args),
                "Installing chocolatey packages",
                "install",
            )?;
        }
        // `choco install` no-ops on a package already held; raising it takes
        // `choco upgrade`.
        upgrade_each(cx, "chocolatey", &held, "choco upgrade -y", |pkg| {
            let mut cmd = Command::new("choco");
            cmd.args(["upgrade", "-y", pkg]);
            Some(cmd)
        })?;
        Ok(())
    }

    fn uninstall(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        let mut args = vec!["uninstall", "-y"];
        let pkg_refs: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
        args.extend(pkg_refs);
        run_pkg_cmd_live(
            cx,
            "chocolatey",
            Command::new("choco").args(&args),
            "Uninstalling chocolatey packages",
            "uninstall",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        let output = run_pkg_query("chocolatey", Command::new("choco").args(["info", package]))?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_choco_info_version(&stdout))
    }

    /// Chocolatey-listed versions carry a fourth build component
    /// (`4.7.1.2019`) semver has no field for and refuses outright.
    fn version_comparable(&self, version: &str) -> bool {
        super::versions::fourpart_comparable(version)
    }

    fn version_meets_minimum(&self, available: &str, min_version: &str) -> bool {
        super::versions::fourpart_version_meets_minimum(available, min_version)
    }

    fn floor_comparable(&self, floor: &str) -> bool {
        super::versions::fourpart_comparable(floor)
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

#[cfg(test)]
mod tests {
    use cfgd_core::command_available;
    use cfgd_core::providers::PackageManager;
    use cfgd_core::providers::PackageManagerExt;

    use super::*;

    #[test]
    fn chocolatey_parse_list_output() {
        let output = "Chocolatey v2.2.2\n\
                      chocolatey 2.2.2\n\
                      nodejs 21.4.0\n\
                      python 3.12.1\n\
                      3 packages installed.";
        let packages = parse_choco_list(output);
        assert!(packages.contains("chocolatey"));
        assert!(packages.contains("nodejs"));
        assert!(packages.contains("python"));
        assert_eq!(packages.len(), 3);
    }

    #[test]
    fn chocolatey_parse_list_canonicalizes_registered_case() {
        // `choco list` echoes the registered id case (e.g. `Wget`, `Cosign`); cfgd must
        // canonicalize to lowercase so a profile `wget` matches for install-diff + prune.
        let output = "Chocolatey v2.7.3\n\
                      Wget 1.21.4\n\
                      Cosign 1.3.1\n\
                      2 packages installed.";
        let packages = parse_choco_list(output);
        assert!(packages.contains("wget"), "Wget should fold to wget");
        assert!(packages.contains("cosign"), "Cosign should fold to cosign");
        assert!(!packages.contains("Wget"));
    }

    #[test]
    fn chocolatey_package_identity_folds_case() {
        assert_eq!(ChocolateyManager.package_identity("Wget"), "wget");
        assert_eq!(ChocolateyManager.package_identity("wget"), "wget");
    }

    #[test]
    fn chocolatey_versions_keep_registered_case_and_version() {
        // The display surface (scan/status) must NOT fold case, and carries the version.
        let output = "Chocolatey v2.7.3\n\
                      Wget 1.21.4\n\
                      1 packages installed.";
        let infos = parse_choco_list_versions(output);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "Wget");
        assert_eq!(infos[0].version, "1.21.4");
    }

    #[test]
    fn chocolatey_parse_list_empty() {
        let packages = parse_choco_list("");
        assert!(packages.is_empty());
    }

    #[test]
    fn chocolatey_parse_list_single_package() {
        let output = "Chocolatey v2.2.2\n\
                      git 2.43.0\n\
                      1 package installed.";
        let packages = parse_choco_list(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("git"));
    }

    #[test]
    fn chocolatey_parse_list_with_cr_endings() {
        // Windows CRLF line endings
        let output =
            "Chocolatey v2.2.2\r\nnodejs 21.4.0\r\npython 3.12.1\r\n2 packages installed.\r\n";
        let packages = parse_choco_list(output);
        assert!(packages.contains("nodejs"));
        assert!(packages.contains("python"));
        assert_eq!(packages.len(), 2);
    }

    #[test]
    fn chocolatey_parse_list_only_header_and_footer() {
        let output = "Chocolatey v2.2.2\n\
                      0 packages installed.";
        let packages = parse_choco_list(output);
        assert!(packages.is_empty());
    }

    #[test]
    fn chocolatey_parse_list_line_without_version_skipped() {
        // Lines without a space (no version) are skipped since split_once returns None
        let output = "Chocolatey v2.2.2\n\
                      malformed_no_space\n\
                      git 2.43.0\n\
                      1 package installed.";
        let packages = parse_choco_list(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("git"));
    }

    #[test]
    fn chocolatey_parse_list_packages_installed_singular() {
        // Singular "package installed." should be filtered
        let output = "Chocolatey v2.3.0\ngit 2.44.0\n1 package installed.\n";
        let packages = parse_choco_list(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("git"));
    }

    #[test]
    fn chocolatey_parse_list_packages_installed_with_cr() {
        // Test the \r variant of "packages installed."
        let output = "Chocolatey v2.3.0\r\ngit 2.44.0\r\n1 package installed.\r\n";
        let packages = parse_choco_list(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("git"));
    }

    #[test]
    fn chocolatey_manager_name_and_traits() {
        let mgr = ChocolateyManager;
        assert_eq!(mgr.name(), "chocolatey");
        assert!(mgr.bootstrap_plan().is_some());
    }

    #[test]
    fn chocolatey_parse_list_multiple_versions() {
        let output = "Chocolatey v2.2.2\n\
                      git 2.43.0\n\
                      git.install 2.43.0\n\
                      nodejs 21.4.0\n\
                      3 packages installed.";
        let packages = parse_choco_list(output);
        assert_eq!(packages.len(), 3);
        assert!(packages.contains("git"));
        assert!(packages.contains("git.install"));
        assert!(packages.contains("nodejs"));
    }

    #[test]
    fn chocolatey_parse_list_real_world_output() {
        let output = "\
Chocolatey v2.2.2
chocolatey 2.2.2
chocolatey-core.extension 1.4.0
git 2.43.0
git.install 2.43.0
nodejs 21.4.0
python 3.12.1
vscode 1.85.1
vscode.install 1.85.1
8 packages installed.";
        let packages = parse_choco_list(output);
        assert_eq!(packages.len(), 8);
        assert!(packages.contains("chocolatey"));
        assert!(packages.contains("chocolatey-core.extension"));
        assert!(packages.contains("git"));
        assert!(packages.contains("git.install"));
        assert!(packages.contains("vscode"));
    }

    #[test]
    fn chocolatey_parse_list_extension_packages() {
        let output = "Chocolatey v2.2.2\n\
                      chocolatey 2.2.2\n\
                      chocolatey-core.extension 1.4.0\n\
                      chocolatey-windowsupdate.extension 1.0.5\n\
                      dotnetfx 4.8.0.20220524\n\
                      4 packages installed.";
        let packages = parse_choco_list(output);
        assert_eq!(packages.len(), 4);
        assert!(packages.contains("chocolatey-core.extension"));
        assert!(packages.contains("chocolatey-windowsupdate.extension"));
        assert!(packages.contains("dotnetfx"));
    }

    #[test]
    fn parse_choco_info_version_basic() {
        let output = "Title: git | 2.44.0\nPublished: 2024-02-23\n";
        assert_eq!(parse_choco_info_version(output), Some("2.44.0".to_string()));
    }

    #[test]
    fn parse_choco_info_version_with_extra_whitespace() {
        let output = "Title: Visual Studio Code |  1.87.2 \n";
        assert_eq!(parse_choco_info_version(output), Some("1.87.2".to_string()));
    }

    #[test]
    fn parse_choco_info_version_no_title_line() {
        let output = "Published: 2024-02-23\nSummary: A tool\n";
        assert_eq!(parse_choco_info_version(output), None);
    }

    #[test]
    fn parse_choco_info_version_no_pipe_separator() {
        // Title without version separator
        let output = "Title: some-package\n";
        assert_eq!(parse_choco_info_version(output), None);
    }

    #[test]
    fn parse_choco_list_filters_meta_lines() {
        let output = "\
Chocolatey v2.2.2\n\
git 2.44.0\n\
nodejs 20.11.1\n\
2 packages installed.\n";
        let result = parse_choco_list(output);
        assert!(result.contains("git"));
        assert!(result.contains("nodejs"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_choco_list_single_package_line() {
        let output = "Chocolatey v2.2.2\ngit 2.44.0\n1 package installed.\n";
        let result = parse_choco_list(output);
        assert!(result.contains("git"));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_choco_list_ignores_carriage_returns() {
        // Windows output often has \r\n line endings
        let output = "Chocolatey v2.2.2\r\ngit 2.44.0\r\n1 package installed.\r\n";
        let result = parse_choco_list(output);
        assert!(result.contains("git"));
        assert_eq!(result.len(), 1, "should handle Windows-style line endings");
    }

    #[test]
    fn parse_choco_info_version_multiple_pipes() {
        // Uses rsplit_once('|') so only the last pipe matters
        let output = "Title: some | package | 1.2.3\n";
        assert_eq!(parse_choco_info_version(output), Some("1.2.3".to_string()));
    }

    #[test]
    fn parse_choco_info_version_empty_string() {
        assert_eq!(parse_choco_info_version(""), None);
    }

    #[test]
    fn parse_choco_info_version_title_with_spaces_around_version() {
        let output = "Title:  Python 3  |  3.12.1  \n";
        assert_eq!(parse_choco_info_version(output), Some("3.12.1".to_string()));
    }

    #[test]
    fn parse_choco_list_no_version_header() {
        // Output without the version header line
        let output = "git 2.44.0\n1 package installed.\n";
        let packages = parse_choco_list(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("git"));
    }

    #[test]
    fn parse_choco_info_version_real_world() {
        let output = "\
Chocolatey v2.2.2
Title: Git | 2.44.0
Published: 2024-02-23T12:00:00.000Z
Number of Downloads: 12345678
Summary: Git - Fast, scalable, distributed revision control system
Description: Git is a free and open source distributed version control system.
Tags: git vcs dvcs
";
        assert_eq!(parse_choco_info_version(output), Some("2.44.0".to_string()));
    }

    #[test]
    #[serial_test::serial]
    fn chocolatey_manager_is_available_checks_choco() {
        // Both sides read `PATH`; without the guard a concurrent test's
        // `PATH` mutation can land between them and they disagree.
        let _path = cfgd_core::test_helpers::path_env_read_guard();
        let mgr = ChocolateyManager;
        let available = mgr.is_available();
        assert_eq!(available, command_available("choco"));
    }

    #[test]
    fn chocolatey_bootstrap_plan_declares_the_installer_shim_dir_on_windows() {
        let plan = ChocolateyManager.bootstrap_plan().expect("always planned");
        assert_eq!(plan.method, "system");
        assert!(plan.requires.is_empty());
        // `bootstrap` is a PowerShell install script; the shims it creates only
        // exist on the platform that can run it.
        if cfg!(windows) {
            assert_eq!(plan.creates_path_dirs.len(), 1);
            assert!(
                plan.creates_path_dirs[0].ends_with("/bin"),
                "{:?}",
                plan.creates_path_dirs
            );
            assert!(!plan.creates_path_dirs[0].contains('\\'));
        } else {
            assert!(plan.creates_path_dirs.is_empty());
        }
    }

    #[test]
    fn chocolatey_path_dirs_matches_the_bootstrap_plans_declaration() {
        let plan = ChocolateyManager.bootstrap_plan().expect("always planned");
        let printer = cfgd_core::test_helpers::test_printer();
        let state = cfgd_core::test_helpers::test_state();
        let cx = cfgd_core::test_helpers::test_package_context(&printer, &state);
        let mgr: Box<dyn PackageManager> = Box::new(ChocolateyManager);
        assert_eq!(mgr.path_dirs(&cx), plan.creates_path_dirs);
    }

    // ---------------------------------------------------------------------------
    // PackageManager trait impls via a fake `choco` binary on PATH. Mirrors
    // the scoop shim approach — choco methods call `Command::new("choco")`
    // directly, so prepending a tempdir with our shim to PATH routes the call
    // through it.
    // ---------------------------------------------------------------------------

    #[cfg(unix)]
    mod choco_shim {
        use super::*;
        use cfgd_core::test_helpers::{
            install_named_path_shim, test_package_context, test_printer, test_state,
        };
        use serial_test::serial;

        fn install_choco_shim(
            exit_code: u8,
            stdout: &str,
            stderr: &str,
        ) -> (tempfile::TempDir, cfgd_core::test_helpers::PathShimGuard) {
            install_named_path_shim("choco", exit_code, stdout, stderr)
        }

        #[test]
        #[serial]
        fn bootstrap_runs_powershell_install_script() {
            // Choco bootstrap shells out to powershell with an iex pipeline.
            // A PATH-shimmed powershell stub returning 0 proves the bootstrap
            // path is executed without needing real Windows.
            let (_bin, _path) = install_named_path_shim("powershell", 0, "", "");
            let p = test_printer();
            ChocolateyManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect("bootstrap Ok via shim");
        }

        /// The chocolatey install script warns on stderr about things the user
        /// must act on (an execution policy left in place, a shell restart).
        /// Those notes are the reason `bootstrap` takes the whole context and
        /// not a bare printer: they belong to the caller's sink, which renders
        /// them under the action's own status line.
        #[test]
        #[serial]
        fn bootstrap_caveats_reach_the_callers_sink() {
            let (_bin, _path) = install_named_path_shim(
                "powershell",
                0,
                "",
                "WARNING: Restart your shell to pick up choco",
            );
            let p = test_printer();
            let notes = cfgd_core::providers::NoteSink::default();
            ChocolateyManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context_with_notes(
                    &p, &notes,
                ))
                .expect("bootstrap Ok via shim");
            let drained = notes.take();
            assert_eq!(drained.len(), 1, "expected one caveat, got {drained:?}");
            assert_eq!(drained[0].tag.as_deref(), Some("chocolatey"));
            assert!(
                drained[0].message.contains("Restart your shell"),
                "got: {}",
                drained[0].message
            );
        }

        #[test]
        #[serial]
        fn bootstrap_propagates_powershell_failure() {
            let (_bin, _path) = install_named_path_shim("powershell", 1, "", "install failed");
            let p = test_printer();
            let err = ChocolateyManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect_err("nonzero powershell must error");
            let _ = err.to_string();
        }

        #[test]
        #[serial]
        fn install_succeeds_when_choco_exits_zero() {
            let (_bin, _path) = install_choco_shim(0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            ChocolateyManager
                .install(&["git".into(), "nodejs".into()], &cx)
                .expect("install Ok");
        }

        #[test]
        #[serial]
        fn install_raises_a_held_package_via_choco_upgrade_not_install() {
            // The listing already carries `git`, so `install` partitions it
            // into `held` and raises it through `choco upgrade -y git`
            // instead of re-running `choco install -y git`, which would
            // no-op; `nodejs` is unheld and still installs.
            let (_bin, _path, log) = cfgd_core::test_helpers::install_named_path_shim_logged(
                "choco",
                0,
                "git 2.44.0\n",
                "",
            );
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            ChocolateyManager
                .install(&["git".into(), "nodejs".into()], &cx)
                .expect("install Ok");
            let argv = log.argv_log();
            assert!(
                argv.contains("upgrade -y git"),
                "held package must be raised via `choco upgrade -y`: {argv}"
            );
            assert!(
                argv.contains("install -y nodejs"),
                "unheld package must still install: {argv}"
            );
            assert!(
                !argv.contains("install -y git"),
                "held package must not be re-run through `choco install`: {argv}"
            );
        }

        #[test]
        #[serial]
        fn install_propagates_nonzero_exit_as_install_failed() {
            let (_bin, _path) = install_choco_shim(1, "", "package not found");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let err = ChocolateyManager
                .install(&["git".into()], &cx)
                .expect_err("non-zero choco install must error");
            assert!(err.to_string().contains("chocolatey"));
        }

        #[test]
        #[serial]
        fn uninstall_succeeds_when_choco_exits_zero() {
            let (_bin, _path) = install_choco_shim(0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            ChocolateyManager
                .uninstall(&["git".into()], &cx)
                .expect("uninstall Ok");
        }

        #[test]
        #[serial]
        fn uninstall_propagates_nonzero_exit_as_uninstall_failed() {
            let (_bin, _path) = install_choco_shim(2, "", "no such package");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let err = ChocolateyManager
                .uninstall(&["git".into()], &cx)
                .expect_err("non-zero choco uninstall must error");
            assert!(err.to_string().contains("chocolatey"));
        }

        #[test]
        #[serial]
        fn choco_declares_no_index_and_refreshing_upgrades_nothing() {
            let (_bin, _path) = install_choco_shim(0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            assert!(
                !ChocolateyManager.has_index(),
                "`choco upgrade all -y` upgrades every package the user never declared"
            );
            ChocolateyManager.refresh_index(&cx).expect("refresh Ok");
        }

        #[test]
        #[serial]
        fn installed_packages_parses_choco_list_output() {
            let stdout = "Chocolatey v2.2.2\ngit 2.44.0\nnodejs 20.11.1\npython 3.12.1\n3 packages installed.\n";
            let (_bin, _path) = install_choco_shim(0, stdout, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = ChocolateyManager.installed_packages(&cx).expect("Ok");
            assert!(pkgs.contains("git"));
            assert!(pkgs.contains("nodejs"));
            assert!(pkgs.contains("python"));
            assert_eq!(pkgs.len(), 3);
        }

        #[test]
        #[serial]
        fn installed_packages_empty_when_output_only_has_summary() {
            let (_bin, _path) =
                install_choco_shim(0, "Chocolatey v2.2.2\n0 packages installed.\n", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = ChocolateyManager.installed_packages(&cx).expect("Ok");
            assert!(pkgs.is_empty());
        }

        #[test]
        #[serial]
        fn available_version_returns_none_on_nonzero_exit() {
            let (_bin, _path) = install_choco_shim(1, "", "not found");
            let v = ChocolateyManager
                .available_version("nonexistent")
                .expect("non-zero → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn available_version_extracts_pipe_separated_field_from_title_line() {
            let info = "Chocolatey v2.2.2\nTitle: Git | 2.44.0\nPublished: now\n";
            let (_bin, _path) = install_choco_shim(0, info, "");
            let v = ChocolateyManager.available_version("git").expect("Ok");
            assert_eq!(v.as_deref(), Some("2.44.0"));
        }

        #[test]
        #[serial]
        fn available_version_returns_none_when_title_field_missing() {
            let info = "Chocolatey v2.2.2\nSummary: foo\n";
            let (_bin, _path) = install_choco_shim(0, info, "");
            let v = ChocolateyManager.available_version("foo").expect("Ok");
            assert_eq!(v, None);
        }
    }
}

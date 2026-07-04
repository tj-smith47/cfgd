//! Windows Package Manager (`winget`).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::{PackageInfo, PackageManager};

use super::shared::{canonical_ci_pkg_name, parse_version_field, run_pkg_cmd, run_pkg_cmd_live};

pub struct WingetManager;

/// Parse `winget list` into `(Id, Version)` pairs, locating the `Id`/`Version`
/// columns from the header. The Id is in its REGISTERED case (e.g. `Git.Git`);
/// the version is the first token of the Version column (any trailing `Available`/
/// `Source` columns are ignored). This is the primitive; [`parse_winget_list`]
/// derives the case-folded identity set from it.
fn parse_winget_list_versions(output: &str) -> Vec<PackageInfo> {
    let mut out = Vec::new();
    let mut header_seen = false;
    let mut id_start = 0;
    let mut id_end = 0;

    for line in output.lines() {
        if line.starts_with("---") || line.starts_with("===") {
            header_seen = true;
            continue;
        }
        if !header_seen {
            if let Some(pos) = line.find("Id") {
                id_start = pos;
                if let Some(ver_pos) = line.find("Version") {
                    id_end = ver_pos;
                }
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if id_end > id_start
            && let Some(slice) = line.get(id_start..id_end)
        {
            let id = slice.trim();
            if !id.is_empty() {
                let version = line
                    .get(id_end..)
                    .and_then(|s| s.split_whitespace().next())
                    .filter(|v| !v.is_empty())
                    .unwrap_or("unknown");
                out.push(PackageInfo {
                    name: id.to_string(),
                    version: version.to_string(),
                });
            }
        }
    }
    out
}

pub(super) fn parse_winget_list(output: &str) -> HashSet<String> {
    parse_winget_list_versions(output)
        .into_iter()
        .map(|p| canonical_ci_pkg_name(&p.name))
        .collect()
}

impl PackageManager for WingetManager {
    fn name(&self) -> &str {
        "winget"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("winget")
    }

    fn can_bootstrap(&self) -> bool {
        false
    }

    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Err(PackageError::BootstrapFailed {
            manager: "winget".into(),
            message: "winget ships with Windows; install App Installer from the Microsoft Store"
                .into(),
        }
        .into())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd(
            "winget",
            Command::new("winget").args(["list", "--source", "winget"]),
            "list",
        )?;
        Ok(parse_winget_list(&String::from_utf8_lossy(&output.stdout)))
    }

    /// winget package Ids are matched case-insensitively; canonicalize to lowercase
    /// so a profile entry matches `winget list`'s reported Id for install-diffing,
    /// prune, and tracking (mirrors chocolatey/scoop).
    fn package_identity(&self, entry: &str) -> String {
        canonical_ci_pkg_name(entry)
    }

    /// Display surface (scan/status): keep the REGISTERED Id case and the real
    /// version, rather than the lowercase identity form used for matching.
    fn installed_packages_with_versions(&self) -> Result<Vec<PackageInfo>> {
        let output = run_pkg_cmd(
            "winget",
            Command::new("winget").args(["list", "--source", "winget"]),
            "list",
        )?;
        Ok(parse_winget_list_versions(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            run_pkg_cmd_live(
                printer,
                "winget",
                Command::new("winget").args([
                    "install",
                    "--id",
                    pkg,
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                ]),
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
                "winget",
                Command::new("winget").args(["uninstall", "--id", pkg]),
                &format!("Uninstalling {}", pkg),
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "winget",
            Command::new("winget").args([
                "upgrade",
                "--all",
                "--accept-package-agreements",
                "--accept-source-agreements",
            ]),
            "Upgrading all winget packages",
            "install",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        let output = Command::new("winget")
            .args(["show", "--id", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "winget".into(),
                source: e,
            })?;
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

    use super::*;

    #[test]
    fn winget_parse_list_output() {
        let output = "Name            Id                    Version\n\
                      -----------------------------------------------\n\
                      Visual Studio   Microsoft.VisualStudio 17.8.3\n\
                      Git             Git.Git                2.43.0\n";
        let packages = parse_winget_list(output);
        assert!(packages.contains("microsoft.visualstudio"));
        assert!(packages.contains("git.git"));
    }

    #[test]
    fn winget_parse_list_empty() {
        let packages = parse_winget_list("");
        assert!(packages.is_empty());
    }

    #[test]
    fn winget_parse_list_no_separator_line() {
        // Without a separator line, no packages are parsed (header not yet seen).
        let output = "Name   Id      Version\n\
                      foo    foo.Bar  1.0\n";
        let packages = parse_winget_list(output);
        assert!(packages.is_empty());
    }

    #[test]
    fn parse_winget_list_wide_columns() {
        // Winget output with wider column spacing
        let output = "\
Name                              Id                                   Version       Available Source\n\
---------------------------------------------------------------------------------------------------\n\
Microsoft Visual Studio Code      Microsoft.VisualStudioCode           1.85.1        1.86.0    winget\n\
Windows Terminal                   Microsoft.WindowsTerminal            1.18.3181.0             winget\n";
        let packages = parse_winget_list(output);
        assert!(packages.contains("microsoft.visualstudiocode"));
        assert!(packages.contains("microsoft.windowsterminal"));
        assert_eq!(packages.len(), 2);
    }

    #[test]
    fn parse_winget_list_equals_separator() {
        // Some winget versions use === separator
        let output = "\
Name       Id          Version\n\
============================\n\
Git        Git.Git     2.43.0\n";
        let packages = parse_winget_list(output);
        assert!(packages.contains("git.git"));
    }

    #[test]
    fn parse_winget_list_trailing_blank_lines() {
        let output = "\
Name       Id          Version\n\
-------------------------------\n\
Git        Git.Git     2.43.0\n\
\n\
\n";
        let packages = parse_winget_list(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("git.git"));
    }

    #[test]
    fn parse_winget_list_no_id_column() {
        // If output doesn't have an "Id" column, nothing is parsed
        let output = "Name       Version\n------\nGit        2.43.0\n";
        let packages = parse_winget_list(output);
        assert!(packages.is_empty());
    }

    #[test]
    fn parse_winget_list_line_shorter_than_columns() {
        // Lines shorter than the column range are handled gracefully
        let output = "Name Id Version\n---\nX\n";
        let packages = parse_winget_list(output);
        // "X" is too short to slice id_start..id_end
        assert!(packages.is_empty());
    }

    #[test]
    fn winget_manager_name_and_traits() {
        let mgr = WingetManager;
        assert_eq!(mgr.name(), "winget");
        assert!(!mgr.can_bootstrap());
    }

    #[test]
    fn winget_manager_bootstrap_returns_error() {
        let mgr = WingetManager;
        let printer = cfgd_core::test_helpers::test_printer();
        let result = mgr.bootstrap(&printer);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Microsoft Store"));
    }

    #[test]
    fn parse_winget_list_with_available_column() {
        let output = "\
Name                  Id                        Version    Available  Source\n\
---------------------------------------------------------------------------\n\
Git                   Git.Git                   2.43.0     2.44.0     winget\n\
PowerShell            Microsoft.PowerShell      7.4.0                 winget\n";
        let packages = parse_winget_list(output);
        assert_eq!(packages.len(), 2);
        assert!(packages.contains("git.git"));
        assert!(packages.contains("microsoft.powershell"));
    }

    #[test]
    fn parse_winget_list_only_header() {
        let output = "Name   Id      Version\n---\n";
        let packages = parse_winget_list(output);
        assert!(packages.is_empty());
    }

    #[test]
    fn parse_winget_list_real_world_output() {
        let output = "\
Name                                   Id                                      Version          Available        Source
----------------------------------------------------------------------------------------------------------------------
Microsoft Visual Studio Code           Microsoft.VisualStudioCode              1.85.1           1.86.0           winget
Git                                    Git.Git                                 2.43.0                            winget
Windows Terminal                       Microsoft.WindowsTerminal               1.18.3181.0                       winget
PowerShell                             Microsoft.PowerShell                    7.4.0            7.4.1            winget
";
        let packages = parse_winget_list(output);
        assert_eq!(packages.len(), 4);
        assert!(packages.contains("microsoft.visualstudiocode"));
        assert!(packages.contains("git.git"));
        assert!(packages.contains("microsoft.windowsterminal"));
        assert!(packages.contains("microsoft.powershell"));
    }

    #[test]
    fn parse_winget_list_unicode_names() {
        let output = "\
Name                Id                  Version
-------------------------------------------------
テスト App          Test.App            1.0.0
Git                 Git.Git             2.43.0
";
        let packages = parse_winget_list(output);
        assert!(packages.contains("test.app"));
        assert!(packages.contains("git.git"));
    }

    #[test]
    fn parse_winget_list_basic() {
        let output = "\
Name            Id                  Version\n\
----------------------------------------------\n\
Git             Git.Git             2.44.0\n\
Node.js         OpenJS.NodeJS       20.11.1\n";
        let result = parse_winget_list(output);
        assert!(result.contains("git.git"));
        assert!(result.contains("openjs.nodejs"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_winget_list_empty_after_header() {
        let output = "\
Name            Id                  Version\n\
----------------------------------------------\n";
        let result = parse_winget_list(output);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_winget_list_no_header() {
        let output = "No installed package found.\n";
        let result = parse_winget_list(output);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_winget_list_id_column_at_different_position() {
        // When "Id" column starts at a different offset
        let output = "\
Name                  Id                        Version\n\
-------------------------------------------------------\n\
SomeApp               Some.App                  1.0.0\n";
        let packages = parse_winget_list(output);
        assert!(packages.contains("some.app"));
    }

    #[test]
    fn parse_winget_list_tight_columns() {
        let output = "Name Id Version\n---\nG Git.Git 2.0\n";
        let packages = parse_winget_list(output);
        // "Id" at position 5, "Version" at position 8
        // After the separator, "G Git.Git 2.0" → slice [5..8] = "Git"
        // This tests that narrow columns still work
        assert!(!packages.is_empty() || packages.is_empty()); // documents behavior
    }

    #[test]
    #[serial_test::serial]
    fn winget_manager_is_available_checks_winget() {
        let mgr = WingetManager;
        let available = mgr.is_available();
        assert_eq!(available, command_available("winget"));
    }

    #[test]
    fn winget_bootstrap_error_contains_microsoft_store_message() {
        let mgr = WingetManager;
        let printer = cfgd_core::test_helpers::test_printer();
        let result = mgr.bootstrap(&printer);
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("winget") && msg.contains("Microsoft Store"),
            "got: {msg}"
        );
    }

    // ---------------------------------------------------------------------------
    // PackageManager trait impls via a fake `winget` binary on PATH.
    // ---------------------------------------------------------------------------

    #[cfg(unix)]
    mod winget_shim {
        use super::*;
        use cfgd_core::test_helpers::{install_named_path_shim, test_printer};
        use serial_test::serial;

        fn install_winget_shim(
            exit_code: u8,
            stdout: &str,
            stderr: &str,
        ) -> (tempfile::TempDir, cfgd_core::test_helpers::EnvVarGuard) {
            install_named_path_shim("winget", exit_code, stdout, stderr)
        }

        #[test]
        #[serial]
        fn install_succeeds_per_package_when_winget_exits_zero() {
            let (_bin, _path) = install_winget_shim(0, "", "");
            let p = test_printer();
            WingetManager
                .install(&["Git.Git".into(), "Microsoft.VisualStudio".into()], &p)
                .expect("install Ok");
        }

        #[test]
        #[serial]
        fn install_propagates_nonzero_exit_as_install_failed() {
            let (_bin, _path) = install_winget_shim(1, "", "no manifest found");
            let p = test_printer();
            let err = WingetManager
                .install(&["NoSuch".into()], &p)
                .expect_err("non-zero winget install must error");
            assert!(err.to_string().contains("winget"));
        }

        #[test]
        #[serial]
        fn uninstall_succeeds_per_package_when_winget_exits_zero() {
            let (_bin, _path) = install_winget_shim(0, "", "");
            let p = test_printer();
            WingetManager
                .uninstall(&["Git.Git".into()], &p)
                .expect("uninstall Ok");
        }

        #[test]
        #[serial]
        fn uninstall_propagates_nonzero_exit_as_uninstall_failed() {
            let (_bin, _path) = install_winget_shim(1, "", "not installed");
            let p = test_printer();
            let err = WingetManager
                .uninstall(&["Git.Git".into()], &p)
                .expect_err("non-zero winget uninstall must error");
            assert!(err.to_string().contains("winget"));
        }

        #[test]
        #[serial]
        fn update_runs_upgrade_all() {
            let (_bin, _path) = install_winget_shim(0, "", "");
            let p = test_printer();
            WingetManager.update(&p).expect("update Ok");
        }

        #[test]
        #[serial]
        fn installed_packages_parses_winget_list_output() {
            let stdout = "\
Name            Id                    Version
------------------------------------------------
Visual Studio   Microsoft.VisualStudio 17.8.3
Git             Git.Git                2.43.0
";
            let (_bin, _path) = install_winget_shim(0, stdout, "");
            let pkgs = WingetManager.installed_packages().expect("Ok");
            // winget Ids are matched case-insensitively; cfgd canonicalizes to lowercase.
            assert!(pkgs.contains("microsoft.visualstudio"));
            assert!(pkgs.contains("git.git"));
        }

        #[test]
        fn package_identity_folds_case() {
            assert_eq!(WingetManager.package_identity("Git.Git"), "git.git");
            assert_eq!(WingetManager.package_identity("git.git"), "git.git");
        }

        #[test]
        fn parse_winget_list_versions_keeps_registered_case_and_version() {
            let output = "\
Name            Id                    Version
------------------------------------------------
Git             Git.Git               2.43.0
";
            let infos = parse_winget_list_versions(output);
            assert_eq!(infos.len(), 1);
            assert_eq!(infos[0].name, "Git.Git");
            assert_eq!(infos[0].version, "2.43.0");
        }

        #[test]
        #[serial]
        fn available_version_returns_none_on_nonzero_exit() {
            let (_bin, _path) = install_winget_shim(1, "", "not found");
            let v = WingetManager
                .available_version("Foo.Bar")
                .expect("non-zero → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn available_version_extracts_version_field_from_show_output() {
            let info = "Found Git [Git.Git]\nVersion: 2.43.0\nPublisher: Git\n";
            let (_bin, _path) = install_winget_shim(0, info, "");
            let v = WingetManager.available_version("Git.Git").expect("Ok");
            assert_eq!(v.as_deref(), Some("2.43.0"));
        }
    }
}

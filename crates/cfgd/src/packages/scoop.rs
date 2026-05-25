//! Scoop package manager (Windows).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{parse_version_field, run_pkg_cmd, run_pkg_cmd_live};

pub struct ScoopManager;

pub(super) fn parse_scoop_list(output: &str) -> HashSet<String> {
    let mut packages = HashSet::new();
    let mut header_passed = false;
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("----") {
            header_passed = true;
            continue;
        }
        if !header_passed || line.is_empty() {
            continue;
        }
        if let Some(name) = line.split_whitespace().next() {
            packages.insert(name.to_string());
        }
    }
    packages
}

impl PackageManager for ScoopManager {
    fn name(&self) -> &str {
        "scoop"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("scoop")
    }

    fn can_bootstrap(&self) -> bool {
        true
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
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

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("scoop", Command::new("scoop").arg("list"), "list")?;
        Ok(parse_scoop_list(&String::from_utf8_lossy(&output.stdout)))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            run_pkg_cmd_live(
                printer,
                "scoop",
                Command::new("scoop").args(["install", pkg]),
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
                "scoop",
                Command::new("scoop").args(["uninstall", pkg]),
                &format!("Uninstalling {}", pkg),
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "scoop",
            Command::new("scoop").args(["update", "*"]),
            "Upgrading all scoop packages",
            "install",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        let output = Command::new("scoop")
            .args(["info", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "scoop".into(),
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
    fn scoop_parse_list_output() {
        let output = "Installed apps:\n\n\
                      Name     Version Source\n\
                      ----     ------- ------\n\
                      7zip     23.01   main\n\
                      ripgrep  14.1.0  main\n\
                      fd       9.0.0   main\n";
        let packages = parse_scoop_list(output);
        assert!(packages.contains("7zip"));
        assert!(packages.contains("ripgrep"));
        assert!(packages.contains("fd"));
        assert_eq!(packages.len(), 3);
    }

    #[test]
    fn scoop_parse_list_empty() {
        let packages = parse_scoop_list("");
        assert!(packages.is_empty());
    }

    #[test]
    fn scoop_parse_list_no_separator() {
        // Without the ---- separator line, nothing is parsed
        let output = "Installed apps:\n\nName  Version  Source\n7zip  23.01    main\n";
        let packages = parse_scoop_list(output);
        assert!(packages.is_empty());
    }

    #[test]
    fn scoop_parse_list_only_separator() {
        let output = "----\n";
        let packages = parse_scoop_list(output);
        assert!(packages.is_empty());
    }

    #[test]
    fn scoop_parse_list_blank_lines_after_separator() {
        let output = "Name   Version  Source\n\
                      ----   -------  ------\n\
                      \n\
                      7zip   23.01    main\n\
                      \n\
                      fd     9.0.0   main\n";
        let packages = parse_scoop_list(output);
        assert_eq!(packages.len(), 2);
        assert!(packages.contains("7zip"));
        assert!(packages.contains("fd"));
    }

    #[test]
    fn scoop_manager_name_and_traits() {
        let mgr = ScoopManager;
        assert_eq!(mgr.name(), "scoop");
        assert!(mgr.can_bootstrap());
    }

    #[test]
    fn scoop_parse_list_single_package() {
        let output = "Name   Version  Source\n\
                      ----   -------  ------\n\
                      git    2.43.0   main\n";
        let packages = parse_scoop_list(output);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("git"));
    }

    #[test]
    fn scoop_parse_list_real_world_output() {
        let output = "\
Installed apps:

Name         Version       Source  Updated             Info
----         -------       ------  -------             ----
7zip         23.01         main    2024-01-15 10:30:00
fd           9.0.0         main    2024-01-10 08:15:00
git          2.43.0.windows.1 main 2024-01-12 14:20:00
ripgrep      14.1.0        main    2024-01-10 08:15:00
";
        let packages = parse_scoop_list(output);
        assert_eq!(packages.len(), 4);
        assert!(packages.contains("7zip"));
        assert!(packages.contains("fd"));
        assert!(packages.contains("git"));
        assert!(packages.contains("ripgrep"));
    }

    #[test]
    fn parse_scoop_list_basic() {
        let output = "\
Name    Version  Source   Updated\n\
----    -------  ------   -------\n\
git     2.44.0   main     2024-03-15\n\
nodejs  20.11.1  main     2024-02-01\n";
        let result = parse_scoop_list(output);
        assert!(result.contains("git"));
        assert!(result.contains("nodejs"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_scoop_list_empty_after_header() {
        let output = "Name    Version\n----    -------\n";
        let result = parse_scoop_list(output);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_scoop_list_skips_before_separator() {
        // Lines before the ---- separator should be ignored
        let output = "Installed apps:\nName    Version\n----    -------\ngit     2.44.0\n";
        let result = parse_scoop_list(output);
        assert!(result.contains("git"));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_scoop_list_multiple_separator_lines() {
        // Only the first ---- triggers header_passed
        let output = "Header line\n----\ngit 2.44.0\n----\nignored 1.0\n";
        let packages = parse_scoop_list(output);
        // After first ----, "git 2.44.0" is parsed.
        // Second "----" just continues (already header_passed), so it's skipped as a line starting with "----"
        // "ignored 1.0" is parsed normally
        assert!(packages.contains("git"));
        assert!(packages.contains("ignored"));
    }

    #[test]
    #[serial_test::serial]
    fn scoop_manager_is_available_checks_scoop() {
        let mgr = ScoopManager;
        let available = mgr.is_available();
        assert_eq!(available, command_available("scoop"));
    }

    #[test]
    fn scoop_manager_can_bootstrap_true() {
        let mgr = ScoopManager;
        assert!(mgr.can_bootstrap());
    }

    // ---------------------------------------------------------------------------
    // PackageManager trait impls via a fake scoop binary on PATH. scoop uses
    // `Command::new("scoop")` directly (no resolver indirection), so PATH
    // manipulation routes the invocation through our shim.
    // ---------------------------------------------------------------------------

    #[cfg(unix)]
    mod scoop_shim {
        use super::*;
        use cfgd_core::test_helpers::{install_named_path_shim, test_printer};
        use serial_test::serial;

        // Local wrapper: scoop is invoked by name via PATH, no env-var seam.
        // Delegates to the shared helper so the shim-script body stays in
        // one place across the package crate.
        fn install_scoop_shim(
            exit_code: u8,
            stdout: &str,
            stderr: &str,
        ) -> (tempfile::TempDir, cfgd_core::test_helpers::EnvVarGuard) {
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
            ScoopManager.bootstrap(&p).expect("bootstrap Ok via shim");
        }

        #[test]
        #[serial]
        fn scoop_bootstrap_propagates_powershell_failure() {
            let (_bin, _path) =
                install_named_path_shim("powershell", 1, "", "scoop install failed");
            let p = test_printer();
            let err = ScoopManager
                .bootstrap(&p)
                .expect_err("non-zero powershell must error");
            let _ = err.to_string();
        }

        #[test]
        #[serial]
        fn scoop_install_invokes_per_package() {
            let (_bin, _path) = install_scoop_shim(0, "", "");
            let p = test_printer();
            ScoopManager
                .install(&["git".into(), "ripgrep".into()], &p)
                .expect("install Ok");
        }

        #[test]
        #[serial]
        fn scoop_install_propagates_nonzero_exit_as_install_failed() {
            let (_bin, _path) = install_scoop_shim(1, "", "couldn't find git");
            let p = test_printer();
            let err = ScoopManager
                .install(&["git".into()], &p)
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
            ScoopManager
                .uninstall(&["git".into()], &p)
                .expect("uninstall Ok");
        }

        #[test]
        #[serial]
        fn scoop_uninstall_propagates_nonzero_exit_as_uninstall_failed() {
            let (_bin, _path) = install_scoop_shim(1, "", "no such package");
            let p = test_printer();
            let err = ScoopManager
                .uninstall(&["git".into()], &p)
                .expect_err("non-zero scoop uninstall must error");
            assert!(err.to_string().contains("scoop"));
        }

        #[test]
        #[serial]
        fn scoop_update_runs_update_star() {
            let (_bin, _path) = install_scoop_shim(0, "", "");
            let p = test_printer();
            ScoopManager.update(&p).expect("update Ok");
        }

        #[test]
        #[serial]
        fn scoop_installed_packages_parses_list_output() {
            let stdout = "Installed apps:\n\nName  Version  Source\n----  -------  ------\ngit  2.44.0  main\nfd  9.0.0  main\n";
            let (_bin, _path) = install_scoop_shim(0, stdout, "");
            let pkgs = ScoopManager.installed_packages().expect("Ok");
            assert!(pkgs.contains("git"));
            assert!(pkgs.contains("fd"));
            assert_eq!(pkgs.len(), 2);
        }

        #[test]
        #[serial]
        fn scoop_installed_packages_empty_when_no_separator() {
            let (_bin, _path) = install_scoop_shim(0, "no apps installed\n", "");
            let pkgs = ScoopManager.installed_packages().expect("Ok");
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

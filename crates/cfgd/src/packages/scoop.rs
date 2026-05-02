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
}

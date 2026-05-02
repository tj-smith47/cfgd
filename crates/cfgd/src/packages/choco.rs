//! Chocolatey package manager (`choco`).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{run_pkg_cmd, run_pkg_cmd_live};

pub struct ChocolateyManager;

pub(super) fn parse_choco_list(output: &str) -> HashSet<String> {
    let mut packages = HashSet::new();
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
        if let Some((name, _version)) = line.split_once(' ') {
            packages.insert(name.to_string());
        }
    }
    packages
}

impl PackageManager for ChocolateyManager {
    fn name(&self) -> &str {
        "chocolatey"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("choco")
    }

    fn can_bootstrap(&self) -> bool {
        true
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
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

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("chocolatey", Command::new("choco").args(["list"]), "list")?;
        Ok(parse_choco_list(&String::from_utf8_lossy(&output.stdout)))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        let mut args = vec!["install", "-y"];
        let pkg_refs: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
        args.extend(pkg_refs);
        run_pkg_cmd_live(
            printer,
            "chocolatey",
            Command::new("choco").args(&args),
            "Installing chocolatey packages",
            "install",
        )?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        let mut args = vec!["uninstall", "-y"];
        let pkg_refs: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
        args.extend(pkg_refs);
        run_pkg_cmd_live(
            printer,
            "chocolatey",
            Command::new("choco").args(&args),
            "Uninstalling chocolatey packages",
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "chocolatey",
            Command::new("choco").args(["upgrade", "all", "-y"]),
            "Upgrading all chocolatey packages",
            "install",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        let output = Command::new("choco")
            .args(["info", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "chocolatey".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_choco_info_version(&stdout))
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
        assert!(mgr.can_bootstrap());
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
    fn chocolatey_manager_is_available_checks_choco() {
        let mgr = ChocolateyManager;
        let available = mgr.is_available();
        assert_eq!(available, command_available("choco"));
    }

    #[test]
    fn chocolatey_manager_can_bootstrap_true() {
        let mgr = ChocolateyManager;
        assert!(mgr.can_bootstrap());
    }
}

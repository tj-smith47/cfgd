//! Homebrew family of package managers: `brew`, `brew-tap`, `brew-cask`.
//!
//! All three are exposed as separate `PackageManager` implementations so a
//! profile can declare formulae, taps, and casks independently while the
//! actual `brew` CLI handles each via different subcommands.

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{brew_available, brew_cmd, brew_path_dirs, run_pkg_cmd, run_pkg_cmd_live};

pub struct BrewManager;

impl BrewManager {
    fn run_brew(&self, args: &[&str]) -> std::result::Result<String, PackageError> {
        let output = run_pkg_cmd("brew", brew_cmd().args(args), "list")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub(super) fn installed_taps(&self) -> Result<HashSet<String>> {
        let output = self.run_brew(&["tap"])?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    pub(super) fn installed_casks(&self) -> Result<HashSet<String>> {
        let output = self.run_brew(&["list", "--cask", "-1"])?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }
}

// --- BrewTapManager ---

pub struct BrewTapManager;

impl PackageManager for BrewTapManager {
    fn name(&self) -> &str {
        "brew-tap"
    }

    fn is_available(&self) -> bool {
        brew_available()
    }

    fn can_bootstrap(&self) -> bool {
        false
    }

    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        BrewManager.installed_taps()
    }

    fn install(&self, taps: &[String], printer: &Printer) -> Result<()> {
        for tap in taps {
            let label = format!("brew tap {}", tap);
            run_pkg_cmd_live(
                printer,
                "brew-tap",
                brew_cmd().args(["tap", tap]),
                &label,
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, taps: &[String], printer: &Printer) -> Result<()> {
        for tap in taps {
            let label = format!("brew untap {}", tap);
            run_pkg_cmd_live(
                printer,
                "brew-tap",
                brew_cmd().args(["untap", tap]),
                &label,
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        // Taps are repository references, not versioned packages; nothing to update
        Ok(())
    }

    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        // Taps don't have versions
        Ok(None)
    }
}

// --- BrewCaskManager ---

pub struct BrewCaskManager;

impl PackageManager for BrewCaskManager {
    fn name(&self) -> &str {
        "brew-cask"
    }

    fn is_available(&self) -> bool {
        brew_available()
    }

    fn can_bootstrap(&self) -> bool {
        false
    }

    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        BrewManager.installed_casks()
    }

    fn install(&self, casks: &[String], printer: &Printer) -> Result<()> {
        if casks.is_empty() {
            return Ok(());
        }
        let label = format!("brew install --cask {}", casks.join(" "));
        run_pkg_cmd_live(
            printer,
            "brew-cask",
            brew_cmd().arg("install").arg("--cask").args(casks),
            &label,
            "install",
        )?;
        Ok(())
    }

    fn uninstall(&self, casks: &[String], printer: &Printer) -> Result<()> {
        if casks.is_empty() {
            return Ok(());
        }
        let label = format!("brew uninstall --cask {}", casks.join(" "));
        run_pkg_cmd_live(
            printer,
            "brew-cask",
            brew_cmd().arg("uninstall").arg("--cask").args(casks),
            &label,
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        // Cask updates are handled by `brew upgrade`; no separate cask update command
        Ok(())
    }

    fn available_version(&self, cask: &str) -> Result<Option<String>> {
        // brew info --json=v2 --cask <pkg> → .casks[0].version
        let output = brew_cmd()
            .args(["info", "--json=v2", "--cask", cask])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew-cask".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "brew-cask".into(),
                message: format!("failed to parse brew info output: {}", e),
            })?;
        Ok(parsed
            .pointer("/casks/0/version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }
}

impl PackageManager for BrewManager {
    fn name(&self) -> &str {
        "brew"
    }

    fn is_available(&self) -> bool {
        brew_available()
    }

    fn can_bootstrap(&self) -> bool {
        true
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        let install_url = "https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh";

        if cfg!(target_os = "linux") && cfgd_core::is_root() {
            // Linuxbrew-as-root: create linuxbrew user, install as that user
            printer.info("Creating linuxbrew system user");
            let user_status = Command::new("useradd")
                .args([
                    "--system",
                    "--create-home",
                    "--shell",
                    "/bin/bash",
                    "linuxbrew",
                ])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: format!("failed to create linuxbrew user: {}", e),
                })?;
            // Exit code 9 = user already exists, which is fine
            if !user_status.success() && user_status.code() != Some(9) {
                return Err(PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: "failed to create linuxbrew system user".into(),
                }
                .into());
            }

            let result = printer
                .run_with_output(
                    Command::new("sudo")
                        .args(["-u", "linuxbrew", "bash", "-c"])
                        .arg(format!(
                            "NONINTERACTIVE=1 /bin/bash -c \"$(curl -fsSL {})\"",
                            install_url
                        )),
                    "Installing Homebrew as linuxbrew user",
                )
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: format!("homebrew install failed: {}", e),
                })?;
            if !result.status.success() {
                return Err(PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: "homebrew install script failed".into(),
                }
                .into());
            }

            // PATH for brew commands will be augmented via brew_cmd()
        } else {
            let result = printer
                .run_with_output(
                    Command::new("bash").arg("-c").arg(format!(
                        "NONINTERACTIVE=1 /bin/bash -c \"$(curl -fsSL {})\"",
                        install_url
                    )),
                    "Installing Homebrew",
                )
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: format!("homebrew install failed: {}", e),
                })?;
            if !result.status.success() {
                return Err(PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: "homebrew install script failed".into(),
                }
                .into());
            }

            // PATH for brew commands will be augmented via brew_cmd()
        }

        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = self.run_brew(&["list", "--formulae", "-1"])?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let label = format!("brew install {}", packages.join(" "));
        run_pkg_cmd_live(
            printer,
            "brew",
            brew_cmd().arg("install").args(packages),
            &label,
            "install",
        )?;
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let label = format!("brew uninstall {}", packages.join(" "));
        run_pkg_cmd_live(
            printer,
            "brew",
            brew_cmd().arg("uninstall").args(packages),
            &label,
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "brew",
            brew_cmd().arg("update"),
            "brew update",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // brew info --json=v2 <pkg> → .formulae[0].versions.stable
        let output = brew_cmd()
            .args(["info", "--json=v2", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "brew".into(),
                message: format!("failed to parse brew info output: {}", e),
            })?;
        Ok(parsed
            .pointer("/formulae/0/versions/stable")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    fn path_dirs(&self) -> Vec<String> {
        brew_path_dirs()
    }

    fn installed_packages_with_versions(&self) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        let output = run_pkg_cmd("brew", brew_cmd().args(["list", "--versions"]), "list")?;
        Ok(parse_brew_versions(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }
}

/// Parse `brew list --versions` output (format: `package 1.2.3`) into PackageInfo.
/// Each line has package name followed by one or more version tokens separated by spaces.
/// We take the last version token as the installed version.
pub(super) fn parse_brew_versions(stdout: &str) -> Vec<cfgd_core::providers::PackageInfo> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut parts = line.splitn(2, ' ');
            let name = parts.next()?.trim();
            let version = parts
                .next()
                .and_then(|v| v.split_whitespace().last())
                .unwrap_or("unknown");
            if name.is_empty() {
                return None;
            }
            Some(cfgd_core::providers::PackageInfo {
                name: name.to_string(),
                version: version.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use cfgd_core::output::Printer;
    use cfgd_core::providers::PackageManager;

    use super::*;

    #[test]
    fn test_parse_brew_versions_basic() {
        let output = "git 2.43.0\nneovim 0.9.5\nripgrep 14.1.0\n";
        let pkgs = parse_brew_versions(output);
        assert_eq!(pkgs.len(), 3);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "git" && p.version == "2.43.0")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "neovim" && p.version == "0.9.5")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "ripgrep" && p.version == "14.1.0")
        );
    }

    #[test]
    fn test_parse_brew_versions_multi_version() {
        // brew list --versions can show multiple versions for some packages
        let output = "python@3.11 3.11.0 3.11.1\nfd 9.0.0\n";
        let pkgs = parse_brew_versions(output);
        assert_eq!(pkgs.len(), 2);
        // Multi-version: take the last token
        assert!(
            pkgs.iter()
                .any(|p| p.name == "python@3.11" && p.version == "3.11.1")
        );
        assert!(pkgs.iter().any(|p| p.name == "fd" && p.version == "9.0.0"));
    }

    #[test]
    fn test_parse_brew_versions_empty() {
        let pkgs = parse_brew_versions("");
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_parse_brew_versions_blank_lines() {
        let output = "\ngit 2.43.0\n\n";
        let pkgs = parse_brew_versions(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "git");
    }

    #[test]
    fn parse_brew_versions_name_only_no_version() {
        // A line with only a name and no version token
        let output = "somepackage\n";
        let pkgs = parse_brew_versions(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "somepackage");
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn parse_brew_versions_whitespace_only() {
        let output = "   \n  \n\n";
        let pkgs = parse_brew_versions(output);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn brew_manager_name_and_bootstrap() {
        let mgr = BrewManager;
        assert_eq!(mgr.name(), "brew");
        assert!(mgr.can_bootstrap());
    }

    #[test]
    fn brew_tap_manager_name_and_bootstrap() {
        let mgr = BrewTapManager;
        assert_eq!(mgr.name(), "brew-tap");
        assert!(!mgr.can_bootstrap());
        // bootstrap is a no-op
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.bootstrap(&printer).unwrap();
    }

    #[test]
    fn brew_cask_manager_name_and_bootstrap() {
        let mgr = BrewCaskManager;
        assert_eq!(mgr.name(), "brew-cask");
        assert!(!mgr.can_bootstrap());
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.bootstrap(&printer).unwrap();
    }

    #[test]
    fn brew_tap_manager_update_is_noop() {
        let mgr = BrewTapManager;
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.update(&printer).unwrap();
    }

    #[test]
    fn brew_tap_manager_available_version_is_none() {
        let mgr = BrewTapManager;
        assert!(mgr.available_version("any").unwrap().is_none());
    }

    #[test]
    fn brew_cask_manager_update_is_noop() {
        let mgr = BrewCaskManager;
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        mgr.update(&printer).unwrap();
    }

    #[test]
    fn brew_manager_path_dirs_returns_vec() {
        let mgr = BrewManager;
        let dirs = mgr.path_dirs();
        // On Linux CI, should return linuxbrew paths
        // On macOS, should return /opt/homebrew or /usr/local paths
        // On Windows, should return empty
        if cfg!(target_os = "windows") {
            assert!(dirs.is_empty());
        } else if cfg!(target_os = "linux") {
            assert_eq!(dirs.len(), 2);
            assert!(dirs[0].contains("linuxbrew"));
        }
    }

    #[test]
    fn parse_brew_versions_with_leading_trailing_whitespace() {
        let output = "  git 2.43.0  \n  fd 9.0.0  \n";
        let pkgs = parse_brew_versions(output);
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.iter().any(|p| p.name == "git"));
        assert!(pkgs.iter().any(|p| p.name == "fd"));
    }

    #[test]
    fn parse_brew_versions_single_package_no_trailing_newline() {
        let output = "ripgrep 14.1.0";
        let pkgs = parse_brew_versions(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "ripgrep");
        assert_eq!(pkgs[0].version, "14.1.0");
    }

    #[test]
    fn brew_manager_path_dirs_non_empty_on_unix() {
        if cfg!(unix) {
            let mgr = BrewManager;
            let dirs = mgr.path_dirs();
            assert!(!dirs.is_empty());
        }
    }

    #[test]
    fn parse_brew_versions_returns_sorted_stable_output() {
        let output = "zsh 5.9\nabc 1.0\nmno 2.0\n";
        let pkgs = parse_brew_versions(output);
        // Should maintain input order
        assert_eq!(pkgs[0].name, "zsh");
        assert_eq!(pkgs[1].name, "abc");
        assert_eq!(pkgs[2].name, "mno");
    }

    #[test]
    fn parse_brew_versions_scoped_package_names() {
        let output = "python@3.11 3.11.7\nnode@20 20.10.0\nopenjdk@17 17.0.9\n";
        let pkgs = parse_brew_versions(output);
        assert_eq!(pkgs.len(), 3);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "python@3.11" && p.version == "3.11.7")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "node@20" && p.version == "20.10.0")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "openjdk@17" && p.version == "17.0.9")
        );
    }

    #[test]
    fn parse_brew_versions_with_tab_separator() {
        // brew normally uses spaces, but test robustness
        let output = "git\t2.43.0\n";
        let pkgs = parse_brew_versions(output);
        // splitn(2, ' ') won't split on tab, so tab is part of the name
        // This documents the actual behavior: no version extracted
        assert_eq!(pkgs.len(), 1);
        // The whole "git\t2.43.0" is the name since split on space fails
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn parse_brew_versions_three_versions_takes_last() {
        let output = "python@3.12 3.12.0 3.12.1 3.12.2\n";
        let pkgs = parse_brew_versions(output);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "python@3.12");
        // split_whitespace().last() should give "3.12.2"
        assert_eq!(pkgs[0].version, "3.12.2");
    }

    #[test]
    fn brew_manager_is_available_checks_brew() {
        let mgr = BrewManager;
        let available = mgr.is_available();
        assert_eq!(available, brew_available());
    }

    #[test]
    fn brew_tap_manager_is_available_checks_brew() {
        let mgr = BrewTapManager;
        let available = mgr.is_available();
        assert_eq!(available, brew_available());
    }

    #[test]
    fn brew_cask_manager_is_available_checks_brew() {
        let mgr = BrewCaskManager;
        let available = mgr.is_available();
        assert_eq!(available, brew_available());
    }

    #[test]
    fn brew_cask_update_returns_ok() {
        let mgr = BrewCaskManager;
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        // brew-cask update is a no-op, should always succeed
        mgr.update(&printer).unwrap();
    }

    #[test]
    fn brew_tap_available_version_always_none() {
        let mgr = BrewTapManager;
        let result = mgr.available_version("homebrew/core").unwrap();
        assert!(result.is_none());
    }
}

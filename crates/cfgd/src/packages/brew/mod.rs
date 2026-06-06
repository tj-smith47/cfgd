//! Homebrew family of package managers: `brew`, `brew-tap`, `brew-cask`.
//!
//! All three are exposed as separate `PackageManager` implementations so a
//! profile can declare formulae, taps, and casks independently while the
//! actual `brew` CLI handles each via different subcommands.

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::PackageManager;

use super::shared::{
    brew_available, brew_cmd, brew_path_dirs, install_batch_then_per_package, run_pkg_cmd,
    run_pkg_cmd_live,
};

pub struct BrewManager;

impl BrewManager {
    fn run_brew(&self, args: &[&str]) -> std::result::Result<String, PackageError> {
        let output = run_pkg_cmd("brew", brew_cmd().args(args), "list")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub(super) fn installed_taps(&self) -> Result<HashSet<String>> {
        Ok(parse_brew_list_set(&self.run_brew(&["tap"])?))
    }

    pub(super) fn installed_casks(&self) -> Result<HashSet<String>> {
        Ok(parse_brew_list_set(
            &self.run_brew(&["list", "--cask", "-1"])?,
        ))
    }
}

/// Parse newline-separated brew list output (taps, casks, formulae) into a
/// trimmed `HashSet`, dropping empty / whitespace-only lines. Shared by
/// `installed_taps`, `installed_casks`, and `installed_packages`.
pub(super) fn parse_brew_list_set(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Pull a string field out of `brew info --json=v2` output by JSON pointer.
/// Used for both formula version (`/formulae/0/versions/stable`) and cask
/// version (`/casks/0/version`). Returns `Ok(None)` when the pointer doesn't
/// resolve or doesn't refer to a string — brew uses a sparse schema, so a
/// missing field is "no version known" not a hard error.
pub(super) fn parse_brew_info_version(
    stdout: &str,
    json_pointer: &str,
    manager: &'static str,
) -> Result<Option<String>> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| PackageError::ListFailed {
            manager: manager.into(),
            message: format!("failed to parse brew info output: {}", e),
        })?;
    Ok(parsed
        .pointer(json_pointer)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
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
        install_batch_then_per_package(printer, "brew-cask", casks, |pkgs| {
            let mut cmd = brew_cmd();
            cmd.arg("install").arg("--cask").args(pkgs);
            cmd
        })?;
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
        parse_brew_info_version(
            &String::from_utf8_lossy(&output.stdout),
            "/casks/0/version",
            "brew-cask",
        )
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
            printer.status_simple(Role::Info, "Creating linuxbrew system user");
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
                .run(
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
                .run(
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
        Ok(parse_brew_list_set(&self.run_brew(&[
            "list",
            "--formulae",
            "-1",
        ])?))
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        install_batch_then_per_package(printer, "brew", packages, |pkgs| {
            let mut cmd = brew_cmd();
            cmd.arg("install").args(pkgs);
            cmd
        })?;
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
        parse_brew_info_version(
            &String::from_utf8_lossy(&output.stdout),
            "/formulae/0/versions/stable",
            "brew",
        )
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
mod tests;

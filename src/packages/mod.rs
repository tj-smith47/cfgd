#![allow(dead_code)]

use std::collections::HashSet;
use std::process::Command;

use crate::config::{MergedProfile, PackagesSpec};
use crate::errors::{PackageError, Result};
use crate::output::Printer;
use crate::providers::{PackageAction, PackageManager};

/// Check if a command is available on the system.
fn command_available(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// --- Brew ---

pub struct BrewManager;

impl BrewManager {
    fn run_brew(&self, args: &[&str]) -> std::result::Result<String, PackageError> {
        let output =
            Command::new("brew")
                .args(args)
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "brew".into(),
                    source: e,
                })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::ListFailed {
                manager: "brew".into(),
                message: stderr,
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub fn installed_taps(&self) -> Result<HashSet<String>> {
        let output = self.run_brew(&["tap"])?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    pub fn installed_casks(&self) -> Result<HashSet<String>> {
        let output = self.run_brew(&["list", "--cask", "-1"])?;
        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    pub fn tap(&self, taps: &[String], printer: &Printer) -> Result<()> {
        for tap in taps {
            printer.info(&format!("brew tap {}", tap));
            let output = Command::new("brew")
                .args(["tap", tap])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "brew".into(),
                    source: e,
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                return Err(PackageError::InstallFailed {
                    manager: "brew".into(),
                    message: format!("tap {} failed: {}", tap, stderr),
                }
                .into());
            }
        }
        Ok(())
    }

    pub fn install_casks(&self, casks: &[String], printer: &Printer) -> Result<()> {
        if casks.is_empty() {
            return Ok(());
        }
        printer.info(&format!("brew install --cask {}", casks.join(" ")));
        let output = Command::new("brew")
            .arg("install")
            .arg("--cask")
            .args(casks)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::InstallFailed {
                manager: "brew".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }

    pub fn uninstall_casks(&self, casks: &[String], printer: &Printer) -> Result<()> {
        if casks.is_empty() {
            return Ok(());
        }
        printer.info(&format!("brew uninstall --cask {}", casks.join(" ")));
        let output = Command::new("brew")
            .arg("uninstall")
            .arg("--cask")
            .args(casks)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::UninstallFailed {
                manager: "brew".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }
}

impl PackageManager for BrewManager {
    fn name(&self) -> &str {
        "brew"
    }

    fn is_available(&self) -> bool {
        command_available("brew")
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
        printer.info(&format!("brew install {}", packages.join(" ")));
        let output = Command::new("brew")
            .arg("install")
            .args(packages)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::InstallFailed {
                manager: "brew".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("brew uninstall {}", packages.join(" ")));
        let output = Command::new("brew")
            .arg("uninstall")
            .args(packages)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::UninstallFailed {
                manager: "brew".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("brew update");
        let output = Command::new("brew").arg("update").output().map_err(|e| {
            PackageError::CommandFailed {
                manager: "brew".into(),
                source: e,
            }
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::InstallFailed {
                manager: "brew".into(),
                message: format!("update failed: {}", stderr),
            }
            .into());
        }
        Ok(())
    }
}

// --- Apt ---

pub struct AptManager;

impl PackageManager for AptManager {
    fn name(&self) -> &str {
        "apt"
    }

    fn is_available(&self) -> bool {
        command_available("apt")
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = Command::new("dpkg-query")
            .args(["-W", "-f", "${Package}\n"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "apt".into(),
                source: e,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::ListFailed {
                manager: "apt".into(),
                message: stderr,
            }
            .into());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("sudo apt install -y {}", packages.join(" ")));
        let output = Command::new("sudo")
            .arg("apt")
            .arg("install")
            .arg("-y")
            .args(packages)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "apt".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::InstallFailed {
                manager: "apt".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("sudo apt remove -y {}", packages.join(" ")));
        let output = Command::new("sudo")
            .arg("apt")
            .arg("remove")
            .arg("-y")
            .args(packages)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "apt".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::UninstallFailed {
                manager: "apt".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("sudo apt update");
        let output = Command::new("sudo")
            .arg("apt")
            .arg("update")
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "apt".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::InstallFailed {
                manager: "apt".into(),
                message: format!("update failed: {}", stderr),
            }
            .into());
        }
        Ok(())
    }
}

// --- Cargo ---

pub struct CargoManager;

impl PackageManager for CargoManager {
    fn name(&self) -> &str {
        "cargo"
    }

    fn is_available(&self) -> bool {
        command_available("cargo")
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = Command::new("cargo")
            .args(["install", "--list"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "cargo".into(),
                source: e,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::ListFailed {
                manager: "cargo".into(),
                message: stderr,
            }
            .into());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // `cargo install --list` format: "package_name v1.2.3:" followed by indented binary names
        // We only care about the package names (lines that don't start with whitespace)
        Ok(stdout
            .lines()
            .filter(|l| !l.starts_with(' ') && !l.is_empty())
            .filter_map(|l| l.split_whitespace().next())
            .map(|s| s.to_string())
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("cargo install {}", pkg));
            let output = Command::new("cargo")
                .args(["install", pkg])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "cargo".into(),
                    source: e,
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                return Err(PackageError::InstallFailed {
                    manager: "cargo".into(),
                    message: format!("{}: {}", pkg, stderr),
                }
                .into());
            }
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("cargo uninstall {}", pkg));
            let output = Command::new("cargo")
                .args(["uninstall", pkg])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "cargo".into(),
                    source: e,
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                return Err(PackageError::UninstallFailed {
                    manager: "cargo".into(),
                    message: format!("{}: {}", pkg, stderr),
                }
                .into());
            }
        }
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        // cargo install re-installs to update; no separate update command
        Ok(())
    }
}

// --- Npm ---

pub struct NpmManager;

impl PackageManager for NpmManager {
    fn name(&self) -> &str {
        "npm"
    }

    fn is_available(&self) -> bool {
        command_available("npm")
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = Command::new("npm")
            .args(["list", "-g", "--depth=0", "--json"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "npm".into(),
                source: e,
            })?;

        // npm list exits non-zero if there are peer dep issues, but still produces valid JSON
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "npm".into(),
                message: format!("failed to parse npm list output: {}", e),
            })?;

        let mut packages = HashSet::new();
        if let Some(deps) = parsed.get("dependencies").and_then(|d| d.as_object()) {
            for key in deps.keys() {
                packages.insert(key.clone());
            }
        }

        Ok(packages)
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("npm install -g {}", packages.join(" ")));
        let output = Command::new("npm")
            .arg("install")
            .arg("-g")
            .args(packages)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "npm".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::InstallFailed {
                manager: "npm".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("npm uninstall -g {}", packages.join(" ")));
        let output = Command::new("npm")
            .arg("uninstall")
            .arg("-g")
            .args(packages)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "npm".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::UninstallFailed {
                manager: "npm".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("npm update -g");
        let output = Command::new("npm")
            .args(["update", "-g"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "npm".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::InstallFailed {
                manager: "npm".into(),
                message: format!("update failed: {}", stderr),
            }
            .into());
        }
        Ok(())
    }
}

// --- Pipx ---

pub struct PipxManager;

impl PackageManager for PipxManager {
    fn name(&self) -> &str {
        "pipx"
    }

    fn is_available(&self) -> bool {
        command_available("pipx")
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = Command::new("pipx")
            .args(["list", "--json"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "pipx".into(),
                source: e,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::ListFailed {
                manager: "pipx".into(),
                message: stderr,
            }
            .into());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "pipx".into(),
                message: format!("failed to parse pipx list output: {}", e),
            })?;

        let mut packages = HashSet::new();
        if let Some(venvs) = parsed.get("venvs").and_then(|v| v.as_object()) {
            for key in venvs.keys() {
                packages.insert(key.clone());
            }
        }

        Ok(packages)
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("pipx install {}", pkg));
            let output = Command::new("pipx")
                .args(["install", pkg])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "pipx".into(),
                    source: e,
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                return Err(PackageError::InstallFailed {
                    manager: "pipx".into(),
                    message: format!("{}: {}", pkg, stderr),
                }
                .into());
            }
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            printer.info(&format!("pipx uninstall {}", pkg));
            let output = Command::new("pipx")
                .args(["uninstall", pkg])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "pipx".into(),
                    source: e,
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                return Err(PackageError::UninstallFailed {
                    manager: "pipx".into(),
                    message: format!("{}: {}", pkg, stderr),
                }
                .into());
            }
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("pipx upgrade-all");
        let output = Command::new("pipx")
            .args(["upgrade-all"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "pipx".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::InstallFailed {
                manager: "pipx".into(),
                message: format!("upgrade-all failed: {}", stderr),
            }
            .into());
        }
        Ok(())
    }
}

// --- Dnf ---

pub struct DnfManager;

impl PackageManager for DnfManager {
    fn name(&self) -> &str {
        "dnf"
    }

    fn is_available(&self) -> bool {
        command_available("dnf")
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = Command::new("dnf")
            .args(["list", "installed", "--quiet"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "dnf".into(),
                source: e,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::ListFailed {
                manager: "dnf".into(),
                message: stderr,
            }
            .into());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // dnf list output: "package-name.arch  version  repo"
        // Extract just the package name (before the first dot or whitespace)
        Ok(stdout
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("Installed") && !l.starts_with("Last"))
            .filter_map(|l| {
                let name = l.split_whitespace().next()?;
                // Strip architecture suffix (e.g., ".x86_64", ".noarch")
                Some(name.rsplit_once('.').map_or(name, |(n, _)| n).to_string())
            })
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("sudo dnf install -y {}", packages.join(" ")));
        let output = Command::new("sudo")
            .arg("dnf")
            .arg("install")
            .arg("-y")
            .args(packages)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "dnf".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::InstallFailed {
                manager: "dnf".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        printer.info(&format!("sudo dnf remove -y {}", packages.join(" ")));
        let output = Command::new("sudo")
            .arg("dnf")
            .arg("remove")
            .arg("-y")
            .args(packages)
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "dnf".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PackageError::UninstallFailed {
                manager: "dnf".into(),
                message: stderr,
            }
            .into());
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        printer.info("sudo dnf check-update");
        let _ = Command::new("sudo")
            .args(["dnf", "check-update"])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "dnf".into(),
                source: e,
            })?;
        // dnf check-update returns exit code 100 if updates are available, which is not an error
        Ok(())
    }
}

// --- Package Reconciler ---

/// Extract desired packages for a given manager from the merged profile.
pub fn desired_packages_for(manager_name: &str, packages: &MergedProfile) -> Vec<String> {
    desired_packages_for_spec(manager_name, &packages.packages)
}

fn desired_packages_for_spec(manager_name: &str, packages: &PackagesSpec) -> Vec<String> {
    match manager_name {
        "brew" => packages
            .brew
            .as_ref()
            .map(|b| b.formulae.clone())
            .unwrap_or_default(),
        "apt" => packages
            .apt
            .as_ref()
            .map(|a| a.install.clone())
            .unwrap_or_default(),
        "cargo" => packages.cargo.clone(),
        "npm" => packages
            .npm
            .as_ref()
            .map(|n| n.global.clone())
            .unwrap_or_default(),
        "pipx" => packages.pipx.clone(),
        "dnf" => packages.dnf.clone(),
        _ => Vec::new(),
    }
}

/// Plan package actions by diffing installed vs desired for all available managers.
pub fn plan_packages(
    profile: &MergedProfile,
    managers: &[&dyn PackageManager],
) -> Result<Vec<PackageAction>> {
    let packages = &profile.packages;
    let mut actions = Vec::new();

    for manager in managers {
        let desired = desired_packages_for_spec(manager.name(), packages);
        if desired.is_empty() {
            continue;
        }

        if !manager.is_available() {
            actions.push(PackageAction::Skip {
                manager: manager.name().to_string(),
                reason: format!("'{}' not available on this system", manager.name()),
                origin: "local".to_string(),
            });
            continue;
        }

        let installed = manager.installed_packages()?;

        let to_install: Vec<String> = desired
            .iter()
            .filter(|p| !installed.contains(*p))
            .cloned()
            .collect();

        if !to_install.is_empty() {
            actions.push(PackageAction::Install {
                manager: manager.name().to_string(),
                packages: to_install,
                origin: "local".to_string(),
            });
        }
    }

    // Brew has special handling for taps and casks
    for manager in managers {
        if manager.name() == "brew" && manager.is_available() {
            plan_brew_extras(packages, &mut actions)?;
            break;
        }
    }

    Ok(actions)
}

/// Plan brew-specific actions: taps and casks.
fn plan_brew_extras(packages: &PackagesSpec, actions: &mut Vec<PackageAction>) -> Result<()> {
    let brew = BrewManager;
    if let Some(ref brew_spec) = packages.brew {
        // Taps
        if !brew_spec.taps.is_empty() {
            let installed_taps = brew.installed_taps()?;
            let to_tap: Vec<String> = brew_spec
                .taps
                .iter()
                .filter(|t| !installed_taps.contains(*t))
                .cloned()
                .collect();
            if !to_tap.is_empty() {
                actions.push(PackageAction::Install {
                    manager: "brew-tap".to_string(),
                    packages: to_tap,
                    origin: "local".to_string(),
                });
            }
        }

        // Casks
        if !brew_spec.casks.is_empty() {
            let installed_casks = brew.installed_casks()?;
            let to_install: Vec<String> = brew_spec
                .casks
                .iter()
                .filter(|c| !installed_casks.contains(*c))
                .cloned()
                .collect();
            if !to_install.is_empty() {
                actions.push(PackageAction::Install {
                    manager: "brew-cask".to_string(),
                    packages: to_install,
                    origin: "local".to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Apply package actions.
pub fn apply_packages(
    actions: &[PackageAction],
    managers: &[&dyn PackageManager],
    printer: &Printer,
) -> Result<()> {
    let brew = BrewManager;

    for action in actions {
        match action {
            PackageAction::Install {
                manager: mgr_name,
                packages,
                ..
            } => {
                // Special handling for brew taps and casks
                if mgr_name == "brew-tap" {
                    brew.tap(packages, printer)?;
                    continue;
                }
                if mgr_name == "brew-cask" {
                    brew.install_casks(packages, printer)?;
                    continue;
                }
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.install(packages, printer)?;
                }
            }
            PackageAction::Uninstall {
                manager: mgr_name,
                packages,
                ..
            } => {
                if mgr_name == "brew-cask" {
                    brew.uninstall_casks(packages, printer)?;
                    continue;
                }
                if let Some(mgr) = managers.iter().find(|m| m.name() == mgr_name) {
                    mgr.uninstall(packages, printer)?;
                }
            }
            PackageAction::Skip {
                manager, reason, ..
            } => {
                printer.warning(&format!("{}: {}", manager, reason));
            }
        }
    }

    Ok(())
}

/// Format package actions as human-readable plan items.
pub fn format_package_actions(actions: &[PackageAction]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| match a {
            PackageAction::Install {
                manager, packages, ..
            } => Some(format!("install via {}: {}", manager, packages.join(", "))),
            PackageAction::Uninstall {
                manager, packages, ..
            } => Some(format!(
                "uninstall via {}: {}",
                manager,
                packages.join(", ")
            )),
            PackageAction::Skip { .. } => None,
        })
        .collect()
}

/// Add a package to the profile's package spec.
pub fn add_package(
    manager_name: &str,
    package_name: &str,
    packages: &mut PackagesSpec,
) -> Result<()> {
    match manager_name {
        "brew" => {
            let brew = packages.brew.get_or_insert_with(Default::default);
            if !brew.formulae.contains(&package_name.to_string()) {
                brew.formulae.push(package_name.to_string());
            }
        }
        "brew-tap" => {
            let brew = packages.brew.get_or_insert_with(Default::default);
            if !brew.taps.contains(&package_name.to_string()) {
                brew.taps.push(package_name.to_string());
            }
        }
        "brew-cask" => {
            let brew = packages.brew.get_or_insert_with(Default::default);
            if !brew.casks.contains(&package_name.to_string()) {
                brew.casks.push(package_name.to_string());
            }
        }
        "apt" => {
            let apt = packages.apt.get_or_insert_with(Default::default);
            if !apt.install.contains(&package_name.to_string()) {
                apt.install.push(package_name.to_string());
            }
        }
        "cargo" => {
            if !packages.cargo.contains(&package_name.to_string()) {
                packages.cargo.push(package_name.to_string());
            }
        }
        "npm" => {
            let npm = packages.npm.get_or_insert_with(Default::default);
            if !npm.global.contains(&package_name.to_string()) {
                npm.global.push(package_name.to_string());
            }
        }
        "pipx" => {
            if !packages.pipx.contains(&package_name.to_string()) {
                packages.pipx.push(package_name.to_string());
            }
        }
        "dnf" => {
            if !packages.dnf.contains(&package_name.to_string()) {
                packages.dnf.push(package_name.to_string());
            }
        }
        _ => {
            return Err(PackageError::ManagerNotAvailable {
                manager: manager_name.to_string(),
            }
            .into());
        }
    }
    Ok(())
}

/// Remove a package from the profile's package spec.
pub fn remove_package(
    manager_name: &str,
    package_name: &str,
    packages: &mut PackagesSpec,
) -> Result<bool> {
    let removed = match manager_name {
        "brew" => {
            if let Some(ref mut brew) = packages.brew {
                let before = brew.formulae.len();
                brew.formulae.retain(|p| p != package_name);
                brew.formulae.len() < before
            } else {
                false
            }
        }
        "brew-tap" => {
            if let Some(ref mut brew) = packages.brew {
                let before = brew.taps.len();
                brew.taps.retain(|p| p != package_name);
                brew.taps.len() < before
            } else {
                false
            }
        }
        "brew-cask" => {
            if let Some(ref mut brew) = packages.brew {
                let before = brew.casks.len();
                brew.casks.retain(|p| p != package_name);
                brew.casks.len() < before
            } else {
                false
            }
        }
        "apt" => {
            if let Some(ref mut apt) = packages.apt {
                let before = apt.install.len();
                apt.install.retain(|p| p != package_name);
                apt.install.len() < before
            } else {
                false
            }
        }
        "cargo" => {
            let before = packages.cargo.len();
            packages.cargo.retain(|p| p != package_name);
            packages.cargo.len() < before
        }
        "npm" => {
            if let Some(ref mut npm) = packages.npm {
                let before = npm.global.len();
                npm.global.retain(|p| p != package_name);
                npm.global.len() < before
            } else {
                false
            }
        }
        "pipx" => {
            let before = packages.pipx.len();
            packages.pipx.retain(|p| p != package_name);
            packages.pipx.len() < before
        }
        "dnf" => {
            let before = packages.dnf.len();
            packages.dnf.retain(|p| p != package_name);
            packages.dnf.len() < before
        }
        _ => {
            return Err(PackageError::ManagerNotAvailable {
                manager: manager_name.to_string(),
            }
            .into());
        }
    };
    Ok(removed)
}

/// Build the default provider registry with all workstation package managers.
pub fn all_package_managers() -> Vec<Box<dyn PackageManager>> {
    vec![
        Box::new(BrewManager),
        Box::new(AptManager),
        Box::new(CargoManager),
        Box::new(NpmManager),
        Box::new(PipxManager),
        Box::new(DnfManager),
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct MockPackageManager {
        mgr_name: &'static str,
        available: bool,
        installed: HashSet<String>,
        installs: Mutex<Vec<Vec<String>>>,
        uninstalls: Mutex<Vec<Vec<String>>>,
    }

    impl MockPackageManager {
        fn new(name: &'static str, available: bool, installed: Vec<&str>) -> Self {
            Self {
                mgr_name: name,
                available,
                installed: installed.into_iter().map(String::from).collect(),
                installs: Mutex::new(Vec::new()),
                uninstalls: Mutex::new(Vec::new()),
            }
        }
    }

    impl PackageManager for MockPackageManager {
        fn name(&self) -> &str {
            self.mgr_name
        }

        fn is_available(&self) -> bool {
            self.available
        }

        fn installed_packages(&self) -> Result<HashSet<String>> {
            Ok(self.installed.clone())
        }

        fn install(&self, packages: &[String], _printer: &Printer) -> Result<()> {
            self.installs.lock().unwrap().push(packages.to_vec());
            Ok(())
        }

        fn uninstall(&self, packages: &[String], _printer: &Printer) -> Result<()> {
            self.uninstalls.lock().unwrap().push(packages.to_vec());
            Ok(())
        }

        fn update(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
    }

    fn test_profile(packages: PackagesSpec) -> MergedProfile {
        MergedProfile {
            packages,
            ..Default::default()
        }
    }

    #[test]
    fn plan_installs_missing_packages() {
        let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
        let profile = test_profile(PackagesSpec {
            cargo: vec!["bat".into(), "ripgrep".into(), "fd-find".into()],
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PackageAction::Install {
                manager, packages, ..
            } => {
                assert_eq!(manager, "cargo");
                assert!(packages.contains(&"ripgrep".to_string()));
                assert!(packages.contains(&"fd-find".to_string()));
                assert!(!packages.contains(&"bat".to_string()));
            }
            _ => panic!("expected Install action"),
        }
    }

    #[test]
    fn plan_skips_unavailable_manager() {
        let mock = MockPackageManager::new("brew", false, vec![]);
        let profile = test_profile(PackagesSpec {
            brew: Some(crate::config::BrewSpec {
                formulae: vec!["ripgrep".into()],
                ..Default::default()
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], PackageAction::Skip { .. }));
    }

    #[test]
    fn plan_empty_when_all_installed() {
        let mock = MockPackageManager::new("cargo", true, vec!["bat", "ripgrep"]);
        let profile = test_profile(PackagesSpec {
            cargo: vec!["bat".into(), "ripgrep".into()],
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert!(actions.is_empty());
    }

    #[test]
    fn plan_skips_manager_with_no_desired_packages() {
        let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
        let profile = test_profile(PackagesSpec::default());

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        assert!(actions.is_empty());
    }

    #[test]
    fn format_actions_produces_readable_strings() {
        let actions = vec![
            PackageAction::Install {
                manager: "brew".into(),
                packages: vec!["ripgrep".into(), "fd".into()],
                origin: "local".into(),
            },
            PackageAction::Skip {
                manager: "apt".into(),
                reason: "not available".into(),
                origin: "local".into(),
            },
        ];

        let formatted = format_package_actions(&actions);
        assert_eq!(formatted.len(), 1);
        assert!(formatted[0].contains("brew"));
        assert!(formatted[0].contains("ripgrep"));
    }

    #[test]
    fn add_package_to_spec() {
        let mut packages = PackagesSpec::default();

        add_package("cargo", "ripgrep", &mut packages).unwrap();
        assert_eq!(packages.cargo, vec!["ripgrep"]);

        // Adding again is idempotent
        add_package("cargo", "ripgrep", &mut packages).unwrap();
        assert_eq!(packages.cargo, vec!["ripgrep"]);

        add_package("brew", "fd", &mut packages).unwrap();
        assert_eq!(packages.brew.as_ref().unwrap().formulae, vec!["fd"]);

        add_package("brew-cask", "firefox", &mut packages).unwrap();
        assert_eq!(packages.brew.as_ref().unwrap().casks, vec!["firefox"]);

        add_package("apt", "curl", &mut packages).unwrap();
        assert_eq!(packages.apt.as_ref().unwrap().install, vec!["curl"]);

        add_package("npm", "typescript", &mut packages).unwrap();
        assert_eq!(packages.npm.as_ref().unwrap().global, vec!["typescript"]);

        add_package("pipx", "black", &mut packages).unwrap();
        assert_eq!(packages.pipx, vec!["black"]);

        add_package("dnf", "gcc", &mut packages).unwrap();
        assert_eq!(packages.dnf, vec!["gcc"]);
    }

    #[test]
    fn add_package_unknown_manager_errors() {
        let mut packages = PackagesSpec::default();
        let result = add_package("unknown", "pkg", &mut packages);
        assert!(result.is_err());
    }

    #[test]
    fn remove_package_from_spec() {
        let mut packages = PackagesSpec {
            cargo: vec!["bat".into(), "ripgrep".into()],
            ..Default::default()
        };

        let removed = remove_package("cargo", "bat", &mut packages).unwrap();
        assert!(removed);
        assert_eq!(packages.cargo, vec!["ripgrep"]);

        let removed = remove_package("cargo", "nonexistent", &mut packages).unwrap();
        assert!(!removed);
    }

    #[test]
    fn remove_package_unknown_manager_errors() {
        let mut packages = PackagesSpec::default();
        let result = remove_package("unknown", "pkg", &mut packages);
        assert!(result.is_err());
    }

    #[test]
    fn apply_calls_install_on_correct_manager() {
        let mock = MockPackageManager::new("cargo", true, vec![]);
        let actions = vec![PackageAction::Install {
            manager: "cargo".into(),
            packages: vec!["ripgrep".into()],
            origin: "local".into(),
        }];

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let printer = Printer::new(crate::output::Verbosity::Quiet);
        apply_packages(&actions, &managers, &printer).unwrap();

        let installs = mock.installs.lock().unwrap();
        assert_eq!(installs.len(), 1);
        assert_eq!(installs[0], vec!["ripgrep"]);
    }

    #[test]
    fn apply_calls_uninstall_on_correct_manager() {
        let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
        let actions = vec![PackageAction::Uninstall {
            manager: "cargo".into(),
            packages: vec!["bat".into()],
            origin: "local".into(),
        }];

        let managers: Vec<&dyn PackageManager> = vec![&mock];
        let printer = Printer::new(crate::output::Verbosity::Quiet);
        apply_packages(&actions, &managers, &printer).unwrap();

        let uninstalls = mock.uninstalls.lock().unwrap();
        assert_eq!(uninstalls.len(), 1);
        assert_eq!(uninstalls[0], vec!["bat"]);
    }

    #[test]
    fn all_package_managers_creates_all_six() {
        let managers = all_package_managers();
        assert_eq!(managers.len(), 6);

        let names: Vec<&str> = managers.iter().map(|m| m.name()).collect();
        assert!(names.contains(&"brew"));
        assert!(names.contains(&"apt"));
        assert!(names.contains(&"cargo"));
        assert!(names.contains(&"npm"));
        assert!(names.contains(&"pipx"));
        assert!(names.contains(&"dnf"));
    }

    #[test]
    fn plan_multiple_managers() {
        let cargo_mock = MockPackageManager::new("cargo", true, vec![]);
        let npm_mock = MockPackageManager::new("npm", true, vec!["typescript"]);

        let profile = test_profile(PackagesSpec {
            cargo: vec!["ripgrep".into()],
            npm: Some(crate::config::NpmSpec {
                global: vec!["typescript".into(), "eslint".into()],
            }),
            ..Default::default()
        });

        let managers: Vec<&dyn PackageManager> = vec![&cargo_mock, &npm_mock];
        let actions = plan_packages(&profile, &managers).unwrap();

        // cargo needs ripgrep, npm needs eslint (typescript already installed)
        assert_eq!(actions.len(), 2);

        let cargo_action = actions.iter().find(|a| match a {
            PackageAction::Install { manager, .. } => manager == "cargo",
            _ => false,
        });
        assert!(cargo_action.is_some());

        let npm_action = actions.iter().find(|a| match a {
            PackageAction::Install { manager, .. } => manager == "npm",
            _ => false,
        });
        assert!(npm_action.is_some());
        if let Some(PackageAction::Install { packages, .. }) = npm_action {
            assert_eq!(packages, &vec!["eslint".to_string()]);
        }
    }
}

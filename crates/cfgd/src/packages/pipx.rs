//! pipx-based package manager.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{
    bootstrap_via_brew_then_system, brew_available, resolve_tool_with_fallbacks, run_pkg_cmd,
    run_pkg_cmd_live, tool_cmd_with_resolver,
};

pub struct PipxManager;

fn pipx_fallbacks() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| vec![PathBuf::from(h).join(".local/bin/pipx")])
        .unwrap_or_default()
}

pub(super) fn find_pipx() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("pipx", &pipx_fallbacks())
}

pub(super) fn pipx_available() -> bool {
    find_pipx().is_some()
}

pub(super) fn pipx_cmd() -> Command {
    tool_cmd_with_resolver("pipx", find_pipx)
}

impl PackageManager for PipxManager {
    fn name(&self) -> &str {
        "pipx"
    }

    fn is_available(&self) -> bool {
        pipx_available()
    }

    fn can_bootstrap(&self) -> bool {
        // Can bootstrap via system package manager or pip
        brew_available()
            || command_available("apt")
            || command_available("dnf")
            || command_available("pip3")
            || command_available("pip")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        if bootstrap_via_brew_then_system(printer, "pipx", "pipx", &["pipx"])? {
            return Ok(());
        }

        // Fall back to pip
        let pip_cmd = if command_available("pip3") {
            "pip3"
        } else if command_available("pip") {
            "pip"
        } else {
            return Err(PackageError::BootstrapFailed {
                manager: "pipx".into(),
                message: "no installation method available".into(),
            }
            .into());
        };

        let label = format!("Installing pipx via {}", pip_cmd);
        let result = printer
            .run_with_output(
                Command::new(pip_cmd).args(["install", "--user", "pipx"]),
                &label,
            )
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "pipx".into(),
                message: format!("{} install failed: {}", pip_cmd, e),
            })?;
        if !result.status.success() {
            return Err(PackageError::BootstrapFailed {
                manager: "pipx".into(),
                message: format!("{} install --user pipx failed", pip_cmd),
            }
            .into());
        }

        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("pipx", pipx_cmd().args(["list", "--json"]), "list")?;
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
            let label = format!("pipx install {}", pkg);
            run_pkg_cmd_live(
                printer,
                "pipx",
                pipx_cmd().args(["install", pkg]),
                &label,
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            let label = format!("pipx uninstall {}", pkg);
            run_pkg_cmd_live(
                printer,
                "pipx",
                pipx_cmd().args(["uninstall", pkg]),
                &label,
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "pipx",
            pipx_cmd().args(["upgrade-all"]),
            "pipx upgrade-all",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // Query PyPI JSON API: https://pypi.org/pypi/<pkg>/json → .info.version
        let url = format!("https://pypi.org/pypi/{}/json", package);
        let output = Command::new("curl")
            .args(["-fsSL", &url])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "pipx".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "pipx".into(),
                message: format!("failed to parse PyPI response: {}", e),
            })?;
        Ok(parsed
            .pointer("/info/version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    fn installed_packages_with_versions(&self) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        let output = run_pkg_cmd("pipx", pipx_cmd().args(["list", "--json"]), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "pipx".into(),
                message: format!("failed to parse pipx list output: {}", e),
            })?;
        Ok(parse_pipx_list_versions(&parsed))
    }
}

/// Parse `pipx list --json` venvs object into PackageInfo.
/// JSON format: `{"venvs": {"pkg": {"metadata": {"main_package": {"package_version": "1.2.3"}}}}}`
pub(super) fn parse_pipx_list_versions(
    parsed: &serde_json::Value,
) -> Vec<cfgd_core::providers::PackageInfo> {
    let mut packages = Vec::new();
    if let Some(venvs) = parsed.get("venvs").and_then(|v| v.as_object()) {
        for (name, info) in venvs {
            let version = info
                .pointer("/metadata/main_package/package_version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            packages.push(cfgd_core::providers::PackageInfo {
                name: name.clone(),
                version,
            });
        }
    }
    packages
}

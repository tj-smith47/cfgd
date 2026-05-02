//! Windows Package Manager (`winget`).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{parse_version_field, run_pkg_cmd, run_pkg_cmd_live};

pub struct WingetManager;

pub(super) fn parse_winget_list(output: &str) -> HashSet<String> {
    let mut packages = HashSet::new();
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
                packages.insert(id.to_string());
            }
        }
    }
    packages
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

//! Snap package manager (Linux only).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

#[cfg(target_os = "linux")]
use super::shared::linux_system_manager_available;
use super::shared::{bootstrap_via_system_manager, run_pkg_cmd, run_pkg_cmd_live, sudo_cmd};

pub struct SnapManager;

impl PackageManager for SnapManager {
    fn name(&self) -> &str {
        "snap"
    }

    fn is_available(&self) -> bool {
        command_available("snap")
    }

    fn can_bootstrap(&self) -> bool {
        // snap is a Linux-only package manager; bootstrappable via apt/dnf/zypper.
        // On non-Linux platforms it is never available.
        #[cfg(target_os = "linux")]
        {
            linux_system_manager_available()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        bootstrap_via_system_manager(printer, "snapd", "snap")
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("snap", Command::new("snap").args(["list"]), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // snap list output: "Name  Version  Rev  Tracking  Publisher  Notes"
        // Skip header line
        Ok(stdout
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().next().map(|s| s.to_string()))
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        // Snap requires individual install commands for --classic flag per package
        for pkg in packages {
            let label = format!("snap install {}", pkg);
            let result = run_pkg_cmd_live(
                printer,
                "snap",
                sudo_cmd("snap").arg("install").arg(pkg),
                &label,
                "install",
            );
            if let Err(ref e) = result {
                // If install fails and stderr mentions classic confinement, retry with --classic
                if e.to_string().contains("classic") {
                    let label = format!("snap install --classic {}", pkg);
                    run_pkg_cmd_live(
                        printer,
                        "snap",
                        sudo_cmd("snap").args(["install", "--classic", pkg]),
                        &label,
                        "install",
                    )?;
                } else {
                    result?;
                }
            }
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let label = format!("snap remove {}", packages.join(" "));
        run_pkg_cmd_live(
            printer,
            "snap",
            sudo_cmd("snap").arg("remove").args(packages),
            &label,
            "uninstall",
        )?;
        Ok(())
    }

    fn update(&self, printer: &Printer) -> Result<()> {
        run_pkg_cmd_live(
            printer,
            "snap",
            sudo_cmd("snap").arg("refresh"),
            "snap refresh",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // snap info <pkg> → parse "latest/stable:" or first channel line for version
        let output = Command::new("snap")
            .args(["info", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "snap".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_snap_info_version(&stdout))
    }
}

/// Parse version from `snap info` output.
/// Looks for "latest/stable:" or "stable:" channel lines.
/// Format: "latest/stable: 0.10.2 2024-01-01 (1234) 12MB classic"
pub(super) fn parse_snap_info_version(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("latest/stable:") || trimmed.starts_with("stable:") {
            let parts: Vec<&str> = trimmed.splitn(2, ':').collect();
            if parts.len() == 2 {
                let version = parts[1].split_whitespace().next().unwrap_or("");
                if !version.is_empty() && version != "^" && version != "--" {
                    return Some(version.to_string());
                }
            }
        }
    }
    None
}

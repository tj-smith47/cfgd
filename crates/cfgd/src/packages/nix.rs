//! Nix package manager (`nix profile` and `nix-env`).

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{run_pkg_cmd, run_pkg_cmd_live, strip_version_suffix};

pub struct NixManager;

impl PackageManager for NixManager {
    fn name(&self) -> &str {
        "nix"
    }

    fn is_available(&self) -> bool {
        command_available("nix-env") || command_available("nix")
    }

    fn can_bootstrap(&self) -> bool {
        command_available("curl")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        let result = printer
            .run_with_output(
                Command::new("bash")
                    .arg("-c")
                    .arg("curl -L https://nixos.org/nix/install | sh -s -- --daemon"),
                "Installing Nix",
            )
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "nix".into(),
                message: format!("nix install failed: {}", e),
            })?;
        if !result.status.success() {
            return Err(PackageError::BootstrapFailed {
                manager: "nix".into(),
                message: "nix install script failed".into(),
            }
            .into());
        }
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        // Try `nix profile list` first (new-style), fall back to `nix-env -q`
        if command_available("nix") {
            let output = Command::new("nix")
                .args(["profile", "list"])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "nix".into(),
                    source: e,
                })?;

            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // nix profile list output: index, flake ref, store path — extract package name
                // from the flake ref or store path
                let packages: HashSet<String> = stdout
                    .lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| {
                        // Format varies; extract the package name from the last path component
                        let parts: Vec<&str> = l.split_whitespace().collect();
                        if parts.len() >= 2 {
                            // Try to extract from flake ref like "nixpkgs#ripgrep"
                            if let Some(name) = parts[1].rsplit('#').next() {
                                return Some(name.to_string());
                            }
                        }
                        None
                    })
                    .collect();
                return Ok(packages);
            }
        }

        // Fallback: nix-env -q
        let output = run_pkg_cmd(
            "nix",
            Command::new("nix-env").args(["-q", "--no-name", "--attr-path"]),
            "list",
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // nix-env -q output: "name-version" — strip version
        Ok(stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| strip_version_suffix(l.trim()))
            .collect())
    }

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            if command_available("nix") {
                let label = format!("nix profile install nixpkgs#{}", pkg);
                run_pkg_cmd_live(
                    printer,
                    "nix",
                    Command::new("nix").args(["profile", "install", &format!("nixpkgs#{}", pkg)]),
                    &label,
                    "install",
                )?;
            } else {
                let label = format!("nix-env -iA nixpkgs.{}", pkg);
                run_pkg_cmd_live(
                    printer,
                    "nix",
                    Command::new("nix-env").args(["-iA", &format!("nixpkgs.{}", pkg)]),
                    &label,
                    "install",
                )?;
            }
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            if command_available("nix") {
                let label = format!("nix profile remove nixpkgs#{}", pkg);
                run_pkg_cmd_live(
                    printer,
                    "nix",
                    Command::new("nix").args(["profile", "remove", &format!("nixpkgs#{}", pkg)]),
                    &label,
                    "uninstall",
                )?;
            } else {
                let label = format!("nix-env -e {}", pkg);
                run_pkg_cmd_live(
                    printer,
                    "nix",
                    Command::new("nix-env").args(["-e", pkg]),
                    &label,
                    "uninstall",
                )?;
            }
        }
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        // Nix packages are pinned; update is a no-op (channels are managed separately)
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // nix search nixpkgs <pkg> --json → parse version from first matching result
        if command_available("nix") {
            let output = Command::new("nix")
                .args(["search", "nixpkgs", package, "--json"])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "nix".into(),
                    source: e,
                })?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(v) = parse_nix_search_version(&stdout) {
                    return Ok(Some(v));
                }
            }
        }
        Ok(None)
    }
}

/// Parse version from `nix search nixpkgs <pkg> --json` output.
/// JSON format: `{"nixpkgs.pkg": {"version": "1.2.3", ...}, ...}`
/// Returns the version of the first result.
pub(super) fn parse_nix_search_version(output: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(output).ok()?;
    let obj = parsed.as_object()?;
    for value in obj.values() {
        if let Some(version) = value.get("version").and_then(|v| v.as_str())
            && !version.is_empty()
        {
            return Some(version.to_string());
        }
    }
    None
}

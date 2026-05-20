use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

// ---------------------------------------------------------------------------
// AppArmorConfigurator
// ---------------------------------------------------------------------------

/// Manages AppArmor profiles for container security.
///
/// Config format:
/// ```yaml
/// apparmor:
///   profiles:
///     - name: cfgd-containerd-default
///       path: /etc/apparmor.d/cfgd-containerd-default
///       content: |
///         #include <tunables/global>
///         profile cfgd-containerd-default flags=(attach_disconnected) {
///           #include <abstractions/base>
///           file,
///           network,
///           capability,
///         }
/// ```
pub struct AppArmorConfigurator;

impl AppArmorConfigurator {
    fn is_profile_loaded(name: &str) -> bool {
        let status_path = Path::new("/sys/kernel/security/apparmor/profiles");
        if status_path.exists()
            && let Ok(content) = fs::read_to_string(status_path)
        {
            return content
                .lines()
                .any(|line| line.split_whitespace().next().is_some_and(|n| n == name));
        }

        Command::new("aa-status")
            .arg("--json")
            .output()
            .ok()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                // Check for exact profile name match — JSON output uses profile names as keys
                // wrapped in quotes: "profile_name"
                let quoted = format!("\"{}\"", name);
                stdout.contains(&quoted)
            })
            .unwrap_or(false)
    }

    fn load_profile(path: &Path) -> Result<()> {
        let output = Command::new("apparmor_parser")
            .arg("-r")
            .arg(path)
            .output()
            .map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "apparmor_parser -r {} failed: {}",
                path.display(),
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
        Ok(())
    }
}

impl SystemConfigurator for AppArmorConfigurator {
    fn name(&self) -> &str {
        "apparmor"
    }

    fn is_available(&self) -> bool {
        Path::new("/sys/kernel/security/apparmor").exists()
            || cfgd_core::command_available("apparmor_parser")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mut drifts = Vec::new();

        let profiles = match desired.get("profiles").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => return Ok(drifts),
        };

        for profile in profiles {
            let name = match profile.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let path = match profile.get("path").and_then(|v| v.as_str()) {
                Some(p) => PathBuf::from(p),
                None => continue,
            };
            if cfgd_core::validate_no_traversal(&path).is_err() {
                continue;
            }

            if !path.exists() {
                drifts.push(SystemDrift {
                    key: format!("apparmor.{}.file", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
                continue;
            }

            if let Some(desired_content) = profile.get("content").and_then(|v| v.as_str())
                && let Ok(current_content) = fs::read_to_string(&path)
                && current_content.trim() != desired_content.trim()
            {
                drifts.push(SystemDrift {
                    key: format!("apparmor.{}.content", name),
                    expected: "updated".to_string(),
                    actual: "outdated".to_string(),
                });
            }

            if !Self::is_profile_loaded(name) {
                drifts.push(SystemDrift {
                    key: format!("apparmor.{}.loaded", name),
                    expected: "loaded".to_string(),
                    actual: "not loaded".to_string(),
                });
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let profiles = match desired.get("profiles").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => return Ok(()),
        };

        for profile in profiles {
            let name = match profile.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let path = match profile.get("path").and_then(|v| v.as_str()) {
                Some(p) => PathBuf::from(p),
                None => continue,
            };
            if cfgd_core::validate_no_traversal(&path).is_err() {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "Skipping AppArmor profile {}: path traversal detected",
                        name
                    ),
                );
                continue;
            }

            if let Some(content) = profile.get("content").and_then(|v| v.as_str()) {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                printer.status_simple(
                    Role::Info,
                    format!("Writing AppArmor profile: {}", path.display()),
                );
                cfgd_core::atomic_write_str(&path, content)?;
            }

            printer.status_simple(Role::Info, format!("Loading AppArmor profile: {}", name));
            Self::load_profile(&path)?;
        }

        Ok(())
    }
}

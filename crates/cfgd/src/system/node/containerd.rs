use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::super::{diff_yaml_mapping, yaml_value_to_string};
use super::format::{find_toml_value, set_toml_value};

// ---------------------------------------------------------------------------
// ContainerdConfigurator
// ---------------------------------------------------------------------------

/// Manages containerd configuration.
///
/// Config format:
/// ```yaml
/// containerd:
///   configPath: /etc/containerd/config.toml
///   settings:
///     SystemdCgroup: true
///     sandbox_image: "registry.k8s.io/pause:3.9"
/// ```
pub struct ContainerdConfigurator;

impl ContainerdConfigurator {
    pub(super) const DEFAULT_CONFIG_PATH: &'static str = "/etc/containerd/config.toml";

    pub(super) fn config_path(desired: &serde_yaml::Value) -> PathBuf {
        desired
            .get("configPath")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(Self::DEFAULT_CONFIG_PATH))
    }

    pub(super) fn read_current_config(path: &Path) -> Result<toml::Table> {
        if !path.exists() {
            return Ok(toml::Table::new());
        }
        let content = fs::read_to_string(path)?;
        content.parse::<toml::Table>().map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "failed to parse containerd config {}: {}",
                    path.display(),
                    e
                ),
            ))
        })
    }

    fn restart_containerd() -> Result<()> {
        let output = Command::new("systemctl")
            .args(["restart", "containerd"])
            .output()
            .map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "systemctl restart containerd failed: {}",
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
        Ok(())
    }
}

impl SystemConfigurator for ContainerdConfigurator {
    fn name(&self) -> &str {
        "containerd"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("containerd")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let config_path = Self::config_path(desired);
        let current = Self::read_current_config(&config_path)?;

        let settings = match desired.get("settings").and_then(|v| v.as_mapping()) {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        Ok(diff_yaml_mapping(
            settings,
            "containerd",
            yaml_value_to_string,
            |key_str| find_toml_value(&current, key_str).unwrap_or_else(|| "<not set>".to_string()),
        ))
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let config_path = Self::config_path(desired);

        let settings = match desired.get("settings").and_then(|v| v.as_mapping()) {
            Some(m) => m,
            None => return Ok(()),
        };

        if settings.is_empty() {
            return Ok(());
        }

        let mut current = Self::read_current_config(&config_path)?;

        for (key, desired_val) in settings {
            let key_str = match key.as_str() {
                Some(k) => k,
                None => continue,
            };
            let desired_str = yaml_value_to_string(desired_val);

            printer.status_simple(
                Role::Info,
                format!("containerd: setting {} = {}", key_str, desired_str),
            );
            set_toml_value(&mut current, key_str, desired_val);
        }

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(&current).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize containerd config: {}",
                e
            )))
        })?;

        // Validate serialized TOML can be re-parsed before writing
        if let Err(e) = content.parse::<toml::Value>() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "containerd config validation failed — aborting write: {}",
                e
            ))));
        }

        // Backup existing config before overwriting
        let backup = cfgd_core::capture_file_state(&config_path).map_err(CfgdError::Io)?;

        cfgd_core::atomic_write_str(&config_path, &content)?;

        printer.status_simple(Role::Info, "Restarting containerd");
        if let Err(e) = Self::restart_containerd() {
            // Restart failed — attempt rollback
            if let Some(ref state) = backup
                && !state.is_symlink
                && !state.oversized
            {
                printer.status_simple(
                    Role::Warn,
                    "containerd restart failed — restoring previous config",
                );
                if let Err(re) = cfgd_core::atomic_write(&config_path, &state.content) {
                    printer.status_simple(
                        Role::Warn,
                        format!("rollback: failed to restore config: {}", re),
                    );
                } else if let Err(re) = Self::restart_containerd() {
                    printer.status_simple(
                        Role::Warn,
                        format!("rollback: containerd restart also failed: {}", re),
                    );
                }
            }
            return Err(e);
        }

        Ok(())
    }
}

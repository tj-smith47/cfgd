use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::super::{diff_yaml_mapping, yaml_value_to_string};

// ---------------------------------------------------------------------------
// KubeletConfigurator
// ---------------------------------------------------------------------------

/// Manages kubelet configuration.
///
/// Config format:
/// ```yaml
/// kubelet:
///   configPath: /var/lib/kubelet/config.yaml
///   settings:
///     maxPods: 110
///     cgroupDriver: systemd
/// ```
pub struct KubeletConfigurator;

impl KubeletConfigurator {
    pub(super) const DEFAULT_CONFIG_PATH: &'static str = "/var/lib/kubelet/config.yaml";

    pub(super) fn config_path(desired: &serde_yaml::Value) -> PathBuf {
        desired
            .get("configPath")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(Self::DEFAULT_CONFIG_PATH))
    }

    pub(super) fn read_current_config(path: &Path) -> Result<serde_yaml::Value> {
        if !path.exists() {
            return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
        let content = fs::read_to_string(path)?;
        serde_yaml::from_str(&content).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to parse kubelet config {}: {}", path.display(), e),
            ))
        })
    }

    fn restart_kubelet() -> Result<()> {
        let output = Command::new("systemctl")
            .args(["restart", "kubelet"])
            .output()
            .map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "systemctl restart kubelet failed: {}",
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
        Ok(())
    }
}

impl SystemConfigurator for KubeletConfigurator {
    fn name(&self) -> &str {
        "kubelet"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("kubelet")
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
            "kubelet",
            yaml_value_to_string,
            |key_str| {
                current
                    .get(key_str)
                    .map(yaml_value_to_string)
                    .unwrap_or_else(|| "<not set>".to_string())
            },
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
        if !current.is_mapping() {
            current = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }
        let Some(current_map) = current.as_mapping_mut() else {
            // Unreachable: we set current to Mapping above. If this somehow
            // triggers, it's an internal logic error — surface it as an error.
            return Err(CfgdError::Io(std::io::Error::other(
                "kubelet config: value is not a mapping after explicit set",
            )));
        };

        for (key, desired_val) in settings {
            let key_str = match key.as_str() {
                Some(k) => k,
                None => continue,
            };
            printer.status_simple(
                Role::Info,
                format!(
                    "kubelet: setting {} = {}",
                    key_str,
                    yaml_value_to_string(desired_val)
                ),
            );
            current_map.insert(
                serde_yaml::Value::String(key_str.to_string()),
                desired_val.clone(),
            );
        }

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_yaml::to_string(&current).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize kubelet config: {}",
                e
            )))
        })?;

        // Validate serialized YAML can be re-parsed before writing
        if let Err(e) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "kubelet config validation failed — aborting write: {}",
                e
            ))));
        }

        // Backup existing config before overwriting
        let backup = cfgd_core::capture_file_state(&config_path).map_err(CfgdError::Io)?;

        cfgd_core::atomic_write_str(&config_path, &content)?;

        printer.status_simple(Role::Info, "Restarting kubelet");
        if let Err(e) = Self::restart_kubelet() {
            if let Some(ref state) = backup
                && !state.is_symlink
                && !state.oversized
            {
                printer.status_simple(
                    Role::Warn,
                    "kubelet restart failed — restoring previous config",
                );
                if let Err(re) = cfgd_core::atomic_write(&config_path, &state.content) {
                    printer.status_simple(
                        Role::Warn,
                        format!("rollback: failed to restore config: {}", re),
                    );
                } else if let Err(re) = Self::restart_kubelet() {
                    printer.status_simple(
                        Role::Warn,
                        format!("rollback: kubelet restart also failed: {}", re),
                    );
                }
            }
            return Err(e);
        }

        Ok(())
    }
}

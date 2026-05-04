use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::super::{diff_yaml_mapping, yaml_value_with_numeric_bools};

// ---------------------------------------------------------------------------
// SysctlConfigurator
// ---------------------------------------------------------------------------

/// Manages kernel parameters via sysctl.
///
/// Config format:
/// ```yaml
/// sysctl:
///   net.ipv4.ip_forward: "1"
///   vm.max_map_count: "262144"
///   net.bridge.bridge-nf-call-iptables: "1"
/// ```
pub struct SysctlConfigurator;

impl SysctlConfigurator {
    /// Validate that a sysctl key contains only safe characters: [a-z0-9._-]
    pub(super) fn validate_sysctl_key(key: &str) -> Result<()> {
        if key.is_empty()
            || !key.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
            })
        {
            return Err(CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                message: format!(
                    "invalid sysctl key '{}': must contain only [a-z0-9._-]",
                    key
                ),
            }));
        }
        Ok(())
    }

    fn read_sysctl(key: &str) -> Result<String> {
        Self::validate_sysctl_key(key)?;
        let path = PathBuf::from("/proc/sys").join(key.replace('.', "/"));
        match fs::read_to_string(&path) {
            Ok(val) => Ok(val.trim().to_string()),
            Err(e) => Err(CfgdError::Io(e)),
        }
    }

    fn write_sysctl(key: &str, value: &str) -> Result<()> {
        Self::validate_sysctl_key(key)?;
        let output = Command::new("sysctl")
            .arg("-w")
            .arg(format!("{}={}", key, value))
            .output()
            .map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "sysctl -w {}={} failed: {}",
                key,
                value,
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
        Ok(())
    }

    fn persist_all_sysctls(entries: &BTreeMap<&str, String>) -> Result<()> {
        Self::persist_all_sysctls_to(Path::new("/etc/sysctl.d"), entries)
    }

    pub(super) fn persist_all_sysctls_to(
        conf_dir: &Path,
        entries: &BTreeMap<&str, String>,
    ) -> Result<()> {
        if !conf_dir.exists() {
            fs::create_dir_all(conf_dir)?;
        }
        let conf_path = conf_dir.join("99-cfgd.conf");

        if entries.is_empty() {
            let _ = fs::remove_file(&conf_path);
            return Ok(());
        }

        let mut content = String::from("# Managed by cfgd — do not edit manually\n");
        for (k, v) in entries {
            content.push_str(&format!("{} = {}\n", k, v));
        }

        cfgd_core::atomic_write_str(&conf_path, &content)?;
        Ok(())
    }
}

impl SystemConfigurator for SysctlConfigurator {
    fn name(&self) -> &str {
        "sysctl"
    }

    fn is_available(&self) -> bool {
        Path::new("/proc/sys").exists()
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        Ok(diff_yaml_mapping(
            mapping,
            "",
            yaml_value_with_numeric_bools,
            |key_str| Self::read_sysctl(key_str).unwrap_or_else(|_| "<unreadable>".to_string()),
        ))
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        let mut all_entries = BTreeMap::new();

        for (key, value) in mapping {
            let key_str = match key.as_str() {
                Some(k) => k,
                None => continue,
            };
            let desired_val = yaml_value_with_numeric_bools(value);

            printer.info(&format!("sysctl -w {}={}", key_str, desired_val));

            Self::write_sysctl(key_str, &desired_val)?;
            all_entries.insert(key_str, desired_val);
        }

        if let Err(e) = Self::persist_all_sysctls(&all_entries) {
            printer.warning(&format!(
                "Failed to persist sysctls: {} (runtime values applied)",
                e
            ));
        }

        Ok(())
    }
}

#[cfg(unix)]
mod node;

#[cfg(unix)]
pub use node::*;

pub mod gpg_keys;
pub use gpg_keys::GpgKeysConfigurator;
pub mod ssh_keys;
pub use ssh_keys::SshKeysConfigurator;
pub mod git_config;
pub use git_config::GitConfigurator;

mod environment;
mod gsettings;
mod kde_config;
mod launch_agent;
mod macos_defaults;
mod shell;
mod systemd_unit;
mod windows_registry;
mod windows_service;
mod xfconf;

pub use environment::EnvironmentConfigurator;
pub use gsettings::GsettingsConfigurator;
pub use kde_config::KdeConfigConfigurator;
pub use launch_agent::LaunchAgentConfigurator;
pub use macos_defaults::MacosDefaultsConfigurator;
pub use shell::ShellConfigurator;
pub use systemd_unit::SystemdUnitConfigurator;
pub use windows_registry::WindowsRegistryConfigurator;
pub use windows_service::WindowsServiceConfigurator;
pub use xfconf::XfconfConfigurator;

use std::process::Command;

use cfgd_core::errors::Result;

use cfgd_core::providers::SystemDrift;

// ---------------------------------------------------------------------------
// Shared helpers used across submodules and node.rs configurators
// ---------------------------------------------------------------------------

/// Run a command and return its trimmed stdout, or empty string on failure.
pub(super) fn read_command_output(cmd: &mut Command) -> String {
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| cfgd_core::stdout_lossy_trimmed(&o))
        .unwrap_or_default()
}

/// Diff a YAML mapping against actual values.
///
/// Iterates every key in `desired` (which must be a YAML mapping), converts each
/// value to its string representation via `value_to_string_fn`, looks up
/// the actual value via `get_actual(key_str)`, and pushes a `SystemDrift` when
/// they differ.  The `key_prefix` is prepended (with a dot separator) to each
/// drift key when non-empty.
///
/// Returns an empty vec if `desired` is not a mapping.
pub(crate) fn diff_yaml_mapping(
    desired: &serde_yaml::Mapping,
    key_prefix: &str,
    value_to_string_fn: fn(&serde_yaml::Value) -> String,
    get_actual: impl Fn(&str) -> String,
) -> Vec<SystemDrift> {
    let mut drifts = Vec::new();

    for (key, desired_val) in desired {
        let key_str = match key.as_str() {
            Some(k) => k,
            None => continue,
        };
        let desired_str = value_to_string_fn(desired_val);
        let actual_str = get_actual(key_str);

        if actual_str != desired_str {
            let drift_key = if key_prefix.is_empty() {
                key_str.to_string()
            } else {
                format!("{}.{}", key_prefix, key_str)
            };
            drifts.push(SystemDrift {
                key: drift_key,
                expected: desired_str,
                actual: actual_str,
            });
        }
    }

    drifts
}

/// Diff a two-level nested YAML mapping (e.g. gsettings schema→key, xfconf channel→property).
///
/// Iterates the outer mapping to get a prefix string, then delegates each inner
/// mapping to `diff_yaml_mapping` with a reader closure that receives `(prefix, key)`.
pub(crate) fn diff_nested_mapping(
    desired: &serde_yaml::Value,
    get_actual: impl Fn(&str, &str) -> String,
) -> Result<Vec<SystemDrift>> {
    let mapping = match desired.as_mapping() {
        Some(m) => m,
        None => return Ok(Vec::new()),
    };

    let mut drifts = Vec::new();
    for (outer_key, inner_values) in mapping {
        let prefix = match outer_key.as_str() {
            Some(s) => s,
            None => continue,
        };
        let values = match inner_values.as_mapping() {
            Some(m) => m,
            None => continue,
        };
        drifts.extend(diff_yaml_mapping(
            values,
            prefix,
            yaml_value_to_string,
            |key| get_actual(prefix, key),
        ));
    }

    Ok(drifts)
}

/// Parse a single line of `reg query` output into `(name, reg_type, value)`.
///
/// Lines have the format `"    NAME    REG_TYPE    VALUE"` with 4-space separators.
/// Returns `None` for empty lines and registry key header lines (those starting with `HKEY_`).
pub(crate) fn parse_reg_line(line: &str) -> Option<(&str, &str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("HKEY_") {
        return None;
    }
    let parts: Vec<&str> = line.splitn(3, "    ").collect();
    if parts.len() == 3 {
        let name = parts[0].trim();
        let reg_type = parts[1].trim();
        let value = parts[2].trim();
        if !name.is_empty() {
            return Some((name, reg_type, value));
        }
    }
    None
}

/// Convert a YAML value to its string representation.
///
/// Booleans use native `true`/`false`.  For contexts that need `"1"`/`"0"`
/// (macOS defaults, sysctl, Windows registry), use `yaml_value_with_numeric_bools`.
pub(crate) fn yaml_value_to_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        _ => format!("{:?}", value),
    }
}

/// Convert a YAML value to its string representation with bools as `"1"`/`"0"`.
///
/// Used by macOS defaults, sysctl, and Windows registry contexts.
pub(crate) fn yaml_value_with_numeric_bools(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Bool(b) => {
            if *b {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::String(s) => s.clone(),
        _ => format!("{:?}", value),
    }
}

#[cfg(test)]
mod tests;

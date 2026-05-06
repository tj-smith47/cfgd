use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use std::collections::BTreeMap;
use std::path::Path;

use super::parse_reg_line;

/// EnvironmentConfigurator — manages system-level environment variables declaratively.
///
/// This handles `spec.system.environment` (privileged, system-wide).
/// For user-level env vars, see `spec.env` which generates `~/.cfgd.env`.
///
/// **Linux**: Writes to `/etc/environment` (PAM) and `/etc/profile.d/cfgd-env.sh` (login shells).
/// **macOS**: Writes to `~/Library/LaunchAgents/com.cfgd.environment.plist` (GUI apps via
///           launchd `EnvironmentVariables`) and `~/.config/cfgd/env.sh` (sourced by shells).
///           Also calls `launchctl setenv` for the current session.
///
/// Config format:
/// ```yaml
/// system:
///   environment:
///     HTTP_PROXY: "http://proxy.corp.com:8080"
///     HTTPS_PROXY: "http://proxy.corp.com:8080"
///     NO_PROXY: "localhost,127.0.0.1,.corp.com"
///     LANG: "en_US.UTF-8"
///     JAVA_HOME: "/usr/lib/jvm/java-17"
/// ```
pub struct EnvironmentConfigurator;

const CFGD_BLOCK_BEGIN: &str = "# BEGIN cfgd managed block";
const CFGD_BLOCK_END: &str = "# END cfgd managed block";

// Linux paths
const LINUX_ETC_ENVIRONMENT: &str = "/etc/environment";
const LINUX_PROFILE_D: &str = "/etc/profile.d/cfgd-env.sh";

// macOS paths
const MACOS_LAUNCHD_PLIST_NAME: &str = "com.cfgd.environment.plist";

impl EnvironmentConfigurator {
    /// Extract desired env vars from the YAML config value.
    fn desired_vars(desired: &serde_yaml::Value) -> BTreeMap<String, String> {
        let mut vars = BTreeMap::new();
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return vars,
        };

        for (key, value) in mapping {
            let key_str = match key.as_str() {
                Some(k) => k,
                None => continue,
            };
            let val_str = match value {
                serde_yaml::Value::String(s) => s.clone(),
                serde_yaml::Value::Number(n) => n.to_string(),
                serde_yaml::Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            vars.insert(key_str.to_string(), val_str);
        }

        vars
    }

    /// Parse a KEY=VALUE or KEY="VALUE" file (e.g., /etc/environment).
    fn parse_env_file(path: &str) -> BTreeMap<String, String> {
        let mut vars = BTreeMap::new();
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return vars,
        };

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                vars.insert(key.to_string(), value.to_string());
            }
        }

        vars
    }

    /// Parse a shell script with `export KEY="VALUE"` lines.
    fn parse_export_file(path: &str) -> BTreeMap<String, String> {
        let mut vars = BTreeMap::new();
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return vars,
        };

        for line in content.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("export ")
                && let Some((key, value)) = rest.split_once('=')
            {
                let key = key.trim();
                let value = value.trim().trim_matches('"').trim_matches('\'');
                vars.insert(key.to_string(), value.to_string());
            }
        }

        vars
    }

    // --- Linux ---

    fn linux_current_vars() -> BTreeMap<String, String> {
        let mut vars = Self::parse_env_file(LINUX_ETC_ENVIRONMENT);
        vars.extend(Self::parse_export_file(LINUX_PROFILE_D));
        vars
    }

    fn linux_write_etc_environment(managed: &BTreeMap<String, String>) -> Result<()> {
        Self::write_etc_environment_to(std::path::Path::new(LINUX_ETC_ENVIRONMENT), managed)
    }

    fn write_etc_environment_to(
        path: &std::path::Path,
        managed: &BTreeMap<String, String>,
    ) -> Result<()> {
        let existing_content = std::fs::read_to_string(path).unwrap_or_default();
        let mut preserved_lines = Vec::new();
        let mut in_cfgd_block = false;

        for line in existing_content.lines() {
            if line.contains(CFGD_BLOCK_BEGIN) {
                in_cfgd_block = true;
                continue;
            }
            if in_cfgd_block {
                if line.contains(CFGD_BLOCK_END) {
                    in_cfgd_block = false;
                    continue;
                }
                continue;
            }
            // Also filter out individual managed keys that might exist outside the block
            let is_managed_key = line
                .split_once('=')
                .map(|(k, _)| managed.contains_key(k.trim()))
                .unwrap_or(false);
            if !is_managed_key {
                preserved_lines.push(line.to_string());
            }
        }

        let mut output = String::new();
        for line in &preserved_lines {
            output.push_str(line);
            output.push('\n');
        }

        if !managed.is_empty() {
            output.push_str(CFGD_BLOCK_BEGIN);
            output.push('\n');
            for (key, value) in managed {
                if value.contains(' ')
                    || value.contains('#')
                    || value.contains('$')
                    || value.contains('"')
                {
                    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
                    output.push_str(&format!("{}=\"{}\"\n", key, escaped));
                } else {
                    output.push_str(&format!("{}={}\n", key, value));
                }
            }
            output.push_str(CFGD_BLOCK_END);
            output.push('\n');
        }

        cfgd_core::atomic_write_str(path, &output).map_err(cfgd_core::errors::CfgdError::Io)?;
        Ok(())
    }

    fn linux_write_profile_d(managed: &BTreeMap<String, String>) -> Result<()> {
        Self::write_profile_d_to(std::path::Path::new(LINUX_PROFILE_D), managed)
    }

    fn write_profile_d_to(
        path: &std::path::Path,
        managed: &BTreeMap<String, String>,
    ) -> Result<()> {
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(cfgd_core::errors::CfgdError::Io)?;
        }

        if managed.is_empty() {
            let _ = std::fs::remove_file(path);
            return Ok(());
        }

        let mut content = String::from("#!/bin/sh\n# Managed by cfgd — do not edit manually\n\n");
        for (key, value) in managed {
            content.push_str(&format!(
                "export {}={}\n",
                key,
                cfgd_core::shell_escape_value(value)
            ));
        }

        cfgd_core::atomic_write_str(path, &content).map_err(cfgd_core::errors::CfgdError::Io)?;
        Ok(())
    }

    // --- macOS ---

    fn macos_env_sh_path() -> std::path::PathBuf {
        cfgd_core::default_config_dir().join("env.sh")
    }

    fn macos_plist_path() -> std::path::PathBuf {
        let home = cfgd_core::expand_tilde(Path::new("~"));
        home.join("Library/LaunchAgents")
            .join(MACOS_LAUNCHD_PLIST_NAME)
    }

    fn macos_current_vars() -> BTreeMap<String, String> {
        let env_sh = Self::macos_env_sh_path();
        Self::parse_export_file(&env_sh.to_string_lossy())
    }

    /// Write `~/.config/cfgd/env.sh` — users source this from their shell rc.
    fn macos_write_env_sh(managed: &BTreeMap<String, String>) -> Result<()> {
        let env_sh = Self::macos_env_sh_path();
        if let Some(parent) = env_sh.parent() {
            std::fs::create_dir_all(parent).map_err(cfgd_core::errors::CfgdError::Io)?;
        }

        if managed.is_empty() {
            let _ = std::fs::remove_file(&env_sh);
            return Ok(());
        }

        let mut content = String::from(
            "#!/bin/sh\n# Managed by cfgd — do not edit manually\n# Source this from your shell rc: . ~/.config/cfgd/env.sh\n\n",
        );
        for (key, value) in managed {
            content.push_str(&format!(
                "export {}={}\n",
                key,
                cfgd_core::shell_escape_value(value)
            ));
        }

        cfgd_core::atomic_write_str(&env_sh, &content).map_err(cfgd_core::errors::CfgdError::Io)?;
        Ok(())
    }

    /// Write a launchd plist that sets EnvironmentVariables for GUI apps.
    fn macos_write_launchd_plist(managed: &BTreeMap<String, String>) -> Result<()> {
        let plist_path = Self::macos_plist_path();
        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent).map_err(cfgd_core::errors::CfgdError::Io)?;
        }

        if managed.is_empty() {
            // Unload and remove
            if let Err(e) = Command::new("launchctl")
                .args(["unload", &plist_path.to_string_lossy()])
                .output()
            {
                tracing::debug!("launchctl unload (cleanup): {e}");
            }
            let _ = std::fs::remove_file(&plist_path);
            return Ok(());
        }

        let mut env_entries = String::new();
        for (key, value) in managed {
            env_entries.push_str(&format!(
                "            <key>{}</key>\n            <string>{}</string>\n",
                cfgd_core::xml_escape(key),
                cfgd_core::xml_escape(value)
            ));
        }

        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.cfgd.environment</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/true</string>
    </array>
    <key>RunAtLoad</key>
    <true />
    <key>EnvironmentVariables</key>
    <dict>
{}    </dict>
</dict>
</plist>
"#,
            env_entries
        );

        cfgd_core::atomic_write_str(&plist_path, &plist)
            .map_err(cfgd_core::errors::CfgdError::Io)?;
        Ok(())
    }

    /// Set env vars in the current launchd session immediately.
    fn macos_launchctl_setenv(managed: &BTreeMap<String, String>, printer: &Printer) {
        for (key, value) in managed {
            let result = Command::new("launchctl")
                .args(["setenv", key, value])
                .output();
            match result {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    printer.warning(&format!(
                        "launchctl setenv {} failed: {}",
                        key,
                        cfgd_core::stderr_lossy_trimmed(&output)
                    ));
                }
                Err(e) => {
                    printer.warning(&format!("launchctl setenv {} failed: {}", key, e));
                }
            }
        }
    }

    // --- Windows ---

    /// Parse the output of `reg query HKCU\Environment` into a map of variable
    /// names to values. Handles both `REG_SZ` and `REG_EXPAND_SZ` value types.
    fn parse_reg_query_output(stdout: &str) -> BTreeMap<String, String> {
        stdout
            .lines()
            .filter_map(|line| {
                let (name, _reg_type, value) = parse_reg_line(line)?;
                Some((name.to_string(), value.to_string()))
            })
            .collect()
    }

    /// Read current user environment variables from the Windows registry.
    /// On non-Windows platforms, returns empty map.
    fn windows_current_vars() -> BTreeMap<String, String> {
        if !cfg!(windows) {
            return BTreeMap::new();
        }
        let output = match Command::new("reg")
            .args(["query", r"HKCU\Environment"])
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return BTreeMap::new(),
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        Self::parse_reg_query_output(&stdout)
    }

    /// Set a user environment variable via `setx` (persists to registry).
    fn windows_set_var(name: &str, value: &str, printer: &Printer) {
        let result = Command::new("setx").args([name, value]).output();
        match result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                // setx writes error messages to stdout, not stderr
                let stdout = cfgd_core::stdout_lossy_trimmed(&output);
                printer.warning(&format!("setx {} failed: {}", name, stdout));
            }
            Err(e) => {
                printer.warning(&format!("setx {} failed: {}", name, e));
            }
        }
    }
}

impl SystemConfigurator for EnvironmentConfigurator {
    fn name(&self) -> &str {
        "environment"
    }

    fn is_available(&self) -> bool {
        cfg!(unix) || cfg!(windows)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        let vars = if cfg!(windows) {
            Self::windows_current_vars()
        } else if cfg!(target_os = "macos") {
            Self::macos_current_vars()
        } else {
            Self::linux_current_vars()
        };
        let mut mapping = serde_yaml::Mapping::new();
        for (key, value) in vars {
            mapping.insert(
                serde_yaml::Value::String(key),
                serde_yaml::Value::String(value),
            );
        }
        Ok(serde_yaml::Value::Mapping(mapping))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let desired_vars = Self::desired_vars(desired);
        let current_vars = if cfg!(windows) {
            Self::windows_current_vars()
        } else if cfg!(target_os = "macos") {
            Self::macos_current_vars()
        } else {
            Self::linux_current_vars()
        };
        let mut drifts = Vec::new();

        for (key, desired_value) in &desired_vars {
            match current_vars.get(key) {
                Some(current_value) if current_value == desired_value => {}
                Some(current_value) => {
                    drifts.push(SystemDrift {
                        key: key.clone(),
                        expected: desired_value.clone(),
                        actual: current_value.clone(),
                    });
                }
                None => {
                    drifts.push(SystemDrift {
                        key: key.clone(),
                        expected: desired_value.clone(),
                        actual: String::new(),
                    });
                }
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let managed = Self::desired_vars(desired);
        if managed.is_empty() {
            return Ok(());
        }

        printer.info(&format!(
            "Managing {} environment variable(s)",
            managed.len()
        ));

        if cfg!(windows) {
            for (key, value) in &managed {
                Self::windows_set_var(key, value, printer);
            }
            printer.success(&format!(
                "Set {} user environment variable(s) via setx",
                managed.len()
            ));
        } else if cfg!(target_os = "macos") {
            // macOS: ~/.config/cfgd/env.sh for shells
            match Self::macos_write_env_sh(&managed) {
                Ok(()) => {
                    printer.success(&format!("Updated {}", Self::macos_env_sh_path().display()));
                    printer.info("Add to your shell rc: . ~/.config/cfgd/env.sh");
                }
                Err(e) => {
                    printer.warning(&format!("Failed to write env.sh: {}", e));
                }
            }

            // macOS: launchd plist for GUI apps
            match Self::macos_write_launchd_plist(&managed) {
                Ok(()) => {
                    printer.success(&format!("Updated {}", Self::macos_plist_path().display()));
                }
                Err(e) => {
                    printer.warning(&format!("Failed to write launchd plist: {}", e));
                }
            }

            // macOS: set vars in current session immediately
            Self::macos_launchctl_setenv(&managed, printer);
        } else {
            // Linux: /etc/environment
            match Self::linux_write_etc_environment(&managed) {
                Ok(()) => {
                    printer.success(&format!("Updated {}", LINUX_ETC_ENVIRONMENT));
                }
                Err(e) => {
                    printer.warning(&format!(
                        "Failed to update {} (may need root): {}",
                        LINUX_ETC_ENVIRONMENT, e
                    ));
                }
            }

            // Linux: /etc/profile.d/cfgd-env.sh
            match Self::linux_write_profile_d(&managed) {
                Ok(()) => {
                    printer.success(&format!("Updated {}", LINUX_PROFILE_D));
                }
                Err(e) => {
                    printer.warning(&format!(
                        "Failed to update {} (may need root): {}",
                        LINUX_PROFILE_D, e
                    ));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;

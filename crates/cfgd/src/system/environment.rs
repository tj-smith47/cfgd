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
mod tests {
    use super::*;

    #[test]
    fn environment_desired_vars_parsing() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
HTTP_PROXY: "http://proxy.example.com:8080"
LANG: "en_US.UTF-8"
MAX_CONNECTIONS: 100
DEBUG: true
"#,
        )
        .unwrap();

        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        assert_eq!(vars.len(), 4);
        assert_eq!(vars["HTTP_PROXY"], "http://proxy.example.com:8080");
        assert_eq!(vars["LANG"], "en_US.UTF-8");
        assert_eq!(vars["MAX_CONNECTIONS"], "100");
        assert_eq!(vars["DEBUG"], "true");
    }

    #[test]
    fn environment_desired_vars_empty_or_wrong_type() {
        for yaml in &[
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
            serde_yaml::Value::String("not a mapping".to_string()),
        ] {
            let vars = EnvironmentConfigurator::desired_vars(yaml);
            assert!(vars.is_empty(), "should be empty for {:?}", yaml);
        }
    }

    #[test]
    fn environment_diff_detects_missing() {
        let ec = EnvironmentConfigurator;
        // Use a unique env var name that definitely doesn't exist
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
CFGD_TEST_NONEXISTENT_VAR_12345: "test_value"
"#,
        )
        .unwrap();

        let drifts = ec.diff(&yaml).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "CFGD_TEST_NONEXISTENT_VAR_12345");
        assert_eq!(drifts[0].expected, "test_value");
        assert!(drifts[0].actual.is_empty());
    }

    #[test]
    fn windows_reg_query_parsing_typical() {
        let output = "\
HKEY_CURRENT_USER\\Environment\n\
\n\
    EDITOR    REG_SZ    code\n\
    GOPATH    REG_SZ    C:\\Users\\user\\go\n\
    Path    REG_EXPAND_SZ    C:\\Users\\user\\.cargo\\bin;%PATH%\n";

        let vars = EnvironmentConfigurator::parse_reg_query_output(output);
        assert_eq!(vars.len(), 3);
        assert_eq!(vars["EDITOR"], "code");
        assert_eq!(vars["GOPATH"], r"C:\Users\user\go");
        assert_eq!(vars["Path"], r"C:\Users\user\.cargo\bin;%PATH%");
    }

    #[test]
    fn windows_reg_query_parsing_empty() {
        let output = "HKEY_CURRENT_USER\\Environment\n\n";
        let vars = EnvironmentConfigurator::parse_reg_query_output(output);
        assert!(vars.is_empty());
    }

    #[test]
    fn windows_reg_query_parsing_blank_input() {
        let vars = EnvironmentConfigurator::parse_reg_query_output("");
        assert!(vars.is_empty());
    }

    #[test]
    fn windows_reg_query_parsing_single_var() {
        let output = "HKEY_CURRENT_USER\\Environment\n\
                       \n\
                           JAVA_HOME    REG_SZ    C:\\Program Files\\Java\\jdk-17\n";
        let vars = EnvironmentConfigurator::parse_reg_query_output(output);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["JAVA_HOME"], r"C:\Program Files\Java\jdk-17");
    }

    #[test]
    fn environment_configurator_available_on_linux() {
        let ec = EnvironmentConfigurator;
        assert!(ec.is_available());
    }

    #[test]
    fn parse_reg_query_output_expand_sz_type() {
        let output = "HKEY_CURRENT_USER\\Environment\n\
                      \n\
                          Path    REG_EXPAND_SZ    %USERPROFILE%\\bin\n";
        let vars = EnvironmentConfigurator::parse_reg_query_output(output);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["Path"], r"%USERPROFILE%\bin");
    }

    #[test]
    fn parse_reg_query_output_mixed_types() {
        let output = "HKEY_CURRENT_USER\\Environment\n\
                      \n\
                          EDITOR    REG_SZ    vim\n\
                          PATH    REG_EXPAND_SZ    C:\\bin;%PATH%\n\
                          COUNT    REG_DWORD    0x5\n";
        let vars = EnvironmentConfigurator::parse_reg_query_output(output);
        assert_eq!(vars.len(), 3);
        assert_eq!(vars["EDITOR"], "vim");
        assert_eq!(vars["PATH"], r"C:\bin;%PATH%");
        // DWORD is parsed as raw value string, not converted
        assert_eq!(vars["COUNT"], "0x5");
    }

    #[test]
    fn environment_desired_vars_skips_complex_values() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
SIMPLE: "value"
NESTED:
  inner: "should be skipped"
LIST:
  - "also skipped"
"#,
        )
        .unwrap();

        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["SIMPLE"], "value");
    }

    #[test]
    fn environment_desired_vars_non_string_keys_skipped() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::String("value".into()),
        );
        mapping.insert(
            serde_yaml::Value::String("VALID".into()),
            serde_yaml::Value::String("ok".into()),
        );
        let yaml = serde_yaml::Value::Mapping(mapping);

        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["VALID"], "ok");
    }

    #[test]
    fn parse_env_file_standard_entries() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env");
        std::fs::write(&env_path, "LANG=en_US.UTF-8\nPATH=/usr/bin\n").unwrap();

        let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
        assert_eq!(vars.len(), 2);
        assert_eq!(vars["LANG"], "en_US.UTF-8");
        assert_eq!(vars["PATH"], "/usr/bin");
    }

    #[test]
    fn parse_env_file_quoted_values() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env");
        std::fs::write(&env_path, "EDITOR=\"vim\"\nSHELL=\"/bin/bash\"\n").unwrap();

        let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
        assert_eq!(vars["EDITOR"], "vim");
        assert_eq!(vars["SHELL"], "/bin/bash");
    }

    #[test]
    fn parse_env_file_skips_comments_and_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env");
        std::fs::write(&env_path, "# comment line\n\n  \nKEY=value\n# another\n").unwrap();

        let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["KEY"], "value");
    }

    #[test]
    fn parse_env_file_nonexistent_returns_empty() {
        let vars = EnvironmentConfigurator::parse_env_file("/nonexistent/path/env");
        assert!(vars.is_empty());
    }

    #[test]
    fn parse_env_file_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env");
        std::fs::write(&env_path, "").unwrap();

        let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
        assert!(vars.is_empty());
    }

    #[test]
    fn parse_env_file_value_with_equals() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env");
        std::fs::write(&env_path, "OPTS=--key=value\n").unwrap();

        let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
        assert_eq!(vars["OPTS"], "--key=value");
    }

    #[test]
    fn parse_export_file_standard_entries() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("env.sh");
        std::fs::write(
            &file_path,
            "#!/bin/sh\nexport FOO=\"bar\"\nexport BAZ='qux'\n",
        )
        .unwrap();

        let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
        assert_eq!(vars.len(), 2);
        assert_eq!(vars["FOO"], "bar");
        assert_eq!(vars["BAZ"], "qux");
    }

    #[test]
    fn parse_export_file_unquoted_values() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("env.sh");
        std::fs::write(&file_path, "export LANG=en_US.UTF-8\n").unwrap();

        let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
        assert_eq!(vars["LANG"], "en_US.UTF-8");
    }

    #[test]
    fn parse_export_file_nonexistent_returns_empty() {
        let vars = EnvironmentConfigurator::parse_export_file("/nonexistent/env.sh");
        assert!(vars.is_empty());
    }

    #[test]
    fn parse_export_file_skips_non_export_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("env.sh");
        std::fs::write(
            &file_path,
            "#!/bin/sh\n# comment\nSOMETHING=not_exported\nexport REAL=yes\n",
        )
        .unwrap();

        let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["REAL"], "yes");
    }

    #[test]
    fn parse_reg_query_output_dword_preserved_as_raw() {
        // parse_reg_query_output uses parse_reg_line which returns raw value
        let output = "HKEY_CURRENT_USER\\Environment\n\
                      \n\
                          Count    REG_DWORD    0x5\n";
        let vars = EnvironmentConfigurator::parse_reg_query_output(output);
        // The raw DWORD hex value is preserved
        assert_eq!(vars["Count"], "0x5");
    }

    #[test]
    fn environment_current_state_returns_mapping() {
        let ec = EnvironmentConfigurator;
        let state = ec.current_state().unwrap();
        assert!(state.is_mapping());
    }

    #[test]
    fn environment_diff_matching_values_no_drift() {
        // We can only test this reliably by setting an env var that matches
        // On Linux, parse_env_file/parse_export_file are used
        let ec = EnvironmentConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = ec.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn environment_diff_non_mapping_desired_returns_empty() {
        let ec = EnvironmentConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        let drifts = ec.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn write_etc_environment_creates_managed_block() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("environment");

        let mut managed = BTreeMap::new();
        managed.insert("EDITOR".to_string(), "vim".to_string());
        managed.insert("LANG".to_string(), "en_US.UTF-8".to_string());

        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(
            content.contains(CFGD_BLOCK_BEGIN),
            "missing block begin marker"
        );
        assert!(content.contains(CFGD_BLOCK_END), "missing block end marker");
        assert!(content.contains("EDITOR=vim\n"));
        assert!(content.contains("LANG=en_US.UTF-8\n"));
    }

    #[test]
    fn write_etc_environment_preserves_existing_non_managed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("environment");
        // Pre-existing content that should be preserved
        std::fs::write(&env_path, "PATH=/usr/bin\nHOME=/root\n").unwrap();

        let mut managed = BTreeMap::new();
        managed.insert("EDITOR".to_string(), "vim".to_string());

        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(content.contains("PATH=/usr/bin"), "existing PATH preserved");
        assert!(content.contains("HOME=/root"), "existing HOME preserved");
        assert!(content.contains("EDITOR=vim"), "managed var added");
    }

    #[test]
    fn write_etc_environment_replaces_existing_managed_block() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("environment");
        // Pre-existing file with an old cfgd block
        std::fs::write(
            &env_path,
            format!(
                "PATH=/usr/bin\n{}\nOLD_VAR=old\n{}\n",
                CFGD_BLOCK_BEGIN, CFGD_BLOCK_END
            ),
        )
        .unwrap();

        let mut managed = BTreeMap::new();
        managed.insert("NEW_VAR".to_string(), "new".to_string());

        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(
            !content.contains("OLD_VAR"),
            "old managed var should be replaced"
        );
        assert!(content.contains("NEW_VAR=new"), "new managed var added");
        assert!(content.contains("PATH=/usr/bin"), "non-managed preserved");
        // Only one block begin/end pair
        assert_eq!(
            content.matches(CFGD_BLOCK_BEGIN).count(),
            1,
            "should have exactly one begin marker"
        );
    }

    #[test]
    fn write_etc_environment_removes_managed_keys_outside_block() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("environment");
        // EDITOR exists outside the block — should be removed when managed
        std::fs::write(&env_path, "EDITOR=nano\nPATH=/usr/bin\n").unwrap();

        let mut managed = BTreeMap::new();
        managed.insert("EDITOR".to_string(), "vim".to_string());

        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(
            !content.contains("EDITOR=nano"),
            "old EDITOR outside block should be removed"
        );
        assert!(content.contains("EDITOR=vim"), "managed EDITOR in block");
        assert!(content.contains("PATH=/usr/bin"), "unmanaged preserved");
    }

    #[test]
    fn write_etc_environment_quotes_values_with_special_chars() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("environment");

        let mut managed = BTreeMap::new();
        managed.insert("OPTS".to_string(), "has spaces".to_string());
        managed.insert("COMMENT".to_string(), "has#hash".to_string());
        managed.insert("EXPAND".to_string(), "$HOME/bin".to_string());
        managed.insert("SIMPLE".to_string(), "noquotes".to_string());

        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(
            content.contains("OPTS=\"has spaces\""),
            "space-containing value should be quoted, got: {}",
            content
        );
        assert!(
            content.contains("COMMENT=\"has#hash\""),
            "hash-containing value should be quoted"
        );
        assert!(
            content.contains("EXPAND=\"$HOME/bin\""),
            "dollar-containing value should be quoted"
        );
        assert!(
            content.contains("SIMPLE=noquotes\n"),
            "simple value should NOT be quoted"
        );
    }

    #[test]
    fn write_etc_environment_empty_managed_removes_block() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("environment");
        std::fs::write(
            &env_path,
            format!(
                "PATH=/usr/bin\n{}\nVAR=val\n{}\n",
                CFGD_BLOCK_BEGIN, CFGD_BLOCK_END
            ),
        )
        .unwrap();

        let managed = BTreeMap::new();
        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(
            !content.contains(CFGD_BLOCK_BEGIN),
            "empty managed should remove block"
        );
        assert!(content.contains("PATH=/usr/bin"), "non-managed preserved");
    }

    #[test]
    fn write_etc_environment_escapes_backslashes_and_quotes_in_values() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("environment");

        let mut managed = BTreeMap::new();
        managed.insert(
            "TRICKY".to_string(),
            r#"has "quotes" and \ slashes"#.to_string(),
        );

        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        // The value has both " and space, so it gets quoted with escaping
        assert!(
            content.contains(r#"TRICKY="has \"quotes\" and \\ slashes""#),
            "backslashes and quotes should be escaped, got: {}",
            content
        );
    }

    #[test]
    fn write_profile_d_creates_shell_exports() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = dir.path().join("cfgd-env.sh");

        let mut managed = BTreeMap::new();
        managed.insert("EDITOR".to_string(), "vim".to_string());
        managed.insert("LANG".to_string(), "en_US.UTF-8".to_string());

        EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();

        let content = std::fs::read_to_string(&profile_path).unwrap();
        assert!(content.starts_with("#!/bin/sh\n"), "missing shebang");
        assert!(
            content.contains("# Managed by cfgd"),
            "missing header comment"
        );
        // shell_escape_value always quotes values
        assert!(
            content.contains("export EDITOR=") && content.contains("vim"),
            "missing EDITOR export, got: {}",
            content
        );
        assert!(
            content.contains("export LANG=") && content.contains("en_US.UTF-8"),
            "missing LANG export, got: {}",
            content
        );
    }

    #[test]
    fn write_profile_d_shell_escapes_values() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = dir.path().join("cfgd-env.sh");

        let mut managed = BTreeMap::new();
        managed.insert("OPTS".to_string(), "has spaces and $vars".to_string());

        EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();

        let content = std::fs::read_to_string(&profile_path).unwrap();
        // shell_escape_value should single-quote values with metacharacters
        assert!(
            content.contains("export OPTS='has spaces and $vars'"),
            "value with metacharacters should be shell-escaped, got: {}",
            content
        );
    }

    #[test]
    fn write_profile_d_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = dir.path().join("deep").join("nested").join("cfgd-env.sh");

        let mut managed = BTreeMap::new();
        managed.insert("KEY".to_string(), "val".to_string());

        EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();
        assert!(profile_path.exists());
    }

    #[test]
    fn write_profile_d_empty_managed_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = dir.path().join("cfgd-env.sh");
        std::fs::write(&profile_path, "old content").unwrap();

        let managed = BTreeMap::new();
        EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();
        assert!(
            !profile_path.exists(),
            "empty managed should remove the file"
        );
    }

    #[test]
    fn parse_reg_query_output_multiple_types_preserved() {
        let output = "\
HKEY_CURRENT_USER\\Environment\n\
\n\
    Editor    REG_SZ    vim\n\
    NumProcs    REG_DWORD    0x4\n\
    ExpandPath    REG_EXPAND_SZ    %HOME%\\bin\n";

        let vars = EnvironmentConfigurator::parse_reg_query_output(output);
        assert_eq!(vars.len(), 3);
        assert_eq!(vars["Editor"], "vim");
        // parse_reg_query_output preserves raw DWORD hex
        assert_eq!(vars["NumProcs"], "0x4");
        assert_eq!(vars["ExpandPath"], "%HOME%\\bin");
    }

    #[test]
    fn parse_env_file_lines_without_equals_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env");
        std::fs::write(&env_path, "VALID=yes\nNO_EQUALS_HERE\nALSO_VALID=true\n").unwrap();

        let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
        assert_eq!(vars.len(), 2);
        assert_eq!(vars["VALID"], "yes");
        assert_eq!(vars["ALSO_VALID"], "true");
    }

    #[test]
    fn parse_env_file_whitespace_around_key_and_value() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env");
        std::fs::write(&env_path, "  KEY  =  value  \n").unwrap();

        let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
        assert_eq!(vars["KEY"], "value");
    }

    #[test]
    fn parse_env_file_empty_value() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env");
        std::fs::write(&env_path, "EMPTY=\n").unwrap();

        let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
        assert_eq!(vars["EMPTY"], "");
    }

    #[test]
    fn parse_env_file_only_comments() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env");
        std::fs::write(&env_path, "# comment 1\n# comment 2\n").unwrap();

        let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
        assert!(vars.is_empty());
    }

    #[test]
    fn parse_export_file_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("env.sh");
        std::fs::write(&file_path, "").unwrap();

        let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
        assert!(vars.is_empty());
    }

    #[test]
    fn parse_export_file_mixed_quote_styles() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("env.sh");
        std::fs::write(
            &file_path,
            "export DOUBLE=\"double_val\"\nexport SINGLE='single_val'\nexport NONE=bare_val\n",
        )
        .unwrap();

        let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
        assert_eq!(vars.len(), 3);
        assert_eq!(vars["DOUBLE"], "double_val");
        assert_eq!(vars["SINGLE"], "single_val");
        assert_eq!(vars["NONE"], "bare_val");
    }

    #[test]
    fn parse_export_file_value_with_equals_sign() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("env.sh");
        std::fs::write(&file_path, "export OPTS=\"--key=value\"\n").unwrap();

        let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
        assert_eq!(vars["OPTS"], "--key=value");
    }

    #[test]
    fn parse_export_file_ignores_comments_and_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("env.sh");
        std::fs::write(
            &file_path,
            "#!/bin/sh\n# a comment\n\nexport REAL=\"yes\"\n\n# tail\n",
        )
        .unwrap();

        let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["REAL"], "yes");
    }

    #[test]
    fn desired_vars_null_values_skipped() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
VALID: "value"
NULL_KEY: null
"#,
        )
        .unwrap();

        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["VALID"], "value");
        assert!(
            !vars.contains_key("NULL_KEY"),
            "null values should be skipped"
        );
    }

    #[test]
    fn desired_vars_bool_converted_to_string() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
FLAG_TRUE: true
FLAG_FALSE: false
"#,
        )
        .unwrap();

        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        assert_eq!(vars["FLAG_TRUE"], "true");
        assert_eq!(vars["FLAG_FALSE"], "false");
    }

    #[test]
    fn desired_vars_number_types() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
INT_VAR: 42
FLOAT_VAR: 3.14
NEGATIVE: -10
"#,
        )
        .unwrap();

        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        assert_eq!(vars["INT_VAR"], "42");
        assert!(vars["FLOAT_VAR"].starts_with("3.14"));
        assert_eq!(vars["NEGATIVE"], "-10");
    }

    #[test]
    fn desired_vars_sequence_and_mapping_skipped() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
GOOD: "ok"
LIST_VAL:
  - "item1"
MAP_VAL:
  nested: "inner"
"#,
        )
        .unwrap();

        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["GOOD"], "ok");
    }

    #[test]
    fn desired_vars_preserves_insertion_order_via_btreemap() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
ZEBRA: "z"
ALPHA: "a"
MIDDLE: "m"
"#,
        )
        .unwrap();

        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        let keys: Vec<&String> = vars.keys().collect();
        // BTreeMap sorts lexicographically
        assert_eq!(keys, vec!["ALPHA", "MIDDLE", "ZEBRA"]);
    }

    #[test]
    fn write_etc_environment_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("new_environment");
        assert!(!env_path.exists());

        let mut managed = BTreeMap::new();
        managed.insert("NEW_VAR".to_string(), "new_value".to_string());

        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(content.contains(CFGD_BLOCK_BEGIN));
        assert!(content.contains("NEW_VAR=new_value\n"));
        assert!(content.contains(CFGD_BLOCK_END));
    }

    #[test]
    fn write_etc_environment_empty_managed_on_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("new_environment");

        let managed = BTreeMap::new();
        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        // No block markers for empty managed set
        assert!(!content.contains(CFGD_BLOCK_BEGIN));
        // File should be empty or just a newline
        assert!(content.trim().is_empty());
    }

    #[test]
    fn environment_apply_empty_desired_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let ec = EnvironmentConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        // Should return Ok(()) without writing anything
        ec.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn environment_apply_non_mapping_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let ec = EnvironmentConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        ec.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn write_etc_environment_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("environment");

        let mut managed = BTreeMap::new();
        managed.insert("FOO".to_string(), "bar".to_string());
        managed.insert("BAZ".to_string(), "qux".to_string());

        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();
        let content1 = std::fs::read_to_string(&env_path).unwrap();

        // Write again with same vars — should produce identical output
        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();
        let content2 = std::fs::read_to_string(&env_path).unwrap();

        assert_eq!(content1, content2, "writing same vars should be idempotent");
    }

    #[test]
    fn write_profile_d_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env.sh");

        let mut managed = BTreeMap::new();
        managed.insert("A".to_string(), "1".to_string());

        EnvironmentConfigurator::write_profile_d_to(&path, &managed).unwrap();
        let content1 = std::fs::read_to_string(&path).unwrap();

        EnvironmentConfigurator::write_profile_d_to(&path, &managed).unwrap();
        let content2 = std::fs::read_to_string(&path).unwrap();

        assert_eq!(content1, content2);
    }

    #[test]
    fn environment_windows_current_vars_empty_on_non_windows() {
        let vars = EnvironmentConfigurator::windows_current_vars();
        assert!(vars.is_empty());
    }

    #[test]
    fn write_etc_environment_escapes_double_quotes_in_value() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("environment");

        let mut managed = BTreeMap::new();
        managed.insert("QUOTED".to_string(), r#"say "hello""#.to_string());

        EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

        let content = std::fs::read_to_string(&env_path).unwrap();
        // The value contains a double-quote, which triggers quoting + escaping
        assert!(
            content.contains(r#"QUOTED="say \"hello\"""#),
            "double quotes in value should be escaped, got: {}",
            content
        );
    }
}

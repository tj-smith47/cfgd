#[cfg(unix)]
mod node;

#[cfg(unix)]
pub use node::*;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

// ---------------------------------------------------------------------------
// Shared helpers used by both mod.rs and node.rs configurators
// ---------------------------------------------------------------------------

/// Extract trimmed stderr from a process output as an owned String.
pub(crate) fn stderr_string(output: &std::process::Output) -> String {
    crate::packages::stderr_lossy(output).trim_end().to_string()
}

/// Diff a YAML mapping against actual values.
///
/// Iterates every key in `desired` (which must be a YAML mapping), converts each
/// value to its string representation via `yaml_value_to_string_fn`, looks up
/// the actual value via `get_actual(key_str)`, and pushes a `SystemDrift` when
/// they differ.  The `key_prefix` is prepended (with a dot separator) to each
/// drift key when non-empty.
///
/// Returns an empty vec if `desired` is not a mapping.
pub(crate) fn diff_yaml_mapping(
    desired: &serde_yaml::Mapping,
    key_prefix: &str,
    yaml_value_to_string_fn: fn(&serde_yaml::Value) -> String,
    get_actual: impl Fn(&str) -> String,
) -> Vec<SystemDrift> {
    let mut drifts = Vec::new();

    for (key, desired_val) in desired {
        let key_str = match key.as_str() {
            Some(k) => k,
            None => continue,
        };
        let desired_str = yaml_value_to_string_fn(desired_val);
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

/// ShellConfigurator — reads/sets default shell via `chsh`.
pub struct ShellConfigurator;

impl SystemConfigurator for ShellConfigurator {
    fn name(&self) -> &str {
        "shell"
    }

    fn is_available(&self) -> bool {
        cfg!(unix)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        let shell = std::env::var("SHELL").unwrap_or_default();
        Ok(serde_yaml::Value::String(shell))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let desired_shell = desired.as_str().unwrap_or_default();
        if desired_shell.is_empty() {
            return Ok(Vec::new());
        }

        let current_shell = std::env::var("SHELL").unwrap_or_default();
        if current_shell == desired_shell {
            return Ok(Vec::new());
        }

        Ok(vec![SystemDrift {
            key: "default-shell".to_string(),
            expected: desired_shell.to_string(),
            actual: current_shell,
        }])
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let desired_shell = desired.as_str().unwrap_or_default();
        if desired_shell.is_empty() {
            return Ok(());
        }

        printer.info(&format!("Setting default shell to {}", desired_shell));

        let output = Command::new("chsh")
            .arg("-s")
            .arg(desired_shell)
            .output()
            .map_err(cfgd_core::errors::CfgdError::Io)?;

        if !output.status.success() {
            printer.warning(&format!(
                "chsh failed (may require password): {}",
                stderr_string(&output)
            ));
        }

        Ok(())
    }
}

/// MacosDefaultsConfigurator — reads/writes macOS `defaults` domains.
pub struct MacosDefaultsConfigurator;

impl SystemConfigurator for MacosDefaultsConfigurator {
    fn name(&self) -> &str {
        "macosDefaults"
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "macos")
            || Command::new("defaults")
                .arg("help")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        let mut drifts = Vec::new();
        for (domain_key, domain_values) in mapping {
            let domain = match domain_key.as_str() {
                Some(d) => d,
                None => continue,
            };
            let values = match domain_values.as_mapping() {
                Some(m) => m,
                None => continue,
            };
            drifts.extend(diff_yaml_mapping(
                values,
                domain,
                yaml_value_to_defaults_string,
                |key_str| read_defaults_value(domain, key_str),
            ));
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        for (domain_key, domain_values) in mapping {
            let domain = match domain_key.as_str() {
                Some(d) => d,
                None => continue,
            };

            let values = match domain_values.as_mapping() {
                Some(m) => m,
                None => continue,
            };

            for (key, desired_value) in values {
                let key_str = match key.as_str() {
                    Some(k) => k,
                    None => continue,
                };

                let (value_type, value_str) = yaml_value_to_defaults_type(desired_value);

                printer.info(&format!(
                    "defaults write {} {} -{} {}",
                    domain, key_str, value_type, value_str
                ));

                let output = Command::new("defaults")
                    .args(["write", domain, key_str])
                    .arg(format!("-{}", value_type))
                    .arg(&value_str)
                    .output()
                    .map_err(cfgd_core::errors::CfgdError::Io)?;

                if !output.status.success() {
                    printer.warning(&format!(
                        "defaults write failed for {}.{}: {}",
                        domain,
                        key_str,
                        stderr_string(&output)
                    ));
                }
            }
        }

        Ok(())
    }
}

fn read_defaults_value(domain: &str, key: &str) -> String {
    Command::new("defaults")
        .args(["read", domain, key])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn yaml_value_to_defaults_string(value: &serde_yaml::Value) -> String {
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

fn yaml_value_to_defaults_type(value: &serde_yaml::Value) -> (&'static str, String) {
    match value {
        serde_yaml::Value::Bool(b) => ("bool", if *b { "true" } else { "false" }.to_string()),
        serde_yaml::Value::Number(n) => {
            if n.is_f64() {
                ("float", n.to_string())
            } else {
                ("int", n.to_string())
            }
        }
        serde_yaml::Value::String(s) => ("string", s.clone()),
        _ => ("string", format!("{:?}", value)),
    }
}

/// SystemdUnitConfigurator — manages systemd unit files and enablement.
pub struct SystemdUnitConfigurator;

impl SystemConfigurator for SystemdUnitConfigurator {
    fn name(&self) -> &str {
        "systemdUnits"
    }

    fn is_available(&self) -> bool {
        Command::new("systemctl")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Sequence(Vec::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mut drifts = Vec::new();

        let units = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(drifts),
        };

        for unit in units {
            let name = match unit.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let desired_enabled = unit
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let is_enabled = Command::new("systemctl")
                .args(["is-enabled", name])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "enabled")
                .unwrap_or(false);

            if is_enabled != desired_enabled {
                drifts.push(SystemDrift {
                    key: format!("{}.enabled", name),
                    expected: desired_enabled.to_string(),
                    actual: is_enabled.to_string(),
                });
            }

            // Check if unit file exists at specified path
            if let Some(unit_file) = unit.get("unitFile").and_then(|v| v.as_str()) {
                let unit_path = std::path::Path::new(unit_file);
                if !unit_path.exists() {
                    drifts.push(SystemDrift {
                        key: format!("{}.unit-file", name),
                        expected: unit_file.to_string(),
                        actual: "missing".to_string(),
                    });
                }
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let units = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(()),
        };

        for unit in units {
            let name = match unit.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let desired_enabled = unit
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            // Copy unit file if specified
            if let Some(unit_file) = unit.get("unitFile").and_then(|v| v.as_str()) {
                let dest = format!("/etc/systemd/system/{}", name);
                printer.info(&format!("Installing unit file: {} → {}", unit_file, dest));

                match std::fs::read(unit_file) {
                    Ok(content) => {
                        if let Err(e) =
                            cfgd_core::atomic_write(std::path::Path::new(&dest), &content)
                        {
                            printer.warning(&format!("Failed to install unit file: {}", e));
                        }
                    }
                    Err(e) => {
                        printer.warning(&format!("Failed to read unit file: {}", e));
                    }
                }

                // Reload systemd
                let _ = Command::new("systemctl").arg("daemon-reload").output();
            }

            // Enable/disable
            let action = if desired_enabled { "enable" } else { "disable" };
            printer.info(&format!("systemctl {} {}", action, name));

            let output = Command::new("systemctl")
                .args([action, name])
                .output()
                .map_err(cfgd_core::errors::CfgdError::Io)?;

            if !output.status.success() {
                printer.warning(&format!(
                    "systemctl {} {} failed: {}",
                    action,
                    name,
                    stderr_string(&output)
                ));
            }
        }

        Ok(())
    }
}

/// LaunchAgentConfigurator — manages macOS LaunchAgent plists.
pub struct LaunchAgentConfigurator;

impl SystemConfigurator for LaunchAgentConfigurator {
    fn name(&self) -> &str {
        "launchAgents"
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "macos")
            || Command::new("launchctl")
                .arg("version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Sequence(Vec::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mut drifts = Vec::new();

        let agents = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(drifts),
        };

        for agent in agents {
            let name = match agent.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let plist_path = launch_agent_plist_path(name);
            if !plist_path.exists() {
                drifts.push(SystemDrift {
                    key: format!("{}.plist", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let agents = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(()),
        };

        for agent in agents {
            let name = match agent.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let program = agent.get("program").and_then(|v| v.as_str()).unwrap_or("");
            let run_at_load = agent
                .get("runAtLoad")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let args: Vec<&str> = agent
                .get("args")
                .and_then(|v| v.as_sequence())
                .map(|seq| seq.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>())
                .unwrap_or_default();

            let plist_content = generate_launch_agent_plist(name, program, &args, run_at_load);
            let plist_path = launch_agent_plist_path(name);

            printer.info(&format!("Writing launch agent: {}", plist_path.display()));

            cfgd_core::atomic_write_str(&plist_path, &plist_content)?;

            // Load the agent
            let _ = Command::new("launchctl")
                .args(["unload", &plist_path.display().to_string()])
                .output();

            let output = Command::new("launchctl")
                .args(["load", &plist_path.display().to_string()])
                .output()
                .map_err(cfgd_core::errors::CfgdError::Io)?;

            if !output.status.success() {
                printer.warning(&format!(
                    "launchctl load failed for {}: {}",
                    name,
                    stderr_string(&output)
                ));
            }
        }

        Ok(())
    }
}

fn launch_agent_plist_path(name: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", name))
}

fn generate_launch_agent_plist(
    label: &str,
    program: &str,
    args: &[&str],
    run_at_load: bool,
) -> String {
    let mut program_args = String::new();
    if !program.is_empty() || !args.is_empty() {
        program_args.push_str("    <key>ProgramArguments</key>\n    <array>\n");
        if !program.is_empty() {
            program_args.push_str(&format!(
                "        <string>{}</string>\n",
                cfgd_core::xml_escape(program)
            ));
        }
        for arg in args {
            program_args.push_str(&format!(
                "        <string>{}</string>\n",
                cfgd_core::xml_escape(arg)
            ));
        }
        program_args.push_str("    </array>\n");
    }

    let run_at_load_str = if run_at_load { "true" } else { "false" };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
{}    <key>RunAtLoad</key>
    <{} />
</dict>
</plist>
"#,
        cfgd_core::xml_escape(label),
        program_args,
        run_at_load_str
    )
}

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
                let value = value.trim().trim_matches('"');
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
        let existing_content = std::fs::read_to_string(LINUX_ETC_ENVIRONMENT).unwrap_or_default();
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
                if value.contains(' ') || value.contains('#') || value.contains('$') {
                    output.push_str(&format!("{}=\"{}\"\n", key, value));
                } else {
                    output.push_str(&format!("{}={}\n", key, value));
                }
            }
            output.push_str(CFGD_BLOCK_END);
            output.push('\n');
        }

        cfgd_core::atomic_write_str(std::path::Path::new(LINUX_ETC_ENVIRONMENT), &output)
            .map_err(cfgd_core::errors::CfgdError::Io)?;
        Ok(())
    }

    fn linux_write_profile_d(managed: &BTreeMap<String, String>) -> Result<()> {
        let profile_d = Path::new("/etc/profile.d");
        if !profile_d.exists() {
            std::fs::create_dir_all(profile_d).map_err(cfgd_core::errors::CfgdError::Io)?;
        }

        if managed.is_empty() {
            let _ = std::fs::remove_file(LINUX_PROFILE_D);
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

        cfgd_core::atomic_write_str(std::path::Path::new(LINUX_PROFILE_D), &content)
            .map_err(cfgd_core::errors::CfgdError::Io)?;
        Ok(())
    }

    // --- macOS ---

    fn macos_env_sh_path() -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        std::path::PathBuf::from(home).join(".config/cfgd/env.sh")
    }

    fn macos_plist_path() -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        std::path::PathBuf::from(home)
            .join("Library/LaunchAgents")
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
            let _ = Command::new("launchctl")
                .args(["unload", &plist_path.to_string_lossy()])
                .output();
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
                        stderr_string(&output)
                    ));
                }
                Err(e) => {
                    printer.warning(&format!("launchctl setenv {} failed: {}", key, e));
                }
            }
        }
    }
}

impl SystemConfigurator for EnvironmentConfigurator {
    fn name(&self) -> &str {
        "environment"
    }

    fn is_available(&self) -> bool {
        cfg!(unix)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        let vars = if cfg!(target_os = "macos") {
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
        let current_vars = if cfg!(target_os = "macos") {
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

        if cfg!(target_os = "macos") {
            // macOS: ~/.config/cfgd/env.sh for shells
            match Self::macos_write_env_sh(&managed) {
                Ok(()) => {
                    printer.success(&format!("Updated {}", Self::macos_env_sh_path().display()));
                    printer.info("Add to your shell rc: . ~/.config/cfgd/env.sh");
                }
                Err(e) => {
                    printer.warning(&format!("Could not write env.sh: {}", e));
                }
            }

            // macOS: launchd plist for GUI apps
            match Self::macos_write_launchd_plist(&managed) {
                Ok(()) => {
                    printer.success(&format!("Updated {}", Self::macos_plist_path().display()));
                }
                Err(e) => {
                    printer.warning(&format!("Could not write launchd plist: {}", e));
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
                        "Could not update {} (may need root): {}",
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
                        "Could not update {} (may need root): {}",
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
    fn shell_configurator_name() {
        let sc = ShellConfigurator;
        assert_eq!(sc.name(), "shell");
    }

    #[test]
    fn shell_configurator_current_state() {
        let sc = ShellConfigurator;
        let state = sc.current_state().unwrap();
        assert!(state.is_string());
    }

    #[test]
    fn shell_configurator_diff_matching() {
        let sc = ShellConfigurator;
        let current = std::env::var("SHELL").unwrap_or_default();
        let desired = serde_yaml::Value::String(current);
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn shell_configurator_diff_different() {
        let sc = ShellConfigurator;
        let desired = serde_yaml::Value::String("/bin/nonexistent_shell".to_string());
        let drifts = sc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "default-shell");
        assert_eq!(drifts[0].expected, "/bin/nonexistent_shell");
    }

    #[test]
    fn shell_configurator_diff_empty() {
        let sc = ShellConfigurator;
        let desired = serde_yaml::Value::String(String::new());
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn macos_defaults_name() {
        let md = MacosDefaultsConfigurator;
        assert_eq!(md.name(), "macosDefaults");
    }

    #[test]
    fn systemd_units_name() {
        let su = SystemdUnitConfigurator;
        assert_eq!(su.name(), "systemdUnits");
    }

    #[test]
    fn launch_agents_name() {
        let la = LaunchAgentConfigurator;
        assert_eq!(la.name(), "launchAgents");
    }

    #[test]
    fn generate_plist_content() {
        let plist =
            generate_launch_agent_plist("com.example.test", "/usr/bin/test", &["--flag"], true);
        assert!(plist.contains("com.example.test"));
        assert!(plist.contains("/usr/bin/test"));
        assert!(plist.contains("--flag"));
        assert!(plist.contains("<true />"));
    }

    #[test]
    fn generate_plist_no_args() {
        let plist = generate_launch_agent_plist("com.example.test", "/usr/bin/test", &[], false);
        assert!(plist.contains("com.example.test"));
        assert!(plist.contains("<false />"));
    }

    #[test]
    fn yaml_value_to_defaults_string_conversion() {
        assert_eq!(
            yaml_value_to_defaults_string(&serde_yaml::Value::Bool(true)),
            "1"
        );
        assert_eq!(
            yaml_value_to_defaults_string(&serde_yaml::Value::Bool(false)),
            "0"
        );
        assert_eq!(
            yaml_value_to_defaults_string(&serde_yaml::Value::Number(42.into())),
            "42"
        );
        assert_eq!(
            yaml_value_to_defaults_string(&serde_yaml::Value::String("hello".into())),
            "hello"
        );
    }

    #[test]
    fn yaml_value_to_defaults_type_detection() {
        let (t, v) = yaml_value_to_defaults_type(&serde_yaml::Value::Bool(true));
        assert_eq!(t, "bool");
        assert_eq!(v, "true");

        let (t, v) = yaml_value_to_defaults_type(&serde_yaml::Value::Number(42.into()));
        assert_eq!(t, "int");
        assert_eq!(v, "42");

        let (t, v) = yaml_value_to_defaults_type(&serde_yaml::Value::String("test".into()));
        assert_eq!(t, "string");
        assert_eq!(v, "test");
    }

    #[test]
    fn systemd_diff_with_no_units() {
        let su = SystemdUnitConfigurator;
        let desired = serde_yaml::Value::Sequence(Vec::new());
        let drifts = su.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn launch_agent_diff_with_no_agents() {
        let la = LaunchAgentConfigurator;
        let desired = serde_yaml::Value::Sequence(Vec::new());
        let drifts = la.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn environment_configurator_name() {
        let ec = EnvironmentConfigurator;
        assert_eq!(ec.name(), "environment");
    }

    #[test]
    fn environment_configurator_is_available() {
        let ec = EnvironmentConfigurator;
        assert_eq!(ec.is_available(), cfg!(unix));
    }

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
    fn environment_desired_vars_empty_mapping() {
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        assert!(vars.is_empty());
    }

    #[test]
    fn environment_desired_vars_non_mapping() {
        let yaml = serde_yaml::Value::String("not a mapping".to_string());
        let vars = EnvironmentConfigurator::desired_vars(&yaml);
        assert!(vars.is_empty());
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
    fn environment_diff_empty_desired() {
        let ec = EnvironmentConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = ec.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    // --- diff_yaml_mapping ---

    #[test]
    fn diff_yaml_mapping_detects_drift() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("key1".into()),
            serde_yaml::Value::String("expected".into()),
        );

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_defaults_string, |_| {
            "actual".to_string()
        });
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "key1");
        assert_eq!(drifts[0].expected, "expected");
        assert_eq!(drifts[0].actual, "actual");
    }

    #[test]
    fn diff_yaml_mapping_no_drift_when_matching() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("key1".into()),
            serde_yaml::Value::String("same".into()),
        );

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_defaults_string, |_| {
            "same".to_string()
        });
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_yaml_mapping_with_prefix() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("setting".into()),
            serde_yaml::Value::String("val".into()),
        );

        let drifts = diff_yaml_mapping(&desired, "sysctl", yaml_value_to_defaults_string, |_| {
            "other".to_string()
        });
        assert_eq!(drifts[0].key, "sysctl.setting");
    }

    #[test]
    fn diff_yaml_mapping_multiple_keys() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("a".into()),
            serde_yaml::Value::String("1".into()),
        );
        desired.insert(
            serde_yaml::Value::String("b".into()),
            serde_yaml::Value::String("2".into()),
        );

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_defaults_string, |k| {
            if k == "a" {
                "1".to_string()
            } else {
                "wrong".to_string()
            }
        });
        // Only "b" should drift
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "b");
    }

    // --- yaml_value_to_defaults_string ---

    #[test]
    fn yaml_value_to_defaults_string_bool_converts_to_01() {
        // macos defaults uses "1"/"0" for bools, not "true"/"false"
        assert_eq!(
            yaml_value_to_defaults_string(&serde_yaml::Value::Bool(true)),
            "1"
        );
        assert_eq!(
            yaml_value_to_defaults_string(&serde_yaml::Value::Bool(false)),
            "0"
        );
    }

    #[test]
    fn yaml_value_to_defaults_string_number_and_string() {
        assert_eq!(
            yaml_value_to_defaults_string(&serde_yaml::Value::Number(42.into())),
            "42"
        );
        assert_eq!(
            yaml_value_to_defaults_string(&serde_yaml::Value::String("hello".into())),
            "hello"
        );
    }

    // --- generate_launch_agent_plist edge cases ---

    #[test]
    fn generate_plist_xml_escaped_args() {
        let plist =
            generate_launch_agent_plist("com.example.test", "/usr/bin/test", &["--key=a&b"], false);
        // Should contain XML-escaped ampersand
        assert!(plist.contains("&amp;") || plist.contains("--key=a&b"));
    }

    #[test]
    fn generate_plist_multiple_args() {
        let plist = generate_launch_agent_plist(
            "com.example.test",
            "/usr/bin/test",
            &["--flag1", "--flag2", "value"],
            true,
        );
        assert!(plist.contains("--flag1"));
        assert!(plist.contains("--flag2"));
        assert!(plist.contains("value"));
    }
}

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

/// ShellConfigurator — reads/sets default shell.
///
/// **Unix**: reads `$SHELL`, uses `chsh -s` to change the default shell.
/// **Windows**: manages Windows Terminal's default profile via its `settings.json`.
///   The desired value is the profile *name* (e.g. "PowerShell", "Git Bash").
pub struct ShellConfigurator;

/// Path to Windows Terminal settings.json (Microsoft Store install).
fn windows_terminal_settings_path() -> Option<std::path::PathBuf> {
    let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
    let path = std::path::PathBuf::from(local_app_data)
        .join("Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState/settings.json");
    if path.exists() { Some(path) } else { None }
}

/// Resolve the human-readable name of the current default profile.
fn resolve_default_profile_name(settings: &serde_json::Value) -> Option<String> {
    let default_guid = settings.get("defaultProfile")?.as_str()?;
    let profiles = settings.get("profiles")?.get("list")?.as_array()?;
    for p in profiles {
        if p.get("guid").and_then(|g| g.as_str()) == Some(default_guid) {
            return p
                .get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}

/// Find the GUID of a profile by its name.
fn find_profile_guid(settings: &serde_json::Value, name: &str) -> Option<String> {
    let profiles = settings.get("profiles")?.get("list")?.as_array()?;
    for p in profiles {
        if p.get("name").and_then(|n| n.as_str()) == Some(name) {
            return p
                .get("guid")
                .and_then(|g| g.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}

impl SystemConfigurator for ShellConfigurator {
    fn name(&self) -> &str {
        "shell"
    }

    fn is_available(&self) -> bool {
        cfg!(unix) || cfg!(windows)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        if cfg!(windows) {
            // Read Windows Terminal settings.json for the default profile name
            let path = match windows_terminal_settings_path() {
                Some(p) => p,
                None => return Ok(serde_yaml::Value::String(String::new())),
            };
            let content =
                std::fs::read_to_string(&path).map_err(cfgd_core::errors::CfgdError::Io)?;
            let settings: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                    message: e.to_string(),
                })
            })?;
            let name = resolve_default_profile_name(&settings).unwrap_or_default();
            Ok(serde_yaml::Value::String(name))
        } else {
            let shell = std::env::var("SHELL").unwrap_or_default();
            Ok(serde_yaml::Value::String(shell))
        }
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let desired_shell = desired.as_str().unwrap_or_default();
        if desired_shell.is_empty() {
            return Ok(Vec::new());
        }

        if cfg!(windows) {
            let path = match windows_terminal_settings_path() {
                Some(p) => p,
                None => {
                    return Ok(vec![SystemDrift {
                        key: "default-shell".to_string(),
                        expected: desired_shell.to_string(),
                        actual: "Windows Terminal not installed".to_string(),
                    }]);
                }
            };
            let content =
                std::fs::read_to_string(&path).map_err(cfgd_core::errors::CfgdError::Io)?;
            let settings: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                    message: e.to_string(),
                })
            })?;
            let current = resolve_default_profile_name(&settings).unwrap_or_default();
            if current == desired_shell {
                return Ok(Vec::new());
            }
            Ok(vec![SystemDrift {
                key: "default-shell".to_string(),
                expected: desired_shell.to_string(),
                actual: current,
            }])
        } else {
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
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let desired_shell = desired.as_str().unwrap_or_default();
        if desired_shell.is_empty() {
            return Ok(());
        }

        if cfg!(windows) {
            let path = match windows_terminal_settings_path() {
                Some(p) => p,
                None => {
                    printer.warning("Windows Terminal not installed; cannot set default profile");
                    return Ok(());
                }
            };
            let content =
                std::fs::read_to_string(&path).map_err(cfgd_core::errors::CfgdError::Io)?;
            let mut settings: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                    message: e.to_string(),
                })
            })?;

            let guid = match find_profile_guid(&settings, desired_shell) {
                Some(g) => g,
                None => {
                    printer.warning(&format!(
                        "No Windows Terminal profile named '{}' found",
                        desired_shell
                    ));
                    return Ok(());
                }
            };

            printer.info(&format!(
                "Setting Windows Terminal default profile to '{}' ({})",
                desired_shell, guid
            ));

            settings["defaultProfile"] = serde_json::Value::String(guid);

            let updated = serde_json::to_string_pretty(&settings).map_err(|e| {
                cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                    message: e.to_string(),
                })
            })?;
            cfgd_core::atomic_write_str(&path, &updated)?;

            Ok(())
        } else {
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

    // --- Windows ---

    /// Parse the output of `reg query HKCU\Environment` into a map of variable
    /// names to values. Handles both `REG_SZ` and `REG_EXPAND_SZ` value types.
    ///
    /// Each data line has the format (with 4-space separators):
    /// `    NAME    REG_SZ    VALUE`
    fn parse_reg_query_output(stdout: &str) -> BTreeMap<String, String> {
        let mut vars = BTreeMap::new();
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("HKEY_") {
                continue;
            }
            // Format: "NAME    REG_SZ    VALUE" or "NAME    REG_EXPAND_SZ    VALUE"
            // Fields are separated by 4 spaces.
            let parts: Vec<&str> = line.splitn(3, "    ").collect();
            if parts.len() == 3 {
                let name = parts[0].trim();
                let value = parts[2].trim();
                if !name.is_empty() {
                    vars.insert(name.to_string(), value.to_string());
                }
            }
        }
        vars
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
            Ok(output) if output.status.success() => {
                // Also set in current process so subsequent operations see the change
                // SAFETY: single-threaded apply phase
                unsafe {
                    std::env::set_var(name, value);
                }
            }
            Ok(output) => {
                printer.warning(&format!("setx {} failed: {}", name, stderr_string(&output)));
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

/// WindowsRegistryConfigurator — reads/writes Windows registry settings.
///
/// Manages `spec.system.windowsRegistry` entries declaratively, analogous to
/// `MacosDefaultsConfigurator` for macOS `defaults` domains.
///
/// Config format:
/// ```yaml
/// system:
///   windowsRegistry:
///     HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced:
///       HideFileExt: 0
///       ShowHiddenFiles: 1
///     HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize:
///       AppsUseLightTheme: 0
/// ```
pub struct WindowsRegistryConfigurator;

impl WindowsRegistryConfigurator {
    /// Parse a registry path into (hive, subpath).
    /// Read a single registry value using `reg query`.
    /// Returns `None` on non-Windows or if the value does not exist.
    fn read_reg_value(key_path: &str, value_name: &str) -> Option<String> {
        if !cfg!(windows) {
            return None;
        }
        let output = Command::new("reg")
            .args(["query", key_path, "/v", value_name])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Self::parse_reg_value_output(&stdout, value_name)
    }

    /// Write a registry value using `reg add`.
    /// Infers the type: numeric values use `REG_DWORD`, otherwise `REG_SZ`.
    fn write_reg_value(
        key_path: &str,
        value_name: &str,
        value: &str,
        printer: &Printer,
    ) -> Result<()> {
        if !cfg!(windows) {
            return Ok(());
        }
        let reg_type = if value.parse::<u32>().is_ok() {
            "REG_DWORD"
        } else {
            "REG_SZ"
        };

        let output = Command::new("reg")
            .args([
                "add", key_path, "/v", value_name, "/t", reg_type, "/d", value, "/f",
            ])
            .output()
            .map_err(cfgd_core::errors::CfgdError::Io)?;

        if !output.status.success() {
            printer.warning(&format!(
                "reg add failed for {}\\{}: {}",
                key_path,
                value_name,
                stderr_string(&output)
            ));
        }
        Ok(())
    }

    /// Parse `reg query` output for a single value.
    ///
    /// Input lines look like: `"    ValueName    REG_DWORD    0x0"`
    /// For DWORD values, converts hex (`0x...`) to decimal string for comparison.
    fn parse_reg_value_output(output: &str, value_name: &str) -> Option<String> {
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("HKEY_") {
                continue;
            }
            // Fields are separated by 4 spaces.
            let parts: Vec<&str> = line.splitn(3, "    ").collect();
            if parts.len() == 3 && parts[0].trim() == value_name {
                let reg_type = parts[1].trim();
                let raw_value = parts[2].trim();
                // Convert DWORD hex to decimal for comparison
                if reg_type == "REG_DWORD"
                    && let Some(hex_str) = raw_value.strip_prefix("0x")
                    && let Ok(n) = u32::from_str_radix(hex_str, 16)
                {
                    return Some(n.to_string());
                }
                return Some(raw_value.to_string());
            }
        }
        None
    }
}

impl SystemConfigurator for WindowsRegistryConfigurator {
    fn name(&self) -> &str {
        "windowsRegistry"
    }

    fn is_available(&self) -> bool {
        cfg!(windows)
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
        for (key_path_val, values_val) in mapping {
            let key_path = match key_path_val.as_str() {
                Some(k) => k,
                None => continue,
            };
            let values = match values_val.as_mapping() {
                Some(m) => m,
                None => continue,
            };
            drifts.extend(diff_yaml_mapping(
                values,
                key_path,
                yaml_value_to_defaults_string,
                |value_name| Self::read_reg_value(key_path, value_name).unwrap_or_default(),
            ));
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        for (key_path_val, values_val) in mapping {
            let key_path = match key_path_val.as_str() {
                Some(k) => k,
                None => continue,
            };
            let values = match values_val.as_mapping() {
                Some(m) => m,
                None => continue,
            };
            for (name_val, desired_val) in values {
                let name = match name_val.as_str() {
                    Some(n) => n,
                    None => continue,
                };
                let desired_str = yaml_value_to_defaults_string(desired_val);
                Self::write_reg_value(key_path, name, &desired_str, printer)?;
                printer.success(&format!("Set {}\\{} = {}", key_path, name, desired_str));
            }
        }

        Ok(())
    }
}

/// WindowsServiceConfigurator — manages Windows Services declaratively.
///
/// Uses `sc.exe` to query, create, configure, and start/stop services.
///
/// Config format:
/// ```yaml
/// system:
///   windowsServices:
///     - name: MyService
///       displayName: My Custom Service
///       binaryPath: C:\path\to\service.exe
///       startType: auto    # auto | manual | disabled
///       state: running     # running | stopped
/// ```
pub struct WindowsServiceConfigurator;

/// Parsed service entry from YAML config.
struct ServiceEntry {
    name: String,
    display_name: Option<String>,
    binary_path: Option<String>,
    start_type: Option<String>,
    state: Option<String>,
}

impl WindowsServiceConfigurator {
    /// Parse the YAML sequence of service entries.
    fn parse_services(desired: &serde_yaml::Value) -> Vec<ServiceEntry> {
        let seq = match desired.as_sequence() {
            Some(s) => s,
            None => return Vec::new(),
        };
        let mut entries = Vec::new();
        for item in seq {
            let name = match item.get("name").and_then(|v| v.as_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            entries.push(ServiceEntry {
                name,
                display_name: item
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                binary_path: item
                    .get("binaryPath")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                start_type: item
                    .get("startType")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                state: item
                    .get("state")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            });
        }
        entries
    }

    /// Query a service's current state and start type via `sc query` and `sc qc`.
    fn query_service(name: &str) -> Option<(String, String)> {
        if !cfg!(windows) {
            return None;
        }
        let query_output = Command::new("sc").args(["query", name]).output().ok()?;
        if !query_output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&query_output.stdout);
        let state = parse_sc_state(&stdout)?;

        let qc_output = Command::new("sc").args(["qc", name]).output().ok()?;
        let qc_stdout = String::from_utf8_lossy(&qc_output.stdout);
        let start_type = parse_sc_start_type(&qc_stdout).unwrap_or_default();

        Some((state, start_type))
    }
}

/// Parse the STATE line from `sc query` output.
///
/// Input format: `"        STATE              : 4  RUNNING"`
/// Returns normalized state: "running", "stopped", etc.
fn parse_sc_state(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("STATE")
            && let Some((_, rest)) = line.split_once(':')
        {
            let rest = rest.trim();
            if let Some(state_word) = rest.split_whitespace().nth(1) {
                return Some(state_word.to_lowercase().replace('_', "-"));
            }
        }
    }
    None
}

/// Parse the START_TYPE line from `sc qc` output.
///
/// Input format: `"        START_TYPE         : 2   AUTO_START"`
/// Returns normalized start type: "auto", "manual", "disabled".
fn parse_sc_start_type(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("START_TYPE")
            && let Some((_, rest)) = line.split_once(':')
        {
            let rest = rest.trim().to_uppercase();
            if rest.contains("AUTO_START") {
                return Some("auto".to_string());
            } else if rest.contains("DEMAND_START") {
                return Some("manual".to_string());
            } else if rest.contains("DISABLED") {
                return Some("disabled".to_string());
            }
        }
    }
    None
}

impl SystemConfigurator for WindowsServiceConfigurator {
    fn name(&self) -> &str {
        "windowsServices"
    }

    fn is_available(&self) -> bool {
        cfg!(windows)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Sequence(Vec::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let entries = Self::parse_services(desired);
        let mut drifts = Vec::new();
        for entry in &entries {
            match Self::query_service(&entry.name) {
                Some((current_state, current_start_type)) => {
                    if let Some(ref desired_state) = entry.state
                        && &current_state != desired_state
                    {
                        drifts.push(SystemDrift {
                            key: format!("{}.state", entry.name),
                            expected: desired_state.clone(),
                            actual: current_state.clone(),
                        });
                    }
                    if let Some(ref desired_start) = entry.start_type
                        && &current_start_type != desired_start
                    {
                        drifts.push(SystemDrift {
                            key: format!("{}.startType", entry.name),
                            expected: desired_start.clone(),
                            actual: current_start_type,
                        });
                    }
                }
                None => {
                    if entry.binary_path.is_some() {
                        drifts.push(SystemDrift {
                            key: format!("{}.exists", entry.name),
                            expected: "present".to_string(),
                            actual: "absent".to_string(),
                        });
                    }
                }
            }
        }
        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let entries = Self::parse_services(desired);
        for entry in &entries {
            let exists = Self::query_service(&entry.name).is_some();

            if !exists {
                if let Some(ref binary_path) = entry.binary_path {
                    let bin_arg = format!("binPath= {}", binary_path);
                    let mut args = vec!["create", &entry.name, &*bin_arg];

                    let display_arg;
                    if let Some(ref dn) = entry.display_name {
                        display_arg = format!("DisplayName= {}", dn);
                        args.push(&display_arg);
                    }

                    let start_arg;
                    if let Some(ref st) = entry.start_type {
                        start_arg = format!(
                            "start= {}",
                            match st.as_str() {
                                "auto" => "auto",
                                "manual" => "demand",
                                "disabled" => "disabled",
                                _ => "demand",
                            }
                        );
                        args.push(&start_arg);
                    }

                    let output = Command::new("sc")
                        .args(&args)
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;
                    if output.status.success() {
                        printer.success(&format!("Created service {}", entry.name));
                    } else {
                        printer.warning(&format!(
                            "Failed to create service {}: {}",
                            entry.name,
                            stderr_string(&output)
                        ));
                    }
                }
            } else {
                // Configure start type on existing service
                if let Some(ref start_type) = entry.start_type {
                    let sc_start = match start_type.as_str() {
                        "auto" => "auto",
                        "manual" => "demand",
                        "disabled" => "disabled",
                        _ => continue,
                    };
                    let start_arg = format!("start= {}", sc_start);
                    let output = Command::new("sc")
                        .args(["config", &entry.name, &start_arg])
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;
                    if !output.status.success() {
                        printer.warning(&format!(
                            "Failed to set start type for {}: {}",
                            entry.name,
                            stderr_string(&output)
                        ));
                    }
                }
            }

            // Set desired state
            if let Some(ref state) = entry.state {
                match state.as_str() {
                    "running" => {
                        let output = Command::new("sc")
                            .args(["start", &entry.name])
                            .output()
                            .map_err(cfgd_core::errors::CfgdError::Io)?;
                        if output.status.success() {
                            printer.success(&format!("Started service {}", entry.name));
                        } else {
                            let stderr = stderr_string(&output);
                            if !stderr.contains("already been started") {
                                printer.warning(&format!(
                                    "Failed to start {}: {}",
                                    entry.name, stderr
                                ));
                            }
                        }
                    }
                    "stopped" => {
                        let output = Command::new("sc")
                            .args(["stop", &entry.name])
                            .output()
                            .map_err(cfgd_core::errors::CfgdError::Io)?;
                        if output.status.success() {
                            printer.success(&format!("Stopped service {}", entry.name));
                        } else {
                            let stderr = stderr_string(&output);
                            if !stderr.contains("has not been started") {
                                printer
                                    .warning(&format!("Failed to stop {}: {}", entry.name, stderr));
                            }
                        }
                    }
                    _ => {}
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
        assert_eq!(ec.is_available(), cfg!(unix) || cfg!(windows));
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

    // --- Windows registry output parsing ---

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

    // --- Windows Terminal helpers ---

    #[test]
    fn parse_windows_terminal_default_profile() {
        let settings = serde_json::json!({
            "defaultProfile": "{574e775e-4f2a-5b96-ac1e-a2962a402336}",
            "profiles": {
                "list": [
                    {"guid": "{574e775e-4f2a-5b96-ac1e-a2962a402336}", "name": "PowerShell"},
                    {"guid": "{0caa0dad-35be-5f56-a8ff-afceeeaa6101}", "name": "Command Prompt"},
                    {"guid": "{2c4de342-38b7-51cf-b940-2309a097f518}", "name": "Git Bash"},
                ]
            }
        });
        let default = resolve_default_profile_name(&settings);
        assert_eq!(default, Some("PowerShell".to_string()));
    }

    #[test]
    fn resolve_default_profile_name_unknown_guid() {
        let settings = serde_json::json!({
            "defaultProfile": "{00000000-0000-0000-0000-000000000000}",
            "profiles": {
                "list": [
                    {"guid": "{574e775e-4f2a-5b96-ac1e-a2962a402336}", "name": "PowerShell"},
                ]
            }
        });
        assert_eq!(resolve_default_profile_name(&settings), None);
    }

    #[test]
    fn resolve_default_profile_name_missing_fields() {
        // No defaultProfile key
        let settings = serde_json::json!({"profiles": {"list": []}});
        assert_eq!(resolve_default_profile_name(&settings), None);

        // No profiles key
        let settings = serde_json::json!({"defaultProfile": "{abc}"});
        assert_eq!(resolve_default_profile_name(&settings), None);

        // Empty object
        let settings = serde_json::json!({});
        assert_eq!(resolve_default_profile_name(&settings), None);
    }

    #[test]
    fn find_windows_terminal_profile_guid_test() {
        let settings = serde_json::json!({
            "profiles": {
                "list": [
                    {"guid": "{574e775e-4f2a-5b96-ac1e-a2962a402336}", "name": "PowerShell"},
                    {"guid": "{2c4de342-38b7-51cf-b940-2309a097f518}", "name": "Git Bash"},
                ]
            }
        });
        assert_eq!(
            find_profile_guid(&settings, "Git Bash"),
            Some("{2c4de342-38b7-51cf-b940-2309a097f518}".to_string())
        );
        assert_eq!(find_profile_guid(&settings, "Nonexistent"), None);
    }

    #[test]
    fn find_profile_guid_missing_profiles() {
        let settings = serde_json::json!({});
        assert_eq!(find_profile_guid(&settings, "PowerShell"), None);
    }

    // --- WindowsRegistryConfigurator ---

    #[test]
    fn registry_configurator_name() {
        assert_eq!(WindowsRegistryConfigurator.name(), "windowsRegistry");
    }

    #[test]
    fn registry_parse_reg_value_dword() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          HideFileExt    REG_DWORD    0x0\n";
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "HideFileExt"),
            Some("0".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_dword_nonzero() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          ShowHidden    REG_DWORD    0x1\n";
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "ShowHidden"),
            Some("1".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_dword_large() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          Timeout    REG_DWORD    0xff\n";
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Timeout"),
            Some("255".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_string() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          Theme    REG_SZ    dark\n";
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Theme"),
            Some("dark".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_missing() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\n";
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Missing"),
            None
        );
    }

    #[test]
    fn registry_parse_reg_value_wrong_name() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          OtherValue    REG_SZ    hello\n";
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Missing"),
            None
        );
    }

    #[test]
    fn registry_parse_reg_value_empty_input() {
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output("", "Anything"),
            None
        );
    }

    #[test]
    fn registry_diff_empty_desired() {
        let wrc = WindowsRegistryConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = wrc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn registry_diff_non_mapping_desired() {
        let wrc = WindowsRegistryConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        let drifts = wrc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn registry_is_available_matches_platform() {
        let wrc = WindowsRegistryConfigurator;
        assert_eq!(wrc.is_available(), cfg!(windows));
    }

    // --- WindowsServiceConfigurator ---

    #[test]
    fn service_configurator_name() {
        assert_eq!(WindowsServiceConfigurator.name(), "windowsServices");
    }

    #[test]
    fn service_configurator_is_available_matches_platform() {
        assert_eq!(WindowsServiceConfigurator.is_available(), cfg!(windows));
    }

    #[test]
    fn service_parse_sc_state_running() {
        let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n\
                      \tSTATE              : 4  RUNNING\n\
                      \t\t\t\t\t(STOPPABLE, NOT_PAUSABLE)\n";
        assert_eq!(parse_sc_state(output), Some("running".to_string()));
    }

    #[test]
    fn service_parse_sc_state_stopped() {
        let output = "SERVICE_NAME: MyService\n\
                      \tSTATE              : 1  STOPPED\n";
        assert_eq!(parse_sc_state(output), Some("stopped".to_string()));
    }

    #[test]
    fn service_parse_sc_state_paused() {
        let output = "\tSTATE              : 7  PAUSED\n";
        assert_eq!(parse_sc_state(output), Some("paused".to_string()));
    }

    #[test]
    fn service_parse_sc_state_no_state_line() {
        let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n";
        assert_eq!(parse_sc_state(output), None);
    }

    #[test]
    fn service_parse_sc_state_empty_input() {
        assert_eq!(parse_sc_state(""), None);
    }

    #[test]
    fn service_parse_sc_start_type_auto() {
        let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n\
                      \tSTART_TYPE         : 2   AUTO_START\n\
                      \tERROR_CONTROL      : 1   NORMAL\n";
        assert_eq!(parse_sc_start_type(output), Some("auto".to_string()));
    }

    #[test]
    fn service_parse_sc_start_type_manual() {
        let output = "\tSTART_TYPE         : 3   DEMAND_START\n";
        assert_eq!(parse_sc_start_type(output), Some("manual".to_string()));
    }

    #[test]
    fn service_parse_sc_start_type_disabled() {
        let output = "\tSTART_TYPE         : 4   DISABLED\n";
        assert_eq!(parse_sc_start_type(output), Some("disabled".to_string()));
    }

    #[test]
    fn service_parse_sc_start_type_no_start_type_line() {
        let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n";
        assert_eq!(parse_sc_start_type(output), None);
    }

    #[test]
    fn service_parse_sc_start_type_empty_input() {
        assert_eq!(parse_sc_start_type(""), None);
    }

    #[test]
    fn service_parse_entries() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: MyService
  displayName: My Custom Service
  binaryPath: C:\path\to\service.exe
  startType: auto
  state: running
- name: AnotherService
  startType: disabled
  state: stopped
"#,
        )
        .unwrap();
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "MyService");
        assert_eq!(
            entries[0].display_name,
            Some("My Custom Service".to_string())
        );
        assert_eq!(
            entries[0].binary_path,
            Some(r"C:\path\to\service.exe".to_string())
        );
        assert_eq!(entries[0].start_type, Some("auto".to_string()));
        assert_eq!(entries[0].state, Some("running".to_string()));
        assert_eq!(entries[1].name, "AnotherService");
        assert_eq!(entries[1].binary_path, None);
        assert_eq!(entries[1].display_name, None);
        assert_eq!(entries[1].start_type, Some("disabled".to_string()));
        assert_eq!(entries[1].state, Some("stopped".to_string()));
    }

    #[test]
    fn service_parse_entries_empty_sequence() {
        let yaml = serde_yaml::Value::Sequence(Vec::new());
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert!(entries.is_empty());
    }

    #[test]
    fn service_parse_entries_non_sequence() {
        let yaml = serde_yaml::Value::String("not a sequence".to_string());
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert!(entries.is_empty());
    }

    #[test]
    fn service_parse_entries_skips_missing_name() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- displayName: No Name Field
  startType: auto
- name: ValidService
  state: running
"#,
        )
        .unwrap();
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ValidService");
    }

    #[test]
    fn service_diff_empty_desired() {
        let wsc = WindowsServiceConfigurator;
        let yaml = serde_yaml::Value::Sequence(Vec::new());
        let drifts = wsc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn service_diff_non_sequence_desired() {
        let wsc = WindowsServiceConfigurator;
        let yaml = serde_yaml::Value::String("not a sequence".into());
        let drifts = wsc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn service_current_state_returns_empty_sequence() {
        let wsc = WindowsServiceConfigurator;
        let state = wsc.current_state().unwrap();
        assert!(state.is_sequence());
        assert!(state.as_sequence().unwrap().is_empty());
    }
}

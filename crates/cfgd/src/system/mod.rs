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

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

// ---------------------------------------------------------------------------
// Shared helpers used by both mod.rs and node.rs configurators
// ---------------------------------------------------------------------------

/// Run a command and return its trimmed stdout, or empty string on failure.
fn read_command_output(cmd: &mut Command) -> String {
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

/// ShellConfigurator — reads/sets default shell.
///
/// **Unix**: reads `$SHELL`, uses `chsh -s` to change the default shell.
/// **Windows**: manages Windows Terminal's default profile via its `settings.json`.
///   The desired value is the profile *name* (e.g. "PowerShell", "Git Bash").
pub struct ShellConfigurator;

/// Path to Windows Terminal settings.json (Store, Preview, or non-Store install).
fn windows_terminal_settings_path() -> Option<std::path::PathBuf> {
    let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
    let base = std::path::PathBuf::from(&local_app_data);

    // Check paths in priority order: Store > Preview > non-Store
    let candidates = [
        base.join("Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState/settings.json"),
        base.join(
            "Packages/Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe/LocalState/settings.json",
        ),
        base.join("Microsoft/Windows Terminal/settings.json"),
    ];

    candidates.into_iter().find(|p| p.exists())
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

/// Load and parse Windows Terminal settings.json.
/// Returns `Ok(None)` if Windows Terminal is not installed.
fn load_terminal_settings() -> Result<Option<(std::path::PathBuf, serde_json::Value)>> {
    let path = match windows_terminal_settings_path() {
        Some(p) => p,
        None => return Ok(None),
    };
    let content = std::fs::read_to_string(&path).map_err(cfgd_core::errors::CfgdError::Io)?;
    let settings: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
            message: e.to_string(),
        })
    })?;
    Ok(Some((path, settings)))
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
            let name = match load_terminal_settings()? {
                Some((_path, settings)) => {
                    resolve_default_profile_name(&settings).unwrap_or_default()
                }
                None => String::new(),
            };
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
            let current = match load_terminal_settings()? {
                Some((_path, settings)) => {
                    resolve_default_profile_name(&settings).unwrap_or_default()
                }
                None => {
                    return Ok(vec![SystemDrift {
                        key: "default-shell".to_string(),
                        expected: desired_shell.to_string(),
                        actual: "Windows Terminal not installed".to_string(),
                    }]);
                }
            };
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
            let (path, mut settings) = match load_terminal_settings()? {
                Some(pair) => pair,
                None => {
                    printer.warning("Windows Terminal not installed; cannot set default profile");
                    return Ok(());
                }
            };

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
                    cfgd_core::stderr_lossy_trimmed(&output)
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
                yaml_value_with_numeric_bools,
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
                        cfgd_core::stderr_lossy_trimmed(&output)
                    ));
                }
            }
        }

        Ok(())
    }
}

fn read_defaults_value(domain: &str, key: &str) -> String {
    read_command_output(Command::new("defaults").args(["read", domain, key]))
}

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

            if let Some(unit_file) = unit.get("unitFile").and_then(|v| v.as_str()) {
                let dest = format!("/etc/systemd/system/{}", name);
                let dest_path = std::path::Path::new(&dest);
                if !dest_path.exists() {
                    drifts.push(SystemDrift {
                        key: format!("{}.unit-file", name),
                        expected: "present".to_string(),
                        actual: "missing".to_string(),
                    });
                } else if let Ok(source_content) = std::fs::read(unit_file)
                    && let Ok(dest_content) = std::fs::read(&dest)
                    && source_content != dest_content
                {
                    drifts.push(SystemDrift {
                        key: format!("{}.unit-file", name),
                        expected: "updated".to_string(),
                        actual: "outdated".to_string(),
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
                if let Err(e) = Command::new("systemctl").arg("daemon-reload").output() {
                    tracing::warn!("systemctl daemon-reload failed: {e}");
                }
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
                    cfgd_core::stderr_lossy_trimmed(&output)
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
            let expected_content = generate_launch_agent_plist(name, program, &args, run_at_load);

            if !plist_path.exists() {
                drifts.push(SystemDrift {
                    key: format!("{}.plist", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            } else if let Ok(current_content) = std::fs::read_to_string(&plist_path)
                && current_content != expected_content
            {
                drifts.push(SystemDrift {
                    key: format!("{}.plist", name),
                    expected: "updated".to_string(),
                    actual: "outdated".to_string(),
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

            // Unload existing agent (best-effort — may not be loaded yet)
            if let Err(e) = Command::new("launchctl")
                .args(["unload", &plist_path.display().to_string()])
                .output()
            {
                tracing::debug!("launchctl unload (pre-load cleanup): {e}");
            }

            let output = Command::new("launchctl")
                .args(["load", &plist_path.display().to_string()])
                .output()
                .map_err(cfgd_core::errors::CfgdError::Io)?;

            if !output.status.success() {
                printer.warning(&format!(
                    "launchctl load failed for {}: {}",
                    name,
                    cfgd_core::stderr_lossy_trimmed(&output)
                ));
            }
        }

        Ok(())
    }
}

fn launch_agent_plist_path(name: &str) -> std::path::PathBuf {
    cfgd_core::expand_tilde(Path::new("~"))
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
                let stdout = String::from_utf8_lossy(&output.stdout);
                printer.warning(&format!("setx {} failed: {}", name, stdout.trim()));
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

    /// Read the registry type of an existing value.
    fn read_reg_type(key_path: &str, value_name: &str) -> Option<String> {
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
        for line in stdout.lines() {
            if let Some((_, reg_type, _)) = parse_reg_line(line)
                && line.contains(value_name)
            {
                return Some(reg_type.to_string());
            }
        }
        None
    }

    /// Write a registry value using `reg add`.
    /// Preserves REG_EXPAND_SZ if the existing value has that type, detects
    /// `%VAR%` patterns, and infers REG_DWORD for numeric values.
    fn write_reg_value(
        key_path: &str,
        value_name: &str,
        value: &str,
        printer: &Printer,
    ) -> Result<()> {
        if !cfg!(windows) {
            return Ok(());
        }

        // Check existing type to preserve REG_EXPAND_SZ
        let existing_type = Self::read_reg_type(key_path, value_name);

        let reg_type = if let Some(ref t) = existing_type
            && t == "REG_EXPAND_SZ"
        {
            "REG_EXPAND_SZ"
        } else if value.contains('%') && value.matches('%').count() >= 2 {
            // Looks like it contains %VAR% patterns
            "REG_EXPAND_SZ"
        } else if value.parse::<u32>().is_ok() {
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
                cfgd_core::stderr_lossy_trimmed(&output)
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
            if let Some((name, reg_type, raw_value)) = parse_reg_line(line)
                && name == value_name
            {
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
                yaml_value_with_numeric_bools,
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
                let desired_str = yaml_value_with_numeric_bools(desired_val);
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

struct ServiceQueryResult {
    state: String,
    start_type: String,
    binary_path: String,
    display_name: String,
}

fn parse_sc_config_value(output: &str, key: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with(key)
            && let Some((_, rest)) = line.split_once(':')
        {
            return Some(rest.trim().to_string());
        }
    }
    None
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

    /// Query a service's current state, start type, binary path, and display name.
    fn query_service(name: &str) -> Option<ServiceQueryResult> {
        if !cfg!(windows) {
            return None;
        }
        let query_output = Command::new("sc.exe").args(["query", name]).output().ok()?;
        if !query_output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&query_output.stdout);
        let state = parse_sc_state(&stdout)?;

        let qc_output = Command::new("sc.exe").args(["qc", name]).output().ok()?;
        let qc_stdout = String::from_utf8_lossy(&qc_output.stdout);
        let start_type = parse_sc_start_type(&qc_stdout).unwrap_or_default();
        let binary_path = parse_sc_config_value(&qc_stdout, "BINARY_PATH_NAME").unwrap_or_default();
        let display_name = parse_sc_config_value(&qc_stdout, "DISPLAY_NAME").unwrap_or_default();

        Some(ServiceQueryResult {
            state,
            start_type,
            binary_path,
            display_name,
        })
    }
}

/// Map user-facing start type to sc.exe value.
/// Returns `None` for unrecognized values.
fn sc_start_type(user_value: &str) -> Option<&'static str> {
    match user_value {
        "auto" => Some("auto"),
        "manual" => Some("demand"),
        "disabled" => Some("disabled"),
        _ => None,
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
                Some(result) => {
                    if let Some(ref desired_state) = entry.state
                        && &result.state != desired_state
                    {
                        drifts.push(SystemDrift {
                            key: format!("{}.state", entry.name),
                            expected: desired_state.clone(),
                            actual: result.state.clone(),
                        });
                    }
                    if let Some(ref desired_start) = entry.start_type
                        && &result.start_type != desired_start
                    {
                        drifts.push(SystemDrift {
                            key: format!("{}.startType", entry.name),
                            expected: desired_start.clone(),
                            actual: result.start_type.clone(),
                        });
                    }
                    if let Some(ref desired_binary) = entry.binary_path
                        && result.binary_path != *desired_binary
                    {
                        drifts.push(SystemDrift {
                            key: format!("{}.binaryPath", entry.name),
                            expected: desired_binary.clone(),
                            actual: result.binary_path.clone(),
                        });
                    }
                    if let Some(ref desired_dn) = entry.display_name
                        && result.display_name != *desired_dn
                    {
                        drifts.push(SystemDrift {
                            key: format!("{}.displayName", entry.name),
                            expected: desired_dn.clone(),
                            actual: result.display_name.clone(),
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
            let mut exists = Self::query_service(&entry.name).is_some();

            if !exists {
                if let Some(ref binary_path) = entry.binary_path {
                    // sc.exe requires key= and value as separate arguments
                    let mut args: Vec<&str> = vec!["create", &entry.name, "binPath=", binary_path];

                    if let Some(ref dn) = entry.display_name {
                        args.push("DisplayName=");
                        args.push(dn);
                    }

                    if let Some(ref st) = entry.start_type {
                        let sc_start = sc_start_type(st).unwrap_or_else(|| {
                            printer.warning(&format!(
                                "Unknown start type '{}' for service {}, using 'demand'",
                                st, entry.name
                            ));
                            "demand"
                        });
                        args.push("start=");
                        args.push(sc_start);
                    }

                    let output = Command::new("sc.exe")
                        .args(&args)
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;
                    if output.status.success() {
                        printer.success(&format!("Created service {}", entry.name));
                        exists = true;
                    } else {
                        // sc.exe writes error messages to stdout
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        printer.warning(&format!(
                            "Failed to create service {}: {}",
                            entry.name,
                            stdout.trim()
                        ));
                    }
                }
            } else {
                // Reconfigure existing service
                let mut config_args: Vec<&str> = vec!["config", &entry.name];
                let mut needs_config = false;

                if let Some(ref start_type) = entry.start_type
                    && let Some(sc_start) = sc_start_type(start_type)
                {
                    config_args.push("start=");
                    config_args.push(sc_start);
                    needs_config = true;
                }
                if let Some(ref bp) = entry.binary_path {
                    config_args.push("binPath=");
                    config_args.push(bp);
                    needs_config = true;
                }
                if let Some(ref dn) = entry.display_name {
                    config_args.push("DisplayName=");
                    config_args.push(dn);
                    needs_config = true;
                }
                if needs_config {
                    let output = Command::new("sc.exe")
                        .args(&config_args)
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;
                    if !output.status.success() {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        printer.warning(&format!(
                            "Failed to configure service {}: {}",
                            entry.name,
                            stdout.trim()
                        ));
                    }
                }
            }

            // Set desired state (only if the service exists)
            if !exists {
                continue;
            }
            if let Some(ref desired_state) = entry.state {
                // Query current state to avoid redundant start/stop and locale-dependent string matching
                let current_state = Self::query_service(&entry.name).map(|r| r.state);
                match desired_state.as_str() {
                    "running" => {
                        if current_state.as_deref() != Some("running") {
                            let output = Command::new("sc.exe")
                                .args(["start", &entry.name])
                                .output()
                                .map_err(cfgd_core::errors::CfgdError::Io)?;
                            if output.status.success() {
                                printer.success(&format!("Started service {}", entry.name));
                            } else {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                printer.warning(&format!(
                                    "Failed to start {}: {}",
                                    entry.name,
                                    stdout.trim()
                                ));
                            }
                        }
                    }
                    "stopped" => {
                        if current_state.as_deref() != Some("stopped") {
                            let output = Command::new("sc.exe")
                                .args(["stop", &entry.name])
                                .output()
                                .map_err(cfgd_core::errors::CfgdError::Io)?;
                            if output.status.success() {
                                printer.success(&format!("Stopped service {}", entry.name));
                            } else {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                printer.warning(&format!(
                                    "Failed to stop {}: {}",
                                    entry.name,
                                    stdout.trim()
                                ));
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

/// GsettingsConfigurator — reads/writes GNOME/GTK desktop settings via `gsettings`.
///
/// Covers GNOME, Cinnamon, MATE, Budgie, and Pantheon desktops (all use dconf/gsettings).
///
/// Config format (same two-level structure as macosDefaults):
/// ```yaml
/// system:
///   gsettings:
///     org.gnome.desktop.interface:
///       color-scheme: prefer-dark
///       font-name: "Cantarell 11"
///     org.gnome.desktop.wm.preferences:
///       button-layout: "close,minimize,maximize:"
/// ```
pub struct GsettingsConfigurator;

/// Strip surrounding single quotes from gsettings output.
/// gsettings returns strings as `'value'`, bools/numbers bare.
fn strip_gsettings_quotes(s: &str) -> &str {
    s.strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(s)
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

fn read_gsettings_value(schema: &str, key: &str) -> String {
    let raw = read_command_output(Command::new("gsettings").args(["get", schema, key]));
    strip_gsettings_quotes(&raw).to_string()
}

impl SystemConfigurator for GsettingsConfigurator {
    fn name(&self) -> &str {
        "gsettings"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("gsettings")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        diff_nested_mapping(desired, read_gsettings_value)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        for (schema_key, schema_values) in mapping {
            let schema = match schema_key.as_str() {
                Some(s) => s,
                None => continue,
            };
            let values = match schema_values.as_mapping() {
                Some(m) => m,
                None => continue,
            };

            for (key, desired_value) in values {
                let key_str = match key.as_str() {
                    Some(k) => k,
                    None => continue,
                };

                let gsettings_val = yaml_value_to_string(desired_value);

                printer.info(&format!(
                    "gsettings set {} {} {}",
                    schema, key_str, gsettings_val
                ));

                let output = Command::new("gsettings")
                    .args(["set", schema, key_str, &gsettings_val])
                    .output()
                    .map_err(cfgd_core::errors::CfgdError::Io)?;

                if !output.status.success() {
                    printer.warning(&format!(
                        "gsettings set failed for {}.{}: {}",
                        schema,
                        key_str,
                        cfgd_core::stderr_lossy_trimmed(&output)
                    ));
                }
            }
        }

        Ok(())
    }
}

/// KdeConfigConfigurator — reads/writes KDE Plasma settings via `kwriteconfig5`/`kwriteconfig6`.
///
/// Config format (three-level: file → group → key → value):
/// ```yaml
/// system:
///   kdeConfig:
///     kdeglobals:
///       General:
///         ColorScheme: BreezeDark
///       KDE:
///         LookAndFeelPackage: org.kde.breezedark.desktop
///     kwinrc:
///       Compositing:
///         Backend: OpenGL
/// ```
pub struct KdeConfigConfigurator;

/// Return the kwriteconfig command name (prefer v6, fallback to v5).
fn kde_write_cmd() -> &'static str {
    if cfgd_core::command_available("kwriteconfig6") {
        "kwriteconfig6"
    } else {
        "kwriteconfig5"
    }
}

/// Return the kreadconfig command name (prefer v6, fallback to v5).
fn kde_read_cmd() -> &'static str {
    if cfgd_core::command_available("kreadconfig6") {
        "kreadconfig6"
    } else {
        "kreadconfig5"
    }
}

fn read_kde_value(file: &str, group: &str, key: &str) -> String {
    read_command_output(
        Command::new(kde_read_cmd()).args(["--file", file, "--group", group, "--key", key]),
    )
}

impl SystemConfigurator for KdeConfigConfigurator {
    fn name(&self) -> &str {
        "kdeConfig"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("kwriteconfig6")
            || cfgd_core::command_available("kwriteconfig5")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let files = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        let mut drifts = Vec::new();
        for (file_key, groups) in files {
            let file = match file_key.as_str() {
                Some(f) => f,
                None => continue,
            };
            let groups_map = match groups.as_mapping() {
                Some(m) => m,
                None => continue,
            };
            for (group_key, keys) in groups_map {
                let group = match group_key.as_str() {
                    Some(g) => g,
                    None => continue,
                };
                let keys_map = match keys.as_mapping() {
                    Some(m) => m,
                    None => continue,
                };
                let prefix = format!("{}.{}", file, group);
                drifts.extend(diff_yaml_mapping(
                    keys_map,
                    &prefix,
                    yaml_value_to_string,
                    |key_str| read_kde_value(file, group, key_str),
                ));
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let files = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        let write_cmd = kde_write_cmd();

        for (file_key, groups) in files {
            let file = match file_key.as_str() {
                Some(f) => f,
                None => continue,
            };
            let groups_map = match groups.as_mapping() {
                Some(m) => m,
                None => continue,
            };
            for (group_key, keys) in groups_map {
                let group = match group_key.as_str() {
                    Some(g) => g,
                    None => continue,
                };
                let keys_map = match keys.as_mapping() {
                    Some(m) => m,
                    None => continue,
                };
                for (key, value) in keys_map {
                    let key_str = match key.as_str() {
                        Some(k) => k,
                        None => continue,
                    };
                    let val_str = yaml_value_to_string(value);

                    printer.info(&format!(
                        "{} --file {} --group {} --key {} {}",
                        write_cmd, file, group, key_str, val_str
                    ));

                    let type_flag = match value {
                        serde_yaml::Value::Bool(_) => Some("bool"),
                        serde_yaml::Value::Number(_) => Some("int"),
                        _ => None,
                    };
                    let mut args = vec!["--file", file, "--group", group, "--key", key_str];
                    if let Some(t) = type_flag {
                        args.extend_from_slice(&["--type", t]);
                    }
                    args.push(&val_str);
                    let output = Command::new(write_cmd)
                        .args(&args)
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;

                    if !output.status.success() {
                        printer.warning(&format!(
                            "{} failed for {}.{}.{}: {}",
                            write_cmd,
                            file,
                            group,
                            key_str,
                            cfgd_core::stderr_lossy_trimmed(&output)
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

/// XfconfConfigurator — reads/writes XFCE desktop settings via `xfconf-query`.
///
/// Config format (two-level: channel → property → value):
/// ```yaml
/// system:
///   xfconf:
///     xfwm4:
///       /general/theme: Default
///       /general/title_font: "Sans Bold 9"
///     xsettings:
///       /Net/ThemeName: Adwaita
///       /Net/IconThemeName: elementary-xfce-dark
/// ```
pub struct XfconfConfigurator;

fn read_xfconf_value(channel: &str, property: &str) -> String {
    read_command_output(Command::new("xfconf-query").args(["-c", channel, "-p", property]))
}

impl SystemConfigurator for XfconfConfigurator {
    fn name(&self) -> &str {
        "xfconf"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("xfconf-query")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        diff_nested_mapping(desired, read_xfconf_value)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        for (channel_key, channel_values) in mapping {
            let channel = match channel_key.as_str() {
                Some(c) => c,
                None => continue,
            };
            let values = match channel_values.as_mapping() {
                Some(m) => m,
                None => continue,
            };

            for (key, desired_value) in values {
                let property = match key.as_str() {
                    Some(p) => p,
                    None => continue,
                };

                let val_str = yaml_value_to_string(desired_value);

                printer.info(&format!(
                    "xfconf-query -c {} -p {} -s {}",
                    channel, property, val_str
                ));

                let output = Command::new("xfconf-query")
                    .args(["-c", channel, "-p", property, "-s", &val_str])
                    .output()
                    .map_err(cfgd_core::errors::CfgdError::Io)?;

                if !output.status.success() {
                    // Property may not exist yet — retry with --create
                    let xfconf_type = match desired_value {
                        serde_yaml::Value::Bool(_) => "bool",
                        serde_yaml::Value::Number(_) => "int",
                        _ => "string",
                    };
                    let create_output = Command::new("xfconf-query")
                        .args([
                            "-c",
                            channel,
                            "-p",
                            property,
                            "--create",
                            "-t",
                            xfconf_type,
                            "-s",
                            &val_str,
                        ])
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;

                    if !create_output.status.success() {
                        printer.warning(&format!(
                            "xfconf-query set failed for {}.{}: {}",
                            channel,
                            property,
                            cfgd_core::stderr_lossy_trimmed(&create_output)
                        ));
                    }
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
    fn yaml_value_with_numeric_bools_conversion() {
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(true)),
            "1"
        );
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(false)),
            "0"
        );
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::Number(42.into())),
            "42"
        );
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::String("hello".into())),
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

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_with_numeric_bools, |_| {
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

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_with_numeric_bools, |_| {
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

        let drifts = diff_yaml_mapping(&desired, "sysctl", yaml_value_with_numeric_bools, |_| {
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

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_with_numeric_bools, |k| {
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

    // --- yaml_value_with_numeric_bools ---

    #[test]
    fn yaml_value_with_numeric_bools_bool_converts_to_01() {
        // macos defaults uses "1"/"0" for bools, not "true"/"false"
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(true)),
            "1"
        );
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(false)),
            "0"
        );
    }

    #[test]
    fn yaml_value_with_numeric_bools_number_and_string() {
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::Number(42.into())),
            "42"
        );
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::String("hello".into())),
            "hello"
        );
    }

    // --- generate_launch_agent_plist edge cases ---

    #[test]
    fn generate_plist_xml_escaped_args() {
        let plist =
            generate_launch_agent_plist("com.example.test", "/usr/bin/test", &["--key=a&b"], false);
        // Should contain XML-escaped ampersand
        assert!(
            plist.contains("&amp;"),
            "ampersand must be XML-escaped in plist"
        );
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

    // --- parse_sc_config_value ---

    #[test]
    fn service_parse_sc_config_value_extracts_binary_path() {
        let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n\
                      \tBINARY_PATH_NAME   : C:\\path\\to\\service.exe\n\
                      \tDISPLAY_NAME       : My Custom Service\n";
        assert_eq!(
            parse_sc_config_value(output, "BINARY_PATH_NAME"),
            Some("C:\\path\\to\\service.exe".to_string())
        );
        assert_eq!(
            parse_sc_config_value(output, "DISPLAY_NAME"),
            Some("My Custom Service".to_string())
        );
    }

    #[test]
    fn service_parse_sc_config_value_missing_key() {
        let output = "SERVICE_NAME: MyService\n";
        assert_eq!(parse_sc_config_value(output, "BINARY_PATH_NAME"), None);
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

    // --- GsettingsConfigurator ---

    #[test]
    fn gsettings_configurator_name() {
        assert_eq!(GsettingsConfigurator.name(), "gsettings");
    }

    #[test]
    fn gsettings_strip_quotes() {
        assert_eq!(strip_gsettings_quotes("'prefer-dark'"), "prefer-dark");
        assert_eq!(strip_gsettings_quotes("'hello world'"), "hello world");
        assert_eq!(strip_gsettings_quotes("true"), "true");
        assert_eq!(strip_gsettings_quotes("42"), "42");
        assert_eq!(strip_gsettings_quotes("''"), "");
        assert_eq!(strip_gsettings_quotes(""), "");
    }

    #[test]
    fn node_yaml_value_with_numeric_bools_conversion() {
        // Strings are bare (no quotes — Command bypasses shell)
        assert_eq!(
            yaml_value_to_string(&serde_yaml::Value::String("dark".into())),
            "dark"
        );
        // Bools use true/false (not 0/1 like yaml_value_with_numeric_bools)
        assert_eq!(yaml_value_to_string(&serde_yaml::Value::Bool(true)), "true");
        assert_eq!(
            yaml_value_to_string(&serde_yaml::Value::Bool(false)),
            "false"
        );
        let n = serde_yaml::Value::Number(serde_yaml::Number::from(42));
        assert_eq!(yaml_value_to_string(&n), "42");
        let f = serde_yaml::Value::Number(serde_yaml::Number::from(1.5));
        assert_eq!(yaml_value_to_string(&f), "1.5");
    }

    #[test]
    fn gsettings_diff_empty_desired() {
        let configurator = GsettingsConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = configurator.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn gsettings_diff_non_mapping() {
        let configurator = GsettingsConfigurator;
        let desired = serde_yaml::Value::String("not a mapping".into());
        let drifts = configurator.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn gsettings_current_state_is_empty_mapping() {
        let configurator = GsettingsConfigurator;
        let state = configurator.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    // --- KdeConfigConfigurator ---

    #[test]
    fn kde_configurator_name() {
        assert_eq!(KdeConfigConfigurator.name(), "kdeConfig");
    }

    #[test]
    fn kde_find_kwrite_cmd() {
        let cmd = kde_write_cmd();
        assert!(cmd == "kwriteconfig6" || cmd == "kwriteconfig5");
    }

    #[test]
    fn kde_find_kread_cmd() {
        let cmd = kde_read_cmd();
        assert!(cmd == "kreadconfig6" || cmd == "kreadconfig5");
    }

    #[test]
    fn kde_diff_empty_desired() {
        let configurator = KdeConfigConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = configurator.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_diff_non_mapping() {
        let configurator = KdeConfigConfigurator;
        let desired = serde_yaml::Value::String("not a mapping".into());
        let drifts = configurator.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_current_state_is_empty_mapping() {
        let configurator = KdeConfigConfigurator;
        let state = configurator.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    // --- XfconfConfigurator ---

    #[test]
    fn xfconf_configurator_name() {
        assert_eq!(XfconfConfigurator.name(), "xfconf");
    }

    #[test]
    fn xfconf_diff_empty_desired() {
        let configurator = XfconfConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = configurator.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn xfconf_diff_non_mapping() {
        let configurator = XfconfConfigurator;
        let desired = serde_yaml::Value::String("not a mapping".into());
        let drifts = configurator.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn xfconf_current_state_is_empty_mapping() {
        let configurator = XfconfConfigurator;
        let state = configurator.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    // --- diff_yaml_mapping edge cases ---

    #[test]
    fn diff_yaml_mapping_empty_desired_produces_no_drifts() {
        let desired = serde_yaml::Mapping::new();
        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| {
            "anything".to_string()
        });
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_yaml_mapping_key_missing_from_actual() {
        // When get_actual returns "" for a key that desired has a value for,
        // it should be reported as drift.
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("new_key".into()),
            serde_yaml::Value::String("desired_value".into()),
        );

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| String::new());
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "new_key");
        assert_eq!(drifts[0].expected, "desired_value");
        assert_eq!(drifts[0].actual, "");
    }

    #[test]
    fn diff_yaml_mapping_null_value_in_desired() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("null_key".into()),
            serde_yaml::Value::Null,
        );

        // yaml_value_to_string formats Null via Debug
        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| String::new());
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "null_key");
        // The Null value is formatted via the Debug fallback
        assert!(!drifts[0].expected.is_empty());
    }

    #[test]
    fn diff_yaml_mapping_non_string_keys_are_skipped() {
        let mut desired = serde_yaml::Mapping::new();
        // Insert a numeric key — should be skipped
        desired.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::String("value".into()),
        );
        // Insert a valid string key
        desired.insert(
            serde_yaml::Value::String("valid".into()),
            serde_yaml::Value::String("expected".into()),
        );

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| {
            "different".to_string()
        });
        // Only the string key should produce drift; numeric key is skipped
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "valid");
    }

    #[test]
    fn diff_yaml_mapping_empty_prefix_no_dot() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("key".into()),
            serde_yaml::Value::String("a".into()),
        );

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| "b".to_string());
        assert_eq!(drifts[0].key, "key"); // No leading dot
    }

    #[test]
    fn diff_yaml_mapping_all_values_match() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("x".into()),
            serde_yaml::Value::String("1".into()),
        );
        desired.insert(
            serde_yaml::Value::String("y".into()),
            serde_yaml::Value::String("2".into()),
        );

        let drifts = diff_yaml_mapping(&desired, "ns", yaml_value_to_string, |k| {
            match k {
                "x" => "1",
                "y" => "2",
                _ => "",
            }
            .to_string()
        });
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_yaml_mapping_bool_value_via_yaml_value_to_string() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("flag".into()),
            serde_yaml::Value::Bool(true),
        );

        // yaml_value_to_string converts bools to "true"/"false"
        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| "false".to_string());
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].expected, "true");
        assert_eq!(drifts[0].actual, "false");
    }

    #[test]
    fn diff_yaml_mapping_bool_value_via_numeric_bools() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("flag".into()),
            serde_yaml::Value::Bool(true),
        );

        // yaml_value_with_numeric_bools converts true→"1", false→"0"
        let drifts = diff_yaml_mapping(&desired, "", yaml_value_with_numeric_bools, |_| {
            "0".to_string()
        });
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].expected, "1");
        assert_eq!(drifts[0].actual, "0");
    }

    // --- diff_nested_mapping edge cases ---

    #[test]
    fn diff_nested_mapping_non_mapping_desired_returns_empty() {
        let desired = serde_yaml::Value::String("not a mapping".into());
        let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_nested_mapping_null_desired_returns_empty() {
        let desired = serde_yaml::Value::Null;
        let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_nested_mapping_empty_outer_mapping() {
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_nested_mapping_inner_not_mapping_is_skipped() {
        // Outer key is valid, but inner value is a string instead of a mapping
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("schema".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let desired = serde_yaml::Value::Mapping(outer);

        let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_nested_mapping_non_string_outer_key_is_skipped() {
        let mut outer = serde_yaml::Mapping::new();
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("key".into()),
            serde_yaml::Value::String("val".into()),
        );
        // Numeric outer key — should be skipped
        outer.insert(
            serde_yaml::Value::Number(99.into()),
            serde_yaml::Value::Mapping(inner),
        );
        let desired = serde_yaml::Value::Mapping(outer);

        let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_nested_mapping_detects_drift_with_prefix() {
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("color-scheme".into()),
            serde_yaml::Value::String("prefer-dark".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("org.gnome.desktop.interface".into()),
            serde_yaml::Value::Mapping(inner),
        );
        let desired = serde_yaml::Value::Mapping(outer);

        let drifts =
            diff_nested_mapping(&desired, |_schema, _key| "prefer-light".to_string()).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "org.gnome.desktop.interface.color-scheme");
        assert_eq!(drifts[0].expected, "prefer-dark");
        assert_eq!(drifts[0].actual, "prefer-light");
    }

    #[test]
    fn diff_nested_mapping_no_drift_when_matching() {
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("key1".into()),
            serde_yaml::Value::String("val1".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("prefix".into()),
            serde_yaml::Value::Mapping(inner),
        );
        let desired = serde_yaml::Value::Mapping(outer);

        let drifts = diff_nested_mapping(&desired, |_prefix, _key| "val1".to_string()).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_nested_mapping_multiple_schemas_and_keys() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
org.gnome.desktop.interface:
  color-scheme: prefer-dark
  font-name: "Cantarell 11"
org.gnome.desktop.wm.preferences:
  button-layout: close,minimize,maximize
"#,
        )
        .unwrap();

        let drifts = diff_nested_mapping(&yaml, |schema, key| {
            // Return matching value for one key, mismatched for the rest
            if schema == "org.gnome.desktop.interface" && key == "font-name" {
                "Cantarell 11".to_string()
            } else {
                "wrong".to_string()
            }
        })
        .unwrap();

        // font-name matches, so only color-scheme and button-layout should drift
        assert_eq!(drifts.len(), 2);
        let keys: Vec<&str> = drifts.iter().map(|d| d.key.as_str()).collect();
        assert!(keys.contains(&"org.gnome.desktop.interface.color-scheme"));
        assert!(keys.contains(&"org.gnome.desktop.wm.preferences.button-layout"));
    }

    #[test]
    fn diff_nested_mapping_passes_outer_key_to_get_actual() {
        // Verify that the get_actual closure receives the correct (prefix, key) arguments
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("prop".into()),
            serde_yaml::Value::String("desired".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("channel".into()),
            serde_yaml::Value::Mapping(inner),
        );
        let desired = serde_yaml::Value::Mapping(outer);

        let drifts = diff_nested_mapping(&desired, |prefix, key| {
            // Echo back the arguments so we can verify they were correct
            format!("{}:{}", prefix, key)
        })
        .unwrap();

        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].actual, "channel:prop");
    }

    // --- Platform availability ---

    #[cfg(target_os = "linux")]
    #[test]
    fn macos_defaults_not_available_on_linux() {
        let md = MacosDefaultsConfigurator;
        assert!(!md.is_available());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn launch_agents_not_available_on_linux() {
        let la = LaunchAgentConfigurator;
        assert!(!la.is_available());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn windows_registry_not_available_on_linux() {
        let wrc = WindowsRegistryConfigurator;
        assert!(!wrc.is_available());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn windows_services_not_available_on_linux() {
        let wsc = WindowsServiceConfigurator;
        assert!(!wsc.is_available());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shell_configurator_available_on_linux() {
        let sc = ShellConfigurator;
        assert!(sc.is_available());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn environment_configurator_available_on_linux() {
        let ec = EnvironmentConfigurator;
        assert!(ec.is_available());
    }

    #[test]
    fn gsettings_availability_depends_on_command() {
        let gc = GsettingsConfigurator;
        // is_available should match whether gsettings is on PATH
        let expected = cfgd_core::command_available("gsettings");
        assert_eq!(gc.is_available(), expected);
    }

    #[test]
    fn kde_availability_depends_on_command() {
        let kc = KdeConfigConfigurator;
        let expected = cfgd_core::command_available("kwriteconfig6")
            || cfgd_core::command_available("kwriteconfig5");
        assert_eq!(kc.is_available(), expected);
    }

    #[test]
    fn xfconf_availability_depends_on_command() {
        let xc = XfconfConfigurator;
        let expected = cfgd_core::command_available("xfconf-query");
        assert_eq!(xc.is_available(), expected);
    }

    // --- parse_reg_line edge cases ---

    #[test]
    fn parse_reg_line_typical_entry() {
        let line = "    MyValue    REG_SZ    hello world";
        let result = parse_reg_line(line);
        assert_eq!(result, Some(("MyValue", "REG_SZ", "hello world")));
    }

    #[test]
    fn parse_reg_line_empty_string() {
        assert_eq!(parse_reg_line(""), None);
    }

    #[test]
    fn parse_reg_line_whitespace_only() {
        assert_eq!(parse_reg_line("   "), None);
    }

    #[test]
    fn parse_reg_line_hkey_header_line() {
        assert_eq!(parse_reg_line("HKEY_CURRENT_USER\\Software\\Test"), None);
    }

    #[test]
    fn parse_reg_line_dword_value() {
        let line = "    Timeout    REG_DWORD    0xff";
        let result = parse_reg_line(line);
        assert_eq!(result, Some(("Timeout", "REG_DWORD", "0xff")));
    }

    #[test]
    fn parse_reg_line_fewer_than_three_parts() {
        // A line without proper 4-space separators
        let line = "just some text";
        assert_eq!(parse_reg_line(line), None);
    }

    // --- yaml_value_to_string edge cases ---

    #[test]
    fn yaml_value_to_string_null() {
        let result = yaml_value_to_string(&serde_yaml::Value::Null);
        // Null goes through the Debug fallback
        assert!(!result.is_empty());
    }

    #[test]
    fn yaml_value_to_string_sequence() {
        let seq = serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::String("a".into()),
            serde_yaml::Value::String("b".into()),
        ]);
        let result = yaml_value_to_string(&seq);
        // Sequences go through the Debug fallback
        assert!(result.contains("a"));
        assert!(result.contains("b"));
    }

    // --- sc_start_type ---

    #[test]
    fn sc_start_type_auto() {
        assert_eq!(sc_start_type("auto"), Some("auto"));
    }

    #[test]
    fn sc_start_type_manual() {
        assert_eq!(sc_start_type("manual"), Some("demand"));
    }

    #[test]
    fn sc_start_type_disabled() {
        assert_eq!(sc_start_type("disabled"), Some("disabled"));
    }

    #[test]
    fn sc_start_type_unrecognized() {
        assert_eq!(sc_start_type("boot"), None);
        assert_eq!(sc_start_type(""), None);
        assert_eq!(sc_start_type("Auto"), None);
    }

    // --- yaml_value_to_defaults_type edge cases ---

    #[test]
    fn yaml_value_to_defaults_type_float() {
        let float_val = serde_yaml::Value::Number(serde_yaml::Number::from(3.14_f64));
        let (t, v) = yaml_value_to_defaults_type(&float_val);
        assert_eq!(t, "float");
        assert!(v.starts_with("3.14"));
    }

    #[test]
    fn yaml_value_to_defaults_type_bool_false() {
        let (t, v) = yaml_value_to_defaults_type(&serde_yaml::Value::Bool(false));
        assert_eq!(t, "bool");
        assert_eq!(v, "false");
    }

    #[test]
    fn yaml_value_to_defaults_type_null_falls_to_string() {
        let (t, _v) = yaml_value_to_defaults_type(&serde_yaml::Value::Null);
        assert_eq!(t, "string");
    }

    #[test]
    fn yaml_value_to_defaults_type_sequence_falls_to_string() {
        let seq = serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("x".into())]);
        let (t, _v) = yaml_value_to_defaults_type(&seq);
        assert_eq!(t, "string");
    }

    // --- yaml_value_with_numeric_bools fallback ---

    #[test]
    fn yaml_value_with_numeric_bools_null_uses_debug() {
        let result = yaml_value_with_numeric_bools(&serde_yaml::Value::Null);
        // Null goes through the Debug fallback
        assert!(!result.is_empty());
    }

    #[test]
    fn yaml_value_with_numeric_bools_sequence_uses_debug() {
        let seq = serde_yaml::Value::Sequence(vec![]);
        let result = yaml_value_with_numeric_bools(&seq);
        assert!(!result.is_empty());
    }

    // --- generate_launch_agent_plist edge cases ---

    #[test]
    fn generate_plist_empty_program_and_no_args() {
        let plist = generate_launch_agent_plist("com.example.empty", "", &[], true);
        assert!(plist.contains("com.example.empty"));
        assert!(plist.contains("<true />"));
        // No ProgramArguments block when both program and args are empty
        assert!(!plist.contains("ProgramArguments"));
    }

    #[test]
    fn generate_plist_xml_escape_in_label() {
        let plist = generate_launch_agent_plist("com.example.<test>&", "/bin/sh", &[], false);
        assert!(plist.contains("com.example.&lt;test&gt;&amp;"));
    }

    #[test]
    fn generate_plist_args_only_no_program() {
        let plist = generate_launch_agent_plist("com.example.test", "", &["--verbose"], false);
        assert!(plist.contains("ProgramArguments"));
        assert!(plist.contains("--verbose"));
        // No <string></string> for empty program
        let program_strings: Vec<&str> = plist
            .lines()
            .filter(|l| l.contains("<string>") && l.contains("</string>"))
            .collect();
        // Only the label string and the arg string should be present
        assert!(
            program_strings.iter().any(|l| l.contains("--verbose")),
            "should contain the arg"
        );
    }

    // --- resolve_default_profile_name edge cases ---

    #[test]
    fn resolve_default_profile_name_profile_without_name() {
        let settings = serde_json::json!({
            "defaultProfile": "{guid-1}",
            "profiles": {
                "list": [
                    {"guid": "{guid-1}"}
                ]
            }
        });
        // Profile entry exists but has no "name" key
        assert_eq!(resolve_default_profile_name(&settings), None);
    }

    #[test]
    fn resolve_default_profile_name_profiles_not_array() {
        let settings = serde_json::json!({
            "defaultProfile": "{guid-1}",
            "profiles": {
                "list": "not-an-array"
            }
        });
        assert_eq!(resolve_default_profile_name(&settings), None);
    }

    // --- find_profile_guid edge cases ---

    #[test]
    fn find_profile_guid_profile_without_guid() {
        let settings = serde_json::json!({
            "profiles": {
                "list": [
                    {"name": "PowerShell"}
                ]
            }
        });
        // Profile has name but no guid
        assert_eq!(find_profile_guid(&settings, "PowerShell"), None);
    }

    #[test]
    fn find_profile_guid_empty_list() {
        let settings = serde_json::json!({
            "profiles": {"list": []}
        });
        assert_eq!(find_profile_guid(&settings, "anything"), None);
    }

    // --- WindowsRegistryConfigurator::parse_reg_value_output edge cases ---

    #[test]
    fn registry_parse_reg_value_expand_sz() {
        let output = "HKEY_CURRENT_USER\\Environment\n\
                      \n\
                          Path    REG_EXPAND_SZ    %SystemRoot%\\system32\n";
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Path"),
            Some(r"%SystemRoot%\system32".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_dword_zero_prefix() {
        // Verify proper hex parsing with leading zeros
        let output = "    Count    REG_DWORD    0x00000010\n";
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Count"),
            Some("16".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_multi_line_picks_correct_name() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          Alpha    REG_SZ    one\n\
                          Beta    REG_SZ    two\n\
                          Gamma    REG_DWORD    0xa\n";
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Beta"),
            Some("two".to_string())
        );
        assert_eq!(
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Gamma"),
            Some("10".to_string())
        );
    }

    // --- EnvironmentConfigurator::parse_reg_query_output edge cases ---

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

    // --- parse_sc_state edge cases ---

    #[test]
    fn parse_sc_state_start_pending() {
        let output = "\tSTATE              : 2  START_PENDING\n";
        assert_eq!(parse_sc_state(output), Some("start-pending".to_string()));
    }

    #[test]
    fn parse_sc_state_stop_pending() {
        let output = "\tSTATE              : 3  STOP_PENDING\n";
        assert_eq!(parse_sc_state(output), Some("stop-pending".to_string()));
    }

    // --- parse_sc_config_value edge cases ---

    #[test]
    fn parse_sc_config_value_empty_input() {
        assert_eq!(parse_sc_config_value("", "ANYTHING"), None);
    }

    #[test]
    fn parse_sc_config_value_value_with_spaces() {
        let output = "\tDISPLAY_NAME       : My Long Service Name\n";
        assert_eq!(
            parse_sc_config_value(output, "DISPLAY_NAME"),
            Some("My Long Service Name".to_string())
        );
    }

    // --- parse_reg_line edge cases ---

    #[test]
    fn parse_reg_line_hkey_local_machine() {
        assert_eq!(parse_reg_line("HKEY_LOCAL_MACHINE\\Software\\Test"), None);
    }

    #[test]
    fn parse_reg_line_with_spaces_in_value() {
        let line = "    MyPath    REG_SZ    C:\\Program Files\\App";
        let result = parse_reg_line(line);
        assert_eq!(result, Some(("MyPath", "REG_SZ", "C:\\Program Files\\App")));
    }

    // --- diff_yaml_mapping with float values ---

    #[test]
    fn diff_yaml_mapping_float_value() {
        let mut desired = serde_yaml::Mapping::new();
        desired.insert(
            serde_yaml::Value::String("opacity".into()),
            serde_yaml::Value::Number(serde_yaml::Number::from(0.85_f64)),
        );

        let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| "1.0".to_string());
        assert_eq!(drifts.len(), 1);
        assert!(drifts[0].expected.starts_with("0.85"));
        assert_eq!(drifts[0].actual, "1.0");
    }

    // --- EnvironmentConfigurator::desired_vars edge cases ---

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

    // --- WindowsServiceConfigurator::parse_services edge cases ---

    #[test]
    fn service_parse_entries_minimal() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: MinimalService
"#,
        )
        .unwrap();
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "MinimalService");
        assert!(entries[0].display_name.is_none());
        assert!(entries[0].binary_path.is_none());
        assert!(entries[0].start_type.is_none());
        assert!(entries[0].state.is_none());
    }

    // --- strip_gsettings_quotes edge cases ---

    #[test]
    fn strip_gsettings_quotes_mismatched_quotes() {
        // Only opening quote, no closing - should return as-is
        assert_eq!(strip_gsettings_quotes("'no-closing"), "'no-closing");
    }

    #[test]
    fn strip_gsettings_quotes_double_quotes_unchanged() {
        // Double quotes are not stripped (gsettings uses single quotes)
        assert_eq!(strip_gsettings_quotes("\"double\""), "\"double\"");
    }

    // --- diff_nested_mapping with mixed matching ---

    #[test]
    fn diff_nested_mapping_multiple_inner_keys_partial_match() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
schema1:
  key-a: val-a
  key-b: val-b
  key-c: val-c
"#,
        )
        .unwrap();

        let drifts = diff_nested_mapping(&yaml, |_schema, key| {
            match key {
                "key-a" => "val-a", // matches
                "key-b" => "wrong", // drift
                "key-c" => "val-c", // matches
                _ => "",
            }
            .to_string()
        })
        .unwrap();

        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "schema1.key-b");
        assert_eq!(drifts[0].expected, "val-b");
        assert_eq!(drifts[0].actual, "wrong");
    }
}

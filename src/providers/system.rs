use std::process::Command;

use crate::errors::Result;
use crate::output::Printer;

use super::{SystemConfigurator, SystemDrift};

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
            .map_err(crate::errors::CfgdError::Io)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            printer.warning(&format!(
                "chsh failed (may require password): {}",
                stderr.trim()
            ));
        }

        Ok(())
    }
}

/// MacosDefaultsConfigurator — reads/writes macOS `defaults` domains.
pub struct MacosDefaultsConfigurator;

impl SystemConfigurator for MacosDefaultsConfigurator {
    fn name(&self) -> &str {
        "macos-defaults"
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
        let mut drifts = Vec::new();

        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(drifts),
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

                let current = read_defaults_value(domain, key_str);
                let desired_str = yaml_value_to_defaults_string(desired_value);

                if current != desired_str {
                    drifts.push(SystemDrift {
                        key: format!("{}.{}", domain, key_str),
                        expected: desired_str,
                        actual: current,
                    });
                }
            }
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
                    .map_err(crate::errors::CfgdError::Io)?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    printer.warning(&format!(
                        "defaults write failed for {}.{}: {}",
                        domain,
                        key_str,
                        stderr.trim()
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
        "systemd-units"
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
            if let Some(unit_file) = unit.get("unit-file").and_then(|v| v.as_str()) {
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
            if let Some(unit_file) = unit.get("unit-file").and_then(|v| v.as_str()) {
                let dest = format!("/etc/systemd/system/{}", name);
                printer.info(&format!("Installing unit file: {} → {}", unit_file, dest));

                if let Err(e) = std::fs::copy(unit_file, &dest) {
                    printer.warning(&format!("Failed to copy unit file: {}", e));
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
                .map_err(crate::errors::CfgdError::Io)?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                printer.warning(&format!(
                    "systemctl {} {} failed: {}",
                    action,
                    name,
                    stderr.trim()
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
        "launch-agents"
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
                .get("run-at-load")
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

            if let Some(parent) = plist_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&plist_path, &plist_content)?;

            // Load the agent
            let _ = Command::new("launchctl")
                .args(["unload", &plist_path.display().to_string()])
                .output();

            let output = Command::new("launchctl")
                .args(["load", &plist_path.display().to_string()])
                .output()
                .map_err(crate::errors::CfgdError::Io)?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                printer.warning(&format!(
                    "launchctl load failed for {}: {}",
                    name,
                    stderr.trim()
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
            program_args.push_str(&format!("        <string>{}</string>\n", program));
        }
        for arg in args {
            program_args.push_str(&format!("        <string>{}</string>\n", arg));
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
        label, program_args, run_at_load_str
    )
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
        assert_eq!(md.name(), "macos-defaults");
    }

    #[test]
    fn systemd_units_name() {
        let su = SystemdUnitConfigurator;
        assert_eq!(su.name(), "systemd-units");
    }

    #[test]
    fn launch_agents_name() {
        let la = LaunchAgentConfigurator;
        assert_eq!(la.name(), "launch-agents");
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
}

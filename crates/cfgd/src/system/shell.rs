use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn shell_configurator_available_on_linux() {
        let sc = ShellConfigurator;
        assert!(sc.is_available());
    }

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

    #[test]
    fn shell_current_state_is_string() {
        let sc = ShellConfigurator;
        let state = sc.current_state().unwrap();
        assert!(state.is_string());
    }

    #[test]
    fn shell_apply_empty_desired_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let sc = ShellConfigurator;
        let yaml = serde_yaml::Value::String(String::new());
        sc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn shell_apply_non_string_desired_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let sc = ShellConfigurator;
        let yaml = serde_yaml::Value::Null;
        sc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn shell_diff_null_desired_returns_empty() {
        let sc = ShellConfigurator;
        let drifts = sc.diff(&serde_yaml::Value::Null).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn shell_diff_number_desired_returns_empty() {
        let sc = ShellConfigurator;
        let drifts = sc.diff(&serde_yaml::Value::Number(42.into())).unwrap();
        assert!(drifts.is_empty());
    }
}

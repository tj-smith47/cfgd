use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::{Printer, Role};

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
                    printer.status_simple(
                        Role::Warn,
                        "Windows Terminal not installed; cannot set default profile",
                    );
                    return Ok(());
                }
            };

            let guid = match find_profile_guid(&settings, desired_shell) {
                Some(g) => g,
                None => {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "No Windows Terminal profile named '{}' found",
                            desired_shell
                        ),
                    );
                    return Ok(());
                }
            };

            printer.status_simple(
                Role::Info,
                format!(
                    "Setting Windows Terminal default profile to '{}' ({})",
                    desired_shell, guid
                ),
            );

            settings["defaultProfile"] = serde_json::Value::String(guid);

            let updated = serde_json::to_string_pretty(&settings).map_err(|e| {
                cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                    message: e.to_string(),
                })
            })?;
            cfgd_core::atomic_write_str(&path, &updated)?;

            Ok(())
        } else {
            printer.status_simple(
                Role::Info,
                format!("Setting default shell to {}", desired_shell),
            );

            let output = Command::new("chsh")
                .arg("-s")
                .arg(desired_shell)
                .output()
                .map_err(cfgd_core::errors::CfgdError::Io)?;

            if !output.status.success() {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "chsh failed (may require password): {}",
                        cfgd_core::stderr_lossy_trimmed(&output)
                    ),
                );
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use cfgd_core::output::Verbosity;
    use cfgd_core::test_helpers::EnvVarGuard;

    use super::*;

    #[test]
    #[serial_test::serial]
    fn windows_terminal_settings_path_returns_none_on_linux() {
        let _la = EnvVarGuard::unset("LOCALAPPDATA");
        let _up = EnvVarGuard::unset("USERPROFILE");
        assert!(windows_terminal_settings_path().is_none());
    }

    #[test]
    #[serial_test::serial]
    fn load_terminal_settings_returns_ok_none_on_linux() {
        let _la = EnvVarGuard::unset("LOCALAPPDATA");
        let result =
            load_terminal_settings().expect("load_terminal_settings should not error on Linux");
        assert!(result.is_none());
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn current_state_returns_shell_env_var() {
        let _g = EnvVarGuard::set("SHELL", "/usr/bin/zsh");
        let sc = ShellConfigurator;
        let state = sc.current_state().expect("current_state should succeed");
        assert_eq!(state.as_str(), Some("/usr/bin/zsh"));
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn diff_desired_matches_shell_returns_empty() {
        let _g = EnvVarGuard::set("SHELL", "/usr/bin/zsh");
        let sc = ShellConfigurator;
        let desired = serde_yaml::Value::String("/usr/bin/zsh".to_string());
        let drifts = sc.diff(&desired).expect("diff should succeed");
        assert!(
            drifts.is_empty(),
            "expected no drift when desired matches SHELL"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn diff_desired_differs_from_shell_returns_drift() {
        let _g = EnvVarGuard::set("SHELL", "/bin/bash");
        let sc = ShellConfigurator;
        let desired = serde_yaml::Value::String("/usr/bin/zsh".to_string());
        let drifts = sc.diff(&desired).expect("diff should succeed");
        assert_eq!(drifts.len(), 1, "expected one drift entry");
        assert_eq!(drifts[0].key, "default-shell");
        assert_eq!(drifts[0].expected, "/usr/bin/zsh");
        assert_eq!(drifts[0].actual, "/bin/bash");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn apply_runs_chsh_on_happy_path() {
        let (_bin_dir, _path_guard) =
            cfgd_core::test_helpers::install_named_path_shim("chsh", 0, "", "");

        let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);
        let sc = ShellConfigurator;
        let desired = serde_yaml::Value::String("/usr/bin/zsh".to_string());
        sc.apply(&desired, &printer)
            .expect("apply should succeed when chsh exits 0");

        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("/usr/bin/zsh"),
            "printer should mention the target shell, got: {captured:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn apply_chsh_failure_returns_ok_with_warning() {
        let (_bin_dir, _path_guard) =
            cfgd_core::test_helpers::install_named_path_shim("chsh", 1, "", "PAM auth failed");

        let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);
        let sc = ShellConfigurator;
        let desired = serde_yaml::Value::String("/usr/bin/zsh".to_string());
        sc.apply(&desired, &printer)
            .expect("apply should return Ok even when chsh fails");

        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("chsh failed"),
            "printer should emit chsh failure warning, got: {captured:?}"
        );
    }

    #[test]
    fn shell_configurator_current_state() {
        let sc = ShellConfigurator;
        let state = sc.current_state().unwrap();
        assert!(state.is_string());
    }

    #[test]
    fn shell_configurator_diff_matching() {
        // Diffing the current state against itself must never report drift on
        // any platform. Deriving `desired` from `current_state()` keeps this
        // OS-agnostic: on Unix it reads $SHELL, on Windows the Windows Terminal
        // default profile — comparing either against itself yields no drift.
        // (Reading $SHELL directly here broke on Windows, where diff() compares
        // against the Windows Terminal profile, not $SHELL.)
        let sc = ShellConfigurator;
        let desired = sc.current_state().unwrap();
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
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let sc = ShellConfigurator;
        let yaml = serde_yaml::Value::String(String::new());
        sc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn shell_apply_non_string_desired_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
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

    // ---------------------------------------------------------------------------
    // windows_terminal_settings_path — drive the candidate-priority logic from
    // Linux by setting LOCALAPPDATA at a tempdir + creating one of the
    // candidate paths. The function is OS-agnostic: it reads the env var and
    // walks a fixed candidate list. We only need to ensure the priority order
    // is honored.
    // ---------------------------------------------------------------------------

    #[test]
    #[serial_test::serial]
    fn windows_terminal_settings_path_returns_store_path_when_present() {
        let appdata = tempfile::tempdir().unwrap();
        let store_dir = appdata
            .path()
            .join("Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState");
        std::fs::create_dir_all(&store_dir).unwrap();
        std::fs::write(store_dir.join("settings.json"), "{}").unwrap();

        let _g = EnvVarGuard::set("LOCALAPPDATA", &appdata.path().display().to_string());
        let p = windows_terminal_settings_path().expect("store path present");
        assert!(
            p.to_string_lossy()
                .contains("Microsoft.WindowsTerminal_8wekyb3d8bbwe"),
            "expected store path, got {p:?}"
        );
        assert!(p.ends_with("settings.json"));
    }

    #[test]
    #[serial_test::serial]
    fn windows_terminal_settings_path_falls_back_to_preview_when_store_missing() {
        let appdata = tempfile::tempdir().unwrap();
        let preview_dir = appdata
            .path()
            .join("Packages/Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe/LocalState");
        std::fs::create_dir_all(&preview_dir).unwrap();
        std::fs::write(preview_dir.join("settings.json"), "{}").unwrap();

        let _g = EnvVarGuard::set("LOCALAPPDATA", &appdata.path().display().to_string());
        let p = windows_terminal_settings_path().expect("preview path present");
        assert!(
            p.to_string_lossy().contains("WindowsTerminalPreview"),
            "expected preview path, got {p:?}"
        );
        assert!(p.ends_with("settings.json"));
    }

    #[test]
    #[serial_test::serial]
    fn windows_terminal_settings_path_falls_back_to_non_store() {
        let appdata = tempfile::tempdir().unwrap();
        let ns_dir = appdata.path().join("Microsoft/Windows Terminal");
        std::fs::create_dir_all(&ns_dir).unwrap();
        std::fs::write(ns_dir.join("settings.json"), "{}").unwrap();

        let _g = EnvVarGuard::set("LOCALAPPDATA", &appdata.path().display().to_string());
        let p = windows_terminal_settings_path().expect("non-store path present");
        let s = p.to_string_lossy();
        assert!(
            s.contains("Microsoft") && s.contains("Windows Terminal"),
            "expected non-store path, got {p:?}"
        );
        assert!(p.ends_with("settings.json"));
    }

    #[test]
    #[serial_test::serial]
    fn windows_terminal_settings_path_prefers_store_over_preview_and_non_store() {
        let appdata = tempfile::tempdir().unwrap();
        // Create all three so the priority order matters.
        for sub in [
            "Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState",
            "Packages/Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe/LocalState",
            "Microsoft/Windows Terminal",
        ] {
            let dir = appdata.path().join(sub);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("settings.json"), "{}").unwrap();
        }

        let _g = EnvVarGuard::set("LOCALAPPDATA", &appdata.path().display().to_string());
        let p = windows_terminal_settings_path().expect("at least one path present");
        // Store wins the priority race.
        assert!(
            p.to_string_lossy()
                .contains("Microsoft.WindowsTerminal_8wekyb3d8bbwe"),
            "store should win priority over preview / non-store, got {p:?}"
        );
        // Specifically NOT the preview path.
        assert!(
            !p.to_string_lossy().contains("Preview"),
            "preview must lose to store, got {p:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn windows_terminal_settings_path_returns_none_when_no_candidates_exist() {
        let appdata = tempfile::tempdir().unwrap();
        let _g = EnvVarGuard::set("LOCALAPPDATA", &appdata.path().display().to_string());
        assert!(windows_terminal_settings_path().is_none());
    }

    // ---------------------------------------------------------------------------
    // load_terminal_settings — drives both the read and parse error paths plus
    // the happy path with a real LOCALAPPDATA-rooted tempdir.
    // ---------------------------------------------------------------------------

    #[test]
    #[serial_test::serial]
    fn load_terminal_settings_returns_parsed_settings_on_valid_json() {
        let appdata = tempfile::tempdir().unwrap();
        let dir = appdata.path().join("Microsoft/Windows Terminal");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"defaultProfile":"{abc}","profiles":{"list":[]}}"#,
        )
        .unwrap();

        let _g = EnvVarGuard::set("LOCALAPPDATA", &appdata.path().display().to_string());
        let (path, settings) = load_terminal_settings()
            .expect("load should succeed")
            .expect("settings should be present");
        assert!(path.to_string_lossy().ends_with("settings.json"));
        assert_eq!(
            settings.get("defaultProfile").and_then(|v| v.as_str()),
            Some("{abc}")
        );
    }

    #[test]
    #[serial_test::serial]
    fn load_terminal_settings_errors_on_invalid_json() {
        let appdata = tempfile::tempdir().unwrap();
        let dir = appdata.path().join("Microsoft/Windows Terminal");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), "not valid json {{{").unwrap();

        let _g = EnvVarGuard::set("LOCALAPPDATA", &appdata.path().display().to_string());
        let err = load_terminal_settings().expect_err("parse should fail");
        // Error must propagate JSON parse failure — the function maps serde_json
        // errors into ConfigError::Invalid.
        let msg = err.to_string();
        assert!(
            !msg.is_empty(),
            "error must have a message describing the parse failure"
        );
    }
}

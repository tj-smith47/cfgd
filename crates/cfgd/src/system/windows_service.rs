use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

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
                    "running" if current_state.as_deref() != Some("running") => {
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
                    "stopped" if current_state.as_deref() != Some("stopped") => {
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

    #[test]
    fn windows_services_not_available_on_linux() {
        let wsc = WindowsServiceConfigurator;
        assert!(!wsc.is_available());
    }

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

    #[test]
    fn service_diff_missing_service_without_binary_path_no_drift() {
        let wsc = WindowsServiceConfigurator;
        // A service entry with only name and state but no binary_path
        // On non-Windows, query_service returns None
        // Without binary_path, no "exists" drift should be produced
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: NonExistentService12345
  state: running
"#,
        )
        .unwrap();
        let drifts = wsc.diff(&yaml).unwrap();
        // On non-Windows, query_service returns None.
        // The code only reports "exists" drift when binary_path is Some,
        // so there should be no drift for this entry.
        assert!(
            !drifts.iter().any(|d| d.key.contains("exists")),
            "should not report exists drift without binary_path"
        );
    }

    #[test]
    fn parse_services_single_full_entry() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: MyService
  displayName: My Service Display
  binaryPath: C:\Program Files\svc.exe
  startType: auto
  state: running
"#,
        )
        .unwrap();
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "MyService");
        assert_eq!(
            entries[0].display_name.as_deref(),
            Some("My Service Display")
        );
        assert_eq!(
            entries[0].binary_path.as_deref(),
            Some("C:\\Program Files\\svc.exe")
        );
        assert_eq!(entries[0].start_type.as_deref(), Some("auto"));
        assert_eq!(entries[0].state.as_deref(), Some("running"));
    }

    #[test]
    fn parse_services_multiple_entries() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: svc1
  binaryPath: /bin/svc1
- name: svc2
  startType: manual
- name: svc3
"#,
        )
        .unwrap();
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "svc1");
        assert!(entries[0].binary_path.is_some());
        assert_eq!(entries[1].name, "svc2");
        assert_eq!(entries[1].start_type.as_deref(), Some("manual"));
        assert_eq!(entries[2].name, "svc3");
        assert!(entries[2].binary_path.is_none());
    }

    #[test]
    fn parse_services_non_sequence_returns_empty() {
        let yaml = serde_yaml::Value::String("not a sequence".into());
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_services_entries_without_name_skipped() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- binaryPath: /bin/noname
- name: valid
- displayName: orphan
"#,
        )
        .unwrap();
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert_eq!(
            entries.len(),
            1,
            "only the entry with 'name' should be included"
        );
        assert_eq!(entries[0].name, "valid");
    }

    #[test]
    fn parse_services_null_input() {
        let yaml = serde_yaml::Value::Null;
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_services_optional_fields_all_none() {
        let yaml: serde_yaml::Value = serde_yaml::from_str("- name: bare\n").unwrap();
        let entries = WindowsServiceConfigurator::parse_services(&yaml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "bare");
        assert!(entries[0].display_name.is_none());
        assert!(entries[0].binary_path.is_none());
        assert!(entries[0].start_type.is_none());
        assert!(entries[0].state.is_none());
    }

    #[test]
    fn parse_sc_state_continue_pending() {
        let output = "\tSTATE              : 5  CONTINUE_PENDING\n";
        assert_eq!(parse_sc_state(output), Some("continue-pending".to_string()));
    }

    #[test]
    fn parse_sc_state_pause_pending() {
        let output = "\tSTATE              : 6  PAUSE_PENDING\n";
        assert_eq!(parse_sc_state(output), Some("pause-pending".to_string()));
    }

    #[test]
    fn parse_sc_state_underscores_become_hyphens() {
        // The function replaces '_' with '-' via .replace('_', "-")
        let output = "\tSTATE              : 2  START_PENDING\n";
        let state = parse_sc_state(output).unwrap();
        assert!(
            !state.contains('_'),
            "underscores should be converted to hyphens"
        );
        assert_eq!(state, "start-pending");
    }

    #[test]
    fn parse_sc_state_full_output_with_extra_lines() {
        let output = "\
SERVICE_NAME: W32Time\n\
\tTYPE               : 30  WIN32\n\
\tSTATE              : 4  RUNNING\n\
\t\t\t\t\t(STOPPABLE, NOT_PAUSABLE, ACCEPTS_SHUTDOWN)\n\
\tWIN32_EXIT_CODE    : 0  (0x0)\n\
\tSERVICE_EXIT_CODE  : 0  (0x0)\n\
\tCHECKPOINT         : 0x0\n\
\tWAIT_HINT          : 0x0\n";
        assert_eq!(parse_sc_state(output), Some("running".to_string()));
    }

    #[test]
    fn parse_sc_start_type_lowercase_input_still_works() {
        // The function uppercases the rest before checking .contains()
        let output = "\tSTART_TYPE         : 2   auto_start\n";
        assert_eq!(parse_sc_start_type(output), Some("auto".to_string()));
    }

    #[test]
    fn parse_sc_start_type_full_qc_output() {
        let output = "\
[SC] QueryServiceConfig SUCCESS\n\
\n\
SERVICE_NAME: Spooler\n\
\tTYPE               : 110  WIN32_OWN_PROCESS  (interactive)\n\
\tSTART_TYPE         : 2   AUTO_START\n\
\tERROR_CONTROL      : 1   NORMAL\n\
\tBINARY_PATH_NAME   : C:\\Windows\\System32\\spoolsv.exe\n\
\tLOAD_ORDER_GROUP   : SpoolerGroup\n\
\tTAG                : 0\n\
\tDISPLAY_NAME       : Print Spooler\n\
\tDEPENDENCIES       : RPCSS\n\
\t                   : http\n\
\tSERVICE_START_NAME : LocalSystem\n";
        assert_eq!(parse_sc_start_type(output), Some("auto".to_string()));
    }

    #[test]
    fn sc_start_type_case_sensitive() {
        // Function is case-sensitive — "Auto" is not "auto"
        assert_eq!(sc_start_type("Auto"), None);
        assert_eq!(sc_start_type("Manual"), None);
        assert_eq!(sc_start_type("Disabled"), None);
    }

    #[test]
    fn parse_sc_state_only_number_no_word() {
        // STATE line with only the number, no state word after it
        let output = "\tSTATE              : 4\n";
        // split_whitespace on "4" yields only one element, nth(1) returns None
        assert_eq!(parse_sc_state(output), None);
    }

    #[test]
    fn parse_sc_state_state_line_with_extra_whitespace() {
        let output = "\t  STATE              :   4   RUNNING   \n";
        assert_eq!(parse_sc_state(output), Some("running".to_string()));
    }

    #[test]
    fn service_apply_empty_sequence_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let wsc = WindowsServiceConfigurator;
        let yaml = serde_yaml::Value::Sequence(Vec::new());
        wsc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn service_apply_non_sequence_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let wsc = WindowsServiceConfigurator;
        let yaml = serde_yaml::Value::String("not a sequence".into());
        wsc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn service_query_returns_none_on_non_windows() {
        assert!(WindowsServiceConfigurator::query_service("AnyService").is_none());
    }

    #[test]
    fn parse_sc_config_value_value_with_colon() {
        let output = "\tBINARY_PATH_NAME   : C:\\path:with:colons\\svc.exe\n";
        assert_eq!(
            parse_sc_config_value(output, "BINARY_PATH_NAME"),
            Some("C:\\path:with:colons\\svc.exe".to_string())
        );
    }
}

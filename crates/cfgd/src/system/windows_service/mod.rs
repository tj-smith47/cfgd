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

/// Build the argument vector for `sc.exe create <name> binPath= <path> ...`.
///
/// Returns `None` when `binary_path` is `None` — sc.exe requires a binPath to
/// create a service, and apply() simply skips the create call in that case.
///
/// Returns `Some((args, unknown_start_type))`:
/// * `args` — the full argument list to pass to `sc.exe`. sc.exe's quirk is
///   that `key= value` pairs must be passed as **two separate arguments** with
///   the trailing `=` glued to the key — pinned in tests.
/// * `unknown_start_type` — `Some(raw_value)` when the user supplied a
///   `startType` that doesn't map to `{auto, manual, disabled}`. apply()
///   surfaces the warning and the helper falls back to `"demand"` so the
///   create still proceeds.
fn sc_create_args(
    name: &str,
    binary_path: Option<&str>,
    display_name: Option<&str>,
    start_type: Option<&str>,
) -> Option<(Vec<String>, Option<String>)> {
    let bp = binary_path?;
    let mut args = vec![
        "create".to_string(),
        name.to_string(),
        "binPath=".to_string(),
        bp.to_string(),
    ];
    if let Some(dn) = display_name {
        args.push("DisplayName=".to_string());
        args.push(dn.to_string());
    }
    let mut unknown = None;
    if let Some(st) = start_type {
        let sc_start = sc_start_type(st).unwrap_or_else(|| {
            unknown = Some(st.to_string());
            "demand"
        });
        args.push("start=".to_string());
        args.push(sc_start.to_string());
    }
    Some((args, unknown))
}

/// Build the argument vector for `sc.exe config <name> ...` to reconfigure an
/// existing service.
///
/// Returns `None` when no field needs updating (apply() skips the call to
/// avoid no-op sc invocations). Returns `Some(args)` when at least one of
/// `binary_path`, `display_name`, or a *recognized* `start_type` needs to be
/// applied. An unknown `start_type` here is **silently dropped** (unlike
/// create) — pinned in tests because the existing apply() pattern preserves
/// rather than warns on reconfigure.
fn sc_config_args(
    name: &str,
    binary_path: Option<&str>,
    display_name: Option<&str>,
    start_type: Option<&str>,
) -> Option<Vec<String>> {
    let mut args = vec!["config".to_string(), name.to_string()];
    let mut needs_config = false;
    if let Some(st) = start_type
        && let Some(sc_start) = sc_start_type(st)
    {
        args.push("start=".to_string());
        args.push(sc_start.to_string());
        needs_config = true;
    }
    if let Some(bp) = binary_path {
        args.push("binPath=".to_string());
        args.push(bp.to_string());
        needs_config = true;
    }
    if let Some(dn) = display_name {
        args.push("DisplayName=".to_string());
        args.push(dn.to_string());
        needs_config = true;
    }
    needs_config.then_some(args)
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
                if let Some((args, unknown_start)) = sc_create_args(
                    &entry.name,
                    entry.binary_path.as_deref(),
                    entry.display_name.as_deref(),
                    entry.start_type.as_deref(),
                ) {
                    if let Some(raw) = unknown_start {
                        printer.warning(&format!(
                            "Unknown start type '{}' for service {}, using 'demand'",
                            raw, entry.name
                        ));
                    }
                    let output = Command::new("sc.exe")
                        .args(&args)
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;
                    if output.status.success() {
                        printer.success(&format!("Created service {}", entry.name));
                        exists = true;
                    } else {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        printer.warning(&format!(
                            "Failed to create service {}: {}",
                            entry.name,
                            stdout.trim()
                        ));
                    }
                }
            } else if let Some(config_args) = sc_config_args(
                &entry.name,
                entry.binary_path.as_deref(),
                entry.display_name.as_deref(),
                entry.start_type.as_deref(),
            ) {
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
mod tests;

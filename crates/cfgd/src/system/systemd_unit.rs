use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::{Printer, Role};

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

/// SystemdUnitConfigurator — manages systemd unit files and enablement.
pub struct SystemdUnitConfigurator;

impl SystemConfigurator for SystemdUnitConfigurator {
    fn name(&self) -> &str {
        "systemdUnits"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("systemctl")
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
                .map(|o| cfgd_core::stdout_lossy_trimmed(&o) == "enabled")
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
                printer.status_simple(
                    Role::Info,
                    format!("Installing unit file: {} → {}", unit_file, dest),
                );

                match std::fs::read(unit_file) {
                    Ok(content) => {
                        if let Err(e) =
                            cfgd_core::atomic_write(std::path::Path::new(&dest), &content)
                        {
                            printer.status_simple(
                                Role::Warn,
                                format!(
                                    "Failed to install unit file: {}",
                                    cfgd_core::output::collapse_to_subject_line(&e)
                                ),
                            );
                        }
                    }
                    Err(e) => {
                        printer.status_simple(
                            Role::Warn,
                            format!(
                                "Failed to read unit file: {}",
                                cfgd_core::output::collapse_to_subject_line(&e)
                            ),
                        );
                    }
                }

                // Reload systemd
                if let Err(e) = Command::new("systemctl").arg("daemon-reload").output() {
                    tracing::warn!("systemctl daemon-reload failed: {e}");
                }
            }

            // Enable/disable
            let action = if desired_enabled { "enable" } else { "disable" };
            printer.status_simple(Role::Info, format!("systemctl {} {}", action, name));

            let output = Command::new("systemctl")
                .args([action, name])
                .output()
                .map_err(cfgd_core::errors::CfgdError::Io)?;

            if !output.status.success() {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "systemctl {} {} failed: {}",
                        action,
                        name,
                        cfgd_core::stderr_lossy_trimmed(&output)
                    ),
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_diff_non_sequence_desired() {
        let su = SystemdUnitConfigurator;
        let desired = serde_yaml::Value::String("not a sequence".into());
        let drifts = su.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn systemd_diff_unit_without_name_skipped() {
        let su = SystemdUnitConfigurator;
        let mut unit = serde_yaml::Mapping::new();
        unit.insert(
            serde_yaml::Value::String("enabled".into()),
            serde_yaml::Value::Bool(true),
        );
        let desired = serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(unit)]);
        let drifts = su.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn systemd_current_state_returns_empty_sequence() {
        let su = SystemdUnitConfigurator;
        let state = su.current_state().unwrap();
        assert!(state.is_sequence());
        assert!(state.as_sequence().unwrap().is_empty());
    }

    #[test]
    fn systemd_diff_detects_missing_unit_file() {
        let su = SystemdUnitConfigurator;
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: cfgd-test-nonexistent.service
  enabled: true
  unitFile: /nonexistent/path/to/unit.service
"#,
        )
        .unwrap();

        let drifts = su.diff(&yaml).unwrap();
        // Should have a drift for the unit file being missing
        // (the enabled check may or may not show depending on systemctl availability)
        let unit_file_drifts: Vec<_> = drifts
            .iter()
            .filter(|d| d.key.contains("unit-file"))
            .collect();
        assert_eq!(unit_file_drifts.len(), 1);
        assert_eq!(unit_file_drifts[0].expected, "present");
        assert_eq!(unit_file_drifts[0].actual, "missing");
    }

    #[test]
    fn systemd_diff_with_unit_file_path_reports_missing_dest() {
        let su = SystemdUnitConfigurator;
        let dir = tempfile::tempdir().unwrap();

        // Create a "source" unit file that exists
        let source_path = dir.path().join("test.service");
        std::fs::write(&source_path, "[Unit]\nDescription=Test\n").unwrap();

        // The diff function checks if /etc/systemd/system/{name} exists.
        // Since cfgd-test-phantom.service won't exist there, we get "missing".
        let yaml_str = format!(
            "- name: cfgd-test-phantom.service\n  enabled: true\n  unitFile: {}\n",
            source_path.display()
        );
        let yaml: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let drifts = su.diff(&yaml).unwrap();
        let unit_file_drifts: Vec<_> = drifts
            .iter()
            .filter(|d| d.key.contains("unit-file"))
            .collect();
        assert_eq!(unit_file_drifts.len(), 1);
        assert_eq!(unit_file_drifts[0].expected, "present");
        assert_eq!(unit_file_drifts[0].actual, "missing");
    }

    #[test]
    fn systemd_diff_default_enabled_is_true() {
        let su = SystemdUnitConfigurator;
        // When "enabled" is omitted, it defaults to true
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: cfgd-test-default-enabled.service
"#,
        )
        .unwrap();

        let drifts = su.diff(&yaml).unwrap();
        // If systemctl is available and the service doesn't exist, we get an "enabled" drift
        // If systemctl is not available, is_enabled returns false -> drift with expected=true
        let enabled_drifts: Vec<_> = drifts
            .iter()
            .filter(|d| d.key.contains("enabled"))
            .collect();
        if !enabled_drifts.is_empty() {
            assert_eq!(enabled_drifts[0].expected, "true");
        }
    }

    #[test]
    fn systemd_apply_empty_sequence_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let su = SystemdUnitConfigurator;
        let yaml = serde_yaml::Value::Sequence(Vec::new());
        su.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn systemd_apply_non_sequence_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let su = SystemdUnitConfigurator;
        let yaml = serde_yaml::Value::String("not a sequence".into());
        su.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn systemd_apply_skips_units_without_name() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let su = SystemdUnitConfigurator;
        let mut unit = serde_yaml::Mapping::new();
        unit.insert(
            serde_yaml::Value::String("enabled".into()),
            serde_yaml::Value::Bool(true),
        );
        let yaml = serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(unit)]);
        su.apply(&yaml, &printer).unwrap();
    }

    #[cfg(target_os = "linux")]
    mod bridge {
        use super::*;
        use cfgd_core::output::test_capture::{assert_snapshot_at, strip_ansi};
        use cfgd_core::output::{Doc, Printer, Role};

        fn snapshot_dir() -> std::path::PathBuf {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/system/snapshots")
        }

        fn assert_snapshot(name: &str, actual: &str) {
            assert_snapshot_at(&snapshot_dir(), name, actual);
        }

        fn normalize_paths(raw: &str, tmpdir: &std::path::Path) -> String {
            raw.replace(&tmpdir.to_string_lossy().to_string(), "<TMPDIR>")
                .replace('\\', "/")
        }

        /// Normalize the variable part of systemctl failure messages so goldens
        /// are stable across D-Bus availability states. Lines matching
        /// `"systemctl <action> <name> failed: <msg>"` have the suffix after
        /// "failed: " replaced with "<SYSTEMCTL_ERROR>".
        fn normalize_systemctl_errors(s: &str) -> String {
            s.lines()
                .map(|line| {
                    if line.contains("systemctl ") && line.contains(" failed: ") {
                        let needle = " failed: ";
                        if let Some(pos) = line.find(needle) {
                            return format!("{}{}<SYSTEMCTL_ERROR>", &line[..pos], needle);
                        }
                    }
                    line.to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
                + if s.ends_with('\n') { "\n" } else { "" }
        }

        #[derive(serde::Serialize)]
        struct UnitApplySummary {
            units_processed: usize,
        }

        #[test]
        fn snapshot_systemd_unit_clean() {
            let yaml: serde_yaml::Value = serde_yaml::from_str(
                r#"
- name: cfgd-snap-test.service
  enabled: false
"#,
            )
            .unwrap();

            let (printer, cap) = Printer::for_test_doc();
            let su = SystemdUnitConfigurator;
            su.apply(&yaml, &printer).unwrap();

            let summary = UnitApplySummary { units_processed: 1 };
            let doc = Doc::new()
                .status(Role::Ok, "systemd units applied")
                .with_data(&summary);
            printer.emit(doc);
            drop(printer);

            let raw = strip_ansi(&cap.human());
            let captured = normalize_systemctl_errors(&raw);

            assert!(
                captured.contains("\n\n"),
                "systemd_unit_clean missing blank line at seam:\n{captured}"
            );
            assert!(
                !captured.contains("\n\n\n"),
                "systemd_unit_clean has duplicate blank line:\n{captured}"
            );

            assert_snapshot("systemd_unit_clean.txt", &captured);
        }

        #[test]
        fn snapshot_systemd_unit_with_warnings() {
            let tmp = tempfile::tempdir().unwrap();
            let nonexistent_unit_file = tmp.path().join("test.service");

            let yaml_str = format!(
                "- name: cfgd-snap-warn.service\n  enabled: true\n  unitFile: {}\n",
                nonexistent_unit_file.display()
            );
            let yaml: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

            let (printer, cap) = Printer::for_test_doc();
            let su = SystemdUnitConfigurator;
            su.apply(&yaml, &printer).unwrap();

            let summary = UnitApplySummary { units_processed: 1 };
            let doc = Doc::new()
                .status(Role::Warn, "systemd units applied with warnings")
                .with_data(&summary);
            printer.emit(doc);
            drop(printer);

            let raw = strip_ansi(&cap.human());
            let path_normalized = normalize_paths(&raw, tmp.path());
            let captured = normalize_systemctl_errors(&path_normalized);

            assert!(
                captured.contains("\n\n"),
                "systemd_unit_with_warnings missing blank line at seam:\n{captured}"
            );
            assert!(
                !captured.contains("\n\n\n"),
                "systemd_unit_with_warnings has duplicate blank line:\n{captured}"
            );

            assert_snapshot("systemd_unit_with_warnings.txt", &captured);
        }
    }
}

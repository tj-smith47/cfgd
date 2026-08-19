use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cfgd_core::errors::Result;
use cfgd_core::output::Role;

use cfgd_core::providers::{SystemConfigurator, SystemContext, SystemDrift};

/// `is-enabled`, `daemon-reload` and `enable`/`disable` are local calls into an
/// already-running manager and answer in milliseconds. The one thing that makes
/// any of them slow is a `systemctl` binary with no manager behind it — a
/// container, WSL, a chroot — where the D-Bus connect alone burns around 90
/// seconds per unit before failing. Bounding them well under that is the
/// difference between `cfgd diff` reporting a unit and appearing to hang on it.
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(15);

/// SystemdUnitConfigurator — manages systemd unit files and enablement.
#[derive(Default)]
pub struct SystemdUnitConfigurator {
    /// Active config directory; relative `unitFile` paths resolve against it
    /// (matching file/secret source resolution). `None` ⇒ process-CWD-relative.
    config_dir: Option<PathBuf>,
}

/// Read every unit file's enablement state in one spawn.
///
/// `systemctl list-unit-files` prints `UNIT-FILE STATE PRESET` per unit, with
/// the same state word `is-enabled` prints, so one call answers for every
/// declared unit. It matters more here than anywhere else in this file: a
/// `systemctl` with no manager behind it burns the ~90s D-Bus connect timeout
/// PER CALL, and this turns a diff of N units from N of those into one.
///
/// Two states the listing cannot answer are deliberately left out of the map so
/// the caller falls back to `is-enabled` for that unit alone: an `alias`, whose
/// enablement is its target's, and any unit the listing does not name at all —
/// a template INSTANCE (`wg-quick@wg0.service`) is enabled as a symlink and
/// only the template (`wg-quick@.service`) is ever listed.
fn snapshot_unit_states() -> HashMap<String, String> {
    let mut cmd = cfgd_core::systemctl_cmd();
    cmd.args(["list-unit-files", "--no-legend", "--no-pager"]);
    let dump = cfgd_core::command_output_with_timeout(&mut cmd, SYSTEMCTL_TIMEOUT)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| cfgd_core::stdout_lossy_trimmed(&o))
        .unwrap_or_default();
    parse_unit_files(&dump)
}

fn parse_unit_files(dump: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in dump.lines() {
        let mut fields = line.split_whitespace();
        let (Some(unit), Some(state)) = (fields.next(), fields.next()) else {
            continue;
        };
        // `--no-legend` drops the header and the trailing count on a systemd
        // that honors it; an older one still prints both, and neither names a
        // unit — a unit file always carries a `.<type>` suffix.
        if !unit.contains('.') || state == "alias" {
            continue;
        }
        map.insert(unit.to_string(), state.to_string());
    }
    map
}

impl SystemdUnitConfigurator {
    /// Resolve a configured `unitFile` to the path cfgd should read it from:
    /// tilde-expanded, then resolved against the config dir when one is set.
    fn resolve_unit_file(&self, unit_file: &str) -> PathBuf {
        let expanded = cfgd_core::expand_tilde(Path::new(unit_file));
        match &self.config_dir {
            Some(dir) => cfgd_core::resolve_relative_path(&expanded, dir).unwrap_or(expanded),
            None => expanded,
        }
    }
}

impl SystemConfigurator for SystemdUnitConfigurator {
    fn name(&self) -> &str {
        "systemdUnits"
    }

    fn is_available(&self) -> bool {
        cfgd_core::systemctl_available()
    }

    fn set_config_dir(&mut self, config_dir: &Path) {
        self.config_dir = Some(config_dir.to_path_buf());
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

        // A profile declaring no units asks systemd nothing at all.
        let states = if units.is_empty() {
            HashMap::new()
        } else {
            snapshot_unit_states()
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

            let is_enabled = match states.get(name) {
                Some(state) => state == "enabled",
                None => {
                    let mut cmd = cfgd_core::systemctl_cmd();
                    cmd.args(["is-enabled", name]);
                    cfgd_core::command_output_with_timeout(&mut cmd, SYSTEMCTL_TIMEOUT)
                        .ok()
                        .map(|o| cfgd_core::stdout_lossy_trimmed(&o) == "enabled")
                        .unwrap_or(false)
                }
            };

            if is_enabled != desired_enabled {
                drifts.push(SystemDrift {
                    key: format!("{}.enabled", name),
                    expected: desired_enabled.to_string(),
                    actual: is_enabled.to_string(),
                });
            }

            if let Some(unit_file) = unit.get("unitFile").and_then(|v| v.as_str()) {
                let source = self.resolve_unit_file(unit_file);
                let dest = format!("/etc/systemd/system/{}", name);
                let dest_path = std::path::Path::new(&dest);
                if !dest_path.exists() {
                    drifts.push(SystemDrift {
                        key: format!("{}.unit-file", name),
                        expected: "present".to_string(),
                        actual: "missing".to_string(),
                    });
                } else if let Ok(source_content) = std::fs::read(&source)
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

    fn apply(&self, desired: &serde_yaml::Value, cx: &SystemContext<'_>) -> Result<()> {
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
                let source = self.resolve_unit_file(unit_file);
                let dest = format!("/etc/systemd/system/{}", name);
                let dest_path = Path::new(&dest);
                cx.report(
                    Role::Info,
                    format!("Installing unit file: {} → {}", source.display(), dest),
                );

                match std::fs::read(&source) {
                    Ok(content) => {
                        if let Err(e) = cfgd_core::atomic_write(dest_path, &content) {
                            cx.report(
                                Role::Warn,
                                format!(
                                    "Failed to install unit file: {}",
                                    cfgd_core::output::collapse_to_subject_line(&e)
                                ),
                            );
                        } else if let Err(e) = cfgd_core::set_file_permissions(dest_path, 0o644) {
                            // systemd unit files are world-readable by convention; the
                            // atomic_write tempfile lands 0600, so widen it explicitly.
                            cx.report(
                                Role::Warn,
                                format!(
                                    "Failed to set unit file mode: {}",
                                    cfgd_core::output::collapse_to_subject_line(&e)
                                ),
                            );
                        }
                    }
                    Err(e) => {
                        cx.report(
                            Role::Warn,
                            format!(
                                "Failed to read unit file: {}",
                                cfgd_core::output::collapse_to_subject_line(&e)
                            ),
                        );
                    }
                }

                // Reload systemd
                let mut reload = cfgd_core::systemctl_cmd();
                reload.arg("daemon-reload");
                if let Err(e) =
                    cfgd_core::command_output_with_timeout(&mut reload, SYSTEMCTL_TIMEOUT)
                {
                    // tracing-ok: daemon-reload is advisory; the enable/disable
                    // result below is what the user is told about
                    tracing::warn!("systemctl daemon-reload failed: {e}");
                }
            }

            // Enable/disable
            let action = if desired_enabled { "enable" } else { "disable" };
            cx.report(Role::Info, format!("systemctl {} {}", action, name));

            let mut cmd = cfgd_core::systemctl_cmd();
            cmd.args([action, name]);
            let output = cfgd_core::command_output_with_timeout(&mut cmd, SYSTEMCTL_TIMEOUT)
                .map_err(cfgd_core::errors::CfgdError::Io)?;

            if !output.status.success() {
                cx.report(
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
    fn resolve_unit_file_resolves_relative_against_config_dir() {
        let mut su = SystemdUnitConfigurator::default();
        // No config dir → relative path is left process-CWD-relative (legacy behavior).
        assert_eq!(
            su.resolve_unit_file("my.service"),
            PathBuf::from("my.service")
        );
        // With a config dir → a relative unitFile resolves against it, not the CWD.
        su.set_config_dir(Path::new("/etc/cfgd"));
        assert_eq!(
            su.resolve_unit_file("my.service"),
            PathBuf::from("/etc/cfgd/my.service")
        );
        // Absolute paths are preserved regardless of config dir.
        assert_eq!(
            su.resolve_unit_file("/abs/x.service"),
            PathBuf::from("/abs/x.service")
        );
    }

    #[test]
    fn systemd_diff_non_sequence_desired() {
        let su = SystemdUnitConfigurator::default();
        let desired = serde_yaml::Value::String("not a sequence".into());
        let drifts = su.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // A non-empty unit list makes `diff` snapshot the manager's listing, so
    // this needs the shim + serial like every other spawning diff test: run
    // plain, its spawn lands in whichever sibling's shim log is live and
    // breaks that test's exactly-one-listing count.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn systemd_diff_unit_without_name_skipped() {
        let _shim = systemctl_shim("");
        let su = SystemdUnitConfigurator::default();
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
        let su = SystemdUnitConfigurator::default();
        let state = su.current_state().unwrap();
        assert!(state.is_sequence());
        assert!(state.as_sequence().unwrap().is_empty());
    }

    /// Point every `systemctl` call at a shim reporting `state` for
    /// `is-enabled`. Without it a diff test asserts about the host's own
    /// systemd — and on a host with the binary but no running manager, each
    /// call burns the D-Bus connect timeout before answering.
    #[cfg(unix)]
    fn systemctl_shim(state: &str) -> cfgd_core::test_helpers::ToolShim {
        cfgd_core::test_helpers::ToolShim::install(cfgd_core::SYSTEMCTL_BIN_ENV, 0, state, "")
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn systemd_diff_detects_missing_unit_file() {
        let _shim = systemctl_shim("enabled\n");
        let su = SystemdUnitConfigurator::default();
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: cfgd-test-nonexistent.service
  enabled: true
  unitFile: /nonexistent/path/to/unit.service
"#,
        )
        .unwrap();

        let drifts = su.diff(&yaml).unwrap();
        // The unit is desired enabled and the shim reports it enabled, so the
        // unit-file drift is the ONLY one — an enabled drift here would mean
        // the is-enabled reading was discarded.
        assert_eq!(drifts.len(), 1, "unexpected drifts: {drifts:?}");
        assert_eq!(drifts[0].key, "cfgd-test-nonexistent.service.unit-file");
        assert_eq!(drifts[0].expected, "present");
        assert_eq!(drifts[0].actual, "missing");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn systemd_diff_with_unit_file_path_reports_missing_dest() {
        let _shim = systemctl_shim("enabled\n");
        let su = SystemdUnitConfigurator::default();
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

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn systemd_diff_default_enabled_is_true() {
        // When "enabled" is omitted it defaults to true, so a unit systemd
        // reports disabled is drift and one it reports enabled is not.
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: cfgd-test-default-enabled.service
"#,
        )
        .unwrap();
        let su = SystemdUnitConfigurator::default();

        {
            let _shim = systemctl_shim("disabled\n");
            let drifts = su.diff(&yaml).unwrap();
            assert_eq!(drifts.len(), 1, "unexpected drifts: {drifts:?}");
            assert_eq!(drifts[0].key, "cfgd-test-default-enabled.service.enabled");
            assert_eq!(
                drifts[0].expected, "true",
                "an omitted `enabled` means true"
            );
            assert_eq!(drifts[0].actual, "false");
        }

        let _shim = systemctl_shim("enabled\n");
        assert!(
            su.diff(&yaml).unwrap().is_empty(),
            "a unit already enabled has not drifted from the default"
        );
    }

    /// Verbatim `systemctl list-unit-files --no-pager` output (systemd 259),
    /// header and trailing count included — the two lines `--no-legend` drops
    /// on a systemd that honours it and an older one still prints. Every state
    /// below was compared against the same host's `systemctl is-enabled` for
    /// the same unit.
    const REAL_UNIT_FILES: &str = "\
UNIT FILE                                                                     STATE           PRESET
cron.service                                                                  enabled         enabled
ssh.service                                                                   disabled        enabled
getty@.service                                                                enabled         enabled
autovt@.service                                                               alias           -
systemd-tmpfiles-clean.timer                                                  static          -

530 unit files listed.
";

    #[test]
    fn listed_states_are_the_states_is_enabled_reports() {
        let states = parse_unit_files(REAL_UNIT_FILES);
        for (unit, expected) in [
            ("cron.service", "enabled"),
            ("ssh.service", "disabled"),
            ("getty@.service", "enabled"),
            ("systemd-tmpfiles-clean.timer", "static"),
        ] {
            assert_eq!(
                states.get(unit).map(String::as_str),
                Some(expected),
                "listing and is-enabled disagree about {unit}"
            );
        }
    }

    #[test]
    fn neither_the_legend_nor_the_count_nor_an_alias_enters_the_map() {
        let states = parse_unit_files(REAL_UNIT_FILES);
        // An alias's enablement is its target's, so it is left to `is-enabled`.
        assert!(!states.contains_key("autovt@.service"));
        // Neither header nor count names a unit; a unit file always carries a
        // `.<type>` suffix.
        assert!(!states.contains_key("UNIT"));
        assert!(!states.contains_key("530"));
        assert_eq!(states.len(), 4);
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn systemd_diff_reads_every_listed_units_enablement_in_one_call() {
        let shim = systemctl_shim(REAL_UNIT_FILES);
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "- name: cron.service\n  enabled: true\n\
             - name: ssh.service\n  enabled: true\n\
             - name: systemd-tmpfiles-clean.timer\n  enabled: false\n",
        )
        .unwrap();

        let drifts = SystemdUnitConfigurator::default().diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("list-unit-files --no-legend"),
            vec!["list-unit-files --no-legend --no-pager"],
            "one listing answers every declared unit — and a diff must not \
             mutate the manager"
        );
        assert_eq!(drifts.len(), 1, "unexpected drifts: {drifts:?}");
        assert_eq!(drifts[0].key, "ssh.service.enabled");
        assert_eq!(drifts[0].expected, "true");
        assert_eq!(drifts[0].actual, "false");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn systemd_diff_falls_back_to_is_enabled_for_a_unit_the_listing_omits() {
        let shim = systemctl_shim(REAL_UNIT_FILES);
        // A template INSTANCE is enabled as a symlink; only the template it is
        // built from is ever listed. An alias is listed but never trusted.
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "- name: cron.service\n  enabled: true\n\
             - name: getty@tty7.service\n  enabled: true\n\
             - name: autovt@.service\n  enabled: true\n",
        )
        .unwrap();

        SystemdUnitConfigurator::default().diff(&yaml).unwrap();

        // `CFGD_SYSTEMCTL_BIN` is process-global and five call sites across
        // three crates spawn through it, so the whole log is not this test's to
        // assert on — a parallel test's `systemctl` lands in it too. Each claim
        // is filtered to the unit it is about.
        assert_eq!(
            shim.argv_lines_naming("list-unit-files"),
            vec!["list-unit-files --no-legend --no-pager"],
            "the listing is read exactly once for the whole block"
        );
        assert_eq!(
            shim.argv_lines_naming("getty@tty7.service"),
            vec!["is-enabled getty@tty7.service"],
            "a template instance the listing omits is asked about once"
        );
        assert_eq!(
            shim.argv_lines_naming("autovt@.service"),
            vec!["is-enabled autovt@.service"],
            "an alias is listed but never trusted, so it is asked about once"
        );
        assert!(
            shim.argv_lines_naming("cron.service").is_empty(),
            "a unit the listing answers is never asked about: {}",
            shim.argv_log()
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn systemd_diff_of_no_units_asks_systemd_nothing() {
        let shim = systemctl_shim(REAL_UNIT_FILES);
        let yaml = serde_yaml::Value::Sequence(Vec::new());
        SystemdUnitConfigurator::default().diff(&yaml).unwrap();
        assert_eq!(
            shim.invocation_count(),
            0,
            "a profile declaring no units spawns nothing: {}",
            shim.argv_log()
        );
    }

    #[test]
    fn systemd_apply_empty_sequence_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let su = SystemdUnitConfigurator::default();
        let yaml = serde_yaml::Value::Sequence(Vec::new());
        su.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn systemd_apply_non_sequence_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let su = SystemdUnitConfigurator::default();
        let yaml = serde_yaml::Value::String("not a sequence".into());
        su.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn systemd_apply_skips_units_without_name() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let su = SystemdUnitConfigurator::default();
        let mut unit = serde_yaml::Mapping::new();
        unit.insert(
            serde_yaml::Value::String("enabled".into()),
            serde_yaml::Value::Bool(true),
        );
        let yaml = serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(unit)]);
        su.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    mod bridge {
        use super::*;
        use crate::system::tests_snapshot_bridge::{
            BridgeApply, assert_single_seam, capture_attached_apply,
        };
        use cfgd_core::output::Role;
        use cfgd_core::output::test_capture::assert_snapshot_at;

        fn snapshot_dir() -> std::path::PathBuf {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/system/snapshots")
        }

        fn assert_snapshot(name: &str, actual: &str) {
            assert_snapshot_at(&snapshot_dir(), name, actual);
        }

        fn normalize_paths(raw: &str, tmpdir: &std::path::Path) -> String {
            cfgd_core::normalize_for_snapshot(raw, &[(tmpdir, "<TMPDIR>")])
        }

        #[derive(serde::Serialize)]
        struct UnitApplySummary {
            units_processed: usize,
        }

        #[test]
        #[serial_test::serial]
        fn snapshot_systemd_unit_clean() {
            // Shimmed, so the golden is what cfgd renders around a systemctl
            // that answered — not what the CI host's own D-Bus happened to say.
            let _shim = super::systemctl_shim("");
            let yaml: serde_yaml::Value = serde_yaml::from_str(
                r#"
- name: cfgd-snap-test.service
  enabled: false
"#,
            )
            .unwrap();

            let su = SystemdUnitConfigurator::default();
            let summary = UnitApplySummary { units_processed: 1 };
            let captured = capture_attached_apply(
                &BridgeApply {
                    configurator: &su,
                    desired: &yaml,
                    key: "cfgd-snap-test.service.enabled",
                    current: "true",
                    target: "false",
                    summary_role: Role::Ok,
                    summary: "systemd units applied",
                },
                &summary,
            );

            assert_single_seam("systemd_unit_clean", &captured);
            assert_snapshot("systemd_unit_clean.txt", &captured);
        }

        #[test]
        #[serial_test::serial]
        fn snapshot_systemd_unit_with_warnings() {
            // The failing half is the unreadable source file; systemctl itself
            // answers, so the golden holds whatever the host runs.
            let _shim = super::systemctl_shim("");
            let tmp = tempfile::tempdir().unwrap();
            let nonexistent_unit_file = tmp.path().join("test.service");

            let yaml_str = format!(
                "- name: cfgd-snap-warn.service\n  enabled: true\n  unitFile: {}\n",
                nonexistent_unit_file.display()
            );
            let yaml: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

            let su = SystemdUnitConfigurator::default();
            let summary = UnitApplySummary { units_processed: 1 };
            let raw = capture_attached_apply(
                &BridgeApply {
                    configurator: &su,
                    desired: &yaml,
                    key: "cfgd-snap-warn.service.unit-file",
                    current: "missing",
                    target: "present",
                    summary_role: Role::Warn,
                    summary: "systemd units applied with warnings",
                },
                &summary,
            );
            let captured = normalize_paths(&raw, tmp.path());

            assert_single_seam("systemd_unit_with_warnings", &captured);
            assert_snapshot("systemd_unit_with_warnings.txt", &captured);
        }
    }
}

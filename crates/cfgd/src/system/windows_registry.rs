use std::collections::HashMap;

use cfgd_core::errors::Result;
use cfgd_core::output::Role;

use cfgd_core::providers::{SystemConfigurator, SystemContext, SystemDrift};

use super::{diff_yaml_mapping, parse_reg_line, yaml_value_with_numeric_bools};

/// One registry key's values, as one `reg query <key>` dump answers for them.
///
/// The dump prints every value under the key with its NAME, its TYPE and its
/// DATA on one line, which is both questions cfgd asks — what is the value, and
/// what type is it — so a key of twenty values is one spawn rather than the
/// forty it used to be (`reg query … /v <name>` once for the value and again
/// for the type, per value). Subkeys appear as full `HKEY_…` paths and are
/// skipped by [`parse_reg_line`], so a subkey can never answer for a value.
#[derive(Default)]
pub(super) struct RegKeySnapshot {
    values: HashMap<String, RegValue>,
}

struct RegValue {
    reg_type: String,
    data: String,
}

impl RegKeySnapshot {
    /// Read one registry key. Empty on non-Windows and for a key that does not
    /// exist — the same "not present" every lookup answered before.
    ///
    /// The seam is the one exception to the platform gate: a host with no
    /// registry has nothing to ask, but a `reg` STANDING IN for one is exactly
    /// how the spawn count is proven off Windows, where the suite runs.
    fn read(key_path: &str) -> Self {
        if !cfg!(windows) && std::env::var(cfgd_core::REG_BIN_ENV).is_err() {
            return Self::default();
        }
        let mut cmd = cfgd_core::reg_cmd();
        cmd.args(["query", key_path]);
        match cfgd_core::command_output_with_timeout(&mut cmd, cfgd_core::COMMAND_TIMEOUT) {
            Ok(output) if output.status.success() => {
                Self::parse(&String::from_utf8_lossy(&output.stdout))
            }
            _ => Self::default(),
        }
    }

    pub(super) fn parse(dump: &str) -> Self {
        let mut values = HashMap::new();
        for line in dump.lines() {
            if let Some((name, reg_type, data)) = parse_reg_line(line) {
                values.entry(name.to_string()).or_insert(RegValue {
                    reg_type: reg_type.to_string(),
                    data: data.to_string(),
                });
            }
        }
        Self { values }
    }

    /// The value's data as cfgd compares it: a `REG_DWORD` in decimal, anything
    /// else verbatim. `None` when the key carries no value of that name.
    pub(super) fn value(&self, value_name: &str) -> Option<String> {
        let entry = self.values.get(value_name)?;
        if entry.reg_type == "REG_DWORD"
            && let Some(hex) = entry.data.strip_prefix("0x")
            && let Ok(n) = u32::from_str_radix(hex, 16)
        {
            return Some(n.to_string());
        }
        Some(entry.data.clone())
    }

    /// The value's registry type, for the write path's `REG_EXPAND_SZ`
    /// preservation.
    pub(super) fn reg_type(&self, value_name: &str) -> Option<&str> {
        self.values.get(value_name).map(|v| v.reg_type.as_str())
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
    /// Write a registry value using `reg add`.
    ///
    /// `snapshot` is the key's pre-write state, read once for the whole key:
    /// the existing type is what decides whether an existing `REG_EXPAND_SZ`
    /// keeps its type. Falls back to `%VAR%` detection and numeric inference
    /// for a value the key does not carry yet.
    fn write_reg_value(
        key_path: &str,
        value_name: &str,
        value: &str,
        snapshot: &RegKeySnapshot,
        cx: &SystemContext<'_>,
    ) -> Result<()> {
        if !cfg!(windows) {
            return Ok(());
        }

        let reg_type = if snapshot.reg_type(value_name) == Some("REG_EXPAND_SZ") {
            "REG_EXPAND_SZ"
        } else if value.contains('%') && value.matches('%').count() >= 2 {
            // Looks like it contains %VAR% patterns
            "REG_EXPAND_SZ"
        } else if value.parse::<u32>().is_ok() {
            "REG_DWORD"
        } else {
            "REG_SZ"
        };

        let output = cfgd_core::reg_cmd()
            .args([
                "add", key_path, "/v", value_name, "/t", reg_type, "/d", value, "/f",
            ])
            .output()
            .map_err(cfgd_core::errors::CfgdError::Io)?;

        if !output.status.success() {
            cx.report(
                Role::Warn,
                format!(
                    "reg add failed for {}\\{}: {}",
                    key_path,
                    value_name,
                    cfgd_core::stderr_lossy_trimmed(&output)
                ),
            );
        }
        Ok(())
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
                // A key declaring no values has nothing to compare, so querying
                // it is a spawn whose answer is discarded.
                Some(m) if !m.is_empty() => m,
                _ => continue,
            };
            // One `reg query <key>` for the whole key, instead of one per value.
            let snapshot = RegKeySnapshot::read(key_path);
            drifts.extend(diff_yaml_mapping(
                values,
                key_path,
                yaml_value_with_numeric_bools,
                |value_name| snapshot.value(value_name).unwrap_or_default(),
            ));
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, cx: &SystemContext<'_>) -> Result<()> {
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
                // Nothing to write, so nothing to read the existing types for.
                Some(m) if !m.is_empty() => m,
                _ => continue,
            };
            // The key's state BEFORE this run's writes — one spawn for the
            // whole key, and the only thing read from it is each value's
            // existing type, which cfgd's own `reg add` is what would change.
            let snapshot = RegKeySnapshot::read(key_path);
            for (name_val, desired_val) in values {
                let name = match name_val.as_str() {
                    Some(n) => n,
                    None => continue,
                };
                let desired_str = yaml_value_with_numeric_bools(desired_val);
                Self::write_reg_value(key_path, name, &desired_str, &snapshot, cx)?;
                cx.report(
                    Role::Ok,
                    format!("Set {}\\{} = {}", key_path, name, desired_str),
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A verbatim `reg query HKCU\Software\cfgd-qp5` dump, captured on
    /// Windows 10.0.26100 (the winserver VM) from a key seeded with one value
    /// of every shape this configurator has to answer for: a plain string, one
    /// carrying spaces, two DWORDs, a `REG_EXPAND_SZ`, an empty string, the
    /// key's `(Default)` value, and a subkey line.
    ///
    /// It is the whole point of the bulk read: every NAME, TYPE and DATA cfgd
    /// asks about is in this one dump, where it used to cost two `reg query
    /// … /v <name>` spawns per value.
    const REG_QUERY_DUMP: &str = "\r\n\
        HKEY_CURRENT_USER\\Software\\cfgd-qp5\r\n\
        \x20   PlainSz    REG_SZ    hello\r\n\
        \x20   SpacedSz    REG_SZ    hello world\r\n\
        \x20   NumDword    REG_DWORD    0x2a\r\n\
        \x20   ZeroDword    REG_DWORD    0x0\r\n\
        \x20   ExpandSz    REG_EXPAND_SZ    %SystemRoot%\\system32\r\n\
        \x20   EmptySz    REG_SZ    \r\n\
        \x20   (Default)    REG_SZ    defaultval\r\n\
        \r\n\
        HKEY_CURRENT_USER\\Software\\cfgd-qp5\\Child\r\n";

    #[test]
    fn snapshot_answers_every_value_and_type_from_one_real_dump() {
        let snapshot = RegKeySnapshot::parse(REG_QUERY_DUMP);

        assert_eq!(snapshot.value("PlainSz").as_deref(), Some("hello"));
        assert_eq!(snapshot.value("SpacedSz").as_deref(), Some("hello world"));
        // A DWORD is compared in decimal, the spelling a declared `42` has.
        assert_eq!(snapshot.value("NumDword").as_deref(), Some("42"));
        assert_eq!(snapshot.value("ZeroDword").as_deref(), Some("0"));
        assert_eq!(
            snapshot.value("ExpandSz").as_deref(),
            Some(r"%SystemRoot%\system32")
        );
        assert_eq!(snapshot.value("(Default)").as_deref(), Some("defaultval"));
        // `reg query` pads an empty value with the separator and nothing else,
        // which the per-value read rendered as the empty string too.
        assert_eq!(snapshot.value("EmptySz"), None);
        assert_eq!(snapshot.value("NoSuchValue"), None);

        // The type half of the dump — the second `reg query` per value this
        // change deletes. It is what keeps an existing REG_EXPAND_SZ from
        // being rewritten as a REG_SZ.
        assert_eq!(snapshot.reg_type("ExpandSz"), Some("REG_EXPAND_SZ"));
        assert_eq!(snapshot.reg_type("NumDword"), Some("REG_DWORD"));
        assert_eq!(snapshot.reg_type("PlainSz"), Some("REG_SZ"));
        assert_eq!(snapshot.reg_type("NoSuchValue"), None);
    }

    #[test]
    fn snapshot_never_answers_a_value_question_with_a_subkey() {
        // The dump's last line names a SUBKEY, not a value. A type lookup once
        // scanned for any line merely CONTAINING the name, so a name appearing
        // in a subkey path (or in another value's data) could answer for it.
        let snapshot = RegKeySnapshot::parse(REG_QUERY_DUMP);
        assert_eq!(snapshot.value("Child"), None);
        assert_eq!(snapshot.reg_type("Child"), None);
    }

    #[test]
    fn snapshot_of_a_missing_key_answers_nothing() {
        // `reg query` on a key that does not exist writes its error to stderr
        // and exits 1, which `RegKeySnapshot::read` turns into an empty
        // snapshot — the same "" every per-value read produced.
        let snapshot = RegKeySnapshot::parse("");
        assert_eq!(snapshot.value("PlainSz"), None);
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn registry_diff_queries_a_key_once_however_many_values_it_declares() {
        // The seam stands in for `reg.exe`, which is what lets the spawn count
        // be proven off Windows. The dump it answers with is the captured one.
        let shim = cfgd_core::test_helpers::ToolShim::install(
            cfgd_core::REG_BIN_ENV,
            0,
            REG_QUERY_DUMP,
            "",
        );
        let mut values = serde_yaml::Mapping::new();
        for (name, desired) in [
            ("PlainSz", "hello"),
            ("SpacedSz", "hello world"),
            ("NumDword", "42"),
            ("ExpandSz", "elsewhere"),
        ] {
            values.insert(
                serde_yaml::Value::String(name.into()),
                serde_yaml::Value::String(desired.into()),
            );
        }
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String(r"HKCU\Software\cfgd-qp5".into()),
            serde_yaml::Value::Mapping(values),
        );

        let drifts = WindowsRegistryConfigurator
            .diff(&serde_yaml::Value::Mapping(outer))
            .unwrap();

        assert_eq!(
            shim.argv_lines_naming("cfgd-qp5"),
            vec![r"query HKCU\Software\cfgd-qp5"],
            "one query answers every value, and its type with it"
        );
        assert_eq!(drifts.len(), 1, "unexpected drifts: {drifts:?}");
        assert_eq!(drifts[0].key, r"HKCU\Software\cfgd-qp5.ExpandSz");
        assert_eq!(drifts[0].actual, r"%SystemRoot%\system32");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn registry_apply_queries_each_key_once_before_writing_it() {
        let shim = cfgd_core::test_helpers::ToolShim::install(
            cfgd_core::REG_BIN_ENV,
            0,
            REG_QUERY_DUMP,
            "",
        );
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let mut values = serde_yaml::Mapping::new();
        values.insert(
            serde_yaml::Value::String("PlainSz".into()),
            serde_yaml::Value::String("hello".into()),
        );
        values.insert(
            serde_yaml::Value::String("ExpandSz".into()),
            serde_yaml::Value::String("elsewhere".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String(r"HKCU\Software\cfgd-qp5".into()),
            serde_yaml::Value::Mapping(values),
        );

        WindowsRegistryConfigurator
            .apply(
                &serde_yaml::Value::Mapping(outer),
                &cfgd_core::providers::SystemContext::new(&printer),
            )
            .unwrap();

        // The writes themselves are `cfg(windows)`-gated, so off Windows the
        // query is the only spawn — and it is the one the per-value type read
        // used to repeat.
        assert_eq!(
            shim.argv_lines_naming("cfgd-qp5"),
            vec![r"query HKCU\Software\cfgd-qp5"],
            "the pre-write state is read once for the whole key"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn registry_diff_of_a_key_declaring_no_values_queries_nothing() {
        let shim = cfgd_core::test_helpers::ToolShim::install(
            cfgd_core::REG_BIN_ENV,
            0,
            REG_QUERY_DUMP,
            "",
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String(r"HKCU\Software\cfgd-qp5-empty".into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );

        let drifts = WindowsRegistryConfigurator
            .diff(&serde_yaml::Value::Mapping(outer))
            .unwrap();

        assert!(drifts.is_empty());
        assert!(
            shim.argv_lines_naming("cfgd-qp5-empty").is_empty(),
            "a key declaring no values is never queried: {}",
            shim.argv_log()
        );
    }

    #[test]
    fn registry_parse_reg_value_dword() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          HideFileExt    REG_DWORD    0x0\n";
        assert_eq!(
            RegKeySnapshot::parse(output).value("HideFileExt"),
            Some("0".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_dword_nonzero() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          ShowHidden    REG_DWORD    0x1\n";
        assert_eq!(
            RegKeySnapshot::parse(output).value("ShowHidden"),
            Some("1".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_dword_large() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          Timeout    REG_DWORD    0xff\n";
        assert_eq!(
            RegKeySnapshot::parse(output).value("Timeout"),
            Some("255".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_string() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          Theme    REG_SZ    dark\n";
        assert_eq!(
            RegKeySnapshot::parse(output).value("Theme"),
            Some("dark".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_missing() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\n";
        assert_eq!(RegKeySnapshot::parse(output).value("Missing"), None);
    }

    #[test]
    fn registry_parse_reg_value_wrong_name() {
        let output = "HKEY_CURRENT_USER\\Software\\Test\n\
                      \n\
                          OtherValue    REG_SZ    hello\n";
        assert_eq!(RegKeySnapshot::parse(output).value("Missing"), None);
    }

    #[test]
    fn registry_parse_reg_value_empty_input() {
        assert_eq!(RegKeySnapshot::parse("").value("Anything"), None);
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

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn windows_registry_not_available_on_linux() {
        let wrc = WindowsRegistryConfigurator;
        assert!(!wrc.is_available());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_registry_is_available_on_windows() {
        let wrc = WindowsRegistryConfigurator;
        assert!(wrc.is_available());
    }

    #[test]
    fn registry_parse_reg_value_expand_sz() {
        let output = "HKEY_CURRENT_USER\\Environment\n\
                      \n\
                          Path    REG_EXPAND_SZ    %SystemRoot%\\system32\n";
        assert_eq!(
            RegKeySnapshot::parse(output).value("Path"),
            Some(r"%SystemRoot%\system32".to_string())
        );
    }

    #[test]
    fn registry_parse_reg_value_dword_zero_prefix() {
        // Verify proper hex parsing with leading zeros
        let output = "    Count    REG_DWORD    0x00000010\n";
        assert_eq!(
            RegKeySnapshot::parse(output).value("Count"),
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
            RegKeySnapshot::parse(output).value("Beta"),
            Some("two".to_string())
        );
        assert_eq!(
            RegKeySnapshot::parse(output).value("Gamma"),
            Some("10".to_string())
        );
    }

    #[test]
    fn windows_registry_current_state_is_empty_mapping() {
        let wrc = WindowsRegistryConfigurator;
        let state = wrc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    #[test]
    fn registry_diff_with_inner_non_mapping_values_skipped() {
        let wrc = WindowsRegistryConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String(r"HKCU\Software\Test".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let desired = serde_yaml::Value::Mapping(outer);
        let drifts = wrc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn registry_diff_non_string_key_path_skipped() {
        let wrc = WindowsRegistryConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("value".into()),
            serde_yaml::Value::String("data".into()),
        );
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(inner),
        );
        let desired = serde_yaml::Value::Mapping(outer);
        let drifts = wrc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn parse_reg_value_output_dword_max_value() {
        let output = "    MaxVal    REG_DWORD    0xffffffff\n";
        assert_eq!(
            RegKeySnapshot::parse(output).value("MaxVal"),
            Some("4294967295".to_string()),
        );
    }

    #[test]
    fn registry_diff_iterates_mapping_and_reports_drift_via_helper() {
        // Drives the diff path that reads each value via `read_reg_value`;
        // on non-Windows the lookup returns "" so every desired entry counts
        // as drift (actual != expected).
        let wrc = WindowsRegistryConfigurator;
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("HideFileExt".into()),
            serde_yaml::Value::Number(0.into()),
        );
        inner.insert(
            serde_yaml::Value::String("Theme".into()),
            serde_yaml::Value::String("dark".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String(
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer".into(),
            ),
            serde_yaml::Value::Mapping(inner),
        );
        let desired = serde_yaml::Value::Mapping(outer);
        let drifts = wrc.diff(&desired).unwrap();
        // Expect drift entries for both child values (actual is empty on non-Windows).
        assert_eq!(drifts.len(), 2);
        assert!(drifts.iter().all(|d| d.actual.is_empty()));
        // The registry path key never opens with "windowsRegistry." — this
        // configurator names the key path itself, not its own name — so this
        // must hold on every OS, not just the ones that can actually read the
        // registry.
        crate::system::assert_keys_undoubled(&wrc, &drifts);
    }

    #[test]
    fn registry_diff_multiple_keypaths_iterate_all_branches() {
        let wrc = WindowsRegistryConfigurator;
        let mut inner_a = serde_yaml::Mapping::new();
        inner_a.insert(
            serde_yaml::Value::String("A".into()),
            serde_yaml::Value::Number(1.into()),
        );
        let mut inner_b = serde_yaml::Mapping::new();
        inner_b.insert(
            serde_yaml::Value::String("B".into()),
            serde_yaml::Value::String("v".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String(r"HKCU\PathA".into()),
            serde_yaml::Value::Mapping(inner_a),
        );
        outer.insert(
            serde_yaml::Value::String(r"HKCU\PathB".into()),
            serde_yaml::Value::Mapping(inner_b),
        );
        let drifts = wrc.diff(&serde_yaml::Value::Mapping(outer)).unwrap();
        assert_eq!(drifts.len(), 2);
    }

    #[test]
    fn registry_apply_no_mapping_value_is_noop() {
        let wrc = WindowsRegistryConfigurator;
        let (printer, _buf) = cfgd_core::output::Printer::for_test();
        let result = wrc.apply(
            &serde_yaml::Value::String("not a mapping".into()),
            &cfgd_core::providers::SystemContext::new(&printer),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn registry_apply_iterates_mapping_writes_each_value() {
        let wrc = WindowsRegistryConfigurator;
        let (printer, _buf) = cfgd_core::output::Printer::for_test();
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("Alpha".into()),
            serde_yaml::Value::Number(1.into()),
        );
        inner.insert(
            serde_yaml::Value::String("Beta".into()),
            serde_yaml::Value::String("text".into()),
        );
        // Non-string subkey is skipped — exercises the inner-name None branch.
        inner.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::String("skipped".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String(r"HKCU\Software\Test".into()),
            serde_yaml::Value::Mapping(inner),
        );
        // Non-mapping inner value is skipped — exercises that None arm.
        outer.insert(
            serde_yaml::Value::String(r"HKCU\Bad".into()),
            serde_yaml::Value::String("not a map".into()),
        );
        // Non-string outer key is skipped.
        let mut inner_skip = serde_yaml::Mapping::new();
        inner_skip.insert(
            serde_yaml::Value::String("X".into()),
            serde_yaml::Value::String("y".into()),
        );
        outer.insert(
            serde_yaml::Value::Number(7.into()),
            serde_yaml::Value::Mapping(inner_skip),
        );
        let result = wrc.apply(
            &serde_yaml::Value::Mapping(outer),
            &cfgd_core::providers::SystemContext::new(&printer),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn registry_name_returns_windows_registry() {
        let wrc = WindowsRegistryConfigurator;
        assert_eq!(wrc.name(), "windowsRegistry");
    }

    #[test]
    fn parse_reg_value_output_sz_with_spaces() {
        let output = "    Description    REG_SZ    A long description with spaces\n";
        assert_eq!(
            RegKeySnapshot::parse(output).value("Description"),
            Some("A long description with spaces".to_string()),
        );
    }

    #[test]
    fn parse_reg_value_output_selects_first_match() {
        // If the same name appears twice, the first one wins
        let output = "\
    Dup    REG_SZ    first\n\
    Dup    REG_SZ    second\n";
        assert_eq!(
            RegKeySnapshot::parse(output).value("Dup"),
            Some("first".to_string()),
        );
    }

    #[test]
    fn registry_parse_reg_value_dword_invalid_hex_returns_raw() {
        // If the hex string after 0x is not valid, from_str_radix fails,
        // so it falls through to return the raw value
        let output = "    BadHex    REG_DWORD    0xZZZZ\n";
        let result = RegKeySnapshot::parse(output).value("BadHex");
        // The DWORD hex parse fails, so the raw value "0xZZZZ" is returned
        assert_eq!(result, Some("0xZZZZ".to_string()));
    }

    #[test]
    fn registry_parse_reg_value_dword_no_0x_prefix() {
        // DWORD without 0x prefix — strip_prefix returns None, falls to raw return
        let output = "    PlainDword    REG_DWORD    42\n";
        let result = RegKeySnapshot::parse(output).value("PlainDword");
        assert_eq!(result, Some("42".to_string()));
    }

    #[test]
    fn registry_apply_empty_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let wrc = WindowsRegistryConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        wrc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn registry_apply_non_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let wrc = WindowsRegistryConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        wrc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn registry_apply_skips_non_string_key_path() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let wrc = WindowsRegistryConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        wrc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn registry_apply_skips_inner_non_mapping() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let wrc = WindowsRegistryConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String(r"HKCU\Software".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        wrc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn registry_apply_skips_non_string_value_name() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let wrc = WindowsRegistryConfigurator;
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::Number(99.into()),
            serde_yaml::Value::String("data".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String(r"HKCU\Software".into()),
            serde_yaml::Value::Mapping(inner),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        wrc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn registry_write_reg_value_noop_on_non_windows() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        // On non-Windows, write_reg_value returns Ok(()) immediately
        WindowsRegistryConfigurator::write_reg_value(
            r"HKCU\Test",
            "TestValue",
            "42",
            &RegKeySnapshot::default(),
            &cfgd_core::providers::SystemContext::new(&printer),
        )
        .unwrap();
    }

    #[test]
    fn registry_snapshot_read_is_empty_on_non_windows() {
        // Off Windows there is no registry to read, and an empty snapshot is
        // what every lookup answered "not present" from before.
        let snapshot = RegKeySnapshot::read(r"HKCU\Test");
        assert_eq!(snapshot.value("Key"), None);
        assert_eq!(snapshot.reg_type("Key"), None);
    }

    // Cross-platform on purpose: `apply()`'s narration is unconditional and
    // `write_reg_value` no-ops before it ever reaches `Command::new("reg")`
    // off Windows (see the guard at the top of `write_reg_value`), so this is
    // the one bridge fixture in this crate that produces the SAME golden
    // whichever OS runs it — including Windows, where `reg add` also runs for
    // real against a key this fixture doesn't need to exist first.
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

        #[derive(serde::Serialize)]
        struct RegistryApplySummary {
            keys_processed: usize,
        }

        #[test]
        fn snapshot_windows_registry_clean() {
            let mut inner = serde_yaml::Mapping::new();
            inner.insert(
                serde_yaml::Value::String("HideFileExt".into()),
                serde_yaml::Value::Number(0.into()),
            );
            let mut outer = serde_yaml::Mapping::new();
            outer.insert(
                serde_yaml::Value::String(
                    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced".into(),
                ),
                serde_yaml::Value::Mapping(inner),
            );
            let desired = serde_yaml::Value::Mapping(outer);

            let wrc = WindowsRegistryConfigurator;
            let summary = RegistryApplySummary { keys_processed: 1 };
            let captured = capture_attached_apply(
                &BridgeApply {
                    configurator: &wrc,
                    desired: &desired,
                    key: r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced.HideFileExt",
                    current: "1",
                    target: "0",
                    summary_role: Role::Ok,
                    summary: "Windows registry applied",
                },
                &summary,
            );

            assert_single_seam("windows_registry_clean", &captured);
            assert_snapshot("windows_registry_clean.txt", &captured);
        }
    }
}

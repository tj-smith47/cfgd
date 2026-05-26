use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::{Printer, Role};

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::{diff_yaml_mapping, parse_reg_line, yaml_value_with_numeric_bools};

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
            printer.status_simple(
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
                printer.status_simple(
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
            WindowsRegistryConfigurator::parse_reg_value_output(output, "MaxVal"),
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
        let result = wrc.apply(&serde_yaml::Value::String("not a mapping".into()), &printer);
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
        let result = wrc.apply(&serde_yaml::Value::Mapping(outer), &printer);
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
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Description"),
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
            WindowsRegistryConfigurator::parse_reg_value_output(output, "Dup"),
            Some("first".to_string()),
        );
    }

    #[test]
    fn registry_parse_reg_value_dword_invalid_hex_returns_raw() {
        // If the hex string after 0x is not valid, from_str_radix fails,
        // so it falls through to return the raw value
        let output = "    BadHex    REG_DWORD    0xZZZZ\n";
        let result = WindowsRegistryConfigurator::parse_reg_value_output(output, "BadHex");
        // The DWORD hex parse fails, so the raw value "0xZZZZ" is returned
        assert_eq!(result, Some("0xZZZZ".to_string()));
    }

    #[test]
    fn registry_parse_reg_value_dword_no_0x_prefix() {
        // DWORD without 0x prefix — strip_prefix returns None, falls to raw return
        let output = "    PlainDword    REG_DWORD    42\n";
        let result = WindowsRegistryConfigurator::parse_reg_value_output(output, "PlainDword");
        assert_eq!(result, Some("42".to_string()));
    }

    #[test]
    fn registry_apply_empty_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let wrc = WindowsRegistryConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        wrc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn registry_apply_non_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let wrc = WindowsRegistryConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        wrc.apply(&yaml, &printer).unwrap();
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
        wrc.apply(&yaml, &printer).unwrap();
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
        wrc.apply(&yaml, &printer).unwrap();
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
        wrc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn registry_write_reg_value_noop_on_non_windows() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        // On non-Windows, write_reg_value returns Ok(()) immediately
        WindowsRegistryConfigurator::write_reg_value(r"HKCU\Test", "TestValue", "42", &printer)
            .unwrap();
    }

    #[test]
    fn registry_read_reg_value_returns_none_on_non_windows() {
        assert_eq!(
            WindowsRegistryConfigurator::read_reg_value(r"HKCU\Test", "Key"),
            None
        );
    }

    #[test]
    fn registry_read_reg_type_returns_none_on_non_windows() {
        assert_eq!(
            WindowsRegistryConfigurator::read_reg_type(r"HKCU\Test", "Key"),
            None
        );
    }
}

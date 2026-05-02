use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::read_command_output;

use super::{diff_nested_mapping, yaml_value_to_string};

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn gsettings_availability_depends_on_command() {
        let gc = GsettingsConfigurator;
        // is_available should match whether gsettings is on PATH
        let expected = cfgd_core::command_available("gsettings");
        assert_eq!(gc.is_available(), expected);
    }

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

    #[test]
    fn strip_gsettings_quotes_only_closing_quote() {
        // No opening quote — returned as-is
        assert_eq!(strip_gsettings_quotes("no-opening'"), "no-opening'");
    }

    #[test]
    fn strip_gsettings_quotes_nested_quotes() {
        // Outer single quotes stripped, inner content preserved
        assert_eq!(strip_gsettings_quotes("'it''s'"), "it''s");
    }

    #[test]
    fn strip_gsettings_quotes_single_char_value() {
        assert_eq!(strip_gsettings_quotes("'x'"), "x");
    }

    #[test]
    fn strip_gsettings_quotes_number_string_unchanged() {
        assert_eq!(strip_gsettings_quotes("100"), "100");
    }

    #[test]
    fn gsettings_apply_empty_mapping_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let gc = GsettingsConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        gc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn gsettings_apply_non_mapping_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let gc = GsettingsConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        gc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn gsettings_apply_skips_non_string_schema_key() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let gc = GsettingsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        gc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn gsettings_apply_skips_inner_non_mapping() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let gc = GsettingsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("org.gnome.test".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        gc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn gsettings_apply_skips_non_string_key() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let gc = GsettingsConfigurator;
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::Number(1.into()),
            serde_yaml::Value::String("value".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("org.gnome.test".into()),
            serde_yaml::Value::Mapping(inner),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        gc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn gsettings_current_state_returns_empty_mapping() {
        let gc = GsettingsConfigurator;
        let state = gc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }
}

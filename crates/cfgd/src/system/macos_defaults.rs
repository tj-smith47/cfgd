use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::{diff_yaml_mapping, read_command_output, yaml_value_with_numeric_bools};

/// MacosDefaultsConfigurator — reads/writes macOS `defaults` domains.
pub struct MacosDefaultsConfigurator;

impl SystemConfigurator for MacosDefaultsConfigurator {
    fn name(&self) -> &str {
        "macosDefaults"
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "macos")
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
        for (domain_key, domain_values) in mapping {
            let domain = match domain_key.as_str() {
                Some(d) => d,
                None => continue,
            };
            let values = match domain_values.as_mapping() {
                Some(m) => m,
                None => continue,
            };
            drifts.extend(diff_yaml_mapping(
                values,
                domain,
                yaml_value_with_numeric_bools,
                |key_str| read_defaults_value(domain, key_str),
            ));
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
                    .map_err(cfgd_core::errors::CfgdError::Io)?;

                if !output.status.success() {
                    printer.warning(&format!(
                        "defaults write failed for {}.{}: {}",
                        domain,
                        key_str,
                        cfgd_core::stderr_lossy_trimmed(&output)
                    ));
                }
            }
        }

        Ok(())
    }
}

fn read_defaults_value(domain: &str, key: &str) -> String {
    read_command_output(Command::new("defaults").args(["read", domain, key]))
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn macos_defaults_not_available_on_linux() {
        let md = MacosDefaultsConfigurator;
        assert!(!md.is_available());
    }

    #[test]
    fn yaml_value_to_defaults_type_float() {
        let float_val = serde_yaml::Value::Number(serde_yaml::Number::from(1.234_f64));
        let (t, v) = yaml_value_to_defaults_type(&float_val);
        assert_eq!(t, "float");
        assert!(v.starts_with("1.234"));
    }

    #[test]
    fn yaml_value_to_defaults_type_bool_false() {
        let (t, v) = yaml_value_to_defaults_type(&serde_yaml::Value::Bool(false));
        assert_eq!(t, "bool");
        assert_eq!(v, "false");
    }

    #[test]
    fn yaml_value_to_defaults_type_null_falls_to_string() {
        let (t, _v) = yaml_value_to_defaults_type(&serde_yaml::Value::Null);
        assert_eq!(t, "string");
    }

    #[test]
    fn yaml_value_to_defaults_type_sequence_falls_to_string() {
        let seq = serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("x".into())]);
        let (t, _v) = yaml_value_to_defaults_type(&seq);
        assert_eq!(t, "string");
    }

    #[test]
    fn macos_defaults_diff_empty_mapping() {
        let md = MacosDefaultsConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = md.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn macos_defaults_diff_non_mapping() {
        let md = MacosDefaultsConfigurator;
        let desired = serde_yaml::Value::String("not a mapping".into());
        let drifts = md.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn macos_defaults_diff_inner_non_mapping_is_skipped() {
        let md = MacosDefaultsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("com.apple.dock".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let desired = serde_yaml::Value::Mapping(outer);
        let drifts = md.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn macos_defaults_diff_non_string_domain_key_skipped() {
        let md = MacosDefaultsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("key".into()),
            serde_yaml::Value::String("val".into()),
        );
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(inner),
        );
        let desired = serde_yaml::Value::Mapping(outer);
        let drifts = md.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn yaml_value_to_defaults_type_mapping_fallback() {
        let mapping = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let (t, _v) = yaml_value_to_defaults_type(&mapping);
        assert_eq!(t, "string");
    }

    #[test]
    fn macos_defaults_current_state_is_empty_mapping() {
        let md = MacosDefaultsConfigurator;
        let state = md.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    #[test]
    fn yaml_value_to_defaults_type_integer_zero() {
        let (t, v) = yaml_value_to_defaults_type(&serde_yaml::Value::Number(0.into()));
        assert_eq!(t, "int");
        assert_eq!(v, "0");
    }

    #[test]
    fn yaml_value_to_defaults_type_negative_int() {
        let (t, v) =
            yaml_value_to_defaults_type(&serde_yaml::Value::Number(serde_yaml::Number::from(-1)));
        assert_eq!(t, "int");
        assert_eq!(v, "-1");
    }

    #[test]
    fn yaml_value_to_defaults_type_empty_string() {
        let (t, v) = yaml_value_to_defaults_type(&serde_yaml::Value::String(String::new()));
        assert_eq!(t, "string");
        assert_eq!(v, "");
    }

    #[test]
    fn yaml_value_to_defaults_type_float_zero() {
        let float_val = serde_yaml::Value::Number(serde_yaml::Number::from(0.0_f64));
        let (t, v) = yaml_value_to_defaults_type(&float_val);
        assert_eq!(t, "float");
        assert_eq!(v, "0.0");
    }

    #[test]
    fn macos_defaults_apply_empty_mapping_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let md = MacosDefaultsConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        md.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn macos_defaults_apply_non_mapping_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let md = MacosDefaultsConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        md.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn macos_defaults_apply_skips_non_string_domain_key() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let md = MacosDefaultsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        md.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn macos_defaults_apply_skips_inner_non_mapping() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let md = MacosDefaultsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("com.apple.dock".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        md.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn macos_defaults_apply_skips_non_string_key_inside_domain() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let md = MacosDefaultsConfigurator;
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::Number(99.into()),
            serde_yaml::Value::String("value".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("com.apple.dock".into()),
            serde_yaml::Value::Mapping(inner),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        // On non-macOS, defaults command won't exist, but the function still
        // iterates and skips the numeric key before reaching the command.
        // This tests the `None => continue` at line 389-391.
        md.apply(&yaml, &printer).unwrap();
    }
}

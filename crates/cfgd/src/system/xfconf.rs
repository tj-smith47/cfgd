use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::{Printer, Role};

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::read_command_output;

use super::{diff_nested_mapping, yaml_value_to_string};

/// XfconfConfigurator — reads/writes XFCE desktop settings via `xfconf-query`.
///
/// Config format (two-level: channel → property → value):
/// ```yaml
/// system:
///   xfconf:
///     xfwm4:
///       /general/theme: Default
///       /general/title_font: "Sans Bold 9"
///     xsettings:
///       /Net/ThemeName: Adwaita
///       /Net/IconThemeName: elementary-xfce-dark
/// ```
pub struct XfconfConfigurator;

fn read_xfconf_value(channel: &str, property: &str) -> String {
    read_command_output(Command::new("xfconf-query").args(["-c", channel, "-p", property]))
}

impl SystemConfigurator for XfconfConfigurator {
    fn name(&self) -> &str {
        "xfconf"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("xfconf-query")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        diff_nested_mapping(desired, read_xfconf_value)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        for (channel_key, channel_values) in mapping {
            let channel = match channel_key.as_str() {
                Some(c) => c,
                None => continue,
            };
            let values = match channel_values.as_mapping() {
                Some(m) => m,
                None => continue,
            };

            for (key, desired_value) in values {
                let property = match key.as_str() {
                    Some(p) => p,
                    None => continue,
                };

                let val_str = yaml_value_to_string(desired_value);

                printer.status_simple(
                    Role::Info,
                    format!("xfconf-query -c {} -p {} -s {}", channel, property, val_str),
                );

                let output = Command::new("xfconf-query")
                    .args(["-c", channel, "-p", property, "-s", &val_str])
                    .output()
                    .map_err(cfgd_core::errors::CfgdError::Io)?;

                if !output.status.success() {
                    // Property may not exist yet — retry with --create
                    let xfconf_type = match desired_value {
                        serde_yaml::Value::Bool(_) => "bool",
                        serde_yaml::Value::Number(_) => "int",
                        _ => "string",
                    };
                    let create_output = Command::new("xfconf-query")
                        .args([
                            "-c",
                            channel,
                            "-p",
                            property,
                            "--create",
                            "-t",
                            xfconf_type,
                            "-s",
                            &val_str,
                        ])
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;

                    if !create_output.status.success() {
                        printer.status_simple(
                            Role::Warn,
                            format!(
                                "xfconf-query set failed for {}.{}: {}",
                                channel,
                                property,
                                cfgd_core::stderr_lossy_trimmed(&create_output)
                            ),
                        );
                    }
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
    fn xfconf_availability_depends_on_command() {
        let xc = XfconfConfigurator;
        let expected = cfgd_core::command_available("xfconf-query");
        assert_eq!(xc.is_available(), expected);
    }

    #[test]
    fn xfconf_apply_empty_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let xc = XfconfConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        xc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn xfconf_apply_non_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let xc = XfconfConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        xc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn xfconf_apply_skips_non_string_channel_key() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let xc = XfconfConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        xc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn xfconf_apply_skips_inner_non_mapping() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let xc = XfconfConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("xfwm4".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        xc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn xfconf_apply_skips_non_string_property_key() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let xc = XfconfConfigurator;
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::Number(1.into()),
            serde_yaml::Value::String("value".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("xfwm4".into()),
            serde_yaml::Value::Mapping(inner),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        xc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn xfconf_current_state_returns_empty_mapping() {
        let xc = XfconfConfigurator;
        let state = xc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }
}

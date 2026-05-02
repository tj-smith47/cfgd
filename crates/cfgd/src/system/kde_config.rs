use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::Printer;

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::read_command_output;

use super::{diff_yaml_mapping, yaml_value_to_string};

/// KdeConfigConfigurator — reads/writes KDE Plasma settings via `kwriteconfig5`/`kwriteconfig6`.
///
/// Config format (three-level: file → group → key → value):
/// ```yaml
/// system:
///   kdeConfig:
///     kdeglobals:
///       General:
///         ColorScheme: BreezeDark
///       KDE:
///         LookAndFeelPackage: org.kde.breezedark.desktop
///     kwinrc:
///       Compositing:
///         Backend: OpenGL
/// ```
pub struct KdeConfigConfigurator;

/// Return the kwriteconfig command name (prefer v6, fallback to v5).
fn kde_write_cmd() -> &'static str {
    if cfgd_core::command_available("kwriteconfig6") {
        "kwriteconfig6"
    } else {
        "kwriteconfig5"
    }
}

/// Return the kreadconfig command name (prefer v6, fallback to v5).
fn kde_read_cmd() -> &'static str {
    if cfgd_core::command_available("kreadconfig6") {
        "kreadconfig6"
    } else {
        "kreadconfig5"
    }
}

fn read_kde_value(file: &str, group: &str, key: &str) -> String {
    read_command_output(
        Command::new(kde_read_cmd()).args(["--file", file, "--group", group, "--key", key]),
    )
}

impl SystemConfigurator for KdeConfigConfigurator {
    fn name(&self) -> &str {
        "kdeConfig"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("kwriteconfig6")
            || cfgd_core::command_available("kwriteconfig5")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let files = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        let mut drifts = Vec::new();
        for (file_key, groups) in files {
            let file = match file_key.as_str() {
                Some(f) => f,
                None => continue,
            };
            let groups_map = match groups.as_mapping() {
                Some(m) => m,
                None => continue,
            };
            for (group_key, keys) in groups_map {
                let group = match group_key.as_str() {
                    Some(g) => g,
                    None => continue,
                };
                let keys_map = match keys.as_mapping() {
                    Some(m) => m,
                    None => continue,
                };
                let prefix = format!("{}.{}", file, group);
                drifts.extend(diff_yaml_mapping(
                    keys_map,
                    &prefix,
                    yaml_value_to_string,
                    |key_str| read_kde_value(file, group, key_str),
                ));
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let files = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        let write_cmd = kde_write_cmd();

        for (file_key, groups) in files {
            let file = match file_key.as_str() {
                Some(f) => f,
                None => continue,
            };
            let groups_map = match groups.as_mapping() {
                Some(m) => m,
                None => continue,
            };
            for (group_key, keys) in groups_map {
                let group = match group_key.as_str() {
                    Some(g) => g,
                    None => continue,
                };
                let keys_map = match keys.as_mapping() {
                    Some(m) => m,
                    None => continue,
                };
                for (key, value) in keys_map {
                    let key_str = match key.as_str() {
                        Some(k) => k,
                        None => continue,
                    };
                    let val_str = yaml_value_to_string(value);

                    printer.info(&format!(
                        "{} --file {} --group {} --key {} {}",
                        write_cmd, file, group, key_str, val_str
                    ));

                    let type_flag = match value {
                        serde_yaml::Value::Bool(_) => Some("bool"),
                        serde_yaml::Value::Number(_) => Some("int"),
                        _ => None,
                    };
                    let mut args = vec!["--file", file, "--group", group, "--key", key_str];
                    if let Some(t) = type_flag {
                        args.extend_from_slice(&["--type", t]);
                    }
                    args.push(&val_str);
                    let output = Command::new(write_cmd)
                        .args(&args)
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;

                    if !output.status.success() {
                        printer.warning(&format!(
                            "{} failed for {}.{}.{}: {}",
                            write_cmd,
                            file,
                            group,
                            key_str,
                            cfgd_core::stderr_lossy_trimmed(&output)
                        ));
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
    fn kde_find_kwrite_cmd() {
        let cmd = kde_write_cmd();
        assert!(cmd == "kwriteconfig6" || cmd == "kwriteconfig5");
    }

    #[test]
    fn kde_find_kread_cmd() {
        let cmd = kde_read_cmd();
        assert!(cmd == "kreadconfig6" || cmd == "kreadconfig5");
    }

    #[test]
    fn kde_availability_depends_on_command() {
        let kc = KdeConfigConfigurator;
        let expected = cfgd_core::command_available("kwriteconfig6")
            || cfgd_core::command_available("kwriteconfig5");
        assert_eq!(kc.is_available(), expected);
    }

    #[test]
    fn kde_apply_empty_mapping_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let kc = KdeConfigConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        kc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn kde_apply_non_mapping_is_noop() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let kc = KdeConfigConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        kc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn kde_apply_skips_non_string_file_key() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let kc = KdeConfigConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn kde_apply_skips_groups_non_mapping() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let kc = KdeConfigConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn kde_apply_skips_non_string_group_key() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let kc = KdeConfigConfigurator;
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::Number(1.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn kde_apply_skips_keys_non_mapping() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let kc = KdeConfigConfigurator;
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::String("General".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn kde_apply_skips_non_string_key_in_group() {
        let (printer, _output) = cfgd_core::output::Printer::for_test();
        let kc = KdeConfigConfigurator;
        let mut keys = serde_yaml::Mapping::new();
        keys.insert(
            serde_yaml::Value::Number(99.into()),
            serde_yaml::Value::String("value".into()),
        );
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::String("General".into()),
            serde_yaml::Value::Mapping(keys),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn kde_diff_non_string_file_key_skipped() {
        let kc = KdeConfigConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        let drifts = kc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_diff_groups_not_mapping_skipped() {
        let kc = KdeConfigConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        let drifts = kc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_diff_non_string_group_key_skipped() {
        let kc = KdeConfigConfigurator;
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::Number(1.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        let drifts = kc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_diff_keys_not_mapping_skipped() {
        let kc = KdeConfigConfigurator;
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::String("General".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        let drifts = kc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_current_state_returns_empty_mapping() {
        let kc = KdeConfigConfigurator;
        let state = kc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }
}

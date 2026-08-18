use std::collections::HashMap;

use cfgd_core::errors::Result;
use cfgd_core::output::Role;

use cfgd_core::providers::{SystemConfigurator, SystemContext, SystemDrift};

use super::read_command_output;

use super::{diff_nested_mapping, yaml_value_to_string};

/// Test seam for every `xfconf-query` spawn in this configurator.
const XFCONF_QUERY_BIN_ENV: &str = "CFGD_XFCONF_QUERY_BIN";

fn xfconf_cmd() -> std::process::Command {
    cfgd_core::tool_cmd(XFCONF_QUERY_BIN_ENV, "xfconf-query")
}

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
    let mut cmd = xfconf_cmd();
    cmd.args(["-c", channel, "-p", property]);
    read_command_output(&mut cmd)
}

/// Read one channel's properties in a single spawn.
///
/// `xfconf-query -c <channel> -l -v` prints `<property><padding><value>` per
/// property, with scalars spelled exactly as `-p <property>` prints them. An
/// ARRAY is the one shape the two disagree on — the listing renders `[a,b]` on
/// one line while the per-property read prints `Value is an array with N
/// items:` and a line per item — so an array is stored as [`Snapshot::Opaque`]
/// and re-read per property, keeping the drift `actual` byte-identical.
fn snapshot_channel(channel: &str) -> HashMap<String, Snapshot> {
    let mut cmd = xfconf_cmd();
    cmd.args(["-c", channel, "-l", "-v"]);
    parse_channel_listing(&read_command_output(&mut cmd))
}

/// A listed property's value, or the marker that only a per-property read can
/// answer for it.
enum Snapshot {
    Value(String),
    Opaque,
}

fn parse_channel_listing(dump: &str) -> HashMap<String, Snapshot> {
    let mut map = HashMap::new();
    for line in dump.lines() {
        let line = line.trim_end();
        let Some((property, rest)) = line.split_once(char::is_whitespace) else {
            // Nothing but padding after the property name. An empty string
            // prints that way, and so would any type this listing renders
            // blank, so the property is re-read rather than assumed empty —
            // the empty-string case costs one spawn and answers the same.
            if !line.trim().is_empty() {
                map.insert(line.trim().to_string(), Snapshot::Opaque);
            }
            continue;
        };
        if property.is_empty() {
            continue;
        }
        let value = rest.trim();
        let entry = if value.starts_with('[') {
            Snapshot::Opaque
        } else {
            Snapshot::Value(value.to_string())
        };
        map.insert(property.to_string(), entry);
    }
    map
}

impl SystemConfigurator for XfconfConfigurator {
    fn name(&self) -> &str {
        "xfconf"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available_with_seam(XFCONF_QUERY_BIN_ENV, "xfconf-query")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        // One channel listing per declared channel, held for this call only.
        let mut snapshots: HashMap<String, HashMap<String, Snapshot>> = HashMap::new();
        if let Some(mapping) = desired.as_mapping() {
            for channel_key in mapping.keys() {
                if let Some(channel) = channel_key.as_str()
                    && !snapshots.contains_key(channel)
                {
                    snapshots.insert(channel.to_string(), snapshot_channel(channel));
                }
            }
        }

        diff_nested_mapping(desired, |channel, property| {
            match snapshots.get(channel).and_then(|props| props.get(property)) {
                Some(Snapshot::Value(v)) => v.clone(),
                Some(Snapshot::Opaque) => read_xfconf_value(channel, property),
                // A property the channel does not carry: the per-property read
                // exits non-zero, which `read_command_output` renders as the
                // same empty string.
                None => String::new(),
            }
        })
    }

    fn apply(&self, desired: &serde_yaml::Value, cx: &SystemContext<'_>) -> Result<()> {
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

                cx.report(
                    Role::Info,
                    format!("xfconf-query -c {} -p {} -s {}", channel, property, val_str),
                );

                let output = xfconf_cmd()
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
                    let create_output = xfconf_cmd()
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
                        cx.report(
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

    /// Verbatim `xfconf-query -c cfgdqp5 -l -v` output (xfconf 4.20, one
    /// property of each shape), captured against a channel XML holding a
    /// string, a spaced string, an int, a bool, an empty string and a
    /// two-element array.
    const REAL_LISTING: &str = "/general/arr         [a,b]\n\
         /general/empty       \n\
         /general/flag        true\n\
         /general/num         42\n\
         /general/theme       Default\n\
         /general/title_font  Sans Bold 9\n";

    fn listed(dump: &str, property: &str) -> Option<String> {
        match parse_channel_listing(dump).remove(property) {
            Some(Snapshot::Value(v)) => Some(v),
            _ => None,
        }
    }

    #[test]
    fn snapshot_answers_scalars_exactly_as_the_per_property_read_does() {
        // Right-hand sides are the captured `xfconf-query -c cfgdqp5 -p
        // <property>` output of the same channel.
        for (property, expected) in [
            ("/general/theme", "Default"),
            ("/general/title_font", "Sans Bold 9"),
            ("/general/num", "42"),
            ("/general/flag", "true"),
        ] {
            assert_eq!(
                listed(REAL_LISTING, property).as_deref(),
                Some(expected),
                "listing and per-property read disagree about {property}"
            );
        }
    }

    #[test]
    fn snapshot_leaves_the_two_shapes_the_listing_renders_differently_opaque() {
        let snapshot = parse_channel_listing(REAL_LISTING);
        // The per-property read prints `Value is an array with 2 items:` and a
        // line per item; the listing prints `[a,b]`. Answering from the listing
        // would put a string no read ever returned in the drift's `actual`.
        assert!(matches!(
            snapshot.get("/general/arr"),
            Some(Snapshot::Opaque)
        ));
        // An empty value is a line of nothing but padding, which is also how a
        // shape this parse does not know would render.
        assert!(matches!(
            snapshot.get("/general/empty"),
            Some(Snapshot::Opaque)
        ));
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn xfconf_diff_reads_a_channel_once_however_many_properties_it_declares() {
        let shim =
            cfgd_core::test_helpers::ToolShim::install(XFCONF_QUERY_BIN_ENV, 0, REAL_LISTING, "");
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "cfgdqp5:\n  \
             /general/theme: Adwaita\n  \
             /general/title_font: Sans Bold 9\n  \
             /general/num: 42\n",
        )
        .unwrap();

        let drifts = XfconfConfigurator.diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("cfgdqp5"),
            vec!["-c cfgdqp5 -l -v"],
            "one listing answers every scalar property in the channel"
        );
        assert_eq!(drifts.len(), 1, "unexpected drifts: {drifts:?}");
        assert_eq!(drifts[0].key, "cfgdqp5./general/theme");
        assert_eq!(drifts[0].actual, "Default");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn xfconf_diff_re_reads_only_the_property_the_listing_cannot_answer() {
        let shim =
            cfgd_core::test_helpers::ToolShim::install(XFCONF_QUERY_BIN_ENV, 0, REAL_LISTING, "");
        let yaml: serde_yaml::Value =
            serde_yaml::from_str("cfgdqp5:\n  /general/theme: Default\n  /general/arr: x\n")
                .unwrap();

        XfconfConfigurator.diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("cfgdqp5"),
            vec!["-c cfgdqp5 -l -v", "-c cfgdqp5 -p /general/arr"],
            "the array is re-read per property; the scalar beside it is not"
        );
    }

    #[test]
    #[serial_test::serial]
    fn xfconf_is_available_exactly_when_an_xfconf_query_binary_resolves() {
        let _path_lock = cfgd_core::test_helpers::path_env_mutation_guard();
        let _dirs = cfgd_core::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let xc = XfconfConfigurator;

        {
            let _empty = cfgd_core::test_helpers::EnvVarGuard::set("PATH", "");
            assert!(
                !xc.is_available(),
                "a host resolving no binaries is not an xfconf host"
            );
        }

        #[cfg(unix)]
        {
            let _probe = cfgd_core::test_helpers::ProbePath::containing(&["xfconf-query"]);
            assert!(
                xc.is_available(),
                "the binary this configurator probes for is named `xfconf-query`"
            );
        }
    }

    #[test]
    fn xfconf_apply_empty_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let xc = XfconfConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        xc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn xfconf_apply_non_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let xc = XfconfConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        xc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
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
        xc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
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
        xc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
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
        xc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn xfconf_current_state_returns_empty_mapping() {
        let xc = XfconfConfigurator;
        let state = xc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }
}

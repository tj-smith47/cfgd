use std::collections::HashMap;

use cfgd_core::errors::Result;
use cfgd_core::output::Role;

use cfgd_core::providers::{SystemConfigurator, SystemContext, SystemDrift};

use super::read_command_output;

use super::{diff_nested_mapping, yaml_value_to_string};

/// Test seam for every `gsettings` spawn in this configurator.
const GSETTINGS_BIN_ENV: &str = "CFGD_GSETTINGS_BIN";

fn gsettings_cmd() -> std::process::Command {
    cfgd_core::tool_cmd(GSETTINGS_BIN_ENV, "gsettings")
}

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

/// Read one schema's keys in a single spawn.
///
/// `gsettings list-recursively <schema>` prints one `schema key value` line per
/// key, with the value in the same GVariant spelling `gsettings get` returns —
/// so the map's values are byte-identical to the per-key read, quotes and all,
/// and [`strip_gsettings_quotes`] applies to both the same way. A schema with
/// CHILD schemas lists their keys too, under their own schema id in the first
/// field, which is why only lines naming the requested schema are kept: a child
/// key would otherwise answer a question about a same-named key of the parent.
fn snapshot_schema(schema: &str) -> HashMap<String, String> {
    let mut cmd = gsettings_cmd();
    cmd.args(["list-recursively", schema]);
    parse_list_recursively(&read_command_output(&mut cmd), schema)
}

fn parse_list_recursively(dump: &str, schema: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in dump.lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(line_schema), Some(key), Some(value)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if line_schema != schema || key.is_empty() {
            continue;
        }
        map.insert(
            key.to_string(),
            strip_gsettings_quotes(value.trim()).to_string(),
        );
    }
    map
}

impl SystemConfigurator for GsettingsConfigurator {
    fn name(&self) -> &str {
        "gsettings"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available_with_seam(GSETTINGS_BIN_ENV, "gsettings")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        // One `list-recursively` per declared schema, memoized for the length of
        // this call: `diff_nested_mapping` asks per key, and a schema block of
        // twenty keys used to be twenty `gsettings get` spawns.
        let mut snapshots: HashMap<String, HashMap<String, String>> = HashMap::new();
        if let Some(mapping) = desired.as_mapping() {
            for schema_key in mapping.keys() {
                if let Some(schema) = schema_key.as_str()
                    && !snapshots.contains_key(schema)
                {
                    snapshots.insert(schema.to_string(), snapshot_schema(schema));
                }
            }
        }

        diff_nested_mapping(desired, |schema, key| {
            snapshots
                .get(schema)
                .and_then(|keys| keys.get(key))
                .cloned()
                .unwrap_or_default()
        })
    }

    fn apply(&self, desired: &serde_yaml::Value, cx: &SystemContext<'_>) -> Result<()> {
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

                cx.report(
                    Role::Info,
                    format!("gsettings set {} {} {}", schema, key_str, gsettings_val),
                );

                let output = gsettings_cmd()
                    .args(["set", schema, key_str, &gsettings_val])
                    .output()
                    .map_err(cfgd_core::errors::CfgdError::Io)?;

                if !output.status.success() {
                    cx.report(
                        Role::Warn,
                        format!(
                            "gsettings set failed for {}.{}: {}",
                            schema,
                            key_str,
                            cfgd_core::stderr_lossy_trimmed(&output)
                        ),
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `gsettings list-recursively org.gnome.desktop.interface`
    /// output (glib 2.86, Ubuntu), plus one line from a DIFFERENT schema to
    /// pin the filter. Every value below was compared against the same host's
    /// `gsettings get` for the same key — see
    /// `snapshot_answers_what_the_per_key_read_answers`.
    const REAL_LISTING: &str = "org.gnome.desktop.interface avatar-directories @as []\n\
         org.gnome.desktop.interface clock-format '24h'\n\
         org.gnome.desktop.interface color-scheme 'default'\n\
         org.gnome.desktop.interface cursor-size 24\n\
         org.gnome.desktop.interface enable-animations true\n\
         org.gnome.desktop.interface font-name 'Adwaita Sans 11'\n\
         org.gnome.desktop.a11y.keyboard bouncekeys-delay 300\n";

    #[test]
    fn snapshot_answers_what_the_per_key_read_answers() {
        let snapshot = parse_list_recursively(REAL_LISTING, "org.gnome.desktop.interface");
        // Right-hand sides are the captured `gsettings get <schema> <key>`
        // output of the same host, folded through `strip_gsettings_quotes` the
        // way the per-key path folds it. A listing value that needed different
        // handling than a `get` value would show up here as a mismatch.
        for (key, expected) in [
            ("color-scheme", "default"),
            ("font-name", "Adwaita Sans 11"),
            ("cursor-size", "24"),
            ("enable-animations", "true"),
            ("clock-format", "24h"),
            // An array reads identically both ways, so it needs no re-read.
            ("avatar-directories", "@as []"),
        ] {
            assert_eq!(
                snapshot.get(key).map(String::as_str),
                Some(expected),
                "listing and per-key read disagree about {key}"
            );
        }
    }

    #[test]
    fn snapshot_never_answers_with_a_child_schemas_key() {
        let snapshot = parse_list_recursively(REAL_LISTING, "org.gnome.desktop.interface");
        assert!(
            !snapshot.contains_key("bouncekeys-delay"),
            "a key listed under another schema must not answer for this one"
        );
        assert_eq!(
            parse_list_recursively(REAL_LISTING, "org.gnome.desktop.a11y.keyboard")
                .get("bouncekeys-delay")
                .map(String::as_str),
            Some("300"),
            "and it must answer for the schema it belongs to"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn gsettings_diff_reads_a_schema_once_however_many_keys_it_declares() {
        let shim = cfgd_core::test_helpers::ToolShim::install(
            GSETTINGS_BIN_ENV,
            0,
            "org.gnome.cfgd-test color-scheme 'default'\n\
             org.gnome.cfgd-test font-name 'Adwaita Sans 11'\n\
             org.gnome.cfgd-test cursor-size 24\n",
            "",
        );
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "org.gnome.cfgd-test:\n  \
             color-scheme: prefer-dark\n  \
             font-name: Adwaita Sans 11\n  \
             cursor-size: 24\n",
        )
        .unwrap();

        let drifts = GsettingsConfigurator.diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("org.gnome.cfgd-test"),
            vec!["list-recursively org.gnome.cfgd-test"],
            "one listing answers every key in the schema"
        );
        assert_eq!(drifts.len(), 1, "unexpected drifts: {drifts:?}");
        assert_eq!(drifts[0].key, "org.gnome.cfgd-test.color-scheme");
        assert_eq!(drifts[0].actual, "default");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn gsettings_diff_reads_each_declared_schema_once() {
        let shim = cfgd_core::test_helpers::ToolShim::install(GSETTINGS_BIN_ENV, 0, "", "");
        let yaml: serde_yaml::Value =
            serde_yaml::from_str("org.gnome.cfgd-a:\n  x: 1\n  y: 2\norg.gnome.cfgd-b:\n  z: 3\n")
                .unwrap();

        GsettingsConfigurator.diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("org.gnome.cfgd-"),
            vec![
                "list-recursively org.gnome.cfgd-a",
                "list-recursively org.gnome.cfgd-b",
            ],
            "one listing per schema, none per key"
        );
    }

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
    #[serial_test::serial]
    fn gsettings_is_available_exactly_when_a_gsettings_binary_resolves() {
        let _path_lock = cfgd_core::test_helpers::path_env_mutation_guard();
        let _dirs = cfgd_core::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let gc = GsettingsConfigurator;

        {
            let _empty = cfgd_core::test_helpers::EnvVarGuard::set("PATH", "");
            assert!(
                !gc.is_available(),
                "a host resolving no binaries is not a gsettings host"
            );
        }

        #[cfg(unix)]
        {
            let _probe = cfgd_core::test_helpers::ProbePath::containing(&["gsettings"]);
            assert!(
                gc.is_available(),
                "the binary this configurator probes for is named `gsettings`"
            );
        }
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
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let gc = GsettingsConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        gc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn gsettings_apply_non_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let gc = GsettingsConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        gc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn gsettings_apply_skips_non_string_schema_key() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let gc = GsettingsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        gc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn gsettings_apply_skips_inner_non_mapping() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let gc = GsettingsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("org.gnome.test".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        gc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn gsettings_apply_skips_non_string_key() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
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
        gc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn gsettings_current_state_returns_empty_mapping() {
        let gc = GsettingsConfigurator;
        let state = gc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    #[test]
    fn gsettings_name_returns_gsettings() {
        let gc = GsettingsConfigurator;
        assert_eq!(gc.name(), "gsettings");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn gsettings_apply_iterates_schemas_and_keys_through_command_path() {
        // Drives the apply loop body: yaml_value_to_string, the report, and one
        // `gsettings set` per key. A shimmed gsettings makes the run identical
        // on every host, so the argv — which schema, which key, which value,
        // and in what order — is what the test asserts. Discarding the Result
        // instead left it asserting only that apply did not panic.
        let (_bin, _path, log) =
            cfgd_core::test_helpers::install_named_path_shim_logged("gsettings", 0, "", "");
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let gc = GsettingsConfigurator;
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("color-scheme".into()),
            serde_yaml::Value::String("prefer-dark".into()),
        );
        inner.insert(
            serde_yaml::Value::String("font-name".into()),
            serde_yaml::Value::String("Cantarell 11".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("org.gnome.desktop.interface.test-only".into()),
            serde_yaml::Value::Mapping(inner),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        gc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .expect("every shimmed `gsettings set` succeeds");
        assert_eq!(
            log.argv_log().lines().collect::<Vec<_>>(),
            vec![
                "set org.gnome.desktop.interface.test-only color-scheme prefer-dark",
                "set org.gnome.desktop.interface.test-only font-name Cantarell 11",
            ],
            "one `gsettings set` per key, in declaration order, schema first"
        );
    }

    // Serial because it SPAWNS `gsettings`: the shimmed apply test above
    // prepends its shim to the process-global PATH, so a `gsettings get` run
    // concurrently resolves to that shim and appends a line to the argv log the
    // other test is asserting on — observed as a stray
    // `get org.gnome.cfgd-test-schema color-scheme` failing the apply test.
    #[test]
    #[serial_test::serial]
    fn gsettings_diff_via_nested_mapping_helper_returns_drift_entries() {
        let gc = GsettingsConfigurator;
        let mut inner = serde_yaml::Mapping::new();
        inner.insert(
            serde_yaml::Value::String("color-scheme".into()),
            serde_yaml::Value::String("prefer-dark".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("org.gnome.cfgd-test-schema".into()),
            serde_yaml::Value::Mapping(inner),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        // Whether gsettings is on PATH or not, diff_nested_mapping returns Ok
        // and produces at most one drift entry (depends on whether the schema
        // is registered). We assert only that diff itself does not fail.
        let drifts = gc.diff(&yaml).unwrap();
        assert!(drifts.len() <= 1);
    }
}

use std::collections::HashMap;

use cfgd_core::errors::Result;
use cfgd_core::output::Role;

use cfgd_core::providers::{SystemConfigurator, SystemContext, SystemDrift};

use super::{diff_yaml_mapping, read_command_output, yaml_value_with_numeric_bools};

/// Test seam for every `defaults` spawn in this configurator.
const DEFAULTS_BIN_ENV: &str = "CFGD_DEFAULTS_BIN";

fn defaults_cmd() -> std::process::Command {
    cfgd_core::tool_cmd(DEFAULTS_BIN_ENV, "defaults")
}

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
                Some(m) if !m.is_empty() => m,
                // A domain declaring no keys has nothing to compare, so reading
                // it is a spawn whose answer is discarded.
                _ => continue,
            };
            // One `defaults read <domain>` for the whole block, instead of one
            // `defaults read <domain> <key>` per declared key.
            let snapshot = snapshot_domain(domain);
            drifts.extend(diff_yaml_mapping(
                values,
                domain,
                yaml_value_with_numeric_bools,
                |key_str| match snapshot.as_ref().map(|keys| keys.get(key_str)) {
                    Some(Some(Entry::Value(v))) => v.clone(),
                    Some(Some(Entry::Opaque)) | None => read_defaults_value(domain, key_str),
                    Some(None) => String::new(),
                },
            ));
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, cx: &SystemContext<'_>) -> Result<()> {
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

                cx.report(
                    Role::Info,
                    format!(
                        "defaults write {} {} -{} {}",
                        domain, key_str, value_type, value_str
                    ),
                );

                let output = defaults_cmd()
                    .args(["write", domain, key_str])
                    .arg(format!("-{}", value_type))
                    .arg(&value_str)
                    .output()
                    .map_err(cfgd_core::errors::CfgdError::Io)?;

                if !output.status.success() {
                    cx.report(
                        Role::Warn,
                        format!(
                            "defaults write failed for {}.{}: {}",
                            domain,
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

fn read_defaults_value(domain: &str, key: &str) -> String {
    let mut cmd = defaults_cmd();
    cmd.args(["read", domain, key]);
    read_command_output(&mut cmd)
}

/// A domain entry's value, or the marker that only a per-key read can answer
/// for it.
enum Entry {
    Value(String),
    Opaque,
}

/// One domain's entries, or `None` when the dump's shape is not one this parse
/// can be trusted on — in which case the caller reads per key, exactly as
/// before.
type DomainSnapshot = Option<HashMap<String, Entry>>;

/// Read one domain in a single spawn.
///
/// `defaults read <domain>` prints the same old-style plist `defaults read
/// <domain> <key>` prints a value out of, so a depth-1 scalar line answers the
/// per-key read byte-for-byte once its optional quotes are removed. Two shapes
/// are left to the per-key read instead of being reconstructed here: a
/// container value (`( … )` / `{ … }`, which the per-key read prints across
/// several lines with a different indent), and any quoted token carrying a
/// backslash (the dump escapes `\n`, `\t` and non-ASCII as `\U00e9`, none of
/// which the per-key read does).
fn snapshot_domain(domain: &str) -> DomainSnapshot {
    let mut cmd = defaults_cmd();
    cmd.args(["read", domain]);
    parse_defaults_dump(&read_command_output(&mut cmd))
}

fn parse_defaults_dump(dump: &str) -> DomainSnapshot {
    // A domain that does not exist (and a host with no `defaults` at all) reads
    // as nothing, and so does every key inside it — the empty map answers each
    // one with the same empty string the per-key read would, without spawning.
    if dump.trim().is_empty() {
        return Some(HashMap::new());
    }
    let mut lines = dump.lines();
    if lines.next().map(str::trim) != Some("{") || dump.trim_end().lines().last()?.trim() != "}" {
        return None;
    }

    let mut map = HashMap::new();
    for line in lines {
        // Depth-1 entries carry exactly four leading spaces; a container's own
        // lines are indented deeper, and its closing `);` / `};` opens with a
        // bracket rather than a key.
        let Some(entry) = line.strip_prefix("    ") else {
            continue;
        };
        if entry.starts_with(' ') || entry.starts_with(')') || entry.starts_with('}') {
            continue;
        }
        let Some((raw_key, raw_value)) = entry.split_once(" = ") else {
            continue;
        };
        // A key whose spelling needs unescaping cannot be matched against the
        // declared one, and a wrong match is a wrong `actual`, so the whole
        // domain falls back rather than answering some of its keys.
        let key = unquote(raw_key.trim())?;
        let value = raw_value.trim();
        let scalar = value
            .strip_suffix(';')
            .filter(|v| !v.starts_with('(') && !v.starts_with('{'))
            .and_then(|v| unquote(v.trim()));
        map.insert(
            key,
            match scalar {
                Some(v) => Entry::Value(v),
                None => Entry::Opaque,
            },
        );
    }
    Some(map)
}

/// Strip a token's surrounding double quotes, refusing any token that carries a
/// backslash — the dump's escapes are not the per-key read's spelling, so an
/// escaped token has to be re-read rather than guessed at.
///
/// A token that OPENS with a quote and does not close with one is refused for
/// the same reason: the split that produced it landed inside a quoted string
/// (a key spelled `"a = b"` splits on its own ` = `), so neither half names
/// what it appears to name.
fn unquote(token: &str) -> Option<String> {
    if token.contains('\\') {
        return None;
    }
    match token.strip_prefix('"') {
        Some(rest) => rest.strip_suffix('"').map(str::to_string),
        None => {
            if token.ends_with('"') {
                return None;
            }
            Some(token.to_string())
        }
    }
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

    /// Verbatim `defaults read io.cfgd.qp5fixture` output (macOS 15, one entry
    /// of each shape the parse has to classify), captured after writing the
    /// domain with `defaults write -bool/-int/-float/-string/-array/-dict`.
    const REAL_DUMP: &str = "{\n\
        \x20   ArrayVal =     (\n\
        \x20       a,\n\
        \x20       b\n\
        \x20   );\n\
        \x20   BoolTrue = 1;\n\
        \x20   DictVal =     {\n\
        \x20       inner = one;\n\
        \x20   };\n\
        \x20   EmptyString = \"\";\n\
        \x20   FloatVal = \"0.5\";\n\
        \x20   IntVal = 24;\n\
        \x20   StringPlain = Adwaita;\n\
        \x20   StringSpaced = \"Sans Bold 9\";\n\
        \x20   Unicode = cafe;\n\
        \x20   \"com.apple.dotted.key\" = dotted;\n\
        }\n";

    /// Verbatim dump of a domain whose values carry a tab, a newline, a
    /// backslash and a non-ASCII character. The per-key read of the same four
    /// keys returns the UNESCAPED bytes (`a<TAB>b`, `line1<LF>line2`, `café`),
    /// so none of these may be answered from the dump.
    const REAL_ESCAPED_DUMP: &str = "{\n\
        \x20   tab = \"a\\tb\";\n\
        \x20   unicode = \"caf\\U00e9\";\n\
        \x20   withBackslash = \"a\\b\";\n\
        \x20   withNewline = \"line1\\nline2\";\n\
        }\n";

    fn value_of(dump: &str, key: &str) -> Option<String> {
        match parse_defaults_dump(dump)?.remove(key) {
            Some(Entry::Value(v)) => Some(v),
            _ => None,
        }
    }

    #[test]
    fn snapshot_answers_scalars_exactly_as_the_per_key_read_does() {
        // Right-hand sides are the captured `defaults read io.cfgd.qp5fixture
        // <key>` output of the same domain.
        for (key, expected) in [
            ("BoolTrue", "1"),
            ("IntVal", "24"),
            ("FloatVal", "0.5"),
            ("StringPlain", "Adwaita"),
            ("StringSpaced", "Sans Bold 9"),
            ("EmptyString", ""),
            ("com.apple.dotted.key", "dotted"),
            ("Unicode", "cafe"),
        ] {
            assert_eq!(
                value_of(REAL_DUMP, key).as_deref(),
                Some(expected),
                "dump and per-key read disagree about {key}"
            );
        }
    }

    #[test]
    fn snapshot_leaves_containers_to_the_per_key_read() {
        let snapshot = parse_defaults_dump(REAL_DUMP).expect("a well-formed dump parses");
        // `defaults read <domain> ArrayVal` prints `(\n    a,\n    b\n)` — a
        // different indent and no trailing `;` — so the dump's spelling would
        // be an `actual` no read ever returns.
        assert!(matches!(snapshot.get("ArrayVal"), Some(Entry::Opaque)));
        assert!(matches!(snapshot.get("DictVal"), Some(Entry::Opaque)));
        // A container's own lines are not entries of the domain.
        assert!(!snapshot.contains_key("inner"));
        assert!(!snapshot.contains_key("a"));
    }

    #[test]
    fn snapshot_leaves_every_escaped_value_to_the_per_key_read() {
        let snapshot = parse_defaults_dump(REAL_ESCAPED_DUMP).expect("a well-formed dump parses");
        for key in ["tab", "unicode", "withBackslash", "withNewline"] {
            assert!(
                matches!(snapshot.get(key), Some(Entry::Opaque)),
                "{key} is escaped in the dump and must be re-read"
            );
        }
    }

    #[test]
    fn a_domain_that_does_not_exist_answers_every_key_with_nothing() {
        // `defaults read <missing domain>` exits non-zero, which the shared
        // read renders as an empty string — the same string the per-key read
        // of any of its keys renders.
        let snapshot = parse_defaults_dump("").expect("an absent domain is a known shape");
        assert!(snapshot.is_empty());
    }

    #[test]
    fn a_key_whose_own_spelling_carries_the_separator_falls_the_domain_back() {
        // `defaults` quotes a key carrying spaces, so a key spelled `a = b`
        // prints as `    "a = b" = 1;` and splits on ITS OWN separator: the
        // halves are `"a` and `b" = 1;`, neither of which names anything. The
        // domain has to fall back to per-key reads rather than answer under a
        // key nobody declared.
        assert!(parse_defaults_dump("{\n    \"a = b\" = 1;\n}\n").is_none());
        // The same shape on the value side.
        assert!(parse_defaults_dump("{\n    A = \"x = y\";\n}\n").is_some());
    }

    #[test]
    fn a_dump_that_is_not_a_plist_falls_back_entirely() {
        assert!(parse_defaults_dump("not a plist\n").is_none());
        assert!(parse_defaults_dump("{\n    A = 1;\n").is_none());
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn macos_defaults_diff_reads_a_domain_once_however_many_keys_it_declares() {
        let shim = cfgd_core::test_helpers::ToolShim::install(DEFAULTS_BIN_ENV, 0, REAL_DUMP, "");
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "io.cfgd.qp5fixture:\n  \
             StringPlain: Adwaita\n  \
             IntVal: 24\n  \
             StringSpaced: Menlo\n  \
             Absent: x\n",
        )
        .unwrap();

        let drifts = MacosDefaultsConfigurator.diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("io.cfgd.qp5fixture"),
            vec!["read io.cfgd.qp5fixture"],
            "one domain read answers every declared key, absent ones included"
        );
        let keys: Vec<&str> = drifts.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "io.cfgd.qp5fixture.StringSpaced",
                "io.cfgd.qp5fixture.Absent"
            ],
            "unexpected drifts: {drifts:?}"
        );
        assert_eq!(drifts[0].actual, "Sans Bold 9");
        assert_eq!(drifts[1].actual, "");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn macos_defaults_diff_re_reads_only_the_key_the_dump_cannot_answer() {
        let shim = cfgd_core::test_helpers::ToolShim::install(DEFAULTS_BIN_ENV, 0, REAL_DUMP, "");
        let yaml: serde_yaml::Value =
            serde_yaml::from_str("io.cfgd.qp5fixture:\n  IntVal: 24\n  ArrayVal: x\n").unwrap();

        MacosDefaultsConfigurator.diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("io.cfgd.qp5fixture"),
            vec![
                "read io.cfgd.qp5fixture",
                "read io.cfgd.qp5fixture ArrayVal"
            ],
            "the container is re-read per key; the scalar beside it is not"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn macos_defaults_diff_of_a_domain_declaring_no_keys_reads_nothing() {
        let shim = cfgd_core::test_helpers::ToolShim::install(DEFAULTS_BIN_ENV, 0, REAL_DUMP, "");
        let yaml: serde_yaml::Value = serde_yaml::from_str("io.cfgd.qp5empty: {}\n").unwrap();

        let drifts = MacosDefaultsConfigurator.diff(&yaml).unwrap();

        assert!(drifts.is_empty());
        assert!(
            shim.argv_lines_naming("io.cfgd.qp5empty").is_empty(),
            "a domain declaring no keys is never read: {}",
            shim.argv_log()
        );
    }

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

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn macos_defaults_not_available_on_linux() {
        let md = MacosDefaultsConfigurator;
        assert!(!md.is_available());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_defaults_is_available_on_macos() {
        let md = MacosDefaultsConfigurator;
        assert!(md.is_available());
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
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let md = MacosDefaultsConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        md.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn macos_defaults_apply_non_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let md = MacosDefaultsConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        md.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn macos_defaults_apply_skips_non_string_domain_key() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let md = MacosDefaultsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        md.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn macos_defaults_apply_skips_inner_non_mapping() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let md = MacosDefaultsConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("com.apple.dock".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        md.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn macos_defaults_apply_skips_non_string_key_inside_domain() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
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
        md.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }
}

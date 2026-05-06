use super::*;
use cfgd_core::providers::SystemConfigurator;

#[test]
fn workstation_configurator_names() {
    let cases: &[(&dyn SystemConfigurator, &str)] = &[
        (&ShellConfigurator, "shell"),
        (&MacosDefaultsConfigurator, "macosDefaults"),
        (&SystemdUnitConfigurator, "systemdUnits"),
        (&LaunchAgentConfigurator, "launchAgents"),
        (&EnvironmentConfigurator, "environment"),
        (&WindowsRegistryConfigurator, "windowsRegistry"),
        (&WindowsServiceConfigurator, "windowsServices"),
        (&GsettingsConfigurator, "gsettings"),
        (&KdeConfigConfigurator, "kdeConfig"),
        (&XfconfConfigurator, "xfconf"),
    ];
    for (c, expected) in cases {
        assert_eq!(c.name(), *expected, "wrong name for {expected}");
    }
}

#[test]
fn yaml_value_with_numeric_bools_conversion() {
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(true)),
        "1"
    );
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(false)),
        "0"
    );
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::Number(42.into())),
        "42"
    );
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::String("hello".into())),
        "hello"
    );
}

#[test]
fn diff_returns_empty_for_empty_inputs() {
    let cases: &[(&dyn SystemConfigurator, serde_yaml::Value)] = &[
        (
            &SystemdUnitConfigurator,
            serde_yaml::Value::Sequence(Vec::new()),
        ),
        (
            &LaunchAgentConfigurator,
            serde_yaml::Value::Sequence(Vec::new()),
        ),
        (
            &EnvironmentConfigurator,
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        ),
        (
            &GsettingsConfigurator,
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        ),
        (
            &GsettingsConfigurator,
            serde_yaml::Value::String("not a mapping".into()),
        ),
        (
            &KdeConfigConfigurator,
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        ),
        (
            &KdeConfigConfigurator,
            serde_yaml::Value::String("not a mapping".into()),
        ),
        (
            &XfconfConfigurator,
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        ),
        (
            &XfconfConfigurator,
            serde_yaml::Value::String("not a mapping".into()),
        ),
    ];
    for (c, input) in cases {
        let drifts = c.diff(input).unwrap();
        assert!(
            drifts.is_empty(),
            "{} should return empty for {:?}",
            c.name(),
            input
        );
    }
}

#[test]
fn diff_yaml_mapping_detects_drift() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("key1".into()),
        serde_yaml::Value::String("expected".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "", yaml_value_with_numeric_bools, |_| {
        "actual".to_string()
    });
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "key1");
    assert_eq!(drifts[0].expected, "expected");
    assert_eq!(drifts[0].actual, "actual");
}

#[test]
fn diff_yaml_mapping_no_drift_when_matching() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("key1".into()),
        serde_yaml::Value::String("same".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "", yaml_value_with_numeric_bools, |_| {
        "same".to_string()
    });
    assert!(drifts.is_empty());
}

#[test]
fn diff_yaml_mapping_with_prefix() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("setting".into()),
        serde_yaml::Value::String("val".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "sysctl", yaml_value_with_numeric_bools, |_| {
        "other".to_string()
    });
    assert_eq!(drifts[0].key, "sysctl.setting");
}

#[test]
fn diff_yaml_mapping_multiple_keys() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("a".into()),
        serde_yaml::Value::String("1".into()),
    );
    desired.insert(
        serde_yaml::Value::String("b".into()),
        serde_yaml::Value::String("2".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "", yaml_value_with_numeric_bools, |k| {
        if k == "a" {
            "1".to_string()
        } else {
            "wrong".to_string()
        }
    });
    // Only "b" should drift
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "b");
}

#[test]
fn yaml_value_with_numeric_bools_bool_converts_to_01() {
    // macos defaults uses "1"/"0" for bools, not "true"/"false"
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(true)),
        "1"
    );
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(false)),
        "0"
    );
}

#[test]
fn yaml_value_with_numeric_bools_number_and_string() {
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::Number(42.into())),
        "42"
    );
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::String("hello".into())),
        "hello"
    );
}

#[test]
fn node_yaml_value_with_numeric_bools_conversion() {
    // Strings are bare (no quotes — Command bypasses shell)
    assert_eq!(
        yaml_value_to_string(&serde_yaml::Value::String("dark".into())),
        "dark"
    );
    // Bools use true/false (not 0/1 like yaml_value_with_numeric_bools)
    assert_eq!(yaml_value_to_string(&serde_yaml::Value::Bool(true)), "true");
    assert_eq!(
        yaml_value_to_string(&serde_yaml::Value::Bool(false)),
        "false"
    );
    let n = serde_yaml::Value::Number(serde_yaml::Number::from(42));
    assert_eq!(yaml_value_to_string(&n), "42");
    let f = serde_yaml::Value::Number(serde_yaml::Number::from(1.5));
    assert_eq!(yaml_value_to_string(&f), "1.5");
}

#[test]
fn diff_yaml_mapping_empty_desired_produces_no_drifts() {
    let desired = serde_yaml::Mapping::new();
    let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| {
        "anything".to_string()
    });
    assert!(drifts.is_empty());
}

#[test]
fn diff_yaml_mapping_key_missing_from_actual() {
    // When get_actual returns "" for a key that desired has a value for,
    // it should be reported as drift.
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("new_key".into()),
        serde_yaml::Value::String("desired_value".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| String::new());
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "new_key");
    assert_eq!(drifts[0].expected, "desired_value");
    assert_eq!(drifts[0].actual, "");
}

#[test]
fn diff_yaml_mapping_null_value_in_desired() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("null_key".into()),
        serde_yaml::Value::Null,
    );

    // yaml_value_to_string formats Null via Debug
    let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| String::new());
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "null_key");
    // The Null value is formatted via the Debug fallback
    assert!(!drifts[0].expected.is_empty());
}

#[test]
fn diff_yaml_mapping_non_string_keys_are_skipped() {
    let mut desired = serde_yaml::Mapping::new();
    // Insert a numeric key — should be skipped
    desired.insert(
        serde_yaml::Value::Number(42.into()),
        serde_yaml::Value::String("value".into()),
    );
    // Insert a valid string key
    desired.insert(
        serde_yaml::Value::String("valid".into()),
        serde_yaml::Value::String("expected".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| {
        "different".to_string()
    });
    // Only the string key should produce drift; numeric key is skipped
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "valid");
}

#[test]
fn diff_yaml_mapping_empty_prefix_no_dot() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("key".into()),
        serde_yaml::Value::String("a".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| "b".to_string());
    assert_eq!(drifts[0].key, "key"); // No leading dot
}

#[test]
fn diff_yaml_mapping_all_values_match() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("x".into()),
        serde_yaml::Value::String("1".into()),
    );
    desired.insert(
        serde_yaml::Value::String("y".into()),
        serde_yaml::Value::String("2".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "ns", yaml_value_to_string, |k| {
        match k {
            "x" => "1",
            "y" => "2",
            _ => "",
        }
        .to_string()
    });
    assert!(drifts.is_empty());
}

#[test]
fn diff_yaml_mapping_bool_value_via_yaml_value_to_string() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("flag".into()),
        serde_yaml::Value::Bool(true),
    );

    // yaml_value_to_string converts bools to "true"/"false"
    let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| "false".to_string());
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].expected, "true");
    assert_eq!(drifts[0].actual, "false");
}

#[test]
fn diff_yaml_mapping_bool_value_via_numeric_bools() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("flag".into()),
        serde_yaml::Value::Bool(true),
    );

    // yaml_value_with_numeric_bools converts true→"1", false→"0"
    let drifts = diff_yaml_mapping(&desired, "", yaml_value_with_numeric_bools, |_| {
        "0".to_string()
    });
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].expected, "1");
    assert_eq!(drifts[0].actual, "0");
}

#[test]
fn diff_nested_mapping_non_mapping_desired_returns_empty() {
    let desired = serde_yaml::Value::String("not a mapping".into());
    let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn diff_nested_mapping_null_desired_returns_empty() {
    let desired = serde_yaml::Value::Null;
    let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn diff_nested_mapping_empty_outer_mapping() {
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn diff_nested_mapping_inner_not_mapping_is_skipped() {
    // Outer key is valid, but inner value is a string instead of a mapping
    let mut outer = serde_yaml::Mapping::new();
    outer.insert(
        serde_yaml::Value::String("schema".into()),
        serde_yaml::Value::String("not a mapping".into()),
    );
    let desired = serde_yaml::Value::Mapping(outer);

    let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn diff_nested_mapping_non_string_outer_key_is_skipped() {
    let mut outer = serde_yaml::Mapping::new();
    let mut inner = serde_yaml::Mapping::new();
    inner.insert(
        serde_yaml::Value::String("key".into()),
        serde_yaml::Value::String("val".into()),
    );
    // Numeric outer key — should be skipped
    outer.insert(
        serde_yaml::Value::Number(99.into()),
        serde_yaml::Value::Mapping(inner),
    );
    let desired = serde_yaml::Value::Mapping(outer);

    let drifts = diff_nested_mapping(&desired, |_, _| String::new()).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn diff_nested_mapping_detects_drift_with_prefix() {
    let mut inner = serde_yaml::Mapping::new();
    inner.insert(
        serde_yaml::Value::String("color-scheme".into()),
        serde_yaml::Value::String("prefer-dark".into()),
    );
    let mut outer = serde_yaml::Mapping::new();
    outer.insert(
        serde_yaml::Value::String("org.gnome.desktop.interface".into()),
        serde_yaml::Value::Mapping(inner),
    );
    let desired = serde_yaml::Value::Mapping(outer);

    let drifts = diff_nested_mapping(&desired, |_schema, _key| "prefer-light".to_string()).unwrap();
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "org.gnome.desktop.interface.color-scheme");
    assert_eq!(drifts[0].expected, "prefer-dark");
    assert_eq!(drifts[0].actual, "prefer-light");
}

#[test]
fn diff_nested_mapping_no_drift_when_matching() {
    let mut inner = serde_yaml::Mapping::new();
    inner.insert(
        serde_yaml::Value::String("key1".into()),
        serde_yaml::Value::String("val1".into()),
    );
    let mut outer = serde_yaml::Mapping::new();
    outer.insert(
        serde_yaml::Value::String("prefix".into()),
        serde_yaml::Value::Mapping(inner),
    );
    let desired = serde_yaml::Value::Mapping(outer);

    let drifts = diff_nested_mapping(&desired, |_prefix, _key| "val1".to_string()).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn diff_nested_mapping_multiple_schemas_and_keys() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
org.gnome.desktop.interface:
  color-scheme: prefer-dark
  font-name: "Cantarell 11"
org.gnome.desktop.wm.preferences:
  button-layout: close,minimize,maximize
"#,
    )
    .unwrap();

    let drifts = diff_nested_mapping(&yaml, |schema, key| {
        // Return matching value for one key, mismatched for the rest
        if schema == "org.gnome.desktop.interface" && key == "font-name" {
            "Cantarell 11".to_string()
        } else {
            "wrong".to_string()
        }
    })
    .unwrap();

    // font-name matches, so only color-scheme and button-layout should drift
    assert_eq!(drifts.len(), 2);
    let keys: Vec<&str> = drifts.iter().map(|d| d.key.as_str()).collect();
    assert!(keys.contains(&"org.gnome.desktop.interface.color-scheme"));
    assert!(keys.contains(&"org.gnome.desktop.wm.preferences.button-layout"));
}

#[test]
fn diff_nested_mapping_passes_outer_key_to_get_actual() {
    // Verify that the get_actual closure receives the correct (prefix, key) arguments
    let mut inner = serde_yaml::Mapping::new();
    inner.insert(
        serde_yaml::Value::String("prop".into()),
        serde_yaml::Value::String("desired".into()),
    );
    let mut outer = serde_yaml::Mapping::new();
    outer.insert(
        serde_yaml::Value::String("channel".into()),
        serde_yaml::Value::Mapping(inner),
    );
    let desired = serde_yaml::Value::Mapping(outer);

    let drifts = diff_nested_mapping(&desired, |prefix, key| {
        // Echo back the arguments so we can verify they were correct
        format!("{}:{}", prefix, key)
    })
    .unwrap();

    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].actual, "channel:prop");
}

#[test]
fn parse_reg_line_typical_entry() {
    let line = "    MyValue    REG_SZ    hello world";
    let result = parse_reg_line(line);
    assert_eq!(result, Some(("MyValue", "REG_SZ", "hello world")));
}

#[test]
fn parse_reg_line_empty_string() {
    assert_eq!(parse_reg_line(""), None);
}

#[test]
fn parse_reg_line_whitespace_only() {
    assert_eq!(parse_reg_line("   "), None);
}

#[test]
fn parse_reg_line_hkey_header_line() {
    assert_eq!(parse_reg_line("HKEY_CURRENT_USER\\Software\\Test"), None);
}

#[test]
fn parse_reg_line_dword_value() {
    let line = "    Timeout    REG_DWORD    0xff";
    let result = parse_reg_line(line);
    assert_eq!(result, Some(("Timeout", "REG_DWORD", "0xff")));
}

#[test]
fn parse_reg_line_fewer_than_three_parts() {
    // A line without proper 4-space separators
    let line = "just some text";
    assert_eq!(parse_reg_line(line), None);
}

#[test]
fn yaml_value_to_string_null() {
    let result = yaml_value_to_string(&serde_yaml::Value::Null);
    // Null goes through the Debug fallback
    assert!(!result.is_empty());
}

#[test]
fn yaml_value_to_string_sequence() {
    let seq = serde_yaml::Value::Sequence(vec![
        serde_yaml::Value::String("a".into()),
        serde_yaml::Value::String("b".into()),
    ]);
    let result = yaml_value_to_string(&seq);
    // Sequences go through the Debug fallback
    assert!(result.contains("a"));
    assert!(result.contains("b"));
}

#[test]
fn yaml_value_with_numeric_bools_null_uses_debug() {
    let result = yaml_value_with_numeric_bools(&serde_yaml::Value::Null);
    // Null goes through the Debug fallback
    assert!(!result.is_empty());
}

#[test]
fn yaml_value_with_numeric_bools_sequence_uses_debug() {
    let seq = serde_yaml::Value::Sequence(vec![]);
    let result = yaml_value_with_numeric_bools(&seq);
    assert!(!result.is_empty());
}

#[test]
fn parse_reg_line_hkey_local_machine() {
    assert_eq!(parse_reg_line("HKEY_LOCAL_MACHINE\\Software\\Test"), None);
}

#[test]
fn parse_reg_line_with_spaces_in_value() {
    let line = "    MyPath    REG_SZ    C:\\Program Files\\App";
    let result = parse_reg_line(line);
    assert_eq!(result, Some(("MyPath", "REG_SZ", "C:\\Program Files\\App")));
}

#[test]
fn diff_yaml_mapping_float_value() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("opacity".into()),
        serde_yaml::Value::Number(serde_yaml::Number::from(0.85_f64)),
    );

    let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| "1.0".to_string());
    assert_eq!(drifts.len(), 1);
    assert!(drifts[0].expected.starts_with("0.85"));
    assert_eq!(drifts[0].actual, "1.0");
}

#[test]
fn diff_nested_mapping_multiple_inner_keys_partial_match() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
schema1:
  key-a: val-a
  key-b: val-b
  key-c: val-c
"#,
    )
    .unwrap();

    let drifts = diff_nested_mapping(&yaml, |_schema, key| {
        match key {
            "key-a" => "val-a", // matches
            "key-b" => "wrong", // drift
            "key-c" => "val-c", // matches
            _ => "",
        }
        .to_string()
    })
    .unwrap();

    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "schema1.key-b");
    assert_eq!(drifts[0].expected, "val-b");
    assert_eq!(drifts[0].actual, "wrong");
}

#[test]
fn yaml_value_with_numeric_bools_float() {
    let float_val = serde_yaml::Value::Number(serde_yaml::Number::from(1.234_f64));
    let result = yaml_value_with_numeric_bools(&float_val);
    assert!(result.starts_with("1.234"));
}

#[test]
fn diff_yaml_mapping_numeric_bools_true_matches_1() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("toggle".into()),
        serde_yaml::Value::Bool(true),
    );
    // With yaml_value_with_numeric_bools, true becomes "1"
    let drifts = diff_yaml_mapping(&desired, "", yaml_value_with_numeric_bools, |_| {
        "1".to_string()
    });
    assert!(
        drifts.is_empty(),
        "true should match '1' with numeric bools converter"
    );
}

#[test]
fn diff_yaml_mapping_string_bools_true_does_not_match_1() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("toggle".into()),
        serde_yaml::Value::Bool(true),
    );
    // With yaml_value_to_string, true becomes "true" which does not match "1"
    let drifts = diff_yaml_mapping(&desired, "", yaml_value_to_string, |_| "1".to_string());
    assert_eq!(
        drifts.len(),
        1,
        "true should NOT match '1' with yaml_value_to_string"
    );
    assert_eq!(drifts[0].expected, "true");
    assert_eq!(drifts[0].actual, "1");
}

#[test]
fn diff_yaml_mapping_all_keys_drift() {
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("a".into()),
        serde_yaml::Value::String("x".into()),
    );
    desired.insert(
        serde_yaml::Value::String("b".into()),
        serde_yaml::Value::String("y".into()),
    );
    desired.insert(
        serde_yaml::Value::String("c".into()),
        serde_yaml::Value::String("z".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "prefix", yaml_value_to_string, |_| String::new());
    assert_eq!(drifts.len(), 3);
    // All drift keys should have the prefix
    for d in &drifts {
        assert!(
            d.key.starts_with("prefix."),
            "drift key '{}' missing prefix",
            d.key
        );
    }
}

#[test]
fn diff_yaml_mapping_get_actual_receives_bare_key() {
    // Verify the get_actual closure receives the raw key, not prefixed
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("mykey".into()),
        serde_yaml::Value::String("val".into()),
    );

    let drifts = diff_yaml_mapping(&desired, "ns", yaml_value_to_string, |k| {
        // Echo back the key we received to verify it's the raw key
        assert_eq!(k, "mykey", "get_actual should receive the bare key");
        "val".to_string()
    });
    assert!(drifts.is_empty());
}

#[test]
fn diff_nested_mapping_two_schemas_all_drift() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
schema-a:
  key1: expected1
schema-b:
  key2: expected2
"#,
    )
    .unwrap();

    let drifts = diff_nested_mapping(&yaml, |_, _| "wrong".to_string()).unwrap();
    assert_eq!(drifts.len(), 2);
    let keys: Vec<&str> = drifts.iter().map(|d| d.key.as_str()).collect();
    assert!(keys.contains(&"schema-a.key1"));
    assert!(keys.contains(&"schema-b.key2"));
}

#[test]
fn diff_nested_mapping_mixed_matching_across_schemas() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
schemaA:
  k1: v1
  k2: v2
schemaB:
  k3: v3
"#,
    )
    .unwrap();

    let drifts = diff_nested_mapping(&yaml, |schema, key| {
        // schemaA.k1 matches, everything else drifts
        if schema == "schemaA" && key == "k1" {
            "v1".to_string()
        } else {
            "mismatch".to_string()
        }
    })
    .unwrap();

    assert_eq!(drifts.len(), 2);
    let keys: Vec<&str> = drifts.iter().map(|d| d.key.as_str()).collect();
    assert!(keys.contains(&"schemaA.k2"));
    assert!(keys.contains(&"schemaB.k3"));
}

#[test]
fn parse_reg_line_hkey_classes_root() {
    assert_eq!(
        parse_reg_line("HKEY_CLASSES_ROOT\\.txt"),
        None,
        "HKEY_CLASSES_ROOT lines should be filtered"
    );
}

#[test]
fn parse_reg_line_hkey_users() {
    assert_eq!(
        parse_reg_line("HKEY_USERS\\.DEFAULT"),
        None,
        "HKEY_USERS lines should be filtered"
    );
}

#[test]
fn parse_reg_line_expand_sz_type() {
    let line = "    Path    REG_EXPAND_SZ    %SystemRoot%\\system32";
    let result = parse_reg_line(line);
    assert_eq!(
        result,
        Some(("Path", "REG_EXPAND_SZ", "%SystemRoot%\\system32"))
    );
}

#[test]
fn parse_reg_line_multi_sz_type() {
    let line = "    MultiVal    REG_MULTI_SZ    val1\\0val2";
    let result = parse_reg_line(line);
    assert_eq!(result, Some(("MultiVal", "REG_MULTI_SZ", "val1\\0val2")));
}

#[test]
fn yaml_value_with_numeric_bools_mapping_uses_debug() {
    let mapping = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let result = yaml_value_with_numeric_bools(&mapping);
    // The Debug format for an empty mapping
    assert!(!result.is_empty());
}

#[test]
fn yaml_value_with_numeric_bools_empty_string() {
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::String(String::new())),
        ""
    );
}

#[test]
fn yaml_value_with_numeric_bools_negative_number() {
    let n = serde_yaml::Value::Number(serde_yaml::Number::from(-5));
    assert_eq!(yaml_value_with_numeric_bools(&n), "-5");
}

#[test]
fn yaml_value_to_string_mapping_uses_debug() {
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("k".into()),
        serde_yaml::Value::String("v".into()),
    );
    let result = yaml_value_to_string(&serde_yaml::Value::Mapping(m));
    // Debug format includes the key and value
    assert!(result.contains("k"));
    assert!(result.contains("v"));
}

#[test]
fn yaml_value_to_string_empty_string() {
    assert_eq!(
        yaml_value_to_string(&serde_yaml::Value::String(String::new())),
        ""
    );
}

#[test]
fn yaml_value_to_string_negative_number() {
    let n = serde_yaml::Value::Number(serde_yaml::Number::from(-42));
    assert_eq!(yaml_value_to_string(&n), "-42");
}

#[test]
fn yaml_value_to_string_float() {
    let f = serde_yaml::Value::Number(serde_yaml::Number::from(1.23_f64));
    let result = yaml_value_to_string(&f);
    assert!(result.starts_with("1.23"));
}

#[test]
fn read_command_output_successful_command() {
    let output = read_command_output(Command::new("echo").arg("hello"));
    assert_eq!(output, "hello");
}

#[test]
fn read_command_output_trims_trailing_newline() {
    // echo outputs "hello\n" but read_command_output should trim it
    let output = read_command_output(Command::new("echo").arg("  spaced  "));
    assert_eq!(output, "spaced");
}

#[test]
fn read_command_output_failed_command_returns_empty() {
    let output = read_command_output(&mut Command::new("false"));
    assert_eq!(output, "");
}

#[test]
fn read_command_output_nonexistent_command_returns_empty() {
    let output = read_command_output(&mut Command::new("cfgd_nonexistent_cmd_12345"));
    assert_eq!(output, "");
}

#[test]
#[cfg(not(windows))] // printf with embedded \n is unreliable on Windows
fn read_command_output_multiline_output() {
    // printf produces multiline output without trailing newline issues
    let output = read_command_output(Command::new("printf").arg("line1\nline2"));
    assert_eq!(output, "line1\nline2");
}

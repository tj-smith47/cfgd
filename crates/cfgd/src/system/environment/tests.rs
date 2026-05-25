use super::*;

#[test]
fn environment_desired_vars_parsing() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
HTTP_PROXY: "http://proxy.example.com:8080"
LANG: "en_US.UTF-8"
MAX_CONNECTIONS: 100
DEBUG: true
"#,
    )
    .unwrap();

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    assert_eq!(vars.len(), 4);
    assert_eq!(vars["HTTP_PROXY"], "http://proxy.example.com:8080");
    assert_eq!(vars["LANG"], "en_US.UTF-8");
    assert_eq!(vars["MAX_CONNECTIONS"], "100");
    assert_eq!(vars["DEBUG"], "true");
}

#[test]
fn environment_desired_vars_empty_or_wrong_type() {
    for yaml in &[
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        serde_yaml::Value::String("not a mapping".to_string()),
    ] {
        let vars = EnvironmentConfigurator::desired_vars(yaml);
        assert!(vars.is_empty(), "should be empty for {:?}", yaml);
    }
}

#[test]
fn environment_diff_detects_missing() {
    let ec = EnvironmentConfigurator;
    // Use a unique env var name that definitely doesn't exist
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
CFGD_TEST_NONEXISTENT_VAR_12345: "test_value"
"#,
    )
    .unwrap();

    let drifts = ec.diff(&yaml).unwrap();
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "CFGD_TEST_NONEXISTENT_VAR_12345");
    assert_eq!(drifts[0].expected, "test_value");
    assert!(drifts[0].actual.is_empty());
}

#[test]
fn windows_reg_query_parsing_typical() {
    let output = "\
HKEY_CURRENT_USER\\Environment\n\
\n\
    EDITOR    REG_SZ    code\n\
    GOPATH    REG_SZ    C:\\Users\\user\\go\n\
    Path    REG_EXPAND_SZ    C:\\Users\\user\\.cargo\\bin;%PATH%\n";

    let vars = EnvironmentConfigurator::parse_reg_query_output(output);
    assert_eq!(vars.len(), 3);
    assert_eq!(vars["EDITOR"], "code");
    assert_eq!(vars["GOPATH"], r"C:\Users\user\go");
    assert_eq!(vars["Path"], r"C:\Users\user\.cargo\bin;%PATH%");
}

#[test]
fn windows_reg_query_parsing_empty() {
    let output = "HKEY_CURRENT_USER\\Environment\n\n";
    let vars = EnvironmentConfigurator::parse_reg_query_output(output);
    assert!(vars.is_empty());
}

#[test]
fn windows_reg_query_parsing_blank_input() {
    let vars = EnvironmentConfigurator::parse_reg_query_output("");
    assert!(vars.is_empty());
}

#[test]
fn windows_reg_query_parsing_single_var() {
    let output = "HKEY_CURRENT_USER\\Environment\n\
                       \n\
                           JAVA_HOME    REG_SZ    C:\\Program Files\\Java\\jdk-17\n";
    let vars = EnvironmentConfigurator::parse_reg_query_output(output);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["JAVA_HOME"], r"C:\Program Files\Java\jdk-17");
}

#[test]
fn environment_configurator_available_on_linux() {
    let ec = EnvironmentConfigurator;
    assert!(ec.is_available());
}

#[test]
fn parse_reg_query_output_expand_sz_type() {
    let output = "HKEY_CURRENT_USER\\Environment\n\
                      \n\
                          Path    REG_EXPAND_SZ    %USERPROFILE%\\bin\n";
    let vars = EnvironmentConfigurator::parse_reg_query_output(output);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["Path"], r"%USERPROFILE%\bin");
}

#[test]
fn parse_reg_query_output_mixed_types() {
    let output = "HKEY_CURRENT_USER\\Environment\n\
                      \n\
                          EDITOR    REG_SZ    vim\n\
                          PATH    REG_EXPAND_SZ    C:\\bin;%PATH%\n\
                          COUNT    REG_DWORD    0x5\n";
    let vars = EnvironmentConfigurator::parse_reg_query_output(output);
    assert_eq!(vars.len(), 3);
    assert_eq!(vars["EDITOR"], "vim");
    assert_eq!(vars["PATH"], r"C:\bin;%PATH%");
    // DWORD is parsed as raw value string, not converted
    assert_eq!(vars["COUNT"], "0x5");
}

#[test]
fn environment_desired_vars_skips_complex_values() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
SIMPLE: "value"
NESTED:
  inner: "should be skipped"
LIST:
  - "also skipped"
"#,
    )
    .unwrap();

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["SIMPLE"], "value");
}

#[test]
fn environment_desired_vars_non_string_keys_skipped() {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::Number(42.into()),
        serde_yaml::Value::String("value".into()),
    );
    mapping.insert(
        serde_yaml::Value::String("VALID".into()),
        serde_yaml::Value::String("ok".into()),
    );
    let yaml = serde_yaml::Value::Mapping(mapping);

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["VALID"], "ok");
}

#[test]
fn parse_env_file_standard_entries() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "LANG=en_US.UTF-8\nPATH=/usr/bin\n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(vars.len(), 2);
    assert_eq!(vars["LANG"], "en_US.UTF-8");
    assert_eq!(vars["PATH"], "/usr/bin");
}

#[test]
fn parse_env_file_quoted_values() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "EDITOR=\"vim\"\nSHELL=\"/bin/bash\"\n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(vars["EDITOR"], "vim");
    assert_eq!(vars["SHELL"], "/bin/bash");
}

#[test]
fn parse_env_file_skips_comments_and_blanks() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "# comment line\n\n  \nKEY=value\n# another\n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["KEY"], "value");
}

#[test]
fn parse_env_file_nonexistent_returns_empty() {
    let vars = EnvironmentConfigurator::parse_env_file("/nonexistent/path/env");
    assert!(vars.is_empty());
}

#[test]
fn parse_env_file_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert!(vars.is_empty());
}

#[test]
fn parse_env_file_value_with_equals() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "OPTS=--key=value\n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(vars["OPTS"], "--key=value");
}

#[test]
fn parse_export_file_standard_entries() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(
        &file_path,
        "#!/bin/sh\nexport FOO=\"bar\"\nexport BAZ='qux'\n",
    )
    .unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert_eq!(vars.len(), 2);
    assert_eq!(vars["FOO"], "bar");
    assert_eq!(vars["BAZ"], "qux");
}

#[test]
fn parse_export_file_unquoted_values() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(&file_path, "export LANG=en_US.UTF-8\n").unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert_eq!(vars["LANG"], "en_US.UTF-8");
}

#[test]
fn parse_export_file_nonexistent_returns_empty() {
    let vars = EnvironmentConfigurator::parse_export_file("/nonexistent/env.sh");
    assert!(vars.is_empty());
}

#[test]
fn parse_export_file_skips_non_export_lines() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(
        &file_path,
        "#!/bin/sh\n# comment\nSOMETHING=not_exported\nexport REAL=yes\n",
    )
    .unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["REAL"], "yes");
}

#[test]
fn parse_reg_query_output_dword_preserved_as_raw() {
    // parse_reg_query_output uses parse_reg_line which returns raw value
    let output = "HKEY_CURRENT_USER\\Environment\n\
                      \n\
                          Count    REG_DWORD    0x5\n";
    let vars = EnvironmentConfigurator::parse_reg_query_output(output);
    // The raw DWORD hex value is preserved
    assert_eq!(vars["Count"], "0x5");
}

#[test]
fn environment_current_state_returns_mapping() {
    let ec = EnvironmentConfigurator;
    let state = ec.current_state().unwrap();
    assert!(state.is_mapping());
}

#[test]
fn environment_diff_matching_values_no_drift() {
    // We can only test this reliably by setting an env var that matches
    // On Linux, parse_env_file/parse_export_file are used
    let ec = EnvironmentConfigurator;
    let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let drifts = ec.diff(&yaml).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn environment_diff_non_mapping_desired_returns_empty() {
    let ec = EnvironmentConfigurator;
    let yaml = serde_yaml::Value::String("not a mapping".into());
    let drifts = ec.diff(&yaml).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn write_etc_environment_creates_managed_block() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");

    let mut managed = BTreeMap::new();
    managed.insert("EDITOR".to_string(), "vim".to_string());
    managed.insert("LANG".to_string(), "en_US.UTF-8".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    assert!(
        content.contains(CFGD_BLOCK_BEGIN),
        "missing block begin marker"
    );
    assert!(content.contains(CFGD_BLOCK_END), "missing block end marker");
    assert!(content.contains("EDITOR=vim\n"));
    assert!(content.contains("LANG=en_US.UTF-8\n"));
}

#[test]
fn write_etc_environment_preserves_existing_non_managed_lines() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");
    // Pre-existing content that should be preserved
    std::fs::write(&env_path, "PATH=/usr/bin\nHOME=/root\n").unwrap();

    let mut managed = BTreeMap::new();
    managed.insert("EDITOR".to_string(), "vim".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    assert!(content.contains("PATH=/usr/bin"), "existing PATH preserved");
    assert!(content.contains("HOME=/root"), "existing HOME preserved");
    assert!(content.contains("EDITOR=vim"), "managed var added");
}

#[test]
fn write_etc_environment_replaces_existing_managed_block() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");
    // Pre-existing file with an old cfgd block
    std::fs::write(
        &env_path,
        format!(
            "PATH=/usr/bin\n{}\nOLD_VAR=old\n{}\n",
            CFGD_BLOCK_BEGIN, CFGD_BLOCK_END
        ),
    )
    .unwrap();

    let mut managed = BTreeMap::new();
    managed.insert("NEW_VAR".to_string(), "new".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    assert!(
        !content.contains("OLD_VAR"),
        "old managed var should be replaced"
    );
    assert!(content.contains("NEW_VAR=new"), "new managed var added");
    assert!(content.contains("PATH=/usr/bin"), "non-managed preserved");
    // Only one block begin/end pair
    assert_eq!(
        content.matches(CFGD_BLOCK_BEGIN).count(),
        1,
        "should have exactly one begin marker"
    );
}

#[test]
fn write_etc_environment_removes_managed_keys_outside_block() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");
    // EDITOR exists outside the block — should be removed when managed
    std::fs::write(&env_path, "EDITOR=nano\nPATH=/usr/bin\n").unwrap();

    let mut managed = BTreeMap::new();
    managed.insert("EDITOR".to_string(), "vim".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    assert!(
        !content.contains("EDITOR=nano"),
        "old EDITOR outside block should be removed"
    );
    assert!(content.contains("EDITOR=vim"), "managed EDITOR in block");
    assert!(content.contains("PATH=/usr/bin"), "unmanaged preserved");
}

#[test]
fn write_etc_environment_quotes_values_with_special_chars() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");

    let mut managed = BTreeMap::new();
    managed.insert("OPTS".to_string(), "has spaces".to_string());
    managed.insert("COMMENT".to_string(), "has#hash".to_string());
    managed.insert("EXPAND".to_string(), "$HOME/bin".to_string());
    managed.insert("SIMPLE".to_string(), "noquotes".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    assert!(
        content.contains("OPTS=\"has spaces\""),
        "space-containing value should be quoted, got: {}",
        content
    );
    assert!(
        content.contains("COMMENT=\"has#hash\""),
        "hash-containing value should be quoted"
    );
    assert!(
        content.contains("EXPAND=\"$HOME/bin\""),
        "dollar-containing value should be quoted"
    );
    assert!(
        content.contains("SIMPLE=noquotes\n"),
        "simple value should NOT be quoted"
    );
}

#[test]
fn write_etc_environment_empty_managed_removes_block() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");
    std::fs::write(
        &env_path,
        format!(
            "PATH=/usr/bin\n{}\nVAR=val\n{}\n",
            CFGD_BLOCK_BEGIN, CFGD_BLOCK_END
        ),
    )
    .unwrap();

    let managed = BTreeMap::new();
    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    assert!(
        !content.contains(CFGD_BLOCK_BEGIN),
        "empty managed should remove block"
    );
    assert!(content.contains("PATH=/usr/bin"), "non-managed preserved");
}

#[test]
fn write_etc_environment_escapes_backslashes_and_quotes_in_values() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");

    let mut managed = BTreeMap::new();
    managed.insert(
        "TRICKY".to_string(),
        r#"has "quotes" and \ slashes"#.to_string(),
    );

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    // The value has both " and space, so it gets quoted with escaping
    assert!(
        content.contains(r#"TRICKY="has \"quotes\" and \\ slashes""#),
        "backslashes and quotes should be escaped, got: {}",
        content
    );
}

#[test]
fn write_profile_d_creates_shell_exports() {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("cfgd-env.sh");

    let mut managed = BTreeMap::new();
    managed.insert("EDITOR".to_string(), "vim".to_string());
    managed.insert("LANG".to_string(), "en_US.UTF-8".to_string());

    EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();

    let content = std::fs::read_to_string(&profile_path).unwrap();
    assert!(content.starts_with("#!/bin/sh\n"), "missing shebang");
    assert!(
        content.contains("# Managed by cfgd"),
        "missing header comment"
    );
    // shell_escape_value always quotes values
    assert!(
        content.contains("export EDITOR=") && content.contains("vim"),
        "missing EDITOR export, got: {}",
        content
    );
    assert!(
        content.contains("export LANG=") && content.contains("en_US.UTF-8"),
        "missing LANG export, got: {}",
        content
    );
}

#[test]
fn write_profile_d_shell_escapes_values() {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("cfgd-env.sh");

    let mut managed = BTreeMap::new();
    managed.insert("OPTS".to_string(), "has spaces and $vars".to_string());

    EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();

    let content = std::fs::read_to_string(&profile_path).unwrap();
    // shell_escape_value should single-quote values with metacharacters
    assert!(
        content.contains("export OPTS='has spaces and $vars'"),
        "value with metacharacters should be shell-escaped, got: {}",
        content
    );
}

#[test]
fn write_profile_d_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("deep").join("nested").join("cfgd-env.sh");

    let mut managed = BTreeMap::new();
    managed.insert("KEY".to_string(), "val".to_string());

    EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();
    assert!(profile_path.exists());
}

#[test]
fn write_profile_d_empty_managed_removes_file() {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("cfgd-env.sh");
    std::fs::write(&profile_path, "old content").unwrap();

    let managed = BTreeMap::new();
    EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();
    assert!(
        !profile_path.exists(),
        "empty managed should remove the file"
    );
}

#[test]
fn parse_reg_query_output_multiple_types_preserved() {
    let output = "\
HKEY_CURRENT_USER\\Environment\n\
\n\
    Editor    REG_SZ    vim\n\
    NumProcs    REG_DWORD    0x4\n\
    ExpandPath    REG_EXPAND_SZ    %HOME%\\bin\n";

    let vars = EnvironmentConfigurator::parse_reg_query_output(output);
    assert_eq!(vars.len(), 3);
    assert_eq!(vars["Editor"], "vim");
    // parse_reg_query_output preserves raw DWORD hex
    assert_eq!(vars["NumProcs"], "0x4");
    assert_eq!(vars["ExpandPath"], "%HOME%\\bin");
}

#[test]
fn parse_env_file_lines_without_equals_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "VALID=yes\nNO_EQUALS_HERE\nALSO_VALID=true\n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(vars.len(), 2);
    assert_eq!(vars["VALID"], "yes");
    assert_eq!(vars["ALSO_VALID"], "true");
}

#[test]
fn parse_env_file_whitespace_around_key_and_value() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "  KEY  =  value  \n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(vars["KEY"], "value");
}

#[test]
fn parse_env_file_empty_value() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "EMPTY=\n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(vars["EMPTY"], "");
}

#[test]
fn parse_env_file_only_comments() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "# comment 1\n# comment 2\n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert!(vars.is_empty());
}

#[test]
fn parse_export_file_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(&file_path, "").unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert!(vars.is_empty());
}

#[test]
fn parse_export_file_mixed_quote_styles() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(
        &file_path,
        "export DOUBLE=\"double_val\"\nexport SINGLE='single_val'\nexport NONE=bare_val\n",
    )
    .unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert_eq!(vars.len(), 3);
    assert_eq!(vars["DOUBLE"], "double_val");
    assert_eq!(vars["SINGLE"], "single_val");
    assert_eq!(vars["NONE"], "bare_val");
}

#[test]
fn parse_export_file_value_with_equals_sign() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(&file_path, "export OPTS=\"--key=value\"\n").unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert_eq!(vars["OPTS"], "--key=value");
}

#[test]
fn parse_export_file_ignores_comments_and_blank_lines() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(
        &file_path,
        "#!/bin/sh\n# a comment\n\nexport REAL=\"yes\"\n\n# tail\n",
    )
    .unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["REAL"], "yes");
}

#[test]
fn desired_vars_null_values_skipped() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
VALID: "value"
NULL_KEY: null
"#,
    )
    .unwrap();

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["VALID"], "value");
    assert!(
        !vars.contains_key("NULL_KEY"),
        "null values should be skipped"
    );
}

#[test]
fn desired_vars_bool_converted_to_string() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
FLAG_TRUE: true
FLAG_FALSE: false
"#,
    )
    .unwrap();

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    assert_eq!(vars["FLAG_TRUE"], "true");
    assert_eq!(vars["FLAG_FALSE"], "false");
}

#[test]
fn desired_vars_number_types() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
INT_VAR: 42
FLOAT_VAR: 3.14
NEGATIVE: -10
"#,
    )
    .unwrap();

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    assert_eq!(vars["INT_VAR"], "42");
    assert!(vars["FLOAT_VAR"].starts_with("3.14"));
    assert_eq!(vars["NEGATIVE"], "-10");
}

#[test]
fn desired_vars_sequence_and_mapping_skipped() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
GOOD: "ok"
LIST_VAL:
  - "item1"
MAP_VAL:
  nested: "inner"
"#,
    )
    .unwrap();

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["GOOD"], "ok");
}

#[test]
fn desired_vars_preserves_insertion_order_via_btreemap() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
ZEBRA: "z"
ALPHA: "a"
MIDDLE: "m"
"#,
    )
    .unwrap();

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    let keys: Vec<&String> = vars.keys().collect();
    // BTreeMap sorts lexicographically
    assert_eq!(keys, vec!["ALPHA", "MIDDLE", "ZEBRA"]);
}

#[test]
fn write_etc_environment_creates_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("new_environment");
    assert!(!env_path.exists());

    let mut managed = BTreeMap::new();
    managed.insert("NEW_VAR".to_string(), "new_value".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    assert!(content.contains(CFGD_BLOCK_BEGIN));
    assert!(content.contains("NEW_VAR=new_value\n"));
    assert!(content.contains(CFGD_BLOCK_END));
}

#[test]
fn write_etc_environment_empty_managed_on_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("new_environment");

    let managed = BTreeMap::new();
    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    // No block markers for empty managed set
    assert!(!content.contains(CFGD_BLOCK_BEGIN));
    // File should be empty or just a newline
    assert!(content.trim().is_empty());
}

#[test]
fn environment_apply_empty_desired_is_noop() {
    let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
    let ec = EnvironmentConfigurator;
    let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    // Should return Ok(()) without writing anything
    ec.apply(&yaml, &printer).unwrap();
}

#[test]
fn environment_apply_non_mapping_is_noop() {
    let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
    let ec = EnvironmentConfigurator;
    let yaml = serde_yaml::Value::String("not a mapping".into());
    ec.apply(&yaml, &printer).unwrap();
}

#[test]
fn write_etc_environment_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");

    let mut managed = BTreeMap::new();
    managed.insert("FOO".to_string(), "bar".to_string());
    managed.insert("BAZ".to_string(), "qux".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();
    let content1 = std::fs::read_to_string(&env_path).unwrap();

    // Write again with same vars — should produce identical output
    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();
    let content2 = std::fs::read_to_string(&env_path).unwrap();

    assert_eq!(content1, content2, "writing same vars should be idempotent");
}

#[test]
fn write_profile_d_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("env.sh");

    let mut managed = BTreeMap::new();
    managed.insert("A".to_string(), "1".to_string());

    EnvironmentConfigurator::write_profile_d_to(&path, &managed).unwrap();
    let content1 = std::fs::read_to_string(&path).unwrap();

    EnvironmentConfigurator::write_profile_d_to(&path, &managed).unwrap();
    let content2 = std::fs::read_to_string(&path).unwrap();

    assert_eq!(content1, content2);
}

#[test]
fn environment_windows_current_vars_empty_on_non_windows() {
    let vars = EnvironmentConfigurator::windows_current_vars();
    assert!(vars.is_empty());
}

#[test]
fn write_etc_environment_escapes_double_quotes_in_value() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");

    let mut managed = BTreeMap::new();
    managed.insert("QUOTED".to_string(), r#"say "hello""#.to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    // The value contains a double-quote, which triggers quoting + escaping
    assert!(
        content.contains(r#"QUOTED="say \"hello\"""#),
        "double quotes in value should be escaped, got: {}",
        content
    );
}

#[test]
fn macos_env_sh_path_is_under_default_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());
    let p = EnvironmentConfigurator::macos_env_sh_path();
    assert_eq!(p, dir.path().join(".config").join("cfgd").join("env.sh"));
}

#[test]
fn macos_plist_path_is_under_home_library_launchagents() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());
    let p = EnvironmentConfigurator::macos_plist_path();
    assert_eq!(
        p,
        dir.path()
            .join("Library")
            .join("LaunchAgents")
            .join("com.cfgd.environment.plist")
    );
}

#[test]
fn macos_write_env_sh_creates_file_with_managed_block() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());

    let mut managed = BTreeMap::new();
    managed.insert("FOO".to_string(), "bar".to_string());
    managed.insert("LANG".to_string(), "en_US.UTF-8".to_string());

    EnvironmentConfigurator::macos_write_env_sh(&managed).unwrap();

    let env_sh = dir.path().join(".config/cfgd/env.sh");
    let content = std::fs::read_to_string(&env_sh).unwrap();
    assert!(content.starts_with("#!/bin/sh\n"), "got: {content}");
    assert!(content.contains("Managed by cfgd"));
    assert!(content.contains("Source this from your shell rc"));
    // shell_escape_value always wraps the value in quotes (`"..."`).
    assert!(content.contains(r#"export FOO="bar""#), "got: {content}");
    assert!(
        content.contains(r#"export LANG="en_US.UTF-8""#),
        "got: {content}"
    );
}

#[test]
fn macos_write_env_sh_empty_managed_removes_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());
    let env_sh = dir.path().join(".config/cfgd/env.sh");
    std::fs::create_dir_all(env_sh.parent().unwrap()).unwrap();
    std::fs::write(&env_sh, "stale\n").unwrap();
    assert!(env_sh.exists());

    EnvironmentConfigurator::macos_write_env_sh(&BTreeMap::new()).unwrap();
    assert!(!env_sh.exists(), "empty managed should have removed env.sh");
}

#[test]
fn macos_write_env_sh_empty_managed_when_file_missing_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());
    EnvironmentConfigurator::macos_write_env_sh(&BTreeMap::new()).unwrap();
    let env_sh = dir.path().join(".config/cfgd/env.sh");
    assert!(!env_sh.exists());
}

#[test]
fn macos_write_env_sh_creates_parent_dir() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());
    // Parent .config/cfgd does not exist yet
    assert!(!dir.path().join(".config").exists());

    let mut managed = BTreeMap::new();
    managed.insert("X".to_string(), "1".to_string());
    EnvironmentConfigurator::macos_write_env_sh(&managed).unwrap();

    assert!(dir.path().join(".config/cfgd/env.sh").exists());
}

#[test]
fn macos_current_vars_roundtrips_through_env_sh() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());

    let mut managed = BTreeMap::new();
    managed.insert("FOO".to_string(), "bar".to_string());
    managed.insert("LANG".to_string(), "C".to_string());
    EnvironmentConfigurator::macos_write_env_sh(&managed).unwrap();

    let read_back = EnvironmentConfigurator::macos_current_vars();
    assert_eq!(read_back.get("FOO").map(String::as_str), Some("bar"));
    assert_eq!(read_back.get("LANG").map(String::as_str), Some("C"));
}

#[test]
fn macos_current_vars_empty_when_file_missing() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());
    let vars = EnvironmentConfigurator::macos_current_vars();
    assert!(vars.is_empty());
}

#[test]
fn macos_write_launchd_plist_writes_well_formed_xml() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());

    let mut managed = BTreeMap::new();
    managed.insert(
        "HTTP_PROXY".to_string(),
        "http://proxy.corp:8080".to_string(),
    );

    EnvironmentConfigurator::macos_write_launchd_plist(&managed).unwrap();

    let plist = dir
        .path()
        .join("Library/LaunchAgents/com.cfgd.environment.plist");
    let content = std::fs::read_to_string(&plist).unwrap();
    assert!(content.starts_with(r#"<?xml version="1.0" encoding="UTF-8"?>"#));
    assert!(content.contains("<key>Label</key>"));
    assert!(content.contains("<string>com.cfgd.environment</string>"));
    assert!(content.contains("<key>RunAtLoad</key>"));
    assert!(content.contains("<key>EnvironmentVariables</key>"));
    assert!(content.contains("<key>HTTP_PROXY</key>"));
    assert!(content.contains("<string>http://proxy.corp:8080</string>"));
    assert!(content.trim_end().ends_with("</plist>"));
}

#[test]
fn macos_write_launchd_plist_xml_escapes_special_chars() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());

    let mut managed = BTreeMap::new();
    managed.insert("TRICKY".to_string(), r#"<value & "quoted">"#.to_string());
    EnvironmentConfigurator::macos_write_launchd_plist(&managed).unwrap();

    let content = std::fs::read_to_string(
        dir.path()
            .join("Library/LaunchAgents/com.cfgd.environment.plist"),
    )
    .unwrap();
    // xml_escape encodes <, >, &, "
    assert!(
        content.contains("&lt;value &amp; &quot;quoted&quot;&gt;"),
        "expected escaped value, got: {content}"
    );
    // Raw special chars must NOT appear in the value section
    assert!(!content.contains(r#"<value & "quoted">"#));
}

#[test]
fn macos_write_launchd_plist_empty_managed_removes_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());
    let plist = dir
        .path()
        .join("Library/LaunchAgents/com.cfgd.environment.plist");
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    std::fs::write(&plist, "stale plist").unwrap();
    assert!(plist.exists());

    // Empty managed shells out to `launchctl unload` (which fails harmlessly
    // on Linux — the function logs and proceeds), then removes the plist.
    EnvironmentConfigurator::macos_write_launchd_plist(&BTreeMap::new()).unwrap();
    assert!(!plist.exists());
}

#[test]
fn macos_write_launchd_plist_creates_parent_dir() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());
    assert!(!dir.path().join("Library").exists());

    let mut managed = BTreeMap::new();
    managed.insert("X".to_string(), "y".to_string());
    EnvironmentConfigurator::macos_write_launchd_plist(&managed).unwrap();

    assert!(
        dir.path()
            .join("Library/LaunchAgents/com.cfgd.environment.plist")
            .exists()
    );
}

#[test]
#[serial_test::serial]
fn macos_launchctl_setenv_warns_through_printer_when_launchctl_unavailable() {
    use cfgd_core::output::Verbosity;
    let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);

    let mut managed = BTreeMap::new();
    managed.insert("TEST_VAR".to_string(), "test_value".to_string());

    EnvironmentConfigurator::macos_launchctl_setenv(&managed, &printer);

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("launchctl setenv"),
        "printer should capture launchctl failure message, got: {captured}"
    );
}

#[test]
#[serial_test::serial]
fn macos_launchctl_setenv_no_output_when_empty_managed() {
    use cfgd_core::output::Verbosity;
    let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);

    EnvironmentConfigurator::macos_launchctl_setenv(&BTreeMap::new(), &printer);

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.is_empty(),
        "no printer output expected for empty managed, got: {captured}"
    );
}

#[test]
#[serial_test::serial]
fn windows_set_var_warns_through_printer_when_setx_unavailable() {
    use cfgd_core::output::Verbosity;
    let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);

    EnvironmentConfigurator::windows_set_var("MY_VAR", "my_value", &printer);

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("setx MY_VAR"),
        "printer should capture setx failure message, got: {captured}"
    );
}

#[test]
fn apply_linux_warns_on_permission_errors_for_etc_paths() {
    use cfgd_core::output::Verbosity;
    let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);
    let ec = EnvironmentConfigurator;

    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
TEST_ENV_VAR: "test_value"
"#,
    )
    .unwrap();

    let result = ec.apply(&yaml, &printer);
    result.expect("apply should succeed on Linux even when /etc paths require root");

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("Managing 1 environment variable(s)"),
        "expected managing message, got: {captured}"
    );
}

#[test]
fn apply_linux_writes_etc_environment_and_profile_d_with_tempdir() {
    use cfgd_core::output::Verbosity;
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let etc_env = dir.path().join("environment");
    let profile_d = dir.path().join("profile.d").join("cfgd-env.sh");

    let mut managed = BTreeMap::new();
    managed.insert("FOO".to_string(), "bar".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&etc_env, &managed).unwrap();
    EnvironmentConfigurator::write_profile_d_to(&profile_d, &managed).unwrap();

    let env_content = std::fs::read_to_string(&etc_env).unwrap();
    assert!(env_content.contains("FOO=bar"), "got: {env_content}");
    assert!(env_content.contains(CFGD_BLOCK_BEGIN));

    let profile_content = std::fs::read_to_string(&profile_d).unwrap();
    assert!(
        profile_content.contains("export FOO=") && profile_content.contains("bar"),
        "got: {profile_content}"
    );

    let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);
    let _ = Arc::new(&printer);

    let yaml: serde_yaml::Value = serde_yaml::from_str(r#"FOO: "bar""#).unwrap();
    let ec = EnvironmentConfigurator;
    let result = ec.apply(&yaml, &printer);
    result.expect("apply should succeed on Linux for a single managed var");

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("Managing 1 environment variable(s)"),
        "got: {captured}"
    );
}

#[test]
fn diff_no_drift_when_desired_is_empty() {
    let ec = EnvironmentConfigurator;
    let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let drifts = ec.diff(&yaml).unwrap();
    assert!(drifts.is_empty(), "empty desired produces no drifts");
}

#[test]
fn diff_reports_changed_value() {
    let ec = EnvironmentConfigurator;
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
CFGD_NONEXISTENT_DRIFT_TEST_VAR: "expected_value"
"#,
    )
    .unwrap();

    let drifts = ec.diff(&yaml).unwrap();
    assert_eq!(drifts.len(), 1);
    let d = &drifts[0];
    assert_eq!(d.key, "CFGD_NONEXISTENT_DRIFT_TEST_VAR");
    assert_eq!(d.expected, "expected_value");
    assert!(d.actual.is_empty(), "absent var should have empty actual");
}

#[test]
fn current_state_returns_yaml_mapping() {
    let ec = EnvironmentConfigurator;
    let state = ec.current_state().unwrap();
    assert!(
        state.is_mapping(),
        "current_state must return a YAML mapping"
    );
    let mapping = state.as_mapping().unwrap();
    for (k, v) in mapping {
        assert!(k.is_string(), "all keys must be strings");
        assert!(v.is_string(), "all values must be strings");
    }
}

#[test]
fn is_available_returns_true_on_unix() {
    let ec = EnvironmentConfigurator;
    assert!(ec.is_available(), "should be available on unix");
}

#[test]
fn macos_write_env_sh_overwrites_existing_content() {
    let dir = tempfile::tempdir().unwrap();
    let _g = cfgd_core::with_test_home_guard(dir.path());

    let env_sh = dir.path().join(".config/cfgd/env.sh");

    let mut first = BTreeMap::new();
    first.insert("OLD_VAR".to_string(), "old_value".to_string());
    EnvironmentConfigurator::macos_write_env_sh(&first).unwrap();
    let initial = std::fs::read_to_string(&env_sh).unwrap();
    assert!(
        initial.contains("OLD_VAR"),
        "first write should have OLD_VAR"
    );

    let mut second = BTreeMap::new();
    second.insert("NEW_VAR".to_string(), "new_value".to_string());
    EnvironmentConfigurator::macos_write_env_sh(&second).unwrap();
    let updated = std::fs::read_to_string(&env_sh).unwrap();
    assert!(
        !updated.contains("OLD_VAR"),
        "second write should not contain OLD_VAR"
    );
    assert!(
        updated.contains("NEW_VAR"),
        "second write should have NEW_VAR"
    );
}

#[test]
#[serial_test::serial]
fn macos_launchctl_setenv_ok_success_path_with_fake_binary() {
    use cfgd_core::output::Verbosity;

    let (_bin_dir, _path_guard) =
        cfgd_core::test_helpers::install_named_path_shim("launchctl", 0, "", "");

    let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);
    let mut managed = BTreeMap::new();
    managed.insert("FOO".to_string(), "bar".to_string());

    EnvironmentConfigurator::macos_launchctl_setenv(&managed, &printer);

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.is_empty(),
        "successful launchctl should produce no printer output, got: {captured}"
    );
}

#[test]
#[serial_test::serial]
fn macos_launchctl_setenv_ok_failure_path_with_fake_binary() {
    use cfgd_core::output::Verbosity;

    let (_bin_dir, _path_guard) = cfgd_core::test_helpers::install_named_path_shim(
        "launchctl",
        1,
        "",
        "operation not permitted",
    );

    let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);
    let mut managed = BTreeMap::new();
    managed.insert("BAR".to_string(), "baz".to_string());

    EnvironmentConfigurator::macos_launchctl_setenv(&managed, &printer);

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("launchctl setenv BAR"),
        "failed launchctl should print warning, got: {captured}"
    );
}

#[test]
#[serial_test::serial]
fn windows_set_var_ok_success_path_with_fake_binary() {
    use cfgd_core::output::Verbosity;

    let (_bin_dir, _path_guard) =
        cfgd_core::test_helpers::install_named_path_shim("setx", 0, "", "");

    let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);

    EnvironmentConfigurator::windows_set_var("MY_KEY", "my_val", &printer);

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.is_empty(),
        "successful setx should produce no printer output, got: {captured}"
    );
}

#[test]
#[serial_test::serial]
fn windows_set_var_ok_failure_path_with_fake_binary() {
    use cfgd_core::output::Verbosity;

    let (_bin_dir, _path_guard) =
        cfgd_core::test_helpers::install_named_path_shim("setx", 1, "", "ERROR: Invalid syntax");

    let (printer, buf) = cfgd_core::output::Printer::for_test_at(Verbosity::Normal);

    EnvironmentConfigurator::windows_set_var("BAD_VAR", "val", &printer);

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("setx BAD_VAR"),
        "failed setx should print warning, got: {captured}"
    );
}

#[test]
#[serial_test::serial]
fn diff_detects_drift_when_current_has_different_value() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");

    let mut current_managed = BTreeMap::new();
    current_managed.insert("CFGD_DIFF_TEST_KEY".to_string(), "old_val".to_string());
    EnvironmentConfigurator::write_etc_environment_to(&env_path, &current_managed).unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(
        vars.get("CFGD_DIFF_TEST_KEY").map(String::as_str),
        Some("old_val")
    );

    let desired: serde_yaml::Value =
        serde_yaml::from_str("CFGD_DIFF_TEST_KEY: \"new_val\"").unwrap();
    let desired_vars = EnvironmentConfigurator::desired_vars(&desired);
    let current_vars = vars;
    let mut drifts = Vec::new();
    for (key, desired_value) in &desired_vars {
        match current_vars.get(key) {
            Some(current_value) if current_value == desired_value => {}
            Some(current_value) => {
                drifts.push(cfgd_core::providers::SystemDrift {
                    key: key.clone(),
                    expected: desired_value.clone(),
                    actual: current_value.clone(),
                });
            }
            None => {
                drifts.push(cfgd_core::providers::SystemDrift {
                    key: key.clone(),
                    expected: desired_value.clone(),
                    actual: String::new(),
                });
            }
        }
    }
    assert_eq!(drifts.len(), 1);
    let d = &drifts[0];
    assert_eq!(d.key, "CFGD_DIFF_TEST_KEY");
    assert_eq!(d.expected, "new_val");
    assert_eq!(d.actual, "old_val");
}

#[test]
#[serial_test::serial]
fn diff_no_drift_when_value_matches_current() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");

    let mut current_managed = BTreeMap::new();
    current_managed.insert("CFGD_MATCH_TEST_KEY".to_string(), "exact_val".to_string());
    EnvironmentConfigurator::write_etc_environment_to(&env_path, &current_managed).unwrap();

    let current_vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(
        current_vars.get("CFGD_MATCH_TEST_KEY").map(String::as_str),
        Some("exact_val")
    );

    let desired_vars = current_managed;
    let mut drifts = Vec::new();
    for (key, desired_value) in &desired_vars {
        match current_vars.get(key) {
            Some(current_value) if current_value == desired_value => {}
            Some(current_value) => {
                drifts.push(cfgd_core::providers::SystemDrift {
                    key: key.clone(),
                    expected: desired_value.clone(),
                    actual: current_value.clone(),
                });
            }
            None => {
                drifts.push(cfgd_core::providers::SystemDrift {
                    key: key.clone(),
                    expected: desired_value.clone(),
                    actual: String::new(),
                });
            }
        }
    }
    assert!(drifts.is_empty(), "matching value should produce no drift");
}

// ---- Additional coverage: write_etc_environment_to edge cases ----

#[test]
fn write_etc_environment_preserves_multiple_non_managed_vars() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");
    std::fs::write(
        &env_path,
        "PATH=/usr/bin\nHOME=/root\nSHELL=/bin/bash\nLANG=C\n",
    )
    .unwrap();

    let mut managed = BTreeMap::new();
    managed.insert("EDITOR".to_string(), "vim".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    assert!(content.contains("PATH=/usr/bin"), "PATH preserved");
    assert!(content.contains("HOME=/root"), "HOME preserved");
    assert!(content.contains("SHELL=/bin/bash"), "SHELL preserved");
    assert!(content.contains("LANG=C"), "LANG preserved");
    assert!(content.contains("EDITOR=vim"), "managed var present");
}

#[test]
fn write_etc_environment_handles_value_with_all_special_chars() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");

    let mut managed = BTreeMap::new();
    managed.insert(
        "COMPLEX".to_string(),
        "path with spaces and $HOME and # and \"quotes\"".to_string(),
    );

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    assert!(
        content.contains("COMPLEX=\""),
        "complex value should be quoted"
    );
    assert!(
        content.contains("\\\"quotes\\\""),
        "internal quotes escaped"
    );
}

#[test]
fn write_profile_d_empty_managed_nonexistent_file_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("nonexistent_dir").join("cfgd-env.sh");

    let managed = BTreeMap::new();
    EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();
    assert!(
        !profile_path.exists(),
        "empty managed should not create file"
    );
}

#[test]
fn write_profile_d_multiple_vars_all_exported() {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("cfgd-env.sh");

    let mut managed = BTreeMap::new();
    managed.insert("A_VAR".to_string(), "alpha".to_string());
    managed.insert("B_VAR".to_string(), "beta".to_string());
    managed.insert("C_VAR".to_string(), "gamma".to_string());

    EnvironmentConfigurator::write_profile_d_to(&profile_path, &managed).unwrap();

    let content = std::fs::read_to_string(&profile_path).unwrap();
    assert!(content.contains("export A_VAR="));
    assert!(content.contains("export B_VAR="));
    assert!(content.contains("export C_VAR="));
    // BTreeMap ordering means A < B < C
    let a_pos = content.find("A_VAR").unwrap();
    let b_pos = content.find("B_VAR").unwrap();
    let c_pos = content.find("C_VAR").unwrap();
    assert!(
        a_pos < b_pos && b_pos < c_pos,
        "vars should be in sorted order"
    );
}

// ---- Additional coverage: parse_env_file edge cases ----

#[test]
fn parse_env_file_multiple_equals_in_value() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "OPTS=--key=val --other=thing\n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(vars["OPTS"], "--key=val --other=thing");
}

#[test]
fn parse_env_file_inline_quotes_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env");
    std::fs::write(&env_path, "MSG=\"hello world\"\n").unwrap();

    let vars = EnvironmentConfigurator::parse_env_file(env_path.to_str().unwrap());
    assert_eq!(
        vars["MSG"], "hello world",
        "outer quotes should be stripped"
    );
}

// ---- Additional coverage: parse_export_file edge cases ----

#[test]
fn parse_export_file_whitespace_around_export() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(&file_path, "  export INDENT=\"yes\"  \n").unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert_eq!(vars["INDENT"], "yes");
}

#[test]
fn parse_export_file_value_with_spaces_unquoted() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(&file_path, "export SPACED=has spaces here\n").unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert_eq!(vars["SPACED"], "has spaces here");
}

#[test]
fn parse_export_file_export_without_equals_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("env.sh");
    std::fs::write(&file_path, "export NOEQUALS\nexport VALID=yes\n").unwrap();

    let vars = EnvironmentConfigurator::parse_export_file(file_path.to_str().unwrap());
    assert_eq!(vars.len(), 1);
    assert_eq!(vars["VALID"], "yes");
}

// ---- Additional coverage: desired_vars with edge-case YAML ----

#[test]
fn desired_vars_empty_string_value_preserved() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
EMPTY_VAL: ""
"#,
    )
    .unwrap();

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    assert_eq!(vars["EMPTY_VAL"], "");
}

#[test]
fn desired_vars_large_number_values() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
BIG_INT: 999999999
PORT: 8080
"#,
    )
    .unwrap();

    let vars = EnvironmentConfigurator::desired_vars(&yaml);
    assert_eq!(vars["BIG_INT"], "999999999");
    assert_eq!(vars["PORT"], "8080");
}

#[test]
fn write_etc_environment_block_markers_on_separate_lines() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("environment");

    let mut managed = BTreeMap::new();
    managed.insert("KEY".to_string(), "val".to_string());

    EnvironmentConfigurator::write_etc_environment_to(&env_path, &managed).unwrap();

    let content = std::fs::read_to_string(&env_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    let begin_idx = lines
        .iter()
        .position(|l| l.contains(CFGD_BLOCK_BEGIN))
        .unwrap();
    let end_idx = lines
        .iter()
        .position(|l| l.contains(CFGD_BLOCK_END))
        .unwrap();
    assert!(
        end_idx > begin_idx,
        "END marker must come after BEGIN marker"
    );
    assert_eq!(
        end_idx - begin_idx,
        2,
        "one var should produce exactly one line between markers"
    );
}

#[test]
fn environment_name_returns_environment() {
    let ec = EnvironmentConfigurator;
    assert_eq!(ec.name(), "environment");
}

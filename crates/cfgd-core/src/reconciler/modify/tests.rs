use std::path::{Path, PathBuf};

use super::*;
use crate::config::{ModifyFormat, ModifySpec};
use crate::errors::{CfgdError, FileError};

fn spec(format: Option<ModifyFormat>, ensure: &str) -> ModifySpec {
    ModifySpec {
        format,
        ensure: Some(serde_yaml::from_str(ensure).expect("ensure fixture parses")),
        script: None,
    }
}

fn script_spec(script: &str) -> ModifySpec {
    ModifySpec {
        format: None,
        ensure: None,
        script: Some(script.to_string()),
    }
}

fn ctx_for(dir: &Path) -> ModifyContext<'_> {
    ModifyContext::new(dir).with_working_dir(dir)
}

fn apply(current: &str, spec: &ModifySpec, target: &str) -> String {
    let path = PathBuf::from(target);
    let dir = std::env::current_dir().expect("cwd");
    compute_modified(current, spec, &path, &ModifyContext::new(&dir))
        .unwrap_or_else(|e| panic!("compute_modified failed: {e}"))
}

fn apply_err(current: &str, spec: &ModifySpec, target: &str) -> CfgdError {
    let path = PathBuf::from(target);
    let dir = std::env::current_dir().expect("cwd");
    compute_modified(current, spec, &path, &ModifyContext::new(&dir))
        .expect_err("expected compute_modified to fail")
}

fn assert_file_err(err: &CfgdError, matcher: impl Fn(&FileError) -> bool, expected: &str) {
    match err {
        CfgdError::File(file_err) if matcher(file_err) => {}
        other => panic!("expected {expected}, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Format inference
// ---------------------------------------------------------------------------

#[test]
fn infer_format_maps_known_extensions() {
    let cases = [
        ("app.ini", ModifyFormat::Ini),
        ("app.INI", ModifyFormat::Ini),
        ("app.json", ModifyFormat::Json),
        ("app.yaml", ModifyFormat::Yaml),
        ("app.yml", ModifyFormat::Yaml),
        ("app.YML", ModifyFormat::Yaml),
        ("app.toml", ModifyFormat::Toml),
    ];
    for (name, expected) in cases {
        assert_eq!(
            infer_format(Path::new(name)),
            Some(expected),
            "extension inference for {name}"
        );
    }
}

#[test]
fn infer_format_rejects_unknown_and_extensionless() {
    assert_eq!(infer_format(Path::new("hosts")), None);
    assert_eq!(infer_format(Path::new("app.conf")), None);
    assert_eq!(infer_format(Path::new(".gitconfig")), None);
}

#[test]
fn unknown_extension_without_explicit_format_is_typed_error() {
    let err = apply_err("", &spec(None, "user:\n  name: x\n"), "/etc/hosts");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyFormatUnknown { .. }),
        "ModifyFormatUnknown",
    );
    assert!(
        err.to_string().contains("modify.format"),
        "error should name the escape hatch: {err}"
    );
}

#[test]
fn explicit_format_overrides_the_extension() {
    // A `.json` target explicitly declared as INI is parsed as INI.
    let out = apply(
        "[core]\nvalue = 1\n",
        &spec(Some(ModifyFormat::Ini), "core:\n  value: 2\n"),
        "/tmp/app.json",
    );
    assert_eq!(out, "[core]\nvalue = 2\n");
}

#[test]
fn resolve_format_prefers_the_explicit_field() {
    let s = spec(Some(ModifyFormat::Toml), "a: 1");
    assert_eq!(
        resolve_format(&s, Path::new("x.ini")).expect("explicit format wins"),
        ModifyFormat::Toml
    );
}

// ---------------------------------------------------------------------------
// Spec shape guards
// ---------------------------------------------------------------------------

#[test]
fn neither_ensure_nor_script_is_typed_error() {
    let s = ModifySpec {
        format: Some(ModifyFormat::Json),
        ensure: None,
        script: None,
    };
    let err = apply_err("{}", &s, "/tmp/a.json");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifySpecInvalid { .. }),
        "ModifySpecInvalid",
    );
}

#[test]
fn both_ensure_and_script_is_typed_error() {
    let s = ModifySpec {
        format: Some(ModifyFormat::Json),
        ensure: Some(serde_yaml::from_str("a: 1").expect("fixture")),
        script: Some("cat".to_string()),
    };
    let err = apply_err("{}", &s, "/tmp/a.json");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifySpecInvalid { .. }),
        "ModifySpecInvalid",
    );
}

#[test]
fn non_mapping_ensure_is_rejected_for_every_format() {
    for (format, target) in [
        (ModifyFormat::Ini, "/tmp/a.ini"),
        (ModifyFormat::Json, "/tmp/a.json"),
        (ModifyFormat::Yaml, "/tmp/a.yaml"),
        (ModifyFormat::Toml, "/tmp/a.toml"),
    ] {
        let s = spec(Some(format), "- just\n- a list\n");
        let err = apply_err("", &s, target);
        assert_file_err(
            &err,
            |e| matches!(e, FileError::ModifyEnsureShape { .. }),
            "ModifyEnsureShape",
        );
    }
}

// ---------------------------------------------------------------------------
// INI
// ---------------------------------------------------------------------------

#[test]
fn ini_updates_existing_key_and_preserves_comments_and_unknown_keys() {
    let current = "\
# managed by hand
[user]
name = Old Name
email = keep@example.com

[core]
editor = vim
";
    let out = apply(
        current,
        &spec(None, "user:\n  name: New Name\n"),
        "/tmp/app.ini",
    );
    assert_eq!(
        out,
        "\
# managed by hand
[user]
name = New Name
email = keep@example.com

[core]
editor = vim
"
    );
}

#[test]
fn ini_preserves_the_files_spacing_around_equals() {
    let current = "[user]\nname   =   Old\n";
    let out = apply(current, &spec(None, "user:\n  name: New\n"), "/tmp/app.ini");
    assert_eq!(out, "[user]\nname   =   New\n");
}

#[test]
fn ini_adds_a_missing_key_to_an_existing_section() {
    let current = "[user]\nname = Ada\n\n[core]\neditor = vim\n";
    let out = apply(
        current,
        &spec(None, "user:\n  email: ada@example.com\n"),
        "/tmp/app.ini",
    );
    assert_eq!(
        out,
        "[user]\nname = Ada\nemail = ada@example.com\n\n[core]\neditor = vim\n"
    );
}

#[test]
fn ini_new_key_lands_above_the_next_sections_comment_block() {
    let current = "\
[user]
name = Ada

# core settings follow
[core]
editor = vim
";
    let out = apply(
        current,
        &spec(None, "user:\n  email: ada@example.com\n"),
        "/tmp/app.ini",
    );
    assert_eq!(
        out,
        "\
[user]
name = Ada
email = ada@example.com

# core settings follow
[core]
editor = vim
"
    );
}

#[test]
fn ini_new_key_in_comment_only_section_goes_after_the_header() {
    let current = "[user]\n# nothing set yet\n";
    let out = apply(current, &spec(None, "user:\n  name: Ada\n"), "/tmp/app.ini");
    assert_eq!(out, "[user]\nname = Ada\n# nothing set yet\n");
}

#[test]
fn ini_creates_a_missing_section_at_the_end() {
    let current = "[user]\nname = Ada\n";
    let out = apply(
        current,
        &spec(None, "core:\n  editor: vim\n  autocrlf: false\n"),
        "/tmp/app.ini",
    );
    assert_eq!(
        out,
        "[user]\nname = Ada\n\n[core]\neditor = vim\nautocrlf = false\n"
    );
}

#[test]
fn ini_empty_current_creates_a_minimal_document() {
    let out = apply(
        "",
        &spec(None, "user:\n  name: Ada\n"),
        "/home/ada/config.ini",
    );
    assert_eq!(out, "[user]\nname = Ada\n");
}

#[test]
fn ini_global_key_without_a_section() {
    let current = "verbose = false\n\n[core]\neditor = vim\n";
    let out = apply(current, &spec(None, "verbose: true\n"), "/tmp/app.ini");
    assert_eq!(out, "verbose = true\n\n[core]\neditor = vim\n");
}

#[test]
fn ini_new_global_key_lands_below_the_banner_comment() {
    let current = "# app config\n# do not delete\n[core]\neditor = vim\n";
    let out = apply(current, &spec(None, "verbose: true\n"), "/tmp/app.ini");
    assert_eq!(
        out,
        "# app config\n# do not delete\nverbose = true\n[core]\neditor = vim\n"
    );
}

#[test]
fn ini_global_key_into_empty_document() {
    let out = apply("", &spec(None, "verbose: true\n"), "/tmp/app.ini");
    assert_eq!(out, "verbose = true\n");
}

#[test]
fn ini_adopts_the_no_space_separator_style() {
    let current = "[Unit]\nDescription=Example\n";
    let out = apply(
        current,
        &spec(None, "Unit:\n  After: network.target\n"),
        "/tmp/app.ini",
    );
    assert_eq!(out, "[Unit]\nDescription=Example\nAfter=network.target\n");
}

#[test]
fn ini_rewrites_every_duplicate_key_in_a_section() {
    let current = "[core]\neditor = vim\neditor = nano\n";
    let out = apply(
        current,
        &spec(None, "core:\n  editor: helix\n"),
        "/tmp/a.ini",
    );
    assert_eq!(out, "[core]\neditor = helix\neditor = helix\n");
}

#[test]
fn ini_same_key_in_two_sections_updates_only_the_named_one() {
    let current = "[a]\nvalue = 1\n\n[b]\nvalue = 2\n";
    let out = apply(current, &spec(None, "b:\n  value: 9\n"), "/tmp/a.ini");
    assert_eq!(out, "[a]\nvalue = 1\n\n[b]\nvalue = 9\n");
}

#[test]
fn ini_preserves_crlf_line_endings() {
    let current = "[user]\r\nname = Ada\r\n";
    let out = apply(
        current,
        &spec(
            None,
            "user:\n  email: ada@example.com\ncore:\n  editor: vim\n",
        ),
        "/tmp/app.ini",
    );
    assert_eq!(
        out,
        "[user]\r\nname = Ada\r\nemail = ada@example.com\r\n\r\n[core]\r\neditor = vim\r\n"
    );
}

#[test]
fn ini_preserves_a_missing_trailing_newline_when_only_updating() {
    let current = "[user]\nname = Old";
    let out = apply(current, &spec(None, "user:\n  name: New\n"), "/tmp/app.ini");
    assert_eq!(out, "[user]\nname = New");
}

#[test]
fn ini_renders_bool_and_number_values() {
    let out = apply(
        "",
        &spec(None, "core:\n  autocrlf: false\n  depth: 42\n"),
        "/tmp/app.ini",
    );
    assert_eq!(out, "[core]\nautocrlf = false\ndepth = 42\n");
}

#[test]
fn ini_replaces_inline_comment_on_an_updated_line() {
    let current = "[core]\neditor = vim ; was nano\n";
    let out = apply(
        current,
        &spec(None, "core:\n  editor: helix\n"),
        "/tmp/a.ini",
    );
    assert_eq!(out, "[core]\neditor = helix\n");
}

#[test]
fn ini_merge_is_idempotent() {
    let s = spec(None, "user:\n  name: Ada\n  email: ada@example.com\n");
    let once = apply("[core]\neditor = vim\n", &s, "/tmp/app.ini");
    let twice = apply(&once, &s, "/tmp/app.ini");
    assert_eq!(once, twice);
}

#[test]
fn ini_rejects_nested_mappings() {
    let err = apply_err(
        "",
        &spec(None, "user:\n  name:\n    first: Ada\n"),
        "/tmp/app.ini",
    );
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyEnsureShape { .. }),
        "ModifyEnsureShape",
    );
    assert!(
        err.to_string().contains("user.name"),
        "names the key: {err}"
    );
}

#[test]
fn ini_rejects_list_values() {
    let err = apply_err("", &spec(None, "core:\n  paths: [a, b]\n"), "/tmp/app.ini");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyEnsureShape { .. }),
        "ModifyEnsureShape",
    );
}

#[test]
fn ini_rejects_null_values() {
    let err = apply_err("", &spec(None, "core:\n  editor:\n"), "/tmp/app.ini");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyEnsureShape { .. }),
        "ModifyEnsureShape",
    );
}

#[test]
fn ini_rejects_non_string_section_names() {
    let err = apply_err("", &spec(None, "42:\n  a: b\n"), "/tmp/app.ini");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyEnsureShape { .. }),
        "ModifyEnsureShape",
    );
}

#[test]
fn ini_rejects_non_string_key_names() {
    let err = apply_err("", &spec(None, "core:\n  42: b\n"), "/tmp/app.ini");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyEnsureShape { .. }),
        "ModifyEnsureShape",
    );
}

#[test]
fn ini_section_header_with_surrounding_whitespace_is_matched() {
    let current = "  [user]  \nname = Old\n";
    let out = apply(current, &spec(None, "user:\n  name: New\n"), "/tmp/app.ini");
    assert_eq!(out, "  [user]  \nname = New\n");
}

// ---------------------------------------------------------------------------
// TOML
// ---------------------------------------------------------------------------

#[test]
fn toml_preserves_comments_layout_and_unknown_keys() {
    let current = "\
# Cargo-ish config
[build]
# the jobs comment
jobs = 4
target = \"x86_64-unknown-linux-gnu\"

[net]
retry = 2
";
    let out = apply(
        current,
        &spec(None, "build:\n  jobs: 8\n"),
        "/tmp/config.toml",
    );
    assert_eq!(
        out,
        "\
# Cargo-ish config
[build]
# the jobs comment
jobs = 8
target = \"x86_64-unknown-linux-gnu\"

[net]
retry = 2
"
    );
}

#[test]
fn toml_preserves_a_trailing_inline_comment_on_an_updated_key() {
    let current = "[build]\njobs = 4 # keep me\n";
    let out = apply(current, &spec(None, "build:\n  jobs: 8\n"), "/tmp/a.toml");
    assert_eq!(out, "[build]\njobs = 8 # keep me\n");
}

#[test]
fn toml_deep_merges_nested_tables() {
    let current = "[a.b]\nkeep = true\nvalue = 1\n";
    let out = apply(
        current,
        &spec(None, "a:\n  b:\n    value: 2\n"),
        "/tmp/a.toml",
    );
    assert_eq!(out, "[a.b]\nkeep = true\nvalue = 2\n");
}

#[test]
fn toml_creates_a_missing_table() {
    let current = "[build]\njobs = 4\n";
    let out = apply(current, &spec(None, "net:\n  retry: 3\n"), "/tmp/a.toml");
    assert!(
        out.starts_with("[build]\njobs = 4\n"),
        "kept original: {out}"
    );
    assert!(out.contains("[net]"), "created table: {out}");
    assert!(out.contains("retry = 3"), "set key: {out}");
}

#[test]
fn toml_empty_current_creates_a_minimal_document() {
    let out = apply("", &spec(None, "build:\n  jobs: 8\n"), "/tmp/a.toml");
    assert_eq!(out, "[build]\njobs = 8\n");
}

#[test]
fn toml_adds_a_key_to_an_existing_table() {
    let current = "[build]\njobs = 4\n";
    let out = apply(
        current,
        &spec(None, "build:\n  target: wasm32-unknown-unknown\n"),
        "/tmp/a.toml",
    );
    assert_eq!(
        out,
        "[build]\njobs = 4\ntarget = \"wasm32-unknown-unknown\"\n"
    );
}

#[test]
fn toml_merges_into_an_inline_table_in_place() {
    let current = "package = { name = \"demo\", version = \"0.1.0\" }\n";
    let out = apply(
        current,
        &spec(None, "package:\n  version: 0.2.0\n"),
        "/tmp/a.toml",
    );
    assert_eq!(out, "package = { name = \"demo\", version = \"0.2.0\" }\n");
}

#[test]
fn toml_writes_arrays_and_nests_mappings_as_sub_tables() {
    let out = apply(
        "",
        &spec(
            None,
            "build:\n  flags: [\"-C\", \"opt-level=3\"]\n  meta:\n    owner: platform\n",
        ),
        "/tmp/a.toml",
    );
    assert_eq!(
        out,
        "[build]\nflags = [\"-C\", \"opt-level=3\"]\n\n[build.meta]\nowner = \"platform\"\n"
    );
}

#[test]
fn toml_writes_mappings_inside_arrays_as_inline_tables() {
    let out = apply(
        "",
        &spec(
            None,
            "bin:\n  entries:\n    - name: cfgd\n      path: src/main.rs\n",
        ),
        "/tmp/a.toml",
    );
    assert_eq!(
        out,
        "[bin]\nentries = [{ name = \"cfgd\", path = \"src/main.rs\" }]\n"
    );
}

#[test]
fn toml_replaces_a_scalar_with_a_table_when_the_spec_nests() {
    let current = "build = 4\n";
    let out = apply(current, &spec(None, "build:\n  jobs: 8\n"), "/tmp/a.toml");
    assert!(out.contains("[build]"), "{out}");
    assert!(out.contains("jobs = 8"), "{out}");
    assert!(!out.contains("build = 4"), "{out}");
}

#[test]
fn toml_writes_floats_and_bools() {
    let out = apply(
        "",
        &spec(None, "profile:\n  ratio: 1.5\n  strict: true\n"),
        "/tmp/a.toml",
    );
    assert!(out.contains("ratio = 1.5"), "{out}");
    assert!(out.contains("strict = true"), "{out}");
}

#[test]
fn toml_merge_is_idempotent() {
    let s = spec(None, "build:\n  jobs: 8\n  target: host\n");
    let once = apply("# top\n[net]\nretry = 2\n", &s, "/tmp/a.toml");
    let twice = apply(&once, &s, "/tmp/a.toml");
    assert_eq!(once, twice);
}

#[test]
fn toml_invalid_current_content_is_a_typed_error() {
    let err = apply_err("this is [not toml\n", &spec(None, "a: 1\n"), "/tmp/a.toml");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyParse { .. }),
        "ModifyParse",
    );
    assert!(err.to_string().contains("not valid toml"), "{err}");
}

#[test]
fn toml_rejects_null_values() {
    let err = apply_err("", &spec(None, "build:\n  jobs:\n"), "/tmp/a.toml");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyEnsureShape { .. }),
        "ModifyEnsureShape",
    );
    assert!(err.to_string().contains("no null"), "{err}");
}

#[test]
fn toml_rejects_non_string_keys() {
    let err = apply_err("", &spec(None, "42: value\n"), "/tmp/a.toml");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyEnsureShape { .. }),
        "ModifyEnsureShape",
    );
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

#[test]
fn json_deep_merge_preserves_unknown_keys() {
    let current = r#"{"editor": {"tabSize": 2, "theme": "dark"}, "telemetry": false}"#;
    let out = apply(
        current,
        &spec(None, "editor:\n  tabSize: 4\n"),
        "/tmp/settings.json",
    );
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["editor"]["tabSize"], 4);
    assert_eq!(parsed["editor"]["theme"], "dark");
    assert_eq!(parsed["telemetry"], false);
    assert!(out.ends_with('\n'), "trailing newline: {out:?}");
}

#[test]
fn json_empty_current_creates_a_minimal_document() {
    let out = apply("", &spec(None, "a:\n  b: 1\n"), "/tmp/a.json");
    assert_eq!(out, "{\n  \"a\": {\n    \"b\": 1\n  }\n}\n");
}

#[test]
fn json_whitespace_only_current_is_treated_as_empty() {
    let out = apply("   \n\n", &spec(None, "a: 1\n"), "/tmp/a.json");
    assert_eq!(out, "{\n  \"a\": 1\n}\n");
}

#[test]
fn json_nested_object_replaces_a_scalar() {
    let out = apply(r#"{"a": 5}"#, &spec(None, "a:\n  b: 1\n"), "/tmp/a.json");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["a"]["b"], 1);
}

#[test]
fn json_writes_lists_nulls_and_floats() {
    let out = apply(
        "{}",
        &spec(None, "list: [1, 2]\nnothing: null\nratio: 0.5\n"),
        "/tmp/a.json",
    );
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["list"], serde_json::json!([1, 2]));
    assert!(parsed["nothing"].is_null());
    assert_eq!(parsed["ratio"], 0.5);
}

#[test]
fn json_merge_is_idempotent() {
    let s = spec(None, "editor:\n  tabSize: 4\n");
    let once = apply(r#"{"telemetry": false}"#, &s, "/tmp/a.json");
    let twice = apply(&once, &s, "/tmp/a.json");
    assert_eq!(once, twice);
}

#[test]
fn json_invalid_current_content_is_a_typed_error() {
    let err = apply_err("{not json", &spec(None, "a: 1\n"), "/tmp/a.json");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyParse { .. }),
        "ModifyParse",
    );
}

#[test]
fn json_non_object_current_content_is_a_typed_error() {
    let err = apply_err("[1, 2, 3]", &spec(None, "a: 1\n"), "/tmp/a.json");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyParse { .. }),
        "ModifyParse",
    );
    assert!(err.to_string().contains("not an object"), "{err}");
}

#[test]
fn json_rejects_non_string_keys() {
    let err = apply_err("{}", &spec(None, "42: value\n"), "/tmp/a.json");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyEnsureShape { .. }),
        "ModifyEnsureShape",
    );
}

// ---------------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------------

#[test]
fn yaml_deep_merge_preserves_unknown_keys() {
    let current = "server:\n  port: 8080\n  host: 0.0.0.0\nlogging:\n  level: info\n";
    let out = apply(
        current,
        &spec(None, "server:\n  port: 9090\n"),
        "/tmp/app.yaml",
    );
    let parsed: serde_yaml::Value = serde_yaml::from_str(&out).expect("valid yaml");
    assert_eq!(parsed["server"]["port"], serde_yaml::Value::from(9090));
    assert_eq!(parsed["server"]["host"], serde_yaml::Value::from("0.0.0.0"));
    assert_eq!(parsed["logging"]["level"], serde_yaml::Value::from("info"));
}

#[test]
fn yaml_empty_current_creates_a_minimal_document() {
    let out = apply("", &spec(None, "a:\n  b: 1\n"), "/tmp/a.yml");
    assert_eq!(out, "a:\n  b: 1\n");
}

#[test]
fn yaml_comment_only_current_still_gets_the_ensured_keys() {
    let out = apply("# just a comment\n", &spec(None, "a: 1\n"), "/tmp/a.yaml");
    assert_eq!(out, "a: 1\n");
}

#[test]
fn yaml_comments_are_not_preserved() {
    // Pins the documented caveat: the YAML engine reflows the document, so a
    // comment-critical target must use `script` mode instead.
    let current = "# keep me?\nserver:\n  port: 8080\n";
    let out = apply(
        current,
        &spec(None, "server:\n  port: 9090\n"),
        "/tmp/app.yaml",
    );
    assert!(
        !out.contains("keep me"),
        "comment survived unexpectedly: {out}"
    );
}

#[test]
fn yaml_merge_is_idempotent() {
    let s = spec(None, "server:\n  port: 9090\n");
    let once = apply("logging:\n  level: info\n", &s, "/tmp/a.yaml");
    let twice = apply(&once, &s, "/tmp/a.yaml");
    assert_eq!(once, twice);
}

#[test]
fn yaml_invalid_current_content_is_a_typed_error() {
    let err = apply_err("a: [unclosed\n", &spec(None, "b: 1\n"), "/tmp/a.yaml");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyParse { .. }),
        "ModifyParse",
    );
}

#[test]
fn yaml_non_mapping_current_content_is_a_typed_error() {
    let err = apply_err("- one\n- two\n", &spec(None, "b: 1\n"), "/tmp/a.yaml");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyParse { .. }),
        "ModifyParse",
    );
    assert!(err.to_string().contains("not a mapping"), "{err}");
}

// ---------------------------------------------------------------------------
// Script mode
// ---------------------------------------------------------------------------

#[test]
fn script_that_cannot_run_is_a_typed_error_with_context() {
    // Cross-platform: a path that resolves to nothing exits non-zero under both
    // `sh -c` and `cmd /C`, so the failure surfaces as a typed error either way.
    let dir = tempfile::tempdir().expect("tempdir");
    let target = PathBuf::from("/tmp/app.ini");
    let err = compute_modified(
        "current\n",
        &script_spec("./definitely-not-a-real-script"),
        &target,
        &ctx_for(dir.path()),
    )
    .expect_err("missing script must fail");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::ModifyScriptFailed { .. }),
        "ModifyScriptFailed",
    );
    assert!(
        err.to_string().contains("definitely-not-a-real-script"),
        "names the script: {err}"
    );
}

#[cfg(unix)]
mod unix_script {
    use super::*;
    use std::io::Write;

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).expect("create script");
        f.write_all(body.as_bytes()).expect("write script");
        drop(f);
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    #[test]
    fn script_receives_current_content_and_returns_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_script(
            dir.path(),
            "filter.sh",
            "#!/bin/sh\ncat\necho '127.0.0.1 added.local'\n",
        );
        let out = compute_modified(
            "127.0.0.1 localhost\n",
            &script_spec("filter.sh"),
            Path::new("/etc/hosts"),
            &ctx_for(dir.path()),
        )
        .expect("filter succeeds");
        assert_eq!(out, "127.0.0.1 localhost\n127.0.0.1 added.local\n");
    }

    #[test]
    fn script_handles_a_large_payload_without_deadlocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_script(dir.path(), "passthrough.sh", "#!/bin/sh\ncat\n");
        let big = "line of text that is reasonably long\n".repeat(20_000);
        let out = compute_modified(
            &big,
            &script_spec("passthrough.sh"),
            Path::new("/etc/hosts"),
            &ctx_for(dir.path()),
        )
        .expect("filter succeeds");
        assert_eq!(out, big);
    }

    #[test]
    fn script_gets_empty_stdin_when_the_target_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_script(
            dir.path(),
            "seed.sh",
            "#!/bin/sh\nif [ -z \"$(cat)\" ]; then echo empty; else echo nonempty; fi\n",
        );
        let out = compute_modified(
            "",
            &script_spec("seed.sh"),
            Path::new("/etc/hosts"),
            &ctx_for(dir.path()),
        )
        .expect("filter succeeds");
        assert_eq!(out, "empty\n");
    }

    #[test]
    fn script_non_zero_exit_carries_stderr_into_the_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_script(
            dir.path(),
            "fail.sh",
            "#!/bin/sh\necho 'refusing: target is managed elsewhere' >&2\nexit 3\n",
        );
        let err = compute_modified(
            "content\n",
            &script_spec("fail.sh"),
            Path::new("/etc/hosts"),
            &ctx_for(dir.path()),
        )
        .expect_err("non-zero exit must fail");
        assert_file_err(
            &err,
            |e| matches!(e, FileError::ModifyScriptFailed { .. }),
            "ModifyScriptFailed",
        );
        let text = err.to_string();
        assert!(text.contains("exit 3"), "carries exit code: {text}");
        assert!(
            text.contains("refusing: target is managed elsewhere"),
            "carries stderr: {text}"
        );
    }

    #[test]
    fn script_non_executable_file_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("noexec.sh");
        std::fs::write(&path, "#!/bin/sh\ncat\n").expect("write");
        let err = compute_modified(
            "content\n",
            &script_spec("noexec.sh"),
            Path::new("/etc/hosts"),
            &ctx_for(dir.path()),
        )
        .expect_err("non-executable script must fail");
        assert!(
            err.to_string().contains("not executable"),
            "explains the fix: {err}"
        );
    }

    #[test]
    fn script_timeout_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_script(dir.path(), "hang.sh", "#!/bin/sh\nsleep 30\n");
        let ctx = ModifyContext::new(dir.path())
            .with_working_dir(dir.path())
            .with_timeout(std::time::Duration::from_millis(200));
        let err = compute_modified(
            "content\n",
            &script_spec("hang.sh"),
            Path::new("/etc/hosts"),
            &ctx,
        )
        .expect_err("hanging script must time out");
        assert_file_err(
            &err,
            |e| matches!(e, FileError::ModifyScriptFailed { .. }),
            "ModifyScriptFailed",
        );
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[test]
    fn script_sees_the_injected_environment() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_script(
            dir.path(),
            "env.sh",
            "#!/bin/sh\necho \"$CFGD_MODULE_NAME\"\n",
        );
        let env = vec![("CFGD_MODULE_NAME".to_string(), "demo".to_string())];
        let ctx = ModifyContext::new(dir.path())
            .with_working_dir(dir.path())
            .with_env(&env);
        let out = compute_modified("", &script_spec("env.sh"), Path::new("/etc/hosts"), &ctx)
            .expect("filter succeeds");
        assert_eq!(out, "demo\n");
    }

    #[test]
    fn script_runs_as_an_inline_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = compute_modified(
            "b\na\n",
            &script_spec("sort"),
            Path::new("/etc/hosts"),
            &ctx_for(dir.path()),
        )
        .expect("filter succeeds");
        assert_eq!(out, "a\nb\n");
    }

    #[test]
    fn script_defaults_to_the_home_working_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("create home");
        write_script(dir.path(), "pwd.sh", "#!/bin/sh\npwd\n");
        let out = crate::with_test_home(&home, || {
            compute_modified(
                "",
                &script_spec("pwd.sh"),
                Path::new("/etc/hosts"),
                &ModifyContext::new(dir.path()),
            )
        })
        .expect("filter succeeds");
        let expected = std::fs::canonicalize(&home).expect("canonicalize home");
        let actual = std::fs::canonicalize(out.trim()).expect("canonicalize pwd");
        assert_eq!(actual, expected);
    }
}

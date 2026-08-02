use std::path::{Path, PathBuf};

use super::*;
use crate::config::{PatchFormat, PatchSpec};
use crate::errors::{CfgdError, FileError};

fn spec(format: Option<PatchFormat>, ensure: &str) -> PatchSpec {
    PatchSpec {
        format,
        ensure: Some(serde_yaml::from_str(ensure).expect("ensure fixture parses")),
        script: None,
        blocked_by: None,
    }
}

fn script_spec(script: &str) -> PatchSpec {
    PatchSpec {
        format: None,
        ensure: None,
        script: Some(script.to_string()),
        blocked_by: None,
    }
}

fn ctx_for(dir: &Path) -> PatchContext<'_> {
    PatchContext::new(dir).with_working_dir(dir)
}

fn apply(current: &str, spec: &PatchSpec, target: &str) -> String {
    let path = PathBuf::from(target);
    let dir = std::env::current_dir().expect("cwd");
    compute_patched(current, spec, &path, &PatchContext::new(&dir))
        .unwrap_or_else(|e| panic!("compute_patched failed: {e}"))
}

fn apply_err(current: &str, spec: &PatchSpec, target: &str) -> CfgdError {
    let path = PathBuf::from(target);
    let dir = std::env::current_dir().expect("cwd");
    compute_patched(current, spec, &path, &PatchContext::new(&dir))
        .expect_err("expected compute_patched to fail")
}

fn assert_file_err(err: &CfgdError, matcher: impl Fn(&FileError) -> bool, expected: &str) {
    match err {
        CfgdError::File(file_err) if matcher(file_err) => {}
        other => panic!("expected {expected}, got: {other:?}"),
    }
}

fn assert_ensure_shape(err: &CfgdError) {
    assert_file_err(
        err,
        |e| matches!(e, FileError::PatchEnsureShape { .. }),
        "PatchEnsureShape",
    );
}

/// Applying the same `ensure` twice must produce byte-identical output — a
/// merge that cannot re-read what it wrote grows the target on every reconcile.
fn assert_converges(current: &str, spec: &PatchSpec, target: &str) -> String {
    let once = apply(current, spec, target);
    let twice = apply(&once, spec, target);
    assert_eq!(once, twice, "second pass changed the file (non-convergent)");
    once
}

// ---------------------------------------------------------------------------
// Format inference
// ---------------------------------------------------------------------------

#[test]
fn infer_format_maps_known_extensions() {
    let cases = [
        ("app.ini", PatchFormat::Ini),
        ("app.INI", PatchFormat::Ini),
        ("app.json", PatchFormat::Json),
        ("app.yaml", PatchFormat::Yaml),
        ("app.yml", PatchFormat::Yaml),
        ("app.YML", PatchFormat::Yaml),
        ("app.toml", PatchFormat::Toml),
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
        |e| matches!(e, FileError::PatchFormatUnknown { .. }),
        "PatchFormatUnknown",
    );
    assert!(
        err.to_string().contains("patch.format"),
        "error should name the escape hatch: {err}"
    );
}

#[test]
fn explicit_format_overrides_the_extension() {
    // A `.json` target explicitly declared as INI is parsed as INI.
    let out = apply(
        "[core]\nvalue = 1\n",
        &spec(Some(PatchFormat::Ini), "core:\n  value: 2\n"),
        "/tmp/app.json",
    );
    assert_eq!(out, "[core]\nvalue = 2\n");
}

// ---------------------------------------------------------------------------
// Spec shape guards
// ---------------------------------------------------------------------------

#[test]
fn neither_ensure_nor_script_is_typed_error() {
    let s = PatchSpec {
        format: Some(PatchFormat::Json),
        ensure: None,
        script: None,
        blocked_by: None,
    };
    let err = apply_err("{}", &s, "/tmp/a.json");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchSpecInvalid { .. }),
        "PatchSpecInvalid",
    );
}

#[test]
fn a_blocked_spec_never_runs_its_filter() {
    // The marker composition sets when a source may not run scripts. Every
    // evaluation path funnels through `compute_patched`, so refusing here is
    // what makes the filter unrunnable from a read-only command.
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("filter-ran.marker");
    let s = PatchSpec {
        blocked_by: Some("acme".to_string()),
        ..script_spec(&format!("touch {} && cat", crate::to_posix_string(&marker)))
    };
    let err = compute_patched("{}\n", &s, Path::new("/tmp/a.json"), &ctx_for(dir.path()))
        .expect_err("a blocked spec must not be evaluated");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchScriptBlocked { .. }),
        "PatchScriptBlocked",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("acme") && msg.contains("allowScripts"),
        "the error must name the source and the opt-in: {msg}"
    );
    assert!(!marker.exists(), "the filter must never have been spawned");
}

#[test]
fn both_ensure_and_script_is_typed_error() {
    let s = PatchSpec {
        format: Some(PatchFormat::Json),
        ensure: Some(serde_yaml::from_str("a: 1").expect("fixture")),
        script: Some("cat".to_string()),
        blocked_by: None,
    };
    let err = apply_err("{}", &s, "/tmp/a.json");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchSpecInvalid { .. }),
        "PatchSpecInvalid",
    );
}

#[test]
fn non_mapping_ensure_is_rejected_for_every_format() {
    for (format, target) in [
        (PatchFormat::Ini, "/tmp/a.ini"),
        (PatchFormat::Json, "/tmp/a.json"),
        (PatchFormat::Yaml, "/tmp/a.yaml"),
        (PatchFormat::Toml, "/tmp/a.toml"),
    ] {
        let s = spec(Some(format), "- just\n- a list\n");
        let err = apply_err("", &s, target);
        assert_ensure_shape(&err);
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
    assert_converges("[core]\neditor = vim\n", &s, "/tmp/app.ini");
}

#[test]
fn ini_rejects_nested_mappings() {
    let err = apply_err(
        "",
        &spec(None, "user:\n  name:\n    first: Ada\n"),
        "/tmp/app.ini",
    );
    assert_ensure_shape(&err);
    assert!(
        err.to_string().contains("user.name"),
        "names the key: {err}"
    );
}

#[test]
fn ini_rejects_list_values() {
    let err = apply_err("", &spec(None, "core:\n  paths: [a, b]\n"), "/tmp/app.ini");
    assert_ensure_shape(&err);
}

#[test]
fn ini_rejects_null_values() {
    let err = apply_err("", &spec(None, "core:\n  editor:\n"), "/tmp/app.ini");
    assert_ensure_shape(&err);
}

#[test]
fn ini_rejects_non_string_section_names() {
    let err = apply_err("", &spec(None, "42:\n  a: b\n"), "/tmp/app.ini");
    assert_ensure_shape(&err);
}

#[test]
fn ini_rejects_non_string_key_names() {
    let err = apply_err("", &spec(None, "core:\n  42: b\n"), "/tmp/app.ini");
    assert_ensure_shape(&err);
}

#[test]
fn ini_section_header_with_surrounding_whitespace_is_matched() {
    let current = "  [user]  \nname = Old\n";
    let out = apply(current, &spec(None, "user:\n  name: New\n"), "/tmp/app.ini");
    assert_eq!(out, "  [user]  \nname = New\n");
}

#[test]
fn ini_section_header_with_trailing_comment_is_edited_not_duplicated() {
    let current = "[a] ; hi\nx = 1\n";
    let out = assert_converges(current, &spec(None, "a:\n  x: 9\n"), "/tmp/app.ini");
    assert_eq!(out, "[a] ; hi\nx = 9\n");
}

#[test]
fn ini_section_header_with_trailing_hash_comment_is_recognized() {
    let current = "[a] # hi\nx = 1\n";
    let out = apply(current, &spec(None, "a:\n  x: 9\n"), "/tmp/app.ini");
    assert_eq!(out, "[a] # hi\nx = 9\n");
}

#[test]
fn ini_bracketed_line_with_trailing_junk_is_not_a_header() {
    // `[a] junk` is not a header, so `a.x` must create a real `[a]` section
    // rather than editing inside the junk line's block.
    let current = "[a] junk\nx = 1\n";
    let out = apply(current, &spec(None, "a:\n  x: 9\n"), "/tmp/app.ini");
    assert_eq!(out, "[a] junk\nx = 1\n\n[a]\nx = 9\n");
}

#[test]
fn ini_duplicate_section_headers_are_all_updated() {
    // git/systemd/configparser read the LAST value; editing only the first
    // block would leave the ensured value overridden while cfgd reports success.
    let current = "[a]\nx = 1\n\n[b]\ny = 2\n\n[a]\nx = 99\n";
    let out = assert_converges(current, &spec(None, "a:\n  x: 9\n"), "/tmp/app.ini");
    assert_eq!(out, "[a]\nx = 9\n\n[b]\ny = 2\n\n[a]\nx = 9\n");
}

#[test]
fn ini_missing_key_is_added_to_the_last_duplicate_section() {
    let current = "[a]\nx = 1\n\n[a]\nx = 2\n";
    let out = apply(current, &spec(None, "a:\n  z: 3\n"), "/tmp/app.ini");
    assert_eq!(out, "[a]\nx = 1\n\n[a]\nx = 2\nz = 3\n");
}

#[test]
fn ini_updates_an_existing_key_in_a_crlf_file() {
    // Exercises the `\r` strip/re-append branch of the value replacer, which
    // the add-only CRLF test never reaches.
    let current = "[user]\r\nname = Old\r\nemail = keep@example.com\r\n";
    let out = assert_converges(current, &spec(None, "user:\n  name: New\n"), "/tmp/app.ini");
    assert_eq!(out, "[user]\r\nname = New\r\nemail = keep@example.com\r\n");
}

#[test]
fn ini_new_key_adopts_the_files_indentation() {
    let current = "[user]\n\tname = Ada\n";
    let out = apply(
        current,
        &spec(None, "user:\n  email: ada@example.com\n"),
        "/tmp/app.ini",
    );
    assert_eq!(out, "[user]\n\tname = Ada\n\temail = ada@example.com\n");
}

#[test]
fn ini_update_keeps_the_lines_existing_indentation() {
    let current = "[user]\n\tname = Old\n";
    let out = assert_converges(current, &spec(None, "user:\n  name: New\n"), "/tmp/app.ini");
    assert_eq!(out, "[user]\n\tname = New\n");
}

#[test]
fn ini_whitespace_only_target_gains_no_leading_blank_line() {
    let out = apply("\n", &spec(None, "a:\n  x: 9\n"), "/tmp/app.ini");
    assert_eq!(out, "[a]\nx = 9\n");
}

#[test]
fn ini_whitespace_only_target_takes_a_global_key_cleanly() {
    let out = apply("\n\n", &spec(None, "verbose: true\n"), "/tmp/app.ini");
    assert_eq!(out, "verbose = true\n");
}

#[test]
fn ini_rejects_a_value_containing_a_newline() {
    // Writing it would inject a bogus `[evil]` section that the next pass
    // cannot read back, re-appending forever.
    let err = apply_err(
        "",
        &spec(None, "a:\n  x: \"line1\\n[evil]\\nz = 1\"\n"),
        "/tmp/app.ini",
    );
    assert_ensure_shape(&err);
    assert!(err.to_string().contains("a.x"), "names the key: {err}");
    assert!(err.to_string().contains("newline"), "{err}");
}

#[test]
fn ini_rejects_a_value_containing_a_carriage_return() {
    let err = apply_err("", &spec(None, "a:\n  x: \"one\\rtwo\"\n"), "/tmp/app.ini");
    assert_ensure_shape(&err);
    assert!(err.to_string().contains("carriage return"), "{err}");
}

#[test]
fn ini_rejects_a_key_name_containing_an_equals_sign() {
    // `x=y = 1` reads back as key `x`, so the merge would never find `x=y`.
    let err = apply_err("", &spec(None, "a:\n  \"x=y\": 1\n"), "/tmp/app.ini");
    assert_ensure_shape(&err);
    assert!(err.to_string().contains("no escape syntax"), "{err}");
}

#[test]
fn ini_rejects_key_and_section_names_with_structural_characters() {
    for ensure in [
        "a:\n  \"x[1]\": 1\n",
        "a:\n  \"x\\ny\": 1\n",
        "\"a\\n[evil]\\nz\":\n  x: 1\n",
        "\"a]b\":\n  x: 1\n",
    ] {
        let err = apply_err("", &spec(None, ensure), "/tmp/app.ini");
        assert_ensure_shape(&err);
    }
}

#[test]
fn ini_rejects_padded_and_empty_names() {
    for ensure in [
        "a:\n  \" x\": 1\n",
        "a:\n  \"x \": 1\n",
        "a:\n  \"\": 1\n",
        "\" a\":\n  x: 1\n",
        "\"\":\n  x: 1\n",
        "\" verbose\": true\n",
    ] {
        let err = apply_err("", &spec(None, ensure), "/tmp/app.ini");
        assert_ensure_shape(&err);
        assert!(
            err.to_string().contains("empty or padded"),
            "explains why: {err}"
        );
    }
}

#[test]
fn ini_rejects_a_key_name_starting_with_a_comment_marker() {
    // `#foo = 1` reads back as a comment, so the merge would never find the key
    // and would append it again on every reconcile.
    for ensure in [
        "a:\n  \"#foo\": 1\n",
        "a:\n  \";foo\": 1\n",
        "\"#top\": 1\n",
    ] {
        let err = apply_err("", &spec(None, ensure), "/tmp/app.ini");
        assert_ensure_shape(&err);
        assert!(
            err.to_string().contains("comment marker"),
            "explains why: {err}"
        );
    }
}

#[test]
fn ini_allows_a_section_name_starting_with_a_comment_marker() {
    // `[#foo]` still parses as a header, so it round-trips and must not be
    // rejected alongside the key-name case.
    let out = assert_converges("", &spec(None, "\"#foo\":\n  x: 1\n"), "/tmp/app.ini");
    assert_eq!(out, "[#foo]\nx = 1\n");
}

#[test]
fn ini_allows_a_comment_marker_inside_a_key_name() {
    // Only a LEADING marker breaks the round-trip; `a#b = 1` reads back as `a#b`.
    let out = assert_converges("", &spec(None, "a:\n  \"x#y\": 1\n"), "/tmp/app.ini");
    assert_eq!(out, "[a]\nx#y = 1\n");
}

#[test]
fn ini_values_are_literal_not_templated() {
    let out = assert_converges(
        "",
        &spec(None, "core:\n  editor: \"{{ tera }} ${SHELL} $HOME\"\n"),
        "/tmp/app.ini",
    );
    assert_eq!(out, "[core]\neditor = {{ tera }} ${SHELL} $HOME\n");
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
    assert_converges("# top\n[net]\nretry = 2\n", &s, "/tmp/a.toml");
}

#[test]
fn toml_invalid_current_content_is_a_typed_error() {
    let err = apply_err("this is [not toml\n", &spec(None, "a: 1\n"), "/tmp/a.toml");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchParse { .. }),
        "PatchParse",
    );
    assert!(err.to_string().contains("not valid toml"), "{err}");
}

#[test]
fn toml_rejects_null_values() {
    let err = apply_err("", &spec(None, "build:\n  jobs:\n"), "/tmp/a.toml");
    assert_ensure_shape(&err);
    assert!(err.to_string().contains("no null"), "{err}");
}

#[test]
fn toml_rejects_non_string_keys() {
    let err = apply_err("", &spec(None, "42: value\n"), "/tmp/a.toml");
    assert_ensure_shape(&err);
}

#[test]
fn toml_replaces_a_table_with_a_scalar_keeping_normal_spacing() {
    let current = "[build]\njobs = 4\n";
    let out = assert_converges(current, &spec(None, "build: 5\n"), "/tmp/a.toml");
    assert_eq!(out, "build = 5\n");
}

#[test]
fn toml_replaces_an_array_of_tables_with_a_table() {
    let current = "[[bin]]\nname = \"one\"\n\n[[bin]]\nname = \"two\"\n";
    let out = assert_converges(current, &spec(None, "bin:\n  name: only\n"), "/tmp/a.toml");
    assert_eq!(out, "[bin]\nname = \"only\"\n");
}

#[test]
fn toml_rejects_integers_beyond_the_signed_64_bit_range() {
    // Falling through to f64 would silently write a different number.
    let err = apply_err(
        "",
        &spec(None, "build:\n  jobs: 18446744073709551615\n"),
        "/tmp/a.toml",
    );
    assert_ensure_shape(&err);
    assert!(err.to_string().contains("64-bit integer range"), "{err}");
}

#[test]
fn toml_values_are_literal_not_templated() {
    let out = assert_converges(
        "",
        &spec(None, "build:\n  target: \"{{ tera }}\"\n"),
        "/tmp/a.toml",
    );
    assert_eq!(out, "[build]\ntarget = \"{{ tera }}\"\n");
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
    assert_converges(r#"{"telemetry": false}"#, &s, "/tmp/a.json");
}

#[test]
fn json_invalid_current_content_is_a_typed_error() {
    let err = apply_err("{not json", &spec(None, "a: 1\n"), "/tmp/a.json");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchParse { .. }),
        "PatchParse",
    );
}

#[test]
fn json_non_object_current_content_is_a_typed_error() {
    let err = apply_err("[1, 2, 3]", &spec(None, "a: 1\n"), "/tmp/a.json");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchParse { .. }),
        "PatchParse",
    );
    assert!(err.to_string().contains("not an object"), "{err}");
}

#[test]
fn json_rejects_non_string_keys() {
    // Writing `42` as `"42"` would make the next pass compare a string key
    // against a number key and append a duplicate on every reconcile.
    let err = apply_err("{}", &spec(None, "42: value\n"), "/tmp/a.json");
    assert_ensure_shape(&err);
    assert!(
        err.to_string()
            .contains("object keys must be strings, but the overlay root has a non-string key: 42"),
        "{err}"
    );
}

#[test]
fn json_rejects_a_non_string_key_nested_in_the_overlay() {
    let err = apply_err("{}", &spec(None, "editor:\n  42: value\n"), "/tmp/a.json");
    assert_ensure_shape(&err);
    assert!(
        err.to_string()
            .contains("but 'editor' has a non-string key: 42"),
        "names the path without a dangling separator: {err}"
    );
}

#[test]
fn json_path_label_keeps_a_literal_trailing_dot_in_a_key_name() {
    // The label strips only the ONE separator dot the walker appends after
    // each segment. A parent key that is itself named with a trailing dot
    // must keep that dot in the rendered label, not have it eaten alongside
    // the separator.
    let err = apply_err("{}", &spec(None, "\"a.\":\n  42: value\n"), "/tmp/a.json");
    assert_ensure_shape(&err);
    assert!(
        err.to_string()
            .contains("but 'a.' has a non-string key: 42"),
        "keeps the key's own trailing dot: {err}"
    );
}

#[test]
fn json_renders_a_structured_key_without_a_debug_form() {
    let err = apply_err("{}", &spec(None, "? [a, b]\n: value\n"), "/tmp/a.json");
    assert_ensure_shape(&err);
    let text = err.to_string();
    assert!(text.contains("has a non-string key: a list"), "{text}");
    assert!(!text.contains("Sequence("), "no Debug form leaked: {text}");
}

#[test]
fn json_rejects_a_non_string_key_inside_a_sequence() {
    // The overlay walk must descend into lists: a mapping nested in a list is
    // just as unable to round-trip as a top-level one.
    let err = apply_err("{}", &spec(None, "list:\n  - 42: x\n"), "/tmp/a.json");
    assert_ensure_shape(&err);
    assert!(
        err.to_string()
            .contains("but 'list.[0]' has a non-string key: 42"),
        "names the path: {err}"
    );
}

#[test]
fn json_rejects_non_finite_numbers() {
    for (ensure, rendered) in [
        ("ratio: .nan\n", "'ratio' is .nan"),
        ("ratio: .inf\n", "'ratio' is .inf"),
        ("ratio: -.inf\n", "'ratio' is -.inf"),
    ] {
        let err = apply_err("{}", &spec(None, ensure), "/tmp/a.json");
        assert_ensure_shape(&err);
        let text = err.to_string();
        assert!(
            text.contains(rendered),
            "names the location and value cleanly: {text}"
        );
        assert!(text.contains("JSON has no NaN or Infinity"), "{text}");
        assert!(!text.contains(".."), "no doubled separator: {text}");
    }
}

#[test]
fn json_rejects_a_non_finite_number_nested_in_a_sequence() {
    let err = apply_err("{}", &spec(None, "list: [1, .inf]\n"), "/tmp/a.json");
    assert_ensure_shape(&err);
    let text = err.to_string();
    assert!(text.contains("'list.[1]' is .inf"), "names it: {text}");
    assert!(!text.contains(".."), "no doubled separator: {text}");
}

#[test]
fn json_unwraps_tagged_values_like_ini_and_toml() {
    let out = assert_converges(
        "{}",
        &spec(None, "v: !Tag 5\nlist:\n  - !Tag inner\n"),
        "/tmp/a.json",
    );
    assert_eq!(
        out,
        "{\n  \"v\": 5,\n  \"list\": [\n    \"inner\"\n  ]\n}\n"
    );
}

#[test]
fn json_target_with_duplicate_keys_takes_the_last_occurrence() {
    // A user-owned file may legally repeat a key; `serde_json` and browsers
    // both take the last. Erroring would fail the whole apply over a quirk.
    let out = assert_converges(
        r#"{"a": 1, "b": 2, "a": 3}"#,
        &spec(None, "c: 4\n"),
        "/tmp/a.json",
    );
    assert_eq!(out, "{\n  \"a\": 3,\n  \"b\": 2,\n  \"c\": 4\n}\n");
}

#[test]
fn json_duplicate_key_is_still_editable() {
    let out = assert_converges(r#"{"a": 1, "a": 2}"#, &spec(None, "a: 9\n"), "/tmp/a.json");
    assert_eq!(out, "{\n  \"a\": 9\n}\n");
}

#[test]
fn json_target_with_nested_duplicate_keys_takes_the_last_occurrence() {
    let out = apply(
        r#"{"outer": {"k": 1, "k": 2}}"#,
        &spec(None, "other: 1\n"),
        "/tmp/a.json",
    );
    let parsed: serde_yaml::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["outer"]["k"], serde_yaml::Value::from(2));
}

#[test]
fn json_preserves_the_targets_key_order() {
    // The whole point of `Patch` is leaving untouched content alone; a
    // `serde_json::Value` round-trip would re-sort a user's settings.json
    // alphabetically. Asserted on the raw text — re-parsing hides ordering.
    let current = r#"{"zeta": 1, "alpha": {"nested": true, "aaa": 2}, "middle": 3}"#;
    let out = assert_converges(
        current,
        &spec(None, "alpha:\n  nested: false\n"),
        "/tmp/settings.json",
    );
    assert_eq!(
        out,
        "{\n  \"zeta\": 1,\n  \"alpha\": {\n    \"nested\": false,\n    \"aaa\": 2\n  },\n  \"middle\": 3\n}\n"
    );
}

#[test]
fn json_appends_new_keys_after_the_existing_ones() {
    let out = apply(
        r#"{"zeta": 1, "alpha": 2}"#,
        &spec(None, "beta: 3\n"),
        "/tmp/a.json",
    );
    assert_eq!(
        out,
        "{\n  \"zeta\": 1,\n  \"alpha\": 2,\n  \"beta\": 3\n}\n"
    );
}

#[test]
fn json_preserves_integers_beyond_the_signed_64_bit_range() {
    let out = apply(
        r#"{"big": 18446744073709551615}"#,
        &spec(None, "other: 1\n"),
        "/tmp/a.json",
    );
    assert!(out.contains("18446744073709551615"), "exact u64: {out}");
}

#[test]
fn json_values_are_literal_not_templated() {
    let out = assert_converges("{}", &spec(None, "editor: \"{{ tera }}\"\n"), "/tmp/a.json");
    assert_eq!(out, "{\n  \"editor\": \"{{ tera }}\"\n}\n");
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
    assert_converges("logging:\n  level: info\n", &s, "/tmp/a.yaml");
}

#[test]
fn yaml_invalid_current_content_is_a_typed_error() {
    let err = apply_err("a: [unclosed\n", &spec(None, "b: 1\n"), "/tmp/a.yaml");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchParse { .. }),
        "PatchParse",
    );
}

#[test]
fn yaml_preserves_the_targets_key_order() {
    // Backs the docs caveat: the YAML engine loses comments and blank lines,
    // but NOT key order. Asserted on the raw text — re-parsing hides ordering.
    let current = "zeta: 1\nalpha: 2\nmiddle: 3\n";
    let out = assert_converges(current, &spec(None, "alpha: 9\n"), "/tmp/a.yaml");
    assert_eq!(out, "zeta: 1\nalpha: 9\nmiddle: 3\n");
}

#[test]
fn yaml_accepts_non_finite_numbers_json_rejects() {
    // YAML can express `.nan`/`.inf`, so the JSON-only guard must not leak here.
    let out = apply("", &spec(None, "ratio: .inf\n"), "/tmp/a.yaml");
    assert_eq!(out, "ratio: .inf\n");
}

#[test]
fn yaml_multi_document_input_is_a_typed_error() {
    let err = apply_err("a: 1\n---\nb: 2\n", &spec(None, "c: 3\n"), "/tmp/a.yaml");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchParse { .. }),
        "PatchParse",
    );
}

#[test]
fn yaml_values_are_literal_not_templated() {
    let out = assert_converges("", &spec(None, "greeting: \"{{ name }}\"\n"), "/tmp/a.yaml");
    assert_eq!(out, "greeting: '{{ name }}'\n");
}

#[test]
fn json_and_yaml_merges_agree_on_the_same_structure() {
    // Both formats route through `deep_merge_yaml`; this pins that they cannot
    // drift apart in nesting or replacement semantics.
    let ensure = "a:\n  b: 2\nlist: [9]\n";
    let json = apply(
        r#"{"a": {"b": 1, "keep": true}, "list": [1, 2], "other": "x"}"#,
        &spec(None, ensure),
        "/tmp/a.json",
    );
    let yaml = apply(
        "a:\n  b: 1\n  keep: true\nlist:\n- 1\n- 2\nother: x\n",
        &spec(None, ensure),
        "/tmp/a.yaml",
    );
    let from_json: serde_yaml::Value = serde_json::from_str(&json).expect("json parses");
    let from_yaml: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("yaml parses");
    assert_eq!(from_json, from_yaml);
}

#[test]
fn yaml_non_mapping_current_content_is_a_typed_error() {
    let err = apply_err("- one\n- two\n", &spec(None, "b: 1\n"), "/tmp/a.yaml");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchParse { .. }),
        "PatchParse",
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
    let err = compute_patched(
        "current\n",
        &script_spec("./definitely-not-a-real-script"),
        &target,
        &ctx_for(dir.path()),
    )
    .expect_err("missing script must fail");
    assert_file_err(
        &err,
        |e| matches!(e, FileError::PatchScriptFailed { .. }),
        "PatchScriptFailed",
    );
    assert!(
        err.to_string().contains("definitely-not-a-real-script"),
        "names the script: {err}"
    );
}

#[test]
fn evaluate_patch_reads_the_target_and_reports_convergence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("settings.json");
    std::fs::write(&target, "{\n  \"telemetry\": false\n}\n").expect("seed target");

    let ensure = spec(None, "telemetry: false");
    let converged =
        evaluate_patch(&ensure, &target, &ctx_for(dir.path())).expect("evaluate succeeds");
    assert!(converged.is_up_to_date());
    assert_eq!(converged.current, converged.patched);

    std::fs::write(&target, "{\n  \"telemetry\": true\n}\n").expect("drift the target");
    let drifted =
        evaluate_patch(&ensure, &target, &ctx_for(dir.path())).expect("evaluate succeeds");
    assert!(!drifted.is_up_to_date());
    assert!(drifted.patched.contains("false"));
}

#[test]
fn evaluate_patch_treats_a_missing_target_as_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outcome = evaluate_patch(
        &spec(None, "telemetry: false"),
        &dir.path().join("absent.json"),
        &ctx_for(dir.path()),
    )
    .expect("a missing target reads as empty");
    assert_eq!(outcome.current, "");
    assert!(!outcome.is_up_to_date());
}

#[test]
fn evaluate_patch_surfaces_an_unreadable_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A directory where a file is expected: readable path, unreadable
    // content. Treating it as empty would overwrite it on apply.
    let err = evaluate_patch(
        &spec(None, "telemetry: false"),
        dir.path(),
        &ctx_for(dir.path()),
    )
    .expect_err("an unreadable target must not read as empty");
    assert!(
        matches!(
            err,
            crate::errors::CfgdError::File(crate::errors::FileError::Io { .. })
        ),
        "expected a typed IO error, got: {err:?}"
    );
}

/// Resolved module rooted at `dir` declaring one env var.
fn module_at(dir: &Path) -> crate::modules::ResolvedModule {
    crate::modules::ResolvedModule {
        name: "hosts-mod".to_string(),
        packages: Vec::new(),
        files: Vec::new(),
        env: vec![crate::config::EnvVar {
            name: "BUILD_HOST".to_string(),
            value: "build.internal".to_string(),
        }],
        aliases: Vec::new(),
        system: std::collections::HashMap::new(),
        pre_apply_scripts: Vec::new(),
        post_apply_scripts: Vec::new(),
        pre_reconcile_scripts: Vec::new(),
        post_reconcile_scripts: Vec::new(),
        on_change_scripts: Vec::new(),
        on_drift_scripts: Vec::new(),
        depends: Vec::new(),
        dir: dir.to_path_buf(),
        platform_skip_reason: None,
        origin: None,
    }
}

#[test]
fn for_origin_binds_a_module_file_to_its_module_directory() {
    let module_dir = tempfile::tempdir().expect("tempdir");
    let modules = vec![module_at(module_dir.path())];
    let binding = PatchBinding::for_origin(
        Path::new("/config"),
        "work",
        crate::reconciler::ReconcileContext::Apply,
        &modules,
        &crate::effective::Origin::Module("hosts-mod".to_string()),
    )
    .expect("a known module resolves");
    assert_eq!(binding.script_dir, module_dir.path());
}

#[test]
fn for_origin_binds_a_profile_file_to_the_config_directory() {
    let binding = PatchBinding::for_origin(
        Path::new("/config"),
        "work",
        crate::reconciler::ReconcileContext::Apply,
        &[],
        &crate::effective::Origin::Profile,
    )
    .expect("the profile origin never needs a module");
    assert_eq!(binding.script_dir, Path::new("/config"));
}

/// Falling back to the profile binding here would anchor the filter at the
/// config directory, silently turning a relative `script:` into an inline
/// command. The unresolvable origin must surface as a typed error instead.
#[test]
fn for_origin_rejects_an_origin_naming_an_absent_module() {
    let err = PatchBinding::for_origin(
        Path::new("/config"),
        "work",
        crate::reconciler::ReconcileContext::Apply,
        &[],
        &crate::effective::Origin::Module("ghost".to_string()),
    )
    .expect_err("an unknown module must not fall back to the profile binding");
    assert!(
        matches!(
            err,
            CfgdError::Module(crate::errors::ModuleError::NotFound { ref name }) if name == "ghost"
        ),
        "expected a typed ModuleError::NotFound, got: {err:?}"
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
        let out = compute_patched(
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
        let out = compute_patched(
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
        let out = compute_patched(
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
        let err = compute_patched(
            "content\n",
            &script_spec("fail.sh"),
            Path::new("/etc/hosts"),
            &ctx_for(dir.path()),
        )
        .expect_err("non-zero exit must fail");
        assert_file_err(
            &err,
            |e| matches!(e, FileError::PatchScriptFailed { .. }),
            "PatchScriptFailed",
        );
        let text = err.to_string();
        assert!(text.contains("exit 3"), "carries exit code: {text}");
        assert!(
            text.contains("refusing: target is managed elsewhere"),
            "carries stderr: {text}"
        );
    }

    #[test]
    fn script_non_executable_file_is_a_typed_error_naming_the_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("noexec.sh");
        std::fs::write(&path, "#!/bin/sh\ncat\n").expect("write");
        let err = compute_patched(
            "content\n",
            &script_spec("noexec.sh"),
            Path::new("/etc/hosts"),
            &ctx_for(dir.path()),
        )
        .expect_err("non-executable script must fail");
        assert_file_err(
            &err,
            |e| matches!(e, FileError::PatchScriptFailed { .. }),
            "PatchScriptFailed",
        );
        let text = err.to_string();
        assert!(text.contains("not executable"), "explains the fix: {text}");
        assert!(text.contains("/etc/hosts"), "names the target: {text}");
    }

    #[test]
    fn script_with_a_missing_working_directory_names_the_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_dir = dir.path().to_path_buf();
        write_script(&script_dir, "filter.sh", "#!/bin/sh\ncat\n");
        let gone = dir.path().join("not-there");
        let ctx = PatchContext::new(&script_dir).with_working_dir(&gone);
        let err = compute_patched(
            "content\n",
            &script_spec("filter.sh"),
            Path::new("/etc/hosts"),
            &ctx,
        )
        .expect_err("missing working directory must fail");
        assert_file_err(
            &err,
            |e| matches!(e, FileError::PatchScriptFailed { .. }),
            "PatchScriptFailed",
        );
        let text = err.to_string();
        assert!(
            text.contains("working directory does not exist"),
            "names the real cause: {text}"
        );
        assert!(text.contains("/etc/hosts"), "names the target: {text}");
    }

    #[test]
    fn documented_hosts_filter_example_is_idempotent() {
        // Ground-truths the `ensure-hosts-entry.sh` block in
        // docs/configuration.md: a filter that drains stdin with a bare `cat`
        // appends its entry on every reconcile.
        let dir = tempfile::tempdir().expect("tempdir");
        write_script(
            dir.path(),
            "ensure-hosts-entry.sh",
            "#!/bin/sh\n\
             content=$(cat)\n\
             printf '%s\\n' \"$content\"\n\
             printf '%s\\n' \"$content\" | grep -q '10.0.0.5 build.internal' \\\n\
               || echo '10.0.0.5 build.internal'\n",
        );
        let spec = script_spec("ensure-hosts-entry.sh");
        let run = |input: &str| {
            compute_patched(input, &spec, Path::new("/etc/hosts"), &ctx_for(dir.path()))
                .expect("filter succeeds")
        };
        let once = run("127.0.0.1 localhost\n");
        assert_eq!(once, "127.0.0.1 localhost\n10.0.0.5 build.internal\n");
        assert_eq!(run(&once), once, "second pass appended again");
    }

    #[test]
    fn freshly_written_scripts_run_under_concurrent_spawns() {
        // Guards the ETXTBSY retry: sibling threads forking while this thread
        // execs a just-written script used to fail at random with
        // "Text file busy". Each thread writes its own script and runs it.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let mut handles = Vec::new();
        for n in 0..8 {
            let root = root.clone();
            handles.push(std::thread::spawn(move || {
                let sub = root.join(format!("t{n}"));
                std::fs::create_dir_all(&sub).expect("create subdir");
                write_script(&sub, "filter.sh", "#!/bin/sh\ncat\n");
                let ctx = PatchContext::new(&sub).with_working_dir(&sub);
                compute_patched(
                    "payload\n",
                    &script_spec("filter.sh"),
                    Path::new("/etc/hosts"),
                    &ctx,
                )
            }));
        }
        for handle in handles {
            let out = handle
                .join()
                .expect("thread did not panic")
                .expect("filter succeeds");
            assert_eq!(out, "payload\n");
        }
    }

    #[test]
    fn script_timeout_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_script(dir.path(), "hang.sh", "#!/bin/sh\nsleep 30\n");
        let ctx = PatchContext::new(dir.path())
            .with_working_dir(dir.path())
            .with_timeout(std::time::Duration::from_millis(200));
        let err = compute_patched(
            "content\n",
            &script_spec("hang.sh"),
            Path::new("/etc/hosts"),
            &ctx,
        )
        .expect_err("hanging script must time out");
        assert_file_err(
            &err,
            |e| matches!(e, FileError::PatchScriptFailed { .. }),
            "PatchScriptFailed",
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
        let ctx = PatchContext::new(dir.path())
            .with_working_dir(dir.path())
            .with_env(&env);
        let out = compute_patched("", &script_spec("env.sh"), Path::new("/etc/hosts"), &ctx)
            .expect("filter succeeds");
        assert_eq!(out, "demo\n");
    }

    #[test]
    fn default_context_injects_no_cfgd_environment() {
        // Pins the default: cfgd-core cannot synthesize the CFGD_* metadata (it
        // needs the config dir, profile, and phase), so a dispatch site must
        // pass `build_module_script_env` output via `with_env`. A wiring change
        // that forgets it shows up here as an empty value, not a silent gap.
        let dir = tempfile::tempdir().expect("tempdir");
        write_script(
            dir.path(),
            "env.sh",
            "#!/bin/sh\necho \"[${CFGD_MODULE_NAME}]\"\n",
        );
        let out = compute_patched(
            "",
            &script_spec("env.sh"),
            Path::new("/etc/hosts"),
            &ctx_for(dir.path()),
        )
        .expect("filter succeeds");
        assert_eq!(out, "[]\n");
    }

    const ENV_ECHO: &str =
        "#!/bin/sh\necho \"[${CFGD_MODULE_NAME}|${BUILD_HOST}|${CFGD_PROFILE}|${CFGD_PHASE}]\"\n";

    #[test]
    fn module_binding_anchors_scripts_at_the_module_dir_with_its_env() {
        let module_dir = tempfile::tempdir().expect("tempdir");
        let home = tempfile::tempdir().expect("tempdir");
        write_script(module_dir.path(), "env.sh", ENV_ECHO);

        let module = module_at(module_dir.path());
        let binding = PatchBinding::module(
            Path::new("/config"),
            "work",
            crate::reconciler::ReconcileContext::Apply,
            &module,
        );
        let out = crate::with_test_home(home.path(), || {
            compute_patched(
                "",
                &script_spec("env.sh"),
                Path::new("/etc/hosts"),
                &binding.context(),
            )
            .expect("filter succeeds")
        });
        assert_eq!(out, "[hosts-mod|build.internal|work|patch]\n");
    }

    #[test]
    fn profile_binding_anchors_scripts_at_the_config_dir_with_no_module() {
        let config_dir = tempfile::tempdir().expect("tempdir");
        let home = tempfile::tempdir().expect("tempdir");
        write_script(config_dir.path(), "env.sh", ENV_ECHO);

        let binding = PatchBinding::profile(
            config_dir.path(),
            "work",
            crate::reconciler::ReconcileContext::Apply,
        );
        let out = crate::with_test_home(home.path(), || {
            compute_patched(
                "",
                &script_spec("env.sh"),
                Path::new("/etc/hosts"),
                &binding.context(),
            )
            .expect("filter succeeds")
        });
        assert_eq!(out, "[||work|patch]\n");
    }

    #[test]
    fn script_runs_as_an_inline_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = compute_patched(
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
            compute_patched(
                "",
                &script_spec("pwd.sh"),
                Path::new("/etc/hosts"),
                &PatchContext::new(dir.path()),
            )
        })
        .expect("filter succeeds");
        let expected = std::fs::canonicalize(&home).expect("canonicalize home");
        let actual = std::fs::canonicalize(out.trim()).expect("canonicalize pwd");
        assert_eq!(actual, expected);
    }
}

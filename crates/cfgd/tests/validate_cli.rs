//! Tests for the `cfgd <kind> validate <file|->` command surface.
//!
//! In-process Doc-capture style (`Printer::for_test_doc()` → `cap.json()`),
//! matching `explain_snapshots.rs`. The CLI command bodies read YAML from a
//! file path (a temp file under the test's tempdir — tests never touch real
//! `$HOME`) and emit a `Doc` carrying `{kind, valid, errors}`.

use std::path::Path;

use cfgd::cli::validate::{
    cmd_clusterconfigpolicy_validate, cmd_configpolicy_validate, cmd_machineconfig_validate,
    cmd_module_validate, cmd_profile_validate, cmd_source_validate, run_validate,
};
use cfgd_core::output::Printer;

/// Write `yaml` to a temp file and return the (tempdir, path) pair. The tempdir
/// must outlive the path, so the caller binds both.
fn yaml_file(yaml: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("doc.yaml");
    std::fs::write(&path, yaml).expect("write temp yaml");
    (dir, path)
}

fn path_str(p: &Path) -> String {
    p.to_str().expect("utf8 temp path").to_string()
}

#[test]
fn validate_module_valid_json() {
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: m\nspec: {}\n";
    let (_dir, path) = yaml_file(yaml);
    let (printer, cap) = Printer::for_test_doc();
    let result = run_validate(&printer, "Module", &path_str(&path));
    drop(printer);
    assert!(result.is_ok(), "valid Module must succeed, got: {result:?}");
    let payload = cap.json().expect("doc captured json");
    assert_eq!(
        payload.get("kind").and_then(|v| v.as_str()),
        Some("Module"),
        "payload must carry kind=Module, got: {payload}"
    );
    assert_eq!(
        payload.get("valid").and_then(|v| v.as_bool()),
        Some(true),
        "valid Module payload must report valid=true, got: {payload}"
    );
    assert_eq!(
        payload
            .get("errors")
            .and_then(|v| v.as_array())
            .map(Vec::len),
        Some(0),
        "valid Module payload must carry an empty errors array, got: {payload}"
    );
}

#[test]
fn validate_rejects_kind_mismatch() {
    // A `kind: Profile` document fed to the `module` validate path must fail
    // with an error naming BOTH the expected and the found kind.
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: p\nspec: {}\n";
    let (_dir, path) = yaml_file(yaml);
    let (printer, _cap) = Printer::for_test_doc();
    let err =
        run_validate(&printer, "Module", &path_str(&path)).expect_err("kind mismatch must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("Module"),
        "mismatch error must name the expected kind 'Module', got: {msg}"
    );
    assert!(
        msg.contains("Profile"),
        "mismatch error must name the found kind 'Profile', got: {msg}"
    );
}

#[test]
fn validate_module_rejects_unknown_field() {
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: m\nspec:\n  bogusField: 1\n";
    let (_dir, path) = yaml_file(yaml);
    let (printer, _cap) = Printer::for_test_doc();
    let result = run_validate(&printer, "Module", &path_str(&path));
    assert!(
        result.is_err(),
        "unknown spec field must fail the command, got: {result:?}"
    );
    // On the invalid path the result Doc is NOT emitted by run_validate; the
    // structured `{kind, valid, errors}` payload travels as the error's
    // `extras` and is rendered once by the central sink. Pin the ground-truth
    // payload shape consumers actually see by driving that sink here.
    let err = result.expect_err("checked is_err above");
    let (sink, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    let code = cfgd::cli::error::render_cli_error(&sink, &err);
    drop(sink);
    assert_eq!(
        code,
        cfgd_core::exit::ExitCode::ConfigInvalid,
        "an invalid document must exit with the config-invalid code (4)"
    );
    let payload = cap.json().expect("sink captured json payload");
    assert_eq!(
        payload.get("valid").and_then(|v| v.as_bool()),
        Some(false),
        "unknown-field payload must report valid=false, got: {payload}"
    );
    assert_eq!(
        payload.get("kind").and_then(|v| v.as_str()),
        Some("Module"),
        "unknown-field payload must carry kind=Module, got: {payload}"
    );
    let errors = payload
        .get("errors")
        .and_then(|v| v.as_array())
        .expect("errors array present");
    assert!(
        errors
            .iter()
            .filter_map(|e| e.as_str())
            .any(|e| e.to_lowercase().contains("bogusfield")),
        "an error must name the unknown field 'bogusField', got: {errors:?}"
    );
}

/// Every `cmd_*_validate` wrapper hardcodes its kind string literal. Drive all
/// six with a minimal VALID document for the matching kind and assert the
/// captured payload reports `valid=true` and carries the expected kind — a
/// typo'd literal (e.g. `"ConfigSouce"`) would route to the unknown-kind path
/// and flip `valid` to false, failing here.
#[test]
fn every_validate_wrapper_round_trips_its_kind() {
    type Wrapper = fn(&Printer, &str) -> anyhow::Result<()>;
    let cases: &[(&str, Wrapper, &str)] = &[
        (
            "Module",
            cmd_module_validate,
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: m\nspec: {}\n",
        ),
        (
            "Profile",
            cmd_profile_validate,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: p\nspec: {}\n",
        ),
        (
            "ConfigSource",
            cmd_source_validate,
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: s\nspec: {}\n",
        ),
        (
            "MachineConfig",
            cmd_machineconfig_validate,
            "apiVersion: cfgd.io/v1alpha1\nkind: MachineConfig\nmetadata:\n  name: mc\nspec:\n  hostname: host1\n  profile: default\n",
        ),
        (
            "ConfigPolicy",
            cmd_configpolicy_validate,
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigPolicy\nmetadata:\n  name: cp\nspec: {}\n",
        ),
        (
            "ClusterConfigPolicy",
            cmd_clusterconfigpolicy_validate,
            "apiVersion: cfgd.io/v1alpha1\nkind: ClusterConfigPolicy\nmetadata:\n  name: ccp\nspec: {}\n",
        ),
    ];

    for (expected_kind, wrapper, yaml) in cases {
        let (_dir, path) = yaml_file(yaml);
        let (printer, cap) = Printer::for_test_doc();
        let result = wrapper(&printer, &path_str(&path));
        drop(printer);
        assert!(
            result.is_ok(),
            "valid {expected_kind} must pass its wrapper, got: {result:?}"
        );
        let payload = cap.json().expect("doc captured json");
        assert_eq!(
            payload.get("kind").and_then(|v| v.as_str()),
            Some(*expected_kind),
            "{expected_kind} wrapper must carry kind={expected_kind}, got: {payload}"
        );
        assert_eq!(
            payload.get("valid").and_then(|v| v.as_bool()),
            Some(true),
            "{expected_kind} wrapper must report valid=true, got: {payload}"
        );
    }
}

/// A nonexistent path through `run_validate`/`read_source` must surface a clear
/// "failed to read" error and never panic.
#[test]
fn validate_nonexistent_path_is_clear_read_error() {
    let (printer, _cap) = Printer::for_test_doc();
    let missing = "/nonexistent/cfgd-validate-does-not-exist.yaml";
    let err = run_validate(&printer, "Module", missing).expect_err("missing path must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("failed to read") && msg.contains(missing),
        "read error must name the failure and the path, got: {msg}"
    );
}

/// An empty file through `run_validate` must surface a clear (missing-kind)
/// validation error, not a panic.
#[test]
fn validate_empty_file_is_clear_error_no_panic() {
    let (_dir, path) = yaml_file("");
    let (printer, _cap) = Printer::for_test_doc();
    let err = run_validate(&printer, "Module", &path_str(&path))
        .expect_err("empty file must fail validation");
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("kind"),
        "empty-file error must name the missing 'kind' field, got: {msg}"
    );
}

/// A multi-document YAML stream (two `---`-separated docs) must surface a clear
/// "YAML syntax error", not a panic. serde_yaml rejects a stream of >1 document.
#[test]
fn validate_multi_document_stream_is_yaml_syntax_error() {
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: a\nspec: {}\n---\napiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: b\nspec: {}\n";
    let (_dir, path) = yaml_file(yaml);
    let (printer, _cap) = Printer::for_test_doc();
    let err = run_validate(&printer, "Module", &path_str(&path))
        .expect_err("multi-doc stream must fail validation");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("YAML syntax error"),
        "multi-doc stream must report a YAML syntax error, got: {msg}"
    );
}

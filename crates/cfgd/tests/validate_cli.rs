//! Tests for the `cfgd <kind> validate <file|->` command surface.
//!
//! In-process Doc-capture style (`Printer::for_test_doc()` → `cap.json()`),
//! matching `explain_snapshots.rs`. The CLI command bodies read YAML from a
//! file path (a temp file under the test's tempdir — tests never touch real
//! `$HOME`) and emit a `Doc` carrying `{kind, valid, errors}`.

use std::path::Path;

use cfgd::cli::validate::run_validate;
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

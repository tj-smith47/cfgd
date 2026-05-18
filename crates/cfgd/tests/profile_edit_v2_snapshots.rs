//! Snapshot tests for `cfgd profile edit`.
//!
//! Cases:
//!   - `profile_edit/valid.txt` — happy path: editor closes, YAML parses,
//!     final Doc emits `Role::Ok "Profile 'X' is valid"` with the
//!     `{valid: true, errors: []}` payload.
//!   - `profile_edit/validation_error_decline.txt` — editor leaves invalid
//!     YAML, user declines re-edit prompt, final Doc emits `Role::Warn
//!     "Saved with validation errors"`.
//!
//! `cmd_profile_edit` builds its Doc inline; these tests reconstruct the
//! exact Doc shape (matching the production code) so the renderer
//! coverage holds without needing a real $EDITOR. The editor invocation
//! itself is exercised by the `#[cfg(unix)]` unit test in
//! `src/cli/profile/tests.rs`.
//!
//! Goldens live under `tests/output_snapshots/profile_edit/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_edit_v2_snapshots

use std::path::Path;

use cfgd_core::output_v2::{Doc, Printer, Role};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn valid_doc(name: &str) -> Doc {
    Doc::new()
        .status(Role::Ok, format!("Profile '{}' is valid", name))
        .with_data(serde_json::json!({
            "name": name,
            "valid": true,
            "errors": Vec::<String>::new(),
        }))
}

fn declined_doc(name: &str, errors: Vec<String>) -> Doc {
    Doc::new()
        .status(Role::Warn, "Saved with validation errors")
        .with_data(serde_json::json!({
            "name": name,
            "valid": false,
            "errors": errors,
        }))
}

#[test]
fn profile_edit_valid_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(valid_doc("default"));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_edit/valid.txt");
}

#[test]
fn profile_edit_valid_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(valid_doc("default"));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "default");
    assert_eq!(json["valid"], true);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_edit/valid.json");
}

#[test]
fn profile_edit_validation_error_decline_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.status_simple(
        Role::Fail,
        "Profile 'default' has errors: missing field `kind`",
    );
    printer.emit(declined_doc(
        "default",
        vec!["missing field `kind`".to_string()],
    ));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "profile_edit/validation_error_decline.txt",
    );
}

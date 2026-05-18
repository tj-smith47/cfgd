//! Snapshot tests for `cfgd profile update`.
//!
//! Cases:
//!   - `profile_update/happy.{txt,json}` — one env-var change applied.
//!   - `profile_update/no_changes.txt` — empty args list emits
//!     `Role::Info "No changes specified"` with `{changes: 0}`.
//!   - `profile_update/add_remove_mixed.txt` — two mixed status lines
//!     plus the final summary Doc, sharing the same buffer surface.
//!
//! `cmd_profile_update` emits both streaming `status_simple` lines (one
//! per add/remove operation) and a final buffered Doc summary; the
//! snapshots reconstruct the exact rendered shape so the renderer
//! coverage holds without exercising the full reconciler.
//!
//! Goldens live under `tests/output_snapshots/profile_update/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_update_v2_snapshots

use std::path::Path;

use cfgd_core::output_v2::{Doc, Printer, Role};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn updated_doc(name: &str, changes: u32) -> Doc {
    Doc::new()
        .status(
            Role::Ok,
            format!("Updated profile '{}' ({} change(s))", name, changes),
        )
        .with_data(serde_json::json!({"name": name, "changes": changes}))
}

fn no_changes_doc(name: &str) -> Doc {
    Doc::new()
        .status(Role::Info, "No changes specified")
        .with_data(serde_json::json!({"name": name, "changes": 0}))
}

#[test]
fn profile_update_happy_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Update Profile: default");
    printer.status_simple(Role::Ok, "Set env: EDITOR=nvim");
    printer.emit(updated_doc("default", 1));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_update/happy.txt");
}

#[test]
fn profile_update_happy_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(updated_doc("default", 1));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "default");
    assert_eq!(json["changes"], 1);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_update/happy.json");
}

#[test]
fn profile_update_no_changes_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Update Profile: default");
    printer.emit(no_changes_doc("default"));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_update/no_changes.txt");
}

#[test]
fn profile_update_no_changes_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(no_changes_doc("default"));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["changes"], 0);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_update/no_changes.json");
}

#[test]
fn profile_update_add_remove_mixed_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Update Profile: default");
    printer.status_simple(Role::Ok, "Added module: nvim");
    printer.status_simple(Role::Ok, "Removed env: EDITOR");
    printer.status_simple(Role::Warn, "Module 'missing' not found in profile");
    printer.emit(updated_doc("default", 2));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "profile_update/add_remove_mixed.txt",
    );
}

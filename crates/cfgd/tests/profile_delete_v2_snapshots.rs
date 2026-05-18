//! Snapshot tests for `cfgd profile delete`.
//!
//! Cases:
//!   - `profile_delete/happy.{txt,json}` — `--yes` path: file removed,
//!     `Role::Ok "Deleted profile '<name>'"` emitted.
//!   - `profile_delete/cancelled.txt` — prompt declined: `Role::Info
//!     "Cancelled"` with `{cancelled: true}` payload.
//!
//! `cmd_profile_delete` builds its emitted Doc inline; the snapshots
//! reconstruct the exact Doc shapes. Active-profile and inheritor refusal
//! paths use `anyhow::bail` (no Doc emit) and are covered by the unit
//! tests in `src/cli/profile/tests.rs`.
//!
//! Goldens live under `tests/output_snapshots/profile_delete/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_delete_v2_snapshots

use std::path::Path;

use cfgd_core::output_v2::{Doc, Printer, Role};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn deleted_doc(name: &str) -> Doc {
    Doc::new()
        .status(Role::Ok, format!("Deleted profile '{}'", name))
        .with_data(serde_json::json!({"name": name, "cancelled": false}))
}

fn cancelled_doc(name: &str) -> Doc {
    Doc::new()
        .status(Role::Info, "Cancelled")
        .with_data(serde_json::json!({"name": name, "cancelled": true}))
}

#[test]
fn profile_delete_happy_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Delete Profile: work");
    printer.emit(deleted_doc("work"));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_delete/happy.txt");
}

#[test]
fn profile_delete_happy_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(deleted_doc("work"));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "work");
    assert_eq!(json["cancelled"], false);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_delete/happy.json");
}

#[test]
fn profile_delete_cancelled_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Delete Profile: work");
    printer.emit(cancelled_doc("work"));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_delete/cancelled.txt");
}

#[test]
fn profile_delete_cancelled_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(cancelled_doc("work"));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["cancelled"], true);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_delete/cancelled.json");
}

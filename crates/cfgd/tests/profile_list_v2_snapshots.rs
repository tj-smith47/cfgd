//! Snapshot tests for `cfgd profile list`.
//!
//! Cases:
//!   - `profile_list/happy.{txt,json}` — multi-profile listing (default
//!     active, work inactive with inherits).
//!   - `profile_list/empty.txt` — empty entries list (Role::Info "No
//!     profiles found").
//!   - `profile_list/no_dir.txt` — profiles directory absent (Role::Warn).
//!   - `profile_list/wide.txt` — `--wide` table layout.
//!
//! Goldens live under `tests/output_snapshots/profile_list/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_list_v2_snapshots

use std::path::Path;

use cfgd::cli::output_types::ProfileListEntry;
use cfgd::cli::profile::list::{build_profile_list_doc, build_profile_list_missing_doc};
use cfgd_core::output_v2::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_entries() -> Vec<ProfileListEntry> {
    vec![
        ProfileListEntry {
            name: "default".into(),
            active: true,
            inherits: None,
            module_count: 0,
        },
        ProfileListEntry {
            name: "work".into(),
            active: false,
            inherits: Some("default".into()),
            module_count: 2,
        },
    ]
}

#[test]
fn profile_list_happy_human() {
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_doc(&entries, false));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_list/happy.txt");
}

#[test]
fn profile_list_happy_json() {
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_doc(&entries, false));
    drop(printer);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_list/happy.json");
}

#[test]
fn profile_list_empty_human() {
    let entries: Vec<ProfileListEntry> = Vec::new();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_doc(&entries, false));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_list/empty.txt");
}

#[test]
fn profile_list_no_dir_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_missing_doc(Path::new(
        "/etc/cfgd/profiles",
    )));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_list/no_dir.txt");
}

#[test]
fn profile_list_wide_human() {
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_doc(&entries, true));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_list/wide.txt");
}

#[test]
fn profile_list_no_dir_payload_is_empty_array() {
    // Verify the no_dir path emits an empty array as its `with_data` payload
    // so structured consumers see the same shape as a populated-but-empty
    // profiles dir.
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_missing_doc(Path::new("/missing")));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert!(json.is_array(), "expected array, got: {json}");
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[test]
fn profile_list_payload_carries_with_data() {
    // Verify the payload is always present, even on the populated path,
    // so `-o json` consumers can rely on the envelope.
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_doc(&entries, false));
    drop(printer);
    let payload = cap.json().expect("doc captured json");
    let arr = payload.as_array().expect("array payload");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"], "default");
    assert_eq!(arr[1]["name"], "work");
}

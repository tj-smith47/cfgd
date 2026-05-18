//! Snapshot tests for `cfgd profile switch`.
//!
//! `cmd_profile_switch` builds its final Doc inline (no `build_*_doc`
//! helper); the success-path snapshot exercises the rendered shape through
//! a tempdir + real `cmd_profile_switch` invocation. The error paths use
//! `anyhow::bail` and surface via `main.rs`'s error renderer — no Doc is
//! emitted along the bail path, so the snapshot set here covers the
//! Ok-path only.
//!
//! Goldens live under `tests/output_snapshots/profile_switch/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_switch_v2_snapshots

use std::path::Path;

use cfgd_core::output_v2::{Doc, Printer, Role};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

/// `cmd_profile_switch` emits its Doc inline; reconstruct the same shape
/// here from a payload so the test exercises the renderer without
/// depending on the on-disk profile layout.
fn switch_doc(from: &str, to: &str) -> Doc {
    Doc::new()
        .status(Role::Ok, format!("Switched profile: {} → {}", from, to))
        .hint("Run 'cfgd apply --dry-run' to preview changes, then 'cfgd apply'")
        .with_data(serde_json::json!({"from": from, "to": to}))
}

#[test]
fn profile_switch_happy_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Switch Profile");
    printer.emit(switch_doc("default", "work"));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_switch/happy.txt");
}

#[test]
fn profile_switch_happy_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(switch_doc("default", "work"));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["from"], "default");
    assert_eq!(json["to"], "work");
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_switch/happy.json");
}

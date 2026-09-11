#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

//! Exit-code and stdout-shape regression tests for `cfgd backup run` and
//! `cfgd backup gc`.
//!
//! Both verbs end in `std::process::exit` when a unit failed, was dirty, was
//! refused, or could not be asked about at all, so these drive the real binary
//! — an in-process call would take the test harness down with it. Running the
//! real binary is also the only way to see stdout exactly as a scripted
//! consumer does: the in-process `Doc` capture keeps a single `Option<Value>`,
//! so a second emitted document overwrites the first instead of appending,
//! which is precisely the failure shape under test here.

mod common;

use assert_cmd::Command;
use common::{backup_profile_setup, strand_a_snapshot};

#[test]
fn backup_run_json_emits_exactly_one_document_when_a_unit_is_busy() {
    // `-o json` promises one top-level document. Reporting the busy unit as a
    // returned error made the central error sink emit a SECOND document after
    // the payload array, so `json.loads` failed with "Extra data" and any
    // single-document reader broke — on a payload `docs/cli-reference.md`
    // documents as parseable.
    let (config_dir, state_dir, _source) = backup_profile_setup();

    let _held =
        cfgd_core::acquire_backup_lock(state_dir.path(), "docs").expect("hold the docs lock");

    let out = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "backup", "run"])
        .arg("--config")
        .arg(config_dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .output()
        .expect("run cfgd backup run");

    assert_eq!(
        out.status.code(),
        Some(1),
        "a unit the user asked to run did not run — the command must exit nonzero"
    );

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("stdout must be ONE json document, got {e}: {stdout:?}");
    });

    let entries = parsed.as_array().expect("the payload is an array");
    assert_eq!(entries.len(), 2, "both units are reported: {entries:?}");
    let docs = entries
        .iter()
        .find(|e| e["name"] == "docs")
        .expect("the busy unit stays in the payload");
    assert_eq!(docs["status"], "skipped");
    assert_eq!(
        entries
            .iter()
            .find(|e| e["name"] == "weekly")
            .expect("the unblocked unit still ran")["status"],
        "success",
        "one unit's collision must not abandon the rest of the set"
    );
}

#[test]
fn backup_run_exits_zero_when_every_unit_runs_clean() {
    let (config_dir, state_dir, _source) = backup_profile_setup();

    let out = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "backup", "run"])
        .arg("--config")
        .arg(config_dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .output()
        .expect("run cfgd backup run");

    assert!(
        out.status.success(),
        "a clean run must exit 0, got {:?}",
        out.status.code()
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("one json document: {e}"));
    assert!(
        parsed
            .as_array()
            .expect("array payload")
            .iter()
            .all(|e| e["status"] == "success"),
        "got: {parsed}"
    );
}

#[test]
fn backup_gc_exits_nonzero_when_a_units_history_cannot_be_read() {
    // A unit nothing could be asked about is a failure of the run, and the
    // failure has to reach a scripted caller: the real binary is the only way
    // to see the code, `cmd_backup_gc` ending in `std::process::exit`.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    cfgd_core::state::StateStore::open_in_dir(state_dir.path())
        .expect("state store")
        .drop_backup_runs_table()
        .expect("take the history away");

    let out = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "backup", "gc"])
        .arg("--config")
        .arg(config_dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .output()
        .expect("run cfgd backup gc");

    assert_eq!(
        out.status.code(),
        Some(1),
        "a history cfgd could not read is a failure the caller must see"
    );

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be ONE json document, got {e}: {stdout:?}"));
    assert_eq!(
        parsed["unreadable"],
        serde_json::json!(["docs", "weekly"]),
        "the payload names every unit that could not be asked: {parsed}"
    );
}

#[test]
fn backup_gc_exits_nonzero_when_a_recorded_payload_cannot_be_removed() {
    // A removal that failed is a different failure from a history that could
    // not be read, and it reaches the caller through the same code.
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = config_dir.path().join("data").join("notes.txt");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "hello backup").unwrap();

    let (stranded, _) = strand_a_snapshot(config_dir.path(), state_dir.path(), &source);
    let held = cfgd_core::test_helpers::hold_payload_unremovable(&stranded);

    let out = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "backup", "gc"])
        .arg("--config")
        .arg(config_dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .output()
        .expect("run cfgd backup gc");

    assert_eq!(
        out.status.code(),
        Some(1),
        "a payload cfgd recorded and could not remove is a failure the caller must see"
    );

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be ONE json document, got {e}: {stdout:?}"));
    assert_eq!(
        parsed["collected"],
        serde_json::json!([]),
        "nothing was collected: {parsed}"
    );
    assert!(
        held.witness_survives(),
        "gc removed what it could not remove"
    );
}

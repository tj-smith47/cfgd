//! Snapshot tests for `cfgd rollback`.
//!
//! Real `cmd_rollback` capture against tempdir state DBs seeded directly
//! through the public `StateStore` API. Snapshots lock the section headers,
//! statuses, kv block, and buffered summary.
//!
//! Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test rollback_snapshots

mod common;

use std::path::Path;

use cfgd::cli::output_types::RollbackOutput;
use cfgd::cli::rollback::{build_rollback_doc, cmd_rollback};
use cfgd_core::output::{Doc, Printer, PromptAnswer, Role, Verbosity};
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;
use pretty_assertions::assert_eq;

use common::{
    rollback_state_no_changes_setup, rollback_state_with_backups_setup,
    rollback_state_with_non_file_actions_setup,
};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

/// Real `cmd_rollback` against a seeded state DB with file backups — restores
/// v1 content of one file from apply 2's backup row. Locks the kv prelude,
/// the Restoring section status, the buffered "Rollback complete" line.
#[test]
fn rollback_happy_human() {
    let (_workspace, state_dir, target, apply_id) = rollback_state_with_backups_setup();

    let (printer, cap) = Printer::for_test_doc();

    cmd_rollback(&printer, apply_id, true, Some(state_dir.path())).unwrap();
    drop(printer);

    let normalized = cap
        .human()
        .replace(&target.display().to_string(), "<TARGET>");
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "rollback/happy.txt", &stripped);
}

/// JSON payload roundtrip — RollbackOutput shape via build_rollback_doc + cap.json().
#[test]
fn rollback_happy_json() {
    let output = RollbackOutput {
        apply_id: 1,
        files_restored: 1,
        files_removed: 0,
        non_file_actions: Vec::new(),
    };
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_rollback_doc(&output));
    drop(printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("rollback doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(RollbackOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "rollback/happy.json");
}

/// Target apply has no subsequent changes — `cmd_rollback` short-circuits
/// with the "already at this apply" status.
#[test]
fn rollback_no_changes_human() {
    let (state_dir, apply_id) = rollback_state_no_changes_setup();

    let (printer, cap) = Printer::for_test_doc();

    cmd_rollback(&printer, apply_id, true, Some(state_dir.path())).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "rollback/no_changes.txt",
        &stripped,
    );
}

/// `yes=false` + `PromptAnswer::Confirm(true)` drives the accept path:
/// prompt fires silently, reconciler restores, "Rollback complete" lands.
/// The accept-confirm-then-success pattern needs a dedicated snapshot
/// because the rejection snapshot stops before the success surface emits.
#[test]
fn rollback_accept_human() {
    let (_workspace, state_dir, target, apply_id) = rollback_state_with_backups_setup();

    let (printer, buf) = Printer::for_test_with_prompt_responses_at(
        vec![PromptAnswer::Confirm(true)],
        Verbosity::Normal,
    );

    cmd_rollback(&printer, apply_id, false, Some(state_dir.path())).unwrap();
    printer.flush();
    drop(printer);

    let raw = buf.lock().unwrap().clone();
    let normalized = raw.replace(&target.display().to_string(), "<TARGET>");
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "rollback/accept.txt", &stripped);
}

/// `yes=false` + `PromptAnswer::Confirm(false)` drives the rejection path:
/// "Aborted" status, no reconciler invocation.
#[test]
fn rollback_aborted_human() {
    let (_workspace, state_dir, target, apply_id) = rollback_state_with_backups_setup();

    let (printer, buf) = Printer::for_test_with_prompt_responses_at(
        vec![PromptAnswer::Confirm(false)],
        Verbosity::Normal,
    );

    cmd_rollback(&printer, apply_id, false, Some(state_dir.path())).unwrap();
    printer.flush();
    drop(printer);

    let raw = buf.lock().unwrap().clone();
    let normalized = raw.replace(&target.display().to_string(), "<TARGET>");
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "rollback/aborted.txt", &stripped);
}

/// Seeded state has a non-file (package) action after the target apply.
/// `cmd_rollback` lists the action under the "Non-file action(s) require
/// manual review" section with bullets — proves the indent-hack closure
/// at rollback.rs:108 under real data (bullets render at section depth,
/// not at column 0).
#[test]
fn rollback_non_file_actions_human() {
    let (state_dir, apply_id) = rollback_state_with_non_file_actions_setup();

    let (printer, cap) = Printer::for_test_doc();

    cmd_rollback(&printer, apply_id, true, Some(state_dir.path())).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "rollback/non_file_actions.txt",
        &stripped,
    );
}

/// Bridge invariant: streaming section drops, buffered Doc emits — combined
/// human surface contains exactly one blank line at the transition.
#[test]
fn rollback_bridge_one_blank_line() {
    let (printer, cap) = Printer::for_test_doc();

    printer.heading("Rollback");
    printer.kv_block([
        ("Target apply ID".to_string(), "1".to_string()),
        ("File backups to restore".to_string(), "1".to_string()),
    ]);
    {
        let rb_sec = printer.section("Restoring");
        rb_sec.status_simple(Role::Ok, "1 file(s) processed");
    }

    let doc = Doc::new()
        .status(Role::Ok, "Rollback complete")
        .with_data(&RollbackOutput {
            apply_id: 1,
            files_restored: 1,
            files_removed: 0,
            non_file_actions: Vec::new(),
        });
    printer.emit(doc);
    drop(printer);

    let captured = strip_ansi(&cap.human());
    assert!(
        captured.contains("\n\n"),
        "bridge missing blank line:\n{captured}"
    );
    assert!(
        !captured.contains("\n\n\n"),
        "bridge has duplicate blank line:\n{captured}"
    );

    assert_snapshot(Path::new(SNAPSHOT_ROOT), "rollback/bridge.txt", &captured);
}

// ─────────────────────────────────────────────────────
// snapshot helpers
// ─────────────────────────────────────────────────────

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for inner in chars.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

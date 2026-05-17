//! Snapshot tests for `cfgd decide`.
//!
//! Goldens live under `tests/output_snapshots/decide/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test decide_v2_snapshots
//!
//! The live `cmd_decide` reads from the SQLite state store; to keep snapshots
//! stable across hosts these tests drive the pure `build_decide_*_doc` helpers
//! with hand-crafted fixtures.

use std::path::Path;

use cfgd::cli::decide::{build_decide_bulk_doc, build_decide_list_doc};
use cfgd_core::output_v2::Printer;
use cfgd_core::state::PendingDecision;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn pending(
    source: &str,
    resource: &str,
    tier: &str,
    action: &str,
    summary: &str,
) -> PendingDecision {
    PendingDecision {
        id: 1,
        source: source.into(),
        resource: resource.into(),
        tier: tier.into(),
        action: action.into(),
        summary: summary.into(),
        created_at: "2026-05-11T00:00:00Z".into(),
        resolved_at: None,
        resolution: None,
    }
}

fn pending_fixture() -> Vec<PendingDecision> {
    vec![
        pending(
            "team-config",
            "packages.brew.curl",
            "recommended",
            "install",
            "Install curl via brew",
        ),
        pending("team-config", "env.EDITOR", "optional", "set", "Set EDITOR"),
    ]
}

#[test]
fn decide_pending_human() {
    let decisions = pending_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_list_doc(&decisions));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/pending.txt");
}

#[test]
fn decide_pending_json() {
    let decisions = pending_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_list_doc(&decisions));
    drop(printer);

    let actual = cap.json().expect("doc captured json");
    let decisions_json = actual
        .get("decisions")
        .expect("payload must expose `decisions` array");
    assert_eq!(
        decisions_json.as_array().map(|a| a.len()),
        Some(2),
        "decisions array must round-trip 2 items, got: {actual:?}"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/pending.json");
}

#[test]
fn decide_empty_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_list_doc(&[]));
    drop(printer);
    let human = cap.human();
    assert!(
        human.contains("No pending decisions"),
        "empty listing must include info status, got:\n{human}"
    );
    assert!(
        !human.contains("Pending Decisions"),
        "empty listing must omit the Pending Decisions section header, got:\n{human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/empty.txt");
}

#[test]
fn decide_after_accept_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_decide_bulk_doc("accepted", 2, None));
    drop(printer);
    let human = cap.human();
    assert!(
        human.contains("ACCEPTED 2 items"),
        "bulk accept summary must report uppercase verb + pluralized count, got:\n{human}"
    );
    assert!(
        human.contains("next reconcile"),
        "bulk accept must hint about next reconcile, got:\n{human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "decide/after_accept.txt");
}

//! Snapshot tests for `cfgd compliance diff`.
//!
//! Goldens live under `tests/output_snapshots/compliance_diff/`.
//! Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test compliance_diff_v2_snapshots

use std::path::Path;

use cfgd::cli::compliance::{ComplianceDiff, build_compliance_diff_doc, compute_compliance_diff};
use cfgd::cli::output_types::ComplianceCheckChange;
use cfgd_core::compliance::{
    ComplianceCheck, ComplianceSnapshot, ComplianceStatus, MachineInfo, compute_summary,
};
use cfgd_core::output_v2::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn fixed_snapshot(timestamp: &str, checks: Vec<ComplianceCheck>) -> ComplianceSnapshot {
    let summary = compute_summary(&checks);
    ComplianceSnapshot {
        timestamp: timestamp.into(),
        machine: MachineInfo {
            hostname: "test-host".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks,
        summary,
    }
}

fn check(
    category: &str,
    target: &str,
    status: ComplianceStatus,
    detail: Option<&str>,
) -> ComplianceCheck {
    ComplianceCheck {
        category: category.into(),
        target: Some(target.into()),
        status,
        detail: detail.map(|d| d.into()),
        ..Default::default()
    }
}

fn happy_snap_pair() -> (ComplianceSnapshot, ComplianceSnapshot) {
    let snap1 = fixed_snapshot(
        "2026-05-14T09:00:00Z",
        vec![
            check("file", "/etc/hosts", ComplianceStatus::Compliant, None),
            check(
                "file",
                "/etc/resolv.conf",
                ComplianceStatus::Compliant,
                None,
            ),
            check("package", "ripgrep", ComplianceStatus::Compliant, None),
        ],
    );
    let snap2 = fixed_snapshot(
        "2026-05-14T12:00:00Z",
        vec![
            // /etc/hosts removed
            check(
                "file",
                "/etc/resolv.conf",
                ComplianceStatus::Violation,
                Some("nameserver mismatch"),
            ),
            check("package", "ripgrep", ComplianceStatus::Compliant, None),
            // package fd added
            check("package", "fd", ComplianceStatus::Compliant, None),
        ],
    );
    (snap1, snap2)
}

fn empty_snap_pair() -> (ComplianceSnapshot, ComplianceSnapshot) {
    let snap = fixed_snapshot(
        "2026-05-14T10:00:00Z",
        vec![check(
            "file",
            "/etc/hosts",
            ComplianceStatus::Compliant,
            None,
        )],
    );
    (snap.clone(), snap)
}

fn changed_only_diff() -> ComplianceDiff {
    ComplianceDiff {
        added: Vec::new(),
        removed: Vec::new(),
        changed: vec![
            ComplianceCheckChange {
                key: "file:/etc/hosts".into(),
                old_status: "Compliant".into(),
                new_status: "Violation".into(),
                detail: Some("hash drift detected".into()),
            },
            ComplianceCheckChange {
                key: "package:ripgrep".into(),
                old_status: "Compliant".into(),
                new_status: "Warning".into(),
                detail: Some("version mismatch: want 14.0.0, have 13.0.0".into()),
            },
            ComplianceCheckChange {
                key: "system:sysctl.vm.swappiness".into(),
                old_status: "Warning".into(),
                new_status: "Compliant".into(),
                detail: None,
            },
        ],
    }
}

// --- happy: added + removed + changed all populated ---

#[test]
fn compliance_diff_happy_human() {
    let (snap1, snap2) = happy_snap_pair();
    let diff = compute_compliance_diff(&snap1, &snap2);
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_diff_doc(1, 2, &snap1, &snap2, &diff));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_diff/happy.txt");
}

#[test]
fn compliance_diff_happy_json() {
    let (snap1, snap2) = happy_snap_pair();
    let diff = compute_compliance_diff(&snap1, &snap2);
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_diff_doc(1, 2, &snap1, &snap2, &diff));
    drop(printer);
    let actual = cap.json().expect("diff Doc carries with_data payload");
    assert_eq!(actual["id1"], 1);
    assert_eq!(actual["id2"], 2);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_diff/happy.json");
}

// --- empty: identical snapshots ---

#[test]
fn compliance_diff_empty_human() {
    let (snap1, snap2) = empty_snap_pair();
    let diff = compute_compliance_diff(&snap1, &snap2);
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_diff_doc(7, 8, &snap1, &snap2, &diff));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_diff/empty.txt");
}

// --- changed-only: no added, no removed, only status flips ---

#[test]
fn compliance_diff_changed_only_human() {
    let snap1 = fixed_snapshot("2026-05-14T09:00:00Z", Vec::new());
    let snap2 = fixed_snapshot("2026-05-14T12:00:00Z", Vec::new());
    let diff = changed_only_diff();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_diff_doc(3, 4, &snap1, &snap2, &diff));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_diff/changed_only.txt");
}

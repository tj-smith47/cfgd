//! Snapshot tests for `cfgd compliance` (snapshot, export, history).
//!
//! Goldens live under `tests/output_snapshots/compliance_{snapshot,export,history}/`.
//! Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test compliance_v2_snapshots

use std::path::Path;

use cfgd::cli::compliance::{
    build_compliance_export_doc, build_compliance_history_doc, build_compliance_summary_doc,
};
use cfgd_core::compliance::{
    ComplianceCheck, ComplianceSnapshot, ComplianceStatus, ComplianceSummary, MachineInfo,
    compute_summary,
};
use cfgd_core::output_v2::Printer;
use cfgd_core::state::ComplianceHistoryRow;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn fixed_snapshot(checks: Vec<ComplianceCheck>) -> ComplianceSnapshot {
    let summary = compute_summary(&checks);
    ComplianceSnapshot {
        timestamp: "2026-05-14T10:00:00Z".into(),
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

fn happy_snapshot() -> ComplianceSnapshot {
    fixed_snapshot(vec![
        check("file", "/etc/hosts", ComplianceStatus::Compliant, None),
        check("package", "ripgrep", ComplianceStatus::Compliant, None),
        check("package", "fd", ComplianceStatus::Compliant, None),
    ])
}

fn violations_snapshot() -> ComplianceSnapshot {
    fixed_snapshot(vec![
        check("file", "/etc/hosts", ComplianceStatus::Compliant, None),
        check(
            "package",
            "ripgrep",
            ComplianceStatus::Warning,
            Some("version mismatch: want 14.0.0, have 13.0.0"),
        ),
        check(
            "system",
            "sysctl.vm.swappiness",
            ComplianceStatus::Violation,
            Some("expected 10, found 60"),
        ),
    ])
}

fn empty_snapshot() -> ComplianceSnapshot {
    let mut snap = fixed_snapshot(vec![]);
    snap.summary = ComplianceSummary {
        compliant: 0,
        warning: 0,
        violation: 0,
    };
    snap
}

// --- compliance snapshot ---

#[test]
fn compliance_snapshot_happy_human() {
    let snap = happy_snapshot();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_summary_doc(&snap));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_snapshot/happy.txt");
}

#[test]
fn compliance_snapshot_happy_json() {
    let snap = happy_snapshot();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_summary_doc(&snap));
    drop(printer);
    let expected = serde_json::json!({
        "snapshot": serde_json::to_value(&snap).unwrap(),
    });
    let actual = cap.json().expect("doc captured json");
    pretty_assertions::assert_eq!(
        actual,
        expected,
        "emit -o json must wrap the snapshot under {{snapshot}}"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_snapshot/happy.json");
}

#[test]
fn compliance_snapshot_violations_human() {
    let snap = violations_snapshot();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_summary_doc(&snap));
    drop(printer);
    cap.assert_human_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "compliance_snapshot/violations.txt",
    );
}

#[test]
fn compliance_snapshot_empty_human() {
    let snap = empty_snapshot();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_summary_doc(&snap));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_snapshot/empty.txt");
}

// --- compliance export ---

#[test]
fn compliance_export_happy_human() {
    let snap = happy_snapshot();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_export_doc(
        &snap,
        Path::new("/var/lib/cfgd/compliance/2026-05-14.json"),
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_export/happy.txt");
}

#[test]
fn compliance_export_happy_json() {
    let snap = happy_snapshot();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_export_doc(
        &snap,
        Path::new("/var/lib/cfgd/compliance/2026-05-14.json"),
    ));
    drop(printer);
    let expected = serde_json::json!({
        "snapshot": serde_json::to_value(&snap).unwrap(),
    });
    let actual = cap.json().expect("doc captured json");
    pretty_assertions::assert_eq!(
        actual,
        expected,
        "emit -o json must wrap the snapshot under {{snapshot}}"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_export/happy.json");
}

// --- compliance history ---

fn populated_entries() -> Vec<ComplianceHistoryRow> {
    vec![
        ComplianceHistoryRow {
            id: 3,
            timestamp: "2026-05-14T12:00:00Z".into(),
            compliant: 12,
            warning: 1,
            violation: 0,
        },
        ComplianceHistoryRow {
            id: 2,
            timestamp: "2026-05-13T09:00:00Z".into(),
            compliant: 11,
            warning: 0,
            violation: 1,
        },
        ComplianceHistoryRow {
            id: 1,
            timestamp: "2026-05-12T08:00:00Z".into(),
            compliant: 10,
            warning: 0,
            violation: 0,
        },
    ]
}

#[test]
fn compliance_history_populated_human() {
    let entries = populated_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_history_doc(&entries));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_history/populated.txt");
}

#[test]
fn compliance_history_populated_json() {
    let entries = populated_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_history_doc(&entries));
    drop(printer);
    let expected = serde_json::json!({
        "entries": serde_json::to_value(&entries).unwrap(),
    });
    let actual = cap.json().expect("doc captured json");
    pretty_assertions::assert_eq!(
        actual,
        expected,
        "emit -o json must wrap entries under {{entries}}"
    );
    cap.assert_json_snapshot_in(
        Path::new(SNAPSHOT_ROOT),
        "compliance_history/populated.json",
    );
}

#[test]
fn compliance_history_empty_human() {
    let entries: Vec<ComplianceHistoryRow> = Vec::new();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_compliance_history_doc(&entries));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "compliance_history/empty.txt");
}

//! Snapshot tests for `cfgd status`.
//!
//! Five cases:
//!   - `status/clean.{txt,json}` — fleet status with a clean last-apply, no
//!     drift, no pending decisions, all modules installed. Exercises the
//!     Last Apply + No-drift + Modules + Managed Resources path.
//!   - `status/drift.{txt,json}` — drift events present, exit-code worthy.
//!     Exercises the Drift section's warn rendering plus the Config Sources
//!     table population.
//!   - `status/per_module.txt` — `cfgd status <module>` path with a known
//!     module, state record, and deployed files (one present, one missing).
//!
//! Goldens live under `tests/output_snapshots/status/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test status_v2_snapshots

use std::path::Path;

use cfgd::cli::status::{
    ModuleStatus, ModuleStatusEntry, StatusOutput, build_fleet_status_doc, build_module_status_doc,
    build_module_status_not_found_doc,
};
use cfgd_core::output_v2::Printer;
use cfgd_core::state::{
    ApplyRecord, ApplyStatus, ConfigSourceRecord, DriftEvent, ManagedResource, PendingDecision,
};
use pretty_assertions::assert_eq;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn clean_output() -> StatusOutput {
    StatusOutput {
        last_apply: Some(ApplyRecord {
            id: 1,
            timestamp: "2026-05-14T10:00:00Z".into(),
            profile: "default".into(),
            plan_hash: "deadbeef".into(),
            status: ApplyStatus::Success,
            summary: Some("12 actions applied".into()),
        }),
        drift: Vec::new(),
        sources: Vec::new(),
        pending_decisions: Vec::new(),
        modules: vec![
            ModuleStatusEntry {
                name: "base".into(),
                packages: 5,
                files: 3,
                status: "installed".into(),
            },
            ModuleStatusEntry {
                name: "dev-tools".into(),
                packages: 18,
                files: 12,
                status: "installed".into(),
            },
        ],
        managed_resources: vec![ManagedResource {
            resource_type: "file".into(),
            resource_id: "~/.bashrc".into(),
            source: "local".into(),
            last_hash: Some("hash1".into()),
            last_applied: Some(1_715_680_800),
        }],
    }
}

fn drift_output() -> StatusOutput {
    StatusOutput {
        last_apply: Some(ApplyRecord {
            id: 2,
            timestamp: "2026-05-14T11:30:00Z".into(),
            profile: "default".into(),
            plan_hash: "cafebabe".into(),
            status: ApplyStatus::Success,
            summary: None,
        }),
        drift: vec![
            DriftEvent {
                id: 10,
                timestamp: "2026-05-14T12:00:00Z".into(),
                resource_type: "file".into(),
                resource_id: "~/.zshrc".into(),
                expected: Some("hash-desired".into()),
                actual: Some("hash-actual".into()),
                resolved_by: None,
                source: "local".into(),
            },
            DriftEvent {
                id: 11,
                timestamp: "2026-05-14T12:01:00Z".into(),
                resource_type: "package".into(),
                resource_id: "ripgrep".into(),
                expected: Some("14.1.0".into()),
                actual: Some("13.0.0".into()),
                resolved_by: None,
                source: "team-config".into(),
            },
        ],
        sources: vec![ConfigSourceRecord {
            id: 1,
            name: "team-config".into(),
            origin_url: "https://github.com/team/config".into(),
            origin_branch: "main".into(),
            last_fetched: Some("2026-05-14T09:00:00Z".into()),
            last_commit: Some("abc123".into()),
            source_version: Some("3.1.0".into()),
            pinned_version: None,
            status: "synced".into(),
        }],
        pending_decisions: vec![PendingDecision {
            id: 5,
            source: "team-config".into(),
            resource: "package/curl".into(),
            tier: "recommended".into(),
            action: "install".into(),
            summary: "install curl 8.5.0".into(),
            created_at: "2026-05-14T08:00:00Z".into(),
            resolved_at: None,
            resolution: None,
        }],
        modules: vec![ModuleStatusEntry {
            name: "shell-config".into(),
            packages: 0,
            files: 4,
            status: "installed".into(),
        }],
        managed_resources: Vec::new(),
    }
}

fn per_module_output() -> ModuleStatus {
    ModuleStatus {
        name: "dev-tools".into(),
        packages: 18,
        files: 12,
        depends: vec!["base".into()],
        status: "installed".into(),
        last_applied: Some("2026-05-14T10:00:00Z".into()),
    }
}

fn per_module_deployed_files() -> Vec<(String, bool)> {
    vec![
        ("/home/user/.config/nvim/init.lua".into(), true),
        ("/home/user/.gitconfig".into(), false),
    ]
}

#[test]
fn status_clean_human() {
    let output = clean_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_fleet_status_doc(&output, &[]));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/clean.txt");
}

#[test]
fn status_clean_json() {
    let output = clean_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_fleet_status_doc(&output, &[]));
    drop(printer);
    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(output)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/clean.json");
}

#[test]
fn status_drift_human() {
    let output = drift_output();
    let sources = vec!["team-config".to_string()];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_fleet_status_doc(&output, &sources));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/drift.txt");
}

#[test]
fn status_drift_json() {
    let output = drift_output();
    let sources = vec!["team-config".to_string()];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_fleet_status_doc(&output, &sources));
    drop(printer);
    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(output)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/drift.json");
}

#[test]
fn status_per_module_human() {
    let output = per_module_output();
    let files = per_module_deployed_files();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_status_doc(&output, &files));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/per_module.txt");
}

#[test]
fn status_per_module_not_found_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_status_not_found_doc("ghost"));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/not_found.txt");
}

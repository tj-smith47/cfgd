//! Snapshot tests for `cfgd status`.
//!
//! Five cases:
//!   - `status/clean.{txt,json}` — fleet status with a clean last-apply, no
//!     drift, no pending decisions, all modules installed. Exercises the
//!     Last Apply + No-drift + Modules + Managed Resources path.
//!   - `status/drift.{txt,json}` — drift events present, exit-code worthy.
//!     Exercises the Drift section's warn rendering plus the Config Sources
//!     table population.
//!   - `status/per_module.txt` — `cfgd status --module <name>` with no
//!     `--scan`: the declared counts, and a Drift section that says nothing
//!     was checked rather than claiming convergence.
//!   - `status/per_module_scanned.{txt,json}` — the same module after a live
//!     scan: the counts, then one row per finding.
//!   - `status/per_module_clean.txt` — scanned and converged, the only
//!     condition under which `✓ No drift detected` may be claimed.
//!   - `status/per_module_wide.txt` — `-o wide`: inventories in place of the
//!     counts, each finding inline on its own row, no Drift section.
//!   - `status/per_module_show_values.txt` — `--show-values`: the same
//!     inventories carrying declared values and whole script bodies.
//!
//! Goldens live under `tests/output_snapshots/status/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test status_snapshots

use std::path::Path;

use cfgd::cli::status::{
    ModuleDrift, ModuleFilePresence, ModuleFileStatus, ModulePackagePresence, ModulePackageStatus,
    ModuleStatus, ModuleStatusEntry, ModuleStatusView, SURFACE_FILES, SURFACE_PACKAGES,
    StatusOutput, build_fleet_status_doc, build_module_status_doc,
    build_module_status_not_found_doc,
};
use cfgd_core::config::{EnvVar, ShellAlias};
use cfgd_core::modules::{HookScripts, ModuleSurfaces};
use cfgd_core::output::Printer;
use cfgd_core::state::{
    ApplyRecord, ApplyStatus, ConfigSourceRecord, DriftEvent, ManagedResource, PendingDecision,
};
use pretty_assertions::assert_eq;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

/// Pinned "now" for every status render: the header's last-scan age is a
/// rendered value, so reading the wall clock here would re-date the golden on
/// every run. `clean_output`'s scan is 5m old (exactly the staleness
/// threshold, so no hint); the drift fixtures' are 2h old, which does hint.
const NOW: &str = "2026-05-14T10:05:00Z";

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
        warnings: Vec::new(),
        classification_degraded: false,
        classification_degraded_code: None,
        classification_degraded_reason: None,
        drift_checked_live: false,
        last_scan_at: Some("2026-05-14T10:00:00Z".into()),
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
        warnings: Vec::new(),
        classification_degraded: false,
        classification_degraded_code: None,
        classification_degraded_reason: None,
        drift_checked_live: false,
        last_scan_at: Some("2026-05-14T08:00:00Z".into()),
    }
}

/// The env, aliases and lifecycle hooks the fixture modules declare. Two
/// hooks, declared out of run order on purpose: the Scripts row and the wide
/// Scripts section both report `preApply` before `postApply` because that is
/// the order they run in, never the order they were written.
fn declared_surfaces(packages: usize, files: usize) -> ModuleSurfaces {
    ModuleSurfaces {
        packages,
        files,
        env: vec![
            EnvVar {
                name: "PAGER".into(),
                value: "less -R".into(),
            },
            EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            },
        ],
        aliases: vec![ShellAlias {
            name: "gs".into(),
            command: "git status --short".into(),
        }],
        scripts: vec![
            HookScripts {
                hook: "preApply",
                bodies: vec!["set -euo pipefail\nmkdir -p ~/.config/nvim".into()],
            },
            HookScripts {
                hook: "postApply",
                bodies: vec![
                    "nvim --headless '+Lazy! sync' +qa".into(),
                    "echo done".into(),
                ],
            },
        ],
        system: vec!["shell".into()],
        depends: vec!["base".into()],
    }
}

/// The recorded-only render: no `--scan`, so no file's content and no
/// package's presence has been checked. Every row says so rather than letting
/// `Path::exists` stand in for health.
fn per_module_output() -> ModuleStatus {
    let declared = declared_surfaces(2, 12);
    ModuleStatus {
        name: "dev-tools".into(),
        packages: declared.packages,
        files: declared.files,
        env: declared.env.len(),
        aliases: declared.aliases.len(),
        scripts: declared.hook_names(),
        declared,
        system: vec!["shell".into()],
        depends: vec!["base".into()],
        status: "installed".into(),
        last_applied: Some("2026-05-14T10:00:00Z".into()),
        package_state: vec![
            ModulePackageStatus {
                name: "neovim".into(),
                manager: None,
                state: ModulePackagePresence::NotScanned,
            },
            ModulePackageStatus {
                name: "ripgrep".into(),
                manager: None,
                state: ModulePackagePresence::NotScanned,
            },
        ],
        deployed_files: vec![
            ModuleFileStatus {
                path: "/home/user/.config/nvim/init.lua".into(),
                state: ModuleFilePresence::NotScanned,
            },
            ModuleFileStatus {
                path: "/home/user/.gitconfig".into(),
                state: ModuleFilePresence::Missing,
            },
        ],
        drift: Vec::new(),
        drift_checked_live: false,
    }
}

/// The scanned render. It pins two things: a drifted file carries that verdict
/// on its own Deployed Files row in the wide view (which has no Drift section),
/// never a converged one; and the compact Drift section groups its rows by
/// surface and sorts alphabetically within each group — the findings below are
/// deliberately listed in neither order.
fn per_module_scanned_output() -> ModuleStatus {
    let declared = declared_surfaces(2, 12);
    ModuleStatus {
        name: "dev-tools".into(),
        packages: declared.packages,
        files: declared.files,
        env: declared.env.len(),
        aliases: declared.aliases.len(),
        scripts: declared.hook_names(),
        declared,
        system: vec!["shell".into()],
        depends: vec!["base".into()],
        status: "installed".into(),
        last_applied: Some("2026-05-14T10:00:00Z".into()),
        package_state: vec![
            ModulePackageStatus {
                name: "neovim".into(),
                manager: Some("brew".into()),
                state: ModulePackagePresence::Installed,
            },
            ModulePackageStatus {
                name: "ripgrep".into(),
                manager: Some("brew".into()),
                state: ModulePackagePresence::NotInstalled,
            },
        ],
        // `module_deployed_files` reads `ORDER BY file_path`, so the rows a
        // real scan hands the renderer are alphabetical.
        deployed_files: vec![
            ModuleFileStatus {
                path: "/home/user/.config/nvim/init.lua".into(),
                state: ModuleFilePresence::Drifted,
            },
            ModuleFileStatus {
                path: "/home/user/.gitconfig".into(),
                state: ModuleFilePresence::Missing,
            },
            ModuleFileStatus {
                path: "/home/user/.zshrc".into(),
                state: ModuleFilePresence::Drifted,
            },
        ],
        // Each producer's own strings — `files/plan.rs` for a file, the
        // `installed`/`missing` pair `cmd_status_module` records for a package
        // — so the terse cause each row renders is derived from what a real
        // scan writes. Listed package-first and file-reverse on purpose: the
        // golden is what proves the section sorts them.
        drift: vec![
            ModuleDrift {
                event: DriftEvent {
                    id: 21,
                    timestamp: "2026-05-14T12:00:01Z".into(),
                    resource_type: "package".into(),
                    resource_id: "brew:ripgrep".into(),
                    expected: Some("installed".into()),
                    actual: Some("missing".into()),
                    resolved_by: None,
                    source: "local".into(),
                },
                owner: "dev-tools".into(),
                surface: SURFACE_PACKAGES,
                item: "ripgrep".into(),
            },
            ModuleDrift {
                event: DriftEvent {
                    id: 20,
                    timestamp: "2026-05-14T12:00:00Z".into(),
                    resource_type: "module".into(),
                    resource_id: "dev-tools/home/user/.zshrc".into(),
                    expected: Some("content matches source".into()),
                    actual: Some("content differs from source".into()),
                    resolved_by: None,
                    source: "local".into(),
                },
                owner: "dev-tools".into(),
                surface: SURFACE_FILES,
                item: "/home/user/.zshrc".into(),
            },
            ModuleDrift {
                event: DriftEvent {
                    id: 22,
                    timestamp: "2026-05-14T12:00:02Z".into(),
                    resource_type: "module".into(),
                    resource_id: "dev-tools/home/user/.config/nvim/init.lua".into(),
                    expected: Some("content matches source".into()),
                    actual: Some("content differs from source".into()),
                    resolved_by: None,
                    source: "local".into(),
                },
                owner: "dev-tools".into(),
                surface: SURFACE_FILES,
                item: "/home/user/.config/nvim/init.lua".into(),
            },
        ],
        drift_checked_live: true,
    }
}

/// The scanned-and-converged render: the scan ran and found nothing, which is
/// the only condition under which the Drift section may claim it.
fn per_module_clean_scanned_output() -> ModuleStatus {
    let mut output = per_module_scanned_output();
    output.package_state = vec![
        ModulePackageStatus {
            name: "neovim".into(),
            manager: Some("brew".into()),
            state: ModulePackagePresence::Installed,
        },
        ModulePackageStatus {
            name: "ripgrep".into(),
            manager: Some("brew".into()),
            state: ModulePackagePresence::Installed,
        },
    ];
    output.deployed_files = vec![ModuleFileStatus {
        path: "/home/user/.config/nvim/init.lua".into(),
        state: ModuleFilePresence::Deployed,
    }];
    output.drift = Vec::new();
    output
}

#[test]
fn status_clean_human() {
    let output = clean_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_fleet_status_doc(
        &output,
        &[],
        Path::new("/etc/cfgd/cfgd.yaml"),
        "default",
        NOW,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/clean.txt");
}

#[test]
fn status_clean_json() {
    let output = clean_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_fleet_status_doc(
        &output,
        &[],
        Path::new("/etc/cfgd/cfgd.yaml"),
        "default",
        NOW,
    ));
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
    printer.emit(build_fleet_status_doc(
        &output,
        &sources,
        Path::new("/etc/cfgd/cfgd.yaml"),
        "default",
        NOW,
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/drift.txt");
}

#[test]
fn status_drift_json() {
    let output = drift_output();
    let sources = vec!["team-config".to_string()];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_fleet_status_doc(
        &output,
        &sources,
        Path::new("/etc/cfgd/cfgd.yaml"),
        "default",
        NOW,
    ));
    drop(printer);
    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(output)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/drift.json");
}

fn emit_module(output: &ModuleStatus, view: ModuleStatusView, golden: &str) {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_status_doc(output, view));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), golden);
}

#[test]
fn status_per_module_human() {
    emit_module(
        &per_module_output(),
        ModuleStatusView::Compact,
        "status/per_module.txt",
    );
}

/// Compact with drift: the counts, then one row per finding naming the module,
/// the surface it was found on and a cause a reader can act on.
#[test]
fn status_per_module_scanned_human() {
    emit_module(
        &per_module_scanned_output(),
        ModuleStatusView::Compact,
        "status/per_module_scanned.txt",
    );
}

/// Compact and converged: `✓ No drift detected` is claimed only because the
/// scan ran — `per_module.txt` (no scan) must say something else.
#[test]
fn status_per_module_clean_human() {
    emit_module(
        &per_module_clean_scanned_output(),
        ModuleStatusView::Compact,
        "status/per_module_clean.txt",
    );
}

/// `-o wide`: inventories instead of counts, each finding inline on the row
/// for the thing it was found on, and no Drift section to state it twice.
#[test]
fn status_per_module_wide_human() {
    emit_module(
        &per_module_scanned_output(),
        ModuleStatusView::Inventory { show_values: false },
        "status/per_module_wide.txt",
    );
}

/// `--show-values`: the same inventories with the declared value beside each
/// name and each script's whole body in place of its condensed label.
#[test]
fn status_per_module_show_values_human() {
    emit_module(
        &per_module_scanned_output(),
        ModuleStatusView::Inventory { show_values: true },
        "status/per_module_show_values.txt",
    );
}

/// The human render and the `-o json` payload state the same per-file and
/// per-package verdicts: a consumer reading `deployedFiles[].state` must not
/// see `deployed` for the file the human render marks drifted.
#[test]
fn status_per_module_scanned_json() {
    let output = per_module_scanned_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_status_doc(&output, ModuleStatusView::Compact));
    drop(printer);
    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(output)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/per_module_scanned.json");
}

#[test]
fn status_per_module_not_found_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_module_status_not_found_doc("ghost"));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "status/not_found.txt");
}

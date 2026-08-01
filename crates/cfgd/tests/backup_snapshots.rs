//! Integration tests for `cfgd backup` and its `cfgd apply` integration.
//!
//! Goldens live under `tests/output_snapshots/backup/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test backup_snapshots
//!
//! Cases:
//!   - `backup/list_empty.{txt,json}`     — no `spec.backups[]` declared.
//!   - `backup/list_populated.{txt,json}` — two declared backups, neither has
//!     run yet (`last_run_status` is "never").
//!   - `backup/run_named.{txt,json}`      — `cfgd backup run docs` against a
//!     real `BackupUnit`; asserts the snapshot file actually landed on disk.
//!   - `backup/run_unknown.txt`           — `cfgd backup run bogus` returns a
//!     typed error listing the valid names.
//!   - apply integration (no goldens; behavioural assertions): a schedule-less
//!     backup runs during `cfgd apply` even when the file/package/module plan
//!     is empty, a `--dry-run` apply runs no backups, and a scheduled backup
//!     is left untouched by `cfgd apply` (daemon/explicit-run only).

mod common;

use std::path::Path;

use cfgd::cli::apply::run_apply;
use cfgd::cli::backup::{build_backup_list_doc, cmd_backup_list, cmd_backup_run};
use cfgd::cli::output_types::BackupListEntry;
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::Printer;
use pretty_assertions::assert_eq;

use common::{apply_args, apply_args_dry_run, backup_profile_setup, cli_for};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

#[test]
fn backup_list_empty_human() {
    let (config_dir, state_dir) = common::empty_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_backup_list(&cli, &printer).unwrap();
    drop(printer);

    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "backup/list_empty.txt",
        &strip_ansi(&cap.human()),
    );
}

#[test]
fn backup_list_empty_json() {
    let (config_dir, state_dir) = common::empty_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_backup_list(&cli, &printer).unwrap();
    drop(printer);

    let payload = cap.json().expect("backup list doc carries a payload");
    assert_eq!(
        payload,
        serde_json::json!([]),
        "no backups declared must roundtrip as an empty array"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "backup/list_empty.json");
}

#[test]
fn backup_list_populated_human() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_backup_list(&cli, &printer).unwrap();
    drop(printer);

    // The table's Source column renders the tempdir-backed fixture path
    // (`backup_profile_setup`'s `notes.txt`), which changes every run.
    let normalized =
        cfgd_core::normalize_for_snapshot(&strip_ansi(&cap.human()), &[(&source, "<SOURCE>")]);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "backup/list_populated.txt",
        &normalized,
    );
}

#[test]
fn backup_list_populated_json() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_backup_list(&cli, &printer).unwrap();
    drop(printer);

    let payload = cap.json().expect("backup list doc carries a payload");
    let names: Vec<&str> = payload
        .as_array()
        .expect("array payload")
        .iter()
        .map(|e| e["name"].as_str().expect("name field"))
        .collect();
    assert_eq!(
        names,
        vec!["docs", "weekly"],
        "list must carry both declared backups, unfiltered by schedule"
    );
    for entry in payload.as_array().unwrap() {
        assert!(
            entry.get("lastRunStatus").is_none(),
            "neither backup has run yet — lastRunStatus must be omitted, not null"
        );
    }

    // `assert_json_snapshot_in` has no normalization hook, and the `source`
    // field embeds the tempdir-backed fixture path — replicate its
    // pretty-print shape manually with normalization applied first.
    let rendered = serde_json::to_string_pretty(&payload).unwrap();
    let normalized = cfgd_core::normalize_for_snapshot(&rendered, &[(&source, "<SOURCE>")]);
    cfgd_core::test_helpers::assert_snapshot_golden(
        Path::new(SNAPSHOT_ROOT),
        "backup/list_populated.json",
        &normalized,
        env!("CARGO_PKG_VERSION"),
    );
}

#[test]
fn backup_run_named_human() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    // A hookless run is clean, so `cmd_backup_run` returns Ok rather than
    // taking the `ExitCode::Error.exit()` path — safe to call in-process.
    cmd_backup_run(&cli, &printer, Some("docs")).unwrap();
    drop(printer);

    let dest_dir = state_dir.path().join("backups").join("docs");
    let snapshots: Vec<_> = std::fs::read_dir(&dest_dir)
        .expect("docs destination dir must exist after a run")
        .collect();
    assert_eq!(
        snapshots.len(),
        1,
        "one run must write exactly one snapshot"
    );

    let weekly_dir = state_dir.path().join("backups").join("weekly");
    assert!(
        !weekly_dir.exists(),
        "naming 'docs' must not touch the unrelated 'weekly' backup"
    );

    let normalized = cfgd_core::normalize_for_snapshot(
        &strip_ansi(&cap.human()),
        &[(&source, "<SOURCE>"), (state_dir.path(), "<STATE_DIR>")],
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "backup/run_named.txt",
        &normalized
    );
}

#[test]
fn backup_run_named_json() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_backup_run(&cli, &printer, Some("docs")).unwrap();
    drop(printer);

    let payload = cap.json().expect("backup run doc carries a payload");
    let entries = payload.as_array().expect("array payload");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "docs");
    assert_eq!(entries[0]["status"], "success");
    assert_eq!(
        entries[0]["clean"], true,
        "a hookless run must report clean: true"
    );

    let normalized = cfgd_core::normalize_for_snapshot(
        &serde_json::to_string_pretty(&payload).unwrap(),
        &[(&source, "<SOURCE>"), (state_dir.path(), "<STATE_DIR>")],
    );
    // The destinationPath's `{timestamp}` component is real-clock-time
    // (`BACKUP_TIMESTAMP_FORMAT`), so it differs on every run.
    let normalized = normalize_backup_timestamp(&normalized);
    cfgd_core::test_helpers::assert_snapshot_golden(
        Path::new(SNAPSHOT_ROOT),
        "backup/run_named.json",
        &normalized,
        env!("CARGO_PKG_VERSION"),
    );
}

#[test]
fn backup_run_unknown_name_errors_with_valid_list_and_snapshots() {
    // An unknown name never reaches `cmd_backup_run`'s
    // `ExitCode::Error.exit()` branch — `run_backup_run`'s `?` propagates the
    // typed error immediately, so this is safe to assert in-process.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, _cap) = Printer::for_test_doc();

    let err = cmd_backup_run(&cli, &printer, Some("bogus")).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("bogus"),
        "error must name the unknown backup: {msg}"
    );
    assert!(
        msg.contains("docs") && msg.contains("weekly"),
        "error must list every valid backup name: {msg}"
    );
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "backup/run_unknown.txt", &msg);
}

#[test]
fn backup_run_all_runs_every_declared_backup_including_scheduled() {
    // An explicit `backup run` (no name) always runs every declared backup —
    // the schedule only gates the automatic run folded into `cfgd apply`.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, _cap) = Printer::for_test_doc();

    cmd_backup_run(&cli, &printer, None).unwrap();

    for name in ["docs", "weekly"] {
        let dir = state_dir.path().join("backups").join(name);
        assert!(
            dir.exists() && std::fs::read_dir(&dir).unwrap().count() == 1,
            "backup run with no name must run '{name}' regardless of its schedule"
        );
    }
}

#[test]
fn build_backup_list_doc_json_matches_serde_roundtrip() {
    // Pure data-roundtrip test on `BackupListEntry`/`build_backup_list_doc` —
    // pins the `-o json` shape without standing up config/state fixtures.
    let entries = vec![BackupListEntry {
        name: "docs".to_string(),
        source: "/home/t/docs".to_string(),
        schedule: None,
        retention: 3,
        last_run_status: Some("success".to_string()),
        last_run_at: Some("2026-01-01T00:00:00Z".to_string()),
        last_run_clean: Some(true),
    }];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_backup_list_doc(&entries));
    drop(printer);

    let expected = serde_json::to_value(&entries).unwrap();
    let actual = cap.json().expect("backup list doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(Vec<BackupListEntry>)"
    );
}

// ---------------------------------------------------------------------------
// `cfgd apply` integration: schedule-less backups run automatically.
// ---------------------------------------------------------------------------

#[test]
fn apply_runs_schedule_less_backups_even_with_an_empty_file_plan() {
    // `backup_profile_setup` declares zero managed files/modules, so the
    // reconciler plan is empty — before the has_actions/pending_backups fix,
    // this hit the "nothing to do" early return and never ran `docs`.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = apply_args();

    let outcome = run_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    assert_eq!(
        outcome.status,
        cfgd_core::state::ApplyStatus::Success,
        "a hookless backup run must not downgrade a converged apply"
    );

    let docs_dir = state_dir.path().join("backups").join("docs");
    assert!(
        docs_dir.exists() && std::fs::read_dir(&docs_dir).unwrap().count() == 1,
        "schedule-less 'docs' backup must run during apply even with no file diff"
    );

    let weekly_dir = state_dir.path().join("backups").join("weekly");
    assert!(
        !weekly_dir.exists(),
        "scheduled 'weekly' backup must NOT run automatically during apply"
    );

    let human = strip_ansi(&cap.human());
    assert!(
        human.contains("backup 'docs'"),
        "human output must report the backup that ran: {human}"
    );
}

#[test]
fn apply_json_output_carries_the_backup_run_record() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    let args = apply_args();

    run_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    let payload = cap.json().expect("apply doc carries a payload");
    let backups = payload["backups"].as_array().expect("backups array");
    assert_eq!(backups.len(), 1, "only the schedule-less backup ran");
    assert_eq!(backups[0]["name"], "docs");
    assert_eq!(backups[0]["clean"], true);
}

#[test]
fn apply_dry_run_skips_backups() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, _cap) = Printer::for_test_doc();
    let args = apply_args_dry_run();

    run_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    let docs_dir = state_dir.path().join("backups").join("docs");
    assert!(
        !docs_dir.exists(),
        "--dry-run must not execute any backup, even a schedule-less one"
    );
}

#[test]
fn apply_dry_run_human_shows_pending_backups() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = apply_args_dry_run();

    run_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    let human = strip_ansi(&cap.human());
    assert!(
        human.contains("Backups (run on apply)") && human.contains("docs"),
        "dry-run preview must surface the schedule-less backup that would run: {human}"
    );
    assert!(
        !human.contains("weekly"),
        "dry-run preview must not list the scheduled backup: {human}"
    );
}

// ─────────────────────────────────────────────────────
// snapshot helpers — local to keep tests/output_snapshots/ self-contained
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

/// Replace the `{timestamp}` component of a `spec.backups[]` snapshot
/// filename (`BACKUP_TIMESTAMP_FORMAT`: `%Y%m%dT%H%M%SZ`, e.g.
/// `20260801T155928Z`) with a stable placeholder — real-clock-time, so the
/// literal digits differ on every run.
fn normalize_backup_timestamp(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(len) = backup_timestamp_span(&chars[i..]) {
            out.push_str("<TIMESTAMP>");
            i += len;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Detect an 8-digit date + `T` + 6-digit time + `Z` span
/// (`BACKUP_TIMESTAMP_FORMAT`: `%Y%m%dT%H%M%SZ`) and return its length in
/// chars.
fn backup_timestamp_span(window: &[char]) -> Option<usize> {
    if window.len() < 16 {
        return None;
    }
    let all_digits = |s: &[char]| s.iter().all(|c| c.is_ascii_digit());
    if !all_digits(&window[0..8]) {
        return None;
    }
    if window[8] != 'T' {
        return None;
    }
    if !all_digits(&window[9..15]) {
        return None;
    }
    if window[15] != 'Z' {
        return None;
    }
    Some(16)
}

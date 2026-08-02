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
//!   - `backup/run_unknown.{txt,json}`    — `cfgd backup run bogus` returns a
//!     typed error listing the valid names in BOTH the human `render_cli_error`
//!     output and the structured payload's `hint` field.
//!   - apply integration (no goldens; behavioural assertions): a schedule-less
//!     backup runs during `cfgd apply` even when the file/package/module plan
//!     is empty, a `--dry-run` apply runs no backups, a scheduled backup is
//!     left untouched by `cfgd apply` (daemon/explicit-run only), a failing
//!     schedule-less backup does not block a sibling unit declared after it
//!     or the rest of apply — it only downgrades the overall status to
//!     `partial` (nonzero exit), matching the `record_source_apply`
//!     best-effort pattern — and a backup failure never raises an
//!     already-`Failed` apply back up to `partial`.

mod common;

use std::path::Path;

use cfgd::cli::apply::run_apply;
use cfgd::cli::backup::{build_backup_list_doc, cmd_backup_list, cmd_backup_run};
use cfgd::cli::output_types::BackupListEntry;
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::Printer;
use pretty_assertions::assert_eq;

use common::{
    apply_args, apply_args_dry_run, backup_profile_setup, backup_profile_with_one_failure_setup,
    cli_for,
};

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
fn backup_run_named_scheduled_backup_runs_alone() {
    // Naming a SCHEDULED backup directly (`Some("weekly")`, not the
    // no-name-runs-all path already covered by
    // `backup_run_all_runs_every_declared_backup_including_scheduled`) must
    // run it — and only it — regardless of its `schedule`. Only the daemon's
    // automatic path (and `cfgd apply`'s schedule-less loop) gates on
    // `schedule`; an explicit `cfgd backup run <name>` never does.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_backup_run(&cli, &printer, Some("weekly")).unwrap();
    drop(printer);

    let payload = cap.json().expect("backup run doc carries a payload");
    let entries = payload.as_array().expect("array payload");
    assert_eq!(entries.len(), 1, "naming 'weekly' must run only 'weekly'");
    assert_eq!(entries[0]["name"], "weekly");
    assert_eq!(entries[0]["status"], "success");

    let weekly_dir = state_dir.path().join("backups").join("weekly");
    assert_eq!(
        std::fs::read_dir(&weekly_dir).unwrap().count(),
        1,
        "the scheduled 'weekly' backup must have written exactly one snapshot"
    );
    let docs_dir = state_dir.path().join("backups").join("docs");
    assert!(
        !docs_dir.exists(),
        "naming 'weekly' must not touch the unrelated 'docs' backup"
    );
}

#[test]
fn backup_run_unknown_name_human_renders_hint_once() {
    // An unknown name never reaches `cmd_backup_run`'s
    // `ExitCode::Error.exit()` branch — `run_backup_run`'s `?` propagates the
    // typed error immediately, so this is safe to assert in-process.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, _cap) = Printer::for_test_doc();

    let err = cmd_backup_run(&cli, &printer, Some("bogus")).unwrap_err();

    // Render through the real CLI-boundary sink (`render_cli_error`) — the
    // ONLY path a user's terminal actually sees. Asserting on `{err:#}`'s raw
    // anyhow chain instead double-prints the message (thiserror auto-derives
    // `source()` from `CfgdError::Backup`'s `#[from]`, and the wrapper's
    // `{0}` interpolation already embeds that same text), which is a test
    // artifact, not real product output — see the sibling `cli/error.rs`
    // `render_cli_error_human_renders_attached_hints` test for the pattern.
    let (render_printer, render_cap) = Printer::for_test_doc();
    cfgd::cli::error::render_cli_error(&render_printer, &err);
    drop(render_printer);

    let human = strip_ansi(&render_cap.human());
    assert_eq!(
        human.matches('✗').count(),
        1,
        "exactly one fail line, got: {human:?}"
    );
    assert!(
        human.contains("bogus"),
        "error must name the unknown backup: {human}"
    );
    assert!(
        human.contains("docs") && human.contains("weekly"),
        "the valid-names hint must render in human mode, got: {human}"
    );
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "backup/run_unknown.txt", &human);
}

#[test]
fn backup_run_unknown_name_json_carries_hint() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, _cap) = Printer::for_test_doc();

    let err = cmd_backup_run(&cli, &printer, Some("bogus")).unwrap_err();

    let (render_printer, render_cap) =
        Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cfgd::cli::error::render_cli_error(&render_printer, &err);
    drop(render_printer);

    let payload = render_cap.json().expect("error doc carries a payload");
    assert_eq!(payload["error"], "not_found");
    assert_eq!(payload["name"], "bogus");
    let hint = payload["hint"].as_str().expect("hint field present");
    assert!(
        hint.contains("docs") && hint.contains("weekly"),
        "json hint must list every valid backup name: {hint}"
    );

    let normalized = serde_json::to_string_pretty(&payload).unwrap();
    cfgd_core::test_helpers::assert_snapshot_golden(
        Path::new(SNAPSHOT_ROOT),
        "backup/run_unknown.json",
        &normalized,
        env!("CARGO_PKG_VERSION"),
    );
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
#[serial_test::serial]
fn backup_run_aborts_on_a_source_constraint_violation_but_list_still_reports() {
    // `backup run` executes hooks and writes snapshots, so it composes in
    // Enforce like apply/plan/daemon: a source contribution that violates the
    // source's own constraints must stop the run, not be recorded and run
    // anyway. `backup list` only reads, so it stays on Report and still shows
    // the inventory.
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (_workspace, config_dir, state_dir, rejected_destination) =
        common::violating_backup_source_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());

    let (sync_printer, _sync_cap) = Printer::for_test_doc();
    cfgd::cli::sync::cmd_sync(&cli, &sync_printer).expect("the source must sync into the cache");
    drop(sync_printer);

    let (printer, _cap) = Printer::for_test_doc();
    let err = cfgd::cli::backup::run_backup_run(&cli, &printer, None)
        .expect_err("a source constraint violation must abort backup run");
    drop(printer);
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&rejected_destination) && msg.contains("acme"),
        "the abort must name the offending destination and source: {msg}"
    );
    assert!(
        !std::path::Path::new(&rejected_destination).exists(),
        "no snapshot may be written by a run that composed a violating source"
    );

    let (list_printer, list_cap) =
        Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cmd_backup_list(&cli, &list_printer).expect("listing stays on Report mode");
    drop(list_printer);
    let payload = list_cap.json().expect("backup list doc carries a payload");
    assert_eq!(
        payload
            .as_array()
            .expect("array payload")
            .iter()
            .filter_map(|e| e["name"].as_str())
            .collect::<Vec<_>>(),
        vec!["exfil"],
        "a read surface still reports the inventory it recorded a violation for"
    );
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
fn apply_backup_failure_does_not_block_subsequent_backups_or_apply() {
    // `run_backup` only returns `Err` on a state-store write failure — an
    // ordinary failure (missing source, a failed hook) is captured into the
    // returned record instead, so this exercises the loop's continuation
    // contract with a real, deterministic "failing unit" rather than trying
    // to force the narrow (and not independently reachable through public
    // API) DB-write-error branch. The observable behavior the coordinator
    // asked to pin — a failing unit doesn't block a sibling unit or the rest
    // of apply, and apply still exits nonzero overall — is identical either
    // way, since both arms of the `match` in `apply.rs` set
    // `status = ApplyStatus::Partial` and fall through to the next unit.
    let (config_dir, state_dir, _ok_source) = backup_profile_with_one_failure_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    let args = apply_args();

    let outcome = run_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    assert_eq!(
        outcome.status,
        cfgd_core::state::ApplyStatus::Partial,
        "a failed backup unit must downgrade apply's overall status"
    );

    let ok_dir = state_dir.path().join("backups").join("ok");
    assert!(
        ok_dir.exists() && std::fs::read_dir(&ok_dir).unwrap().count() == 1,
        "the 'ok' backup declared AFTER the failing 'broken' one must still have run"
    );
    let broken_dir = state_dir.path().join("backups").join("broken");
    assert!(
        !broken_dir.exists(),
        "the failing 'broken' backup must not have produced an artifact"
    );

    let payload = cap.json().expect("apply doc carries a payload");
    let backups = payload["backups"].as_array().expect("backups array");
    assert_eq!(
        backups.len(),
        2,
        "both declared backups must be reported, not just the one before the failure"
    );
    let broken = backups
        .iter()
        .find(|b| b["name"] == "broken")
        .expect("broken backup reported");
    assert_eq!(broken["clean"], false);
    let ok = backups
        .iter()
        .find(|b| b["name"] == "ok")
        .expect("ok backup reported");
    assert_eq!(ok["clean"], true);
}

#[test]
fn apply_failed_file_phase_stays_failed_after_a_backup_also_fails() {
    // Regression for a coordinator finding on the I2 fix itself: the backup
    // loop's downgrade must never raise a `Failed` apply back up to
    // `Partial`. `single_failed_file_and_broken_backup_setup` gives a file
    // phase with `failed == total` (the reconciler's own status math yields
    // `ApplyStatus::Failed`, not `Partial` — see
    // `crates/cfgd-core/src/reconciler/apply.rs`), then a schedule-less
    // backup whose source doesn't exist also runs and produces an
    // `Ok(BackupRunStatus::Failed)` record. Both `apply.rs` call sites that
    // touch `status` on an unclean/errored backup now route through the
    // same `downgrade_to_partial` helper, which is a no-op unless `status`
    // is currently `Success` — so this also stands as proof for the `Err`
    // arm (state-store write failure), which isn't independently reachable
    // from a public-API test (see the sibling
    // `apply_backup_failure_does_not_block_subsequent_backups_or_apply`
    // test's note): both arms share the exact same guarded call, so pinning
    // the guard via the reachable `Ok(Failed)` arm pins it for both.
    let (config_dir, state_dir) = common::single_failed_file_and_broken_backup_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    let args = apply_args();

    let outcome = run_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    assert_eq!(
        outcome.status,
        cfgd_core::state::ApplyStatus::Failed,
        "a failing backup must not downgrade an already-Failed apply to Partial"
    );

    let payload = cap.json().expect("apply doc carries a payload");
    assert_eq!(payload["status"], "failed");
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

#[test]
fn backup_run_reports_a_busy_unit_and_still_runs_the_others() {
    // The engine allows one writer per unit. `backup run` with no name must not
    // abandon the rest of the set over one unit another process holds — but the
    // command still exits nonzero, because a run the user asked for did not
    // happen. Driven through `run_backup_run` because the nonzero exit is a
    // `process::exit` in `cmd_backup_run`, which would take the test binary
    // with it.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let _held = cfgd_core::acquire_backup_lock(state_dir.path(), "docs").expect("hold docs");

    let outcome = cfgd::cli::backup::run_backup_run(&cli, &printer, None)
        .expect("a busy unit is an outcome, not an error");
    drop(printer);

    assert_eq!(
        outcome.busy,
        vec!["docs".to_string()],
        "the busy unit must be carried out to the exit-code decision"
    );
    assert!(
        !outcome.fully_clean(),
        "a run the user asked for did not happen — the command must exit nonzero"
    );

    let human = strip_ansi(&cap.human());
    assert!(
        human.contains("already running") && human.contains("docs"),
        "the busy unit must be reported: {human}"
    );
    // `apply` renders the same event as Skipped; the unit IS being backed up,
    // just not by us. Only the exit code distinguishes the two surfaces.
    assert!(
        human.contains("— backup 'docs'"),
        "a busy unit renders with the Skipped role, matching apply: {human:?}"
    );
    assert!(
        !human.contains("✗ backup 'docs'"),
        "a busy unit is not a failed backup: {human:?}"
    );
    assert!(
        !state_dir.path().join("backups").join("docs").exists(),
        "a refused run must not touch the busy unit's destination"
    );
    assert_eq!(
        std::fs::read_dir(state_dir.path().join("backups").join("weekly"))
            .expect("the unblocked unit must still have run")
            .count(),
        1,
        "one unit's collision must not abandon the rest of the set"
    );
}

#[test]
fn backup_run_json_payload_marks_the_busy_unit_skipped() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    let _held = cfgd_core::acquire_backup_lock(state_dir.path(), "docs").expect("hold docs");

    cfgd::cli::backup::run_backup_run(&cli, &printer, None).expect("busy is not an error");
    drop(printer);

    let payload = cap.json().expect("backup run doc carries a payload");
    let entries = payload.as_array().expect("array payload");
    let docs = entries
        .iter()
        .find(|e| e["name"] == "docs")
        .expect("the busy unit stays in the payload");
    assert_eq!(docs["status"], "skipped");
    assert_eq!(docs["clean"], false);
    assert!(
        docs["error"]
            .as_str()
            .is_some_and(|e| e.contains("already running")),
        "the payload must say why: {docs}"
    );
}

#[test]
fn apply_skips_a_busy_backup_without_failing_the_apply() {
    // Unlike `backup run`, apply did not ask for this specific unit — the unit
    // IS being backed up, just by whoever holds the lock — so the skip is
    // reported and the apply stays clean.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let _held = cfgd_core::acquire_backup_lock(state_dir.path(), "docs").expect("hold docs");

    let result = run_apply(&cli, &printer, &apply_args()).expect("apply must not error");
    drop(printer);

    let human = strip_ansi(&cap.human());
    assert!(
        human.contains("already running"),
        "the skip must be visible: {human}"
    );
    assert_eq!(
        result.status,
        cfgd_core::state::ApplyStatus::Success,
        "a unit another process is backing up is not an apply failure"
    );
    assert!(
        !state_dir.path().join("backups").join("docs").exists(),
        "a skipped unit's destination must be untouched"
    );
}

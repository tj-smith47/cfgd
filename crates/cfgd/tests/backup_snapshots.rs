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
//!   - `backup/rollback.{txt,json}`       — `cfgd backup rollback docs` putting
//!     the copy a restore left back over the source, and leaving a copy of what
//!     it displaced.
//!   - `backup/rollback_list.{txt,json}`  — the no-name listing of what could be
//!     rolled back, driven through the pure builder so the Copy column's width
//!     does not depend on the host's temp root.
//!   - `backup/rollback_list_empty.txt`   — the same listing on a machine where
//!     nothing has been displaced.
//!   - `backup/rollback_no_copy.{txt,json}` — `cfgd backup rollback docs` on a
//!     unit with no copy beside its source: the typed `no_rollback_copy` error
//!     and its read-only-surface hint (`cfgd backup list <name>`, never the
//!     destructive restore), in both channels.
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
use cfgd::cli::backup::{
    RestoreArgs, build_backup_list_doc, build_backup_rollback_list_doc, cmd_backup_list,
    cmd_backup_rollback, cmd_backup_run, run_backup_restore, run_backup_rollback,
};
use cfgd::cli::output_types::{BackupListEntry, BackupRollbackEntry};
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::Printer;
use pretty_assertions::assert_eq;

use common::{
    apply_args, apply_args_dry_run, backup_list_profile_setup, backup_profile_setup,
    backup_profile_with_one_failure_setup, cli_for,
};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

#[test]
fn backup_list_empty_human() {
    let (config_dir, state_dir) = common::empty_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_backup_list(&cli, &printer, None, false).unwrap();
    drop(printer);

    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "backup/list_empty.txt",
        &cfgd_core::output::strip_ansi(&cap.human()),
    );
}

#[test]
fn backup_list_empty_json() {
    let (config_dir, state_dir) = common::empty_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_backup_list(&cli, &printer, None, false).unwrap();
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
    // A fixed `source:` on purpose: the table pads every column to the widest
    // Source, so a tempdir path would make the golden's whole layout depend on
    // how long the host's temp root is.
    let (config_dir, state_dir) = backup_list_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_backup_list(&cli, &printer, None, false).unwrap();
    drop(printer);

    // Next Run counts down to a real future clock time, so how far away it
    // reads depends on the hour the suite runs.
    let normalized = normalize_countdown(&normalize_iso8601(&cfgd_core::output::strip_ansi(
        &cap.human(),
    )));
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

    cmd_backup_list(&cli, &printer, None, false).unwrap();
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

    // The scheduled unit answers the operator's actual question ("is the timer
    // going to fire?"); the schedule-less one runs during apply, on no clock of
    // its own, and must not invent one.
    let docs = payload
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "docs")
        .expect("docs entry");
    assert!(
        docs.get("nextRunAt").is_none(),
        "a schedule-less unit has no next run: {docs}"
    );
    let weekly = payload
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "weekly")
        .expect("weekly entry");
    let next = weekly["nextRunAt"]
        .as_str()
        .expect("a cron unit carries nextRunAt");
    assert!(
        next > cfgd_core::utc_now_iso8601().as_str(),
        "the next cron occurrence must be in the future: {next}"
    );

    // `assert_json_snapshot_in` has no normalization hook, and the `source`
    // field embeds the tempdir-backed fixture path — replicate its
    // pretty-print shape manually with normalization applied first.
    let rendered = serde_json::to_string_pretty(&payload).unwrap();
    let normalized = cfgd_core::normalize_for_snapshot(&rendered, &[(&source, "<SOURCE>")]);
    let normalized = normalize_iso8601(&normalized);
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

    // The run skeleton names the config file and times itself, and the
    // snapshot subject carries the run's own clock stamp — all three are
    // host-varying, so all three are normalized before the compare.
    let config_file = config_dir.path().join("cfgd.yaml");
    let normalized = cfgd_core::normalize_for_snapshot(
        &cfgd_core::output::strip_ansi(&cap.human()),
        &[
            (&source, "<SOURCE>"),
            (&config_file, "<CONFIG_DIR>/cfgd.yaml"),
            (config_dir.path(), "<CONFIG_DIR>"),
            (state_dir.path(), "<STATE_DIR>"),
        ],
    );
    let normalized =
        cfgd_core::normalize_snapshot_durations(&normalize_backup_timestamp(&normalized));
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

    let human = cfgd_core::output::strip_ansi(&render_cap.human());
    assert_eq!(
        human.matches('✗').count(),
        1,
        "exactly one fail line, got: {human:?}"
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
    cmd_backup_list(&cli, &list_printer, None, false).expect("listing stays on Report mode");
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
fn backup_list_still_reports_the_inventory_when_the_state_store_cannot_open() {
    // The declared units come from config, which loaded fine; only the run
    // history is lost. A permissions problem on `state.db` must not hide the
    // half of the command that still works.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    // A directory where `state.db` belongs makes the open fail without needing
    // to depend on the test process's privileges (root ignores mode bits).
    std::fs::create_dir_all(state_dir.path().join("state.db")).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_backup_list(&cli, &printer, None, false)
        .expect("an unreadable state store must not fail the listing");
    drop(printer);

    let human = cfgd_core::output::strip_ansi(&cap.human());
    assert!(
        human.contains("Backup history unavailable"),
        "the degradation must be visible: {human}"
    );
    assert!(
        human.contains("docs") && human.contains("weekly"),
        "the declared inventory must still render: {human}"
    );
    assert!(
        human.contains("never"),
        "every unit degrades to a 'never' last run: {human}"
    );
    // The count comes out of the same store, so it degrades with the history
    // rather than reading zero — which would be a unit whose snapshots are all
    // gone, the one state that would send someone looking for a bug. Unknown
    // for every unit, the column has nothing to say and is dropped rather
    // than padded.
    let header = human
        .lines()
        .find(|l| l.starts_with("Name"))
        .unwrap_or_else(|| panic!("no header in:\n{human}"));
    assert!(
        !header.contains("Snapshots") && !header.contains("Status"),
        "a column no row can fill is dropped, not padded: {header}"
    );
    let row = human
        .lines()
        .find(|l| l.starts_with("docs"))
        .unwrap_or_else(|| panic!("no docs row in:\n{human}"));
    assert!(
        !row.split_whitespace().any(|cell| cell == "0"),
        "an unknown snapshot count never reads 0: {row}"
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
        next_run_at: None,
        snapshots: Some(2),
    }];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_backup_list_doc(&entries, "2026-01-01T02:00:00Z"));
    drop(printer);

    let expected = serde_json::to_value(&entries).unwrap();
    let actual = cap.json().expect("backup list doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(Vec<BackupListEntry>)"
    );
}

// ---------------------------------------------------------------------------
// `backup rollback`
// ---------------------------------------------------------------------------

/// Take a snapshot, clobber the source, restore — which leaves the clobbered
/// bytes beside the source as the copy a rollback puts back.
fn restored_docs(cli: &cfgd::cli::Cli, source: &Path, live: &str) {
    run_docs(cli);
    std::fs::write(source, live).unwrap();
    let (printer, _cap) = Printer::for_test_doc();
    run_backup_restore(cli, &printer, &restore_args("docs"))
        .unwrap()
        .expect("a --yes restore is never declined");
}

#[test]
fn backup_rollback_puts_back_what_the_restore_overwrote() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    restored_docs(&cli, &source, "the version I actually wanted");
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "hello backup",
        "the restore landed first"
    );

    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    let outcome = run_backup_rollback(&cli, &printer, "docs", true)
        .unwrap()
        .expect("a --yes rollback is never declined");
    drop(printer);

    assert!(outcome.is_clean(), "outcome: {outcome:?}");
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "the version I actually wanted",
        "the rollback puts the pre-restore contents back"
    );

    let payload = cap.json().expect("rollback doc carries a payload");
    let rendered = serde_json::to_string_pretty(&payload).unwrap();
    // The safety copy's own stamp is real clock time, the same
    // `BACKUP_TIMESTAMP_FORMAT` the snapshot names carry.
    let normalized = normalize_backup_timestamp(&cfgd_core::normalize_for_snapshot(
        &rendered,
        &[(&source, "<SOURCE>")],
    ));
    cfgd_core::test_helpers::assert_snapshot_golden(
        Path::new(SNAPSHOT_ROOT),
        "backup/rollback.json",
        &normalized,
        env!("CARGO_PKG_VERSION"),
    );
}

/// The newest `<source>.cfgd-backup*` beside `source` — what
/// `cfgd_core::backup::rollback_copy` selects, read off the filesystem so an
/// integration test needs no `BackupUnit` of its own.
fn newest_sidecar_beside(source: &Path) -> std::path::PathBuf {
    let dir = source.parent().expect("source has a parent");
    let base = format!(
        "{}.cfgd-backup",
        source.file_name().expect("source name").to_string_lossy()
    );
    std::fs::read_dir(dir)
        .expect("read the source directory")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&base))
        // Mtime AND name, the tie-break `rollback_copy` itself breaks toward
        // the stamped spelling: a filesystem with coarse timestamps can stamp
        // the primary and its stamped neighbour alike, and picking the other
        // one would fail this test for a reason it is not about.
        .max_by_key(|e| {
            (
                e.metadata()
                    .and_then(|m| m.modified())
                    .expect("sidecar mtime"),
                e.file_name(),
            )
        })
        .map(|e| e.path())
        .expect("a sidecar beside the source")
}

#[test]
fn backup_rollback_is_its_own_inverse() {
    // A rollback copies the contents it displaces aside, so the second one puts
    // THOSE back: the pair returns the source to where it started rather than
    // the second being a no-op.
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    restored_docs(&cli, &source, "live");
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "hello backup");

    let roll = || {
        let (printer, _cap) = Printer::for_test_doc();
        run_backup_rollback(&cli, &printer, "docs", true)
            .unwrap()
            .expect("a --yes rollback is never declined");
    };
    roll();
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "live");

    // The second rollback's payload is the stamped sidecar the first left, and
    // the safety copy it takes reuses the primary — whose prune then deletes
    // that very payload. Staging it first is what makes that harmless, and this
    // is the assertion that says so out loud.
    let payload = newest_sidecar_beside(&source);
    roll();
    assert!(
        !payload.exists(),
        "the prune really does delete the copy the rollback was reading from"
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "hello backup",
        "the second rollback puts back what the first displaced"
    );
}

#[test]
fn backup_rollback_human() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    restored_docs(&cli, &source, "live");

    let (printer, cap) = Printer::for_test_doc();
    run_backup_rollback(&cli, &printer, "docs", true).unwrap();
    drop(printer);

    let config_file = config_dir.path().join("cfgd.yaml");
    let normalized = cfgd_core::normalize_for_snapshot(
        &cfgd_core::output::strip_ansi(&cap.human()),
        &[
            (&source, "<SOURCE>"),
            (&config_file, "<CONFIG_DIR>/cfgd.yaml"),
            (config_dir.path(), "<CONFIG_DIR>"),
            (state_dir.path(), "<STATE_DIR>"),
        ],
    );
    let normalized =
        cfgd_core::normalize_snapshot_durations(&normalize_backup_timestamp(&normalized));
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "backup/rollback.txt", &normalized,);
}

#[test]
fn backup_rollback_list_human() {
    // The pure builder, like `build_backup_list_doc_json_matches_serde_roundtrip`
    // and for the same reason: the Copy column pads to its widest cell, so a
    // tempdir-backed path would make the whole table's layout host-dependent.
    let entries = vec![BackupRollbackEntry {
        name: "docs".to_string(),
        copy: "/var/lib/app/notes.txt.cfgd-backup".to_string(),
        created: "2026-01-01T00:00:00Z".to_string(),
        size_bytes: 4096,
    }];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_backup_rollback_list_doc(
        &entries,
        "2026-01-01T02:00:00Z",
    ));
    drop(printer);

    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "backup/rollback_list.txt",
        &cfgd_core::output::strip_ansi(&cap.human()),
    );
}

#[test]
fn backup_rollback_list_json_matches_serde_roundtrip() {
    let entries = vec![BackupRollbackEntry {
        name: "docs".to_string(),
        copy: "/var/lib/app/notes.txt.cfgd-backup".to_string(),
        created: "2026-01-01T00:00:00Z".to_string(),
        size_bytes: 4096,
    }];
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_backup_rollback_list_doc(
        &entries,
        "2026-01-01T02:00:00Z",
    ));
    drop(printer);

    let actual = cap.json().expect("rollback listing carries a payload");
    assert_eq!(actual, serde_json::to_value(&entries).unwrap());
    let rendered = serde_json::to_string_pretty(&actual).unwrap();
    cfgd_core::test_helpers::assert_snapshot_golden(
        Path::new(SNAPSHOT_ROOT),
        "backup/rollback_list.json",
        &rendered,
        env!("CARGO_PKG_VERSION"),
    );
}

#[test]
fn backup_rollback_lists_only_the_units_that_have_a_copy() {
    // The two shared units declare ONE source, so a third unit pointing
    // somewhere nothing has displaced is what makes this the mixed case: the
    // listing answers what a rollback COULD put back, and a unit with no copy
    // beside its source is absent rather than listed with an empty cell.
    let (config_dir, state_dir, source) = backup_profile_setup();
    let untouched = config_dir.path().join("data").join("other.txt");
    std::fs::write(&untouched, "never displaced").unwrap();
    let profile = config_dir.path().join("profiles").join("withbackups.yaml");
    let mut yaml = std::fs::read_to_string(&profile).unwrap();
    yaml.push_str(&format!(
        "    - name: other\n      source: {}\n      retention: 3\n",
        untouched.display()
    ));
    std::fs::write(&profile, yaml).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    restored_docs(&cli, &source, "live");

    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cmd_backup_rollback(&cli, &printer, None, false).unwrap();
    drop(printer);

    let payload = cap.json().expect("the listing carries a payload");
    let entries = payload.as_array().expect("array payload");
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["docs", "weekly"], "{payload}");
    assert_eq!(entries[0]["sizeBytes"], "live".len());
    assert!(
        entries[0]["copy"]
            .as_str()
            .expect("copy path")
            .ends_with(".cfgd-backup"),
        "{payload}"
    );
}

#[test]
fn backup_rollback_listing_is_empty_when_nothing_was_displaced() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());

    let (printer, cap) = Printer::for_test_doc();
    cmd_backup_rollback(&cli, &printer, None, false).unwrap();
    drop(printer);

    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "backup/rollback_list_empty.txt",
        &cfgd_core::output::strip_ansi(&cap.human()),
    );
}

#[test]
fn backup_rollback_without_a_copy_is_a_typed_refusal() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());

    let (printer, _cap) = Printer::for_test_doc();
    let err = run_backup_rollback(&cli, &printer, "docs", true).unwrap_err();
    drop(printer);

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("a missing copy is the CLI's typed error, not a bare anyhow string");
    assert_eq!(meta.error_kind, "no_rollback_copy");
    assert_eq!(meta.name, "docs");
    assert!(
        meta.hints
            .iter()
            .any(|h| h.contains("cfgd backup restore <name>") && !h.contains("restore docs")),
        "the hint states how a copy comes to exist without instructing the destructive write \
         that would create one: {meta:?}"
    );
    assert!(
        meta.hints
            .iter()
            .any(|h| h.contains("cfgd backup list docs")),
        "the hint points at the read-only surface that shows this unit's snapshots: {meta:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "hello backup",
        "a refused rollback touches nothing"
    );
    assert_eq!(
        cfgd_core::exit::exit_code_for_error(
            err.downcast_ref::<cfgd_core::errors::CfgdError>()
                .expect("the typed error survives the CLI wrapper")
        ),
        cfgd_core::exit::ExitCode::NotFound,
        "the artifact the verb exists to put back is not there — exit 6, like every other \
         named-but-missing resource"
    );

    let (render_printer, render_cap) = Printer::for_test_doc();
    cfgd::cli::error::render_cli_error(&render_printer, &err);
    drop(render_printer);
    let human = cfgd_core::output::strip_ansi(&render_cap.human());
    assert_eq!(human.matches('✗').count(), 1, "one fail line: {human:?}");
    let normalized = cfgd_core::normalize_for_snapshot(&human, &[(&source, "<SOURCE>")]);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "backup/rollback_no_copy.txt",
        &normalized,
    );
}

#[test]
fn backup_rollback_without_a_copy_carries_its_hint_in_json() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());

    let (printer, _cap) = Printer::for_test_doc();
    let err = run_backup_rollback(&cli, &printer, "docs", true).unwrap_err();
    drop(printer);

    let (render_printer, render_cap) =
        Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cfgd::cli::error::render_cli_error(&render_printer, &err);
    drop(render_printer);

    let payload = render_cap.json().expect("error doc carries a payload");
    assert_eq!(payload["error"], "no_rollback_copy");
    assert_eq!(payload["name"], "docs");
    let rendered = serde_json::to_string_pretty(&payload).unwrap();
    let normalized = cfgd_core::normalize_for_snapshot(&rendered, &[(&source, "<SOURCE>")]);
    cfgd_core::test_helpers::assert_snapshot_golden(
        Path::new(SNAPSHOT_ROOT),
        "backup/rollback_no_copy.json",
        &normalized,
        env!("CARGO_PKG_VERSION"),
    );
}

#[test]
fn backup_rollback_without_yes_refuses_when_no_prompt_is_available() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    restored_docs(&cli, &source, "live");

    let (printer, _cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    let err = run_backup_rollback(&cli, &printer, "docs", false).unwrap_err();
    drop(printer);

    let meta = err
        .downcast_ref::<cfgd::cli::error::CliErrorMeta>()
        .expect("the refusal carries structured metadata");
    assert_eq!(meta.error_kind, "confirmation_required");
    assert!(
        meta.hints.iter().any(|h| h.contains("--yes")),
        "the remedy must ride along: {:?}",
        meta.hints
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "hello backup",
        "a rollback that was never confirmed must not have touched the source"
    );
}

#[test]
fn backup_rollback_declined_at_the_prompt_changes_nothing() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    restored_docs(&cli, &source, "live");

    let (printer, cap) = Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(false),
    ]);
    let declined = run_backup_rollback(&cli, &printer, "docs", false).unwrap();
    drop(printer);

    assert!(
        declined.is_none(),
        "a declined rollback produces no outcome"
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "hello backup",
        "declining must leave the source exactly as the restore left it"
    );

    let payload = cap
        .json()
        .expect("even a declined rollback emits its payload");
    let normalized =
        cfgd_core::normalize_for_snapshot(&payload.to_string(), &[(&source, "<SOURCE>")]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&normalized).unwrap(),
        serde_json::json!({
            "name": "docs",
            "copy": "<SOURCE>.cfgd-backup",
            "restoredTo": "<SOURCE>",
            "restored": false,
            "declined": true,
        }),
        "a decline exits 0, so it must not claim `clean: false` — the key is absent entirely"
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

    let human = cfgd_core::output::strip_ansi(&cap.human());
    assert!(
        human.contains("Backups") && human.contains("backup:docs"),
        "the backup that ran renders as its own owner group inside the run: {human}"
    );
    assert!(
        human.contains("snapshot notes.txt."),
        "the group's snapshot line names the artifact it wrote: {human}"
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

    let human = cfgd_core::output::strip_ansi(&cap.human());
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

/// Replace every ISO 8601 UTC stamp (`YYYY-MM-DDTHH:MM:SSZ`) with a stable
/// placeholder — `backup list`'s Next Run column and `nextRunAt` field are real
/// future clock times. Safe to apply wholesale in the `list` cases, where no
/// unit has a recorded run and `lastRunAt` is therefore absent.
fn normalize_iso8601(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        if iso8601_span(&chars[i..]) {
            out.push_str("<NEXT_RUN>");
            i += 20;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Replace every `cfgd_core::humanize_until` countdown (`in 4h`, `due now`)
/// with the same `<NEXT_RUN>` placeholder [`normalize_iso8601`] uses for the
/// instant it renders — a daily cron's distance from the suite's start hour is
/// not a golden's to pin. The backward direction is deliberately NOT folded: a
/// run the fixture just performed reads `just now` on any host.
///
/// Line ends are trimmed as part of the same pass, because the placeholder is
/// wider than every countdown it stands in for: the table pads a cell to its
/// column, so `in 4h` and `in 23h` leave different amounts of trailing space
/// behind one identical placeholder.
fn normalize_countdown(raw: &str) -> String {
    let mut out = raw.replace("due now", "<NEXT_RUN>");
    let mut from = 0;
    while let Some(offset) = out[from..].find("in ") {
        let start = from + offset;
        let rest = &out[start + 3..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || !matches!(rest[digits..].chars().next(), Some('m' | 'h' | 'd')) {
            from = start + 3;
            continue;
        }
        out.replace_range(start..start + 3 + digits + 1, "<NEXT_RUN>");
        from = start + "<NEXT_RUN>".len();
    }
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        + if out.ends_with('\n') { "\n" } else { "" }
}

/// True when `window` opens on an ISO 8601 UTC stamp (`2026-08-02T03:00:00Z`,
/// 20 chars).
fn iso8601_span(window: &[char]) -> bool {
    if window.len() < 20 {
        return false;
    }
    let digits = |s: &[char]| s.iter().all(|c| c.is_ascii_digit());
    digits(&window[0..4])
        && window[4] == '-'
        && digits(&window[5..7])
        && window[7] == '-'
        && digits(&window[8..10])
        && window[10] == 'T'
        && digits(&window[11..13])
        && window[13] == ':'
        && digits(&window[14..16])
        && window[16] == ':'
        && digits(&window[17..19])
        && window[19] == 'Z'
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
    // The engine appends `-N` when two snapshots of one unit render the same
    // second, so the suffix is absorbed into the placeholder rather than left
    // to flip a golden on a fast host.
    let mut len = 16;
    if window.len() > 17 && window[16] == '-' && window[17].is_ascii_digit() {
        len = 18;
        while len < window.len() && window[len].is_ascii_digit() {
            len += 1;
        }
    }
    Some(len)
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

    let skipped: Vec<&str> = outcome
        .reports
        .iter()
        .filter_map(|r| r.skipped.as_deref())
        .collect();
    assert_eq!(
        skipped.len(),
        1,
        "the busy unit must be carried out to the exit-code decision"
    );
    assert!(
        skipped[0].starts_with("pid "),
        "the report names the holder: {skipped:?}"
    );
    assert!(
        !outcome.fully_clean(),
        "a run the user asked for did not happen — the command must exit nonzero"
    );

    let human = cfgd_core::output::strip_ansi(&cap.human());
    assert!(
        human.contains("already running") && human.contains("docs"),
        "the busy unit must be reported: {human}"
    );
    // `apply` renders the same event as Skipped; the unit IS being backed up,
    // just not by us. Only the exit code distinguishes the two surfaces.
    assert!(
        human.contains("backup:docs") && human.contains("∅ snapshot"),
        "a busy unit renders with the Skipped role inside its own group, matching apply: {human:?}"
    );
    assert!(
        !human.contains("✗ snapshot"),
        "a busy unit is not a failed backup: {human:?}"
    );
    assert!(
        human.contains("◉ 1 action not attempted"),
        "the snapshot the held lock cost the run is counted, not silently dropped: {human:?}"
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

    let human = cfgd_core::output::strip_ansi(&cap.human());
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

// ---------------------------------------------------------------------------
// `backup list <name> --snapshots`
// ---------------------------------------------------------------------------

#[test]
fn backup_list_filters_to_the_named_unit() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_backup_list(&cli, &printer, Some("docs"), false).unwrap();
    drop(printer);

    let payload = cap.json().expect("backup list doc carries a payload");
    let entries = payload.as_array().expect("array payload");
    assert_eq!(entries.len(), 1, "naming 'docs' must list only 'docs'");
    assert_eq!(entries[0]["name"], "docs");
}

#[test]
fn backup_list_unknown_name_is_a_not_found_error() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, _cap) = Printer::for_test_doc();

    let err = cmd_backup_list(&cli, &printer, Some("bogus"), false).unwrap_err();
    assert_eq!(
        cfgd_core::exit::exit_code_for_error(
            err.downcast_ref::<cfgd_core::errors::CfgdError>()
                .expect("the typed error survives the CLI wrapper")
        ),
        cfgd_core::exit::ExitCode::NotFound,
        "an unknown backup name is exit 6, the same as `backup run`"
    );
}

#[test]
fn backup_list_snapshots_without_a_name_is_rejected() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, _cap) = Printer::for_test_doc();

    let err = cmd_backup_list(&cli, &printer, None, true).unwrap_err();
    assert!(
        format!("{err}").contains("--snapshots"),
        "the refusal must name the flag: {err}"
    );
}

#[test]
fn backup_list_snapshots_of_a_unit_that_never_ran_is_empty() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_backup_list(&cli, &printer, Some("docs"), true).unwrap();
    drop(printer);

    assert_eq!(
        cap.json().expect("payload"),
        serde_json::json!([]),
        "a unit with no runs lists no snapshots"
    );
}

#[test]
fn backup_list_snapshots_human() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);

    let (printer, cap) = Printer::for_test_doc();
    cmd_backup_list(&cli, &printer, Some("docs"), true).unwrap();
    drop(printer);

    let normalized = normalize_backup_timestamp(&cfgd_core::output::strip_ansi(&cap.human()));
    let normalized = normalize_iso8601(&normalized);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "backup/list_snapshots.txt",
        &normalized,
    );
}

#[test]
fn backup_list_counts_the_snapshots_a_unit_actually_holds() {
    // The count is the unit's own snapshots, not the inventory's row count:
    // `weekly` never ran and must stay at zero while `docs` reports its one.
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);

    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cmd_backup_list(&cli, &printer, None, false).unwrap();
    drop(printer);

    let payload = cap.json().expect("payload");
    let counts: Vec<(String, i64)> = payload
        .as_array()
        .expect("array payload")
        .iter()
        .map(|e| {
            (
                e["name"].as_str().expect("name").to_string(),
                e["snapshots"].as_i64().expect("snapshot count"),
            )
        })
        .collect();
    assert_eq!(
        counts,
        vec![("docs".to_string(), 1), ("weekly".to_string(), 0)],
        "each row counts its own unit's snapshots: {payload}"
    );
}

/// `docs`'s recorded `lastRunAt`, read through the same surface the assertions
/// below read — so a restore that re-anchored it is caught by comparison rather
/// than by a literal nobody can predict.
fn docs_last_run_at(cli: &cfgd::cli::Cli) -> String {
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cmd_backup_list(cli, &printer, None, false).unwrap();
    drop(printer);
    cap.json().expect("payload")[0]["lastRunAt"]
        .as_str()
        .expect("docs has a recorded run")
        .to_string()
}

/// The pin for the safety copy's home: after a restore, `backup list` counts
/// exactly the snapshots the operator asked for, `--snapshots` lists exactly
/// those, the unit's Last Run is still the run's, and the copy of what the
/// restore overwrote exists at its own path beside the source — a
/// `.cfgd-backup` sidecar, outside the unit's destination.
#[test]
fn backup_list_after_a_restore_counts_only_the_requested_snapshots() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);
    let last_run_at = docs_last_run_at(&cli);

    std::fs::write(&source, "clobbered").unwrap();
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    run_backup_restore(&cli, &printer, &restore_args("docs"))
        .unwrap()
        .expect("a --yes restore is never declined");
    drop(printer);
    let restore = cap.json().expect("restore payload");
    let safety = restore["safetyCopy"]
        .as_str()
        .expect("restoring over the live source reports where its contents went");
    assert_eq!(
        safety,
        cfgd_core::to_posix_string(cfgd_core::reconciler::cfgd_backup_path(&source, "")),
        "the safety copy is the sidecar beside the source: {restore}"
    );
    assert_eq!(
        std::fs::read_to_string(safety).unwrap(),
        "clobbered",
        "the sidecar holds what the restore overwrote"
    );
    assert!(
        !Path::new(safety).starts_with(state_dir.path()),
        "the safety copy must not live under the unit's destination: {safety}"
    );

    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cmd_backup_list(&cli, &printer, None, false).unwrap();
    drop(printer);
    let payload = cap.json().expect("payload");
    let docs = &payload.as_array().expect("array payload")[0];
    assert_eq!(
        docs["snapshots"], 1,
        "one run was requested, so one snapshot is counted: {payload}"
    );
    assert!(
        docs.get("safetySnapshots").is_none(),
        "there is no safety share of a count that never includes one: {payload}"
    );
    assert_eq!(
        docs["lastRunAt"].as_str(),
        Some(last_run_at.as_str()),
        "a restore must not become the unit's last run: {payload}"
    );

    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cmd_backup_list(&cli, &printer, Some("docs"), true).unwrap();
    drop(printer);
    let listed = cap.json().expect("payload");
    assert_eq!(
        listed.as_array().expect("array payload").len(),
        1,
        "--snapshots lists the unit's own snapshots and never the safety copy: {listed}"
    );

    let (printer, cap) = Printer::for_test_doc();
    cmd_backup_list(&cli, &printer, None, false).unwrap();
    drop(printer);
    let human = cfgd_core::output::strip_ansi(&cap.human());
    let row = human
        .lines()
        .find(|l| l.starts_with("docs"))
        .unwrap_or_else(|| panic!("no docs row in:\n{human}"));
    assert!(
        !row.contains("safety"),
        "the count is a plain number, with no safety share to annotate: {row}"
    );
    assert!(
        row.contains("Success") && row.contains("just now"),
        "the row carries the run's verdict and how long ago it ran: {row}"
    );
}

#[test]
fn backup_list_snapshots_json_shape() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);

    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cmd_backup_list(&cli, &printer, Some("docs"), true).unwrap();
    drop(printer);

    let payload = cap.json().expect("payload");
    let rendered = serde_json::to_string_pretty(&payload).unwrap();
    let normalized = normalize_iso8601(&normalize_backup_timestamp(&rendered));
    cfgd_core::test_helpers::assert_snapshot_golden(
        Path::new(SNAPSHOT_ROOT),
        "backup/list_snapshots.json",
        &normalized,
        env!("CARGO_PKG_VERSION"),
    );
}

// ---------------------------------------------------------------------------
// `backup restore`
// ---------------------------------------------------------------------------

/// Take one `docs` snapshot, so a restore/list test has something to act on.
fn run_docs(cli: &cfgd::cli::Cli) {
    let (printer, _cap) = Printer::for_test_doc();
    cmd_backup_run(cli, &printer, Some("docs")).unwrap();
}

/// `RestoreArgs` for a `--yes` restore of the newest snapshot into the source.
fn restore_args<'a>(name: &'a str) -> RestoreArgs<'a> {
    RestoreArgs {
        name,
        at: None,
        to: None,
        yes: true,
    }
}

#[test]
fn backup_restore_json_shape() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);
    std::fs::write(&source, "clobbered").unwrap();

    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    let outcome = run_backup_restore(&cli, &printer, &restore_args("docs"))
        .unwrap()
        .expect("a --yes restore is never declined");
    drop(printer);

    assert!(outcome.is_clean(), "outcome: {outcome:?}");
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "hello backup",
        "the snapshot's contents must be back in the source"
    );

    let payload = cap.json().expect("restore doc carries a payload");
    let rendered = serde_json::to_string_pretty(&payload).unwrap();
    let normalized = cfgd_core::normalize_for_snapshot(
        &rendered,
        &[(&source, "<SOURCE>"), (state_dir.path(), "<STATE_DIR>")],
    );
    let normalized = normalize_backup_timestamp(&normalized);
    cfgd_core::test_helpers::assert_snapshot_golden(
        Path::new(SNAPSHOT_ROOT),
        "backup/restore.json",
        &normalized,
        env!("CARGO_PKG_VERSION"),
    );
}

/// A restore is what an operator reaches for when something has gone wrong,
/// and none of its work depends on the profile's modules — the resolution is
/// spent on a header row. So a module whose git source cannot be reached
/// degrades to a stated reason and an absent row, and the data still goes back.
#[test]
fn a_restore_completes_over_a_module_whose_source_cannot_be_reached() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    // The snapshot is taken before the profile names the module: `backup run`
    // executes the profile's own hooks and so stays fatal on a profile it
    // cannot resolve.
    run_docs(&cli);

    let module_dir = config_dir.path().join("modules").join("broken");
    std::fs::create_dir_all(&module_dir).unwrap();
    // A port nothing listens on: the clone is refused at connect, so the fixture
    // neither reaches the network nor waits on a timeout.
    std::fs::write(
        module_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: broken\nspec:\n  files:\n    - source: https://127.0.0.1:1/broken.git\n      target: ~/broken.txt\n",
    )
    .unwrap();
    let profile = config_dir.path().join("profiles").join("withbackups.yaml");
    let with_module = std::fs::read_to_string(&profile)
        .unwrap()
        .replace("  modules: []\n", "  modules:\n    - broken\n");
    std::fs::write(&profile, with_module).unwrap();

    std::fs::write(&source, "clobbered").unwrap();
    let (printer, cap) = Printer::for_test_doc();
    let outcome = run_backup_restore(&cli, &printer, &restore_args("docs"))
        .unwrap()
        .expect("a --yes restore is never declined");
    drop(printer);

    assert!(outcome.is_clean(), "outcome: {outcome:?}");
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "hello backup",
        "the restore must put the snapshot back despite the unreachable module"
    );
    let human = cfgd_core::output::strip_ansi(&cap.human());
    assert!(
        human.contains("Modules not resolved"),
        "the reader is owed the reason the row is missing: {human}"
    );
    assert!(
        !human
            .lines()
            .any(|l| l.trim_start().starts_with("Modules ")),
        "an unresolved profile names no modules rather than guessing at them: {human}"
    );

    // The sibling verb keeps refusing: it executes the profile's own hooks, so
    // a profile it cannot resolve is a run it cannot make.
    let (printer, _cap) = Printer::for_test_doc();
    assert!(
        cmd_backup_run(&cli, &printer, Some("docs")).is_err(),
        "`cfgd backup run` must still refuse a profile it cannot resolve"
    );
}

#[test]
fn backup_restore_human() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);

    let (printer, cap) = Printer::for_test_doc();
    run_backup_restore(&cli, &printer, &restore_args("docs")).unwrap();
    drop(printer);

    // The restore opens with the run skeleton's header, so the config file it
    // names is host-varying the same way `backup run`'s is.
    let config_file = config_dir.path().join("cfgd.yaml");
    let normalized = cfgd_core::normalize_for_snapshot(
        &cfgd_core::output::strip_ansi(&cap.human()),
        &[
            (&source, "<SOURCE>"),
            (&config_file, "<CONFIG_DIR>/cfgd.yaml"),
            (config_dir.path(), "<CONFIG_DIR>"),
            (state_dir.path(), "<STATE_DIR>"),
        ],
    );
    // The restore now closes with the run rollup `backup run` closes with, and
    // that line wears the run's elapsed time.
    let normalized =
        cfgd_core::normalize_snapshot_durations(&normalize_backup_timestamp(&normalized));
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "backup/restore.txt", &normalized,);
}

/// `backup restore <name> --at <bogus>` must surface the CLI's typed
/// `snapshot_not_found` error (with the available-snapshot hint), not a bare
/// `anyhow` string — the same pinning the core-level `select_snapshot` gets
/// in `cfgd-core/src/backup/tests.rs`, proven here through the CLI wrapper
/// that names it `snapshot_not_found` and attaches the hint.
#[test]
fn backup_restore_at_unknown_snapshot_is_a_snapshot_not_found_error() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);

    let (printer, _cap) = Printer::for_test_doc();
    let err = run_backup_restore(
        &cli,
        &printer,
        &RestoreArgs {
            name: "docs",
            at: Some("no-such-snapshot"),
            to: None,
            yes: true,
        },
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("no-such-snapshot"),
        "expected the bogus snapshot name in the error, got: {msg}"
    );

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("a snapshot miss must be the CLI's typed error, not a bare anyhow string");
    assert_eq!(meta.error_kind, "snapshot_not_found");
    assert_eq!(meta.name, "docs");
    assert!(
        meta.hints
            .iter()
            .any(|h| h.contains("available snapshots:")),
        "expected the available-snapshots hint, got: {meta:?}"
    );
}

#[test]
fn backup_restore_to_redirects_and_omits_the_safety_copy() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);
    std::fs::write(&source, "live").unwrap();

    let elsewhere = state_dir.path().join("inspect").join("notes.txt");
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    run_backup_restore(
        &cli,
        &printer,
        &RestoreArgs {
            name: "docs",
            at: None,
            to: Some(&elsewhere),
            yes: true,
        },
    )
    .unwrap()
    .expect("a --yes restore is never declined");
    drop(printer);

    let payload = cap.json().expect("payload");
    assert_eq!(payload["restored"], true);
    assert!(
        payload.get("safetyCopy").is_none(),
        "--to leaves the live source alone, so no safety copy is taken: {payload}"
    );
    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "hello backup");
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "live",
        "--to must not touch the unit's source"
    );
}

#[test]
fn backup_restore_at_selects_an_older_snapshot_by_timestamp() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());

    let (p1, _c1) = Printer::for_test_doc();
    cmd_backup_run(&cli, &p1, Some("docs")).unwrap();
    drop(p1);
    // `namePattern` stamps to the second. Both snapshots survive either way —
    // the engine suffixes a collision — but two snapshots sharing one stamp make
    // this test's `--at <stamp>` fragment match both, which is an ambiguity
    // error rather than the selection being asserted below.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&source, "second generation").unwrap();
    let (p2, _c2) = Printer::for_test_doc();
    cmd_backup_run(&cli, &p2, Some("docs")).unwrap();
    drop(p2);

    let (list_printer, list_cap) =
        Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    cmd_backup_list(&cli, &list_printer, Some("docs"), true).unwrap();
    drop(list_printer);
    let listed = list_cap.json().expect("payload");
    let entries = listed.as_array().expect("array");
    assert_eq!(entries.len(), 2, "two runs a second apart, two snapshots");
    let oldest = entries[1]["name"]
        .as_str()
        .expect("oldest name")
        .to_string();
    let stamp = oldest
        .rsplit_once('.')
        .expect("the default namePattern ends in .{timestamp}")
        .1
        .to_string();

    let (printer, _cap) = Printer::for_test_doc();
    let outcome = run_backup_restore(
        &cli,
        &printer,
        &RestoreArgs {
            name: "docs",
            at: Some(&stamp),
            to: None,
            yes: true,
        },
    )
    .unwrap()
    .expect("restore ran");
    drop(printer);

    assert_eq!(
        outcome.snapshot, oldest,
        "--at must reach the OLDER snapshot"
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "hello backup",
        "the older snapshot's contents are what landed"
    );
}

#[test]
fn backup_restore_unknown_snapshot_lists_the_alternatives() {
    let (config_dir, state_dir, _source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);

    let (printer, _cap) = Printer::for_test_doc();
    let err = run_backup_restore(
        &cli,
        &printer,
        &RestoreArgs {
            name: "docs",
            at: Some("99991231T000000Z"),
            to: None,
            yes: true,
        },
    )
    .unwrap_err();
    drop(printer);

    assert_eq!(
        cfgd_core::exit::exit_code_for_error(
            err.downcast_ref::<cfgd_core::errors::CfgdError>()
                .expect("the typed error survives the CLI wrapper")
        ),
        cfgd_core::exit::ExitCode::NotFound,
        "an unknown snapshot is exit 6, like every other named-but-missing resource"
    );

    let (render_printer, render_cap) = Printer::for_test_doc();
    cfgd::cli::error::render_cli_error(&render_printer, &err);
    drop(render_printer);
    let human = cfgd_core::output::strip_ansi(&render_cap.human());
    assert_eq!(human.matches('✗').count(), 1, "one fail line: {human:?}");
    assert!(
        human.contains("available snapshots: notes.txt."),
        "the alternatives must render in human mode: {human:?}"
    );
}

#[test]
fn backup_restore_without_yes_refuses_when_no_prompt_is_available() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);
    std::fs::write(&source, "live").unwrap();

    // No seeded prompt answer and structured output: `prompt_confirm` refuses,
    // and that must surface as an ERROR, never as a silent "aborted".
    let (printer, _cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);
    let err = run_backup_restore(
        &cli,
        &printer,
        &RestoreArgs {
            name: "docs",
            at: None,
            to: None,
            yes: false,
        },
    )
    .unwrap_err();
    drop(printer);

    let meta = err
        .downcast_ref::<cfgd::cli::error::CliErrorMeta>()
        .expect("the refusal carries structured metadata");
    assert_eq!(meta.error_kind, "confirmation_required");
    assert!(
        meta.hints.iter().any(|h| h.contains("--yes")),
        "the remedy must ride along: {:?}",
        meta.hints
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "live",
        "a restore that was never confirmed must not have touched the source"
    );
}

#[test]
fn backup_restore_declined_at_the_prompt_changes_nothing() {
    let (config_dir, state_dir, source) = backup_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    run_docs(&cli);
    std::fs::write(&source, "live").unwrap();
    let before = std::fs::read_dir(state_dir.path().join("backups").join("docs"))
        .unwrap()
        .count();

    let (printer, cap) = Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(false),
    ]);
    let declined = run_backup_restore(
        &cli,
        &printer,
        &RestoreArgs {
            name: "docs",
            at: None,
            to: None,
            yes: false,
        },
    )
    .unwrap();
    drop(printer);

    assert!(declined.is_none(), "a declined restore produces no outcome");
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "live",
        "declining must leave the source exactly as it was"
    );
    assert_eq!(
        std::fs::read_dir(state_dir.path().join("backups").join("docs"))
            .unwrap()
            .count(),
        before,
        "declining must not take a safety copy either"
    );

    let payload = cap
        .json()
        .expect("even a declined restore emits its payload");
    let normalized =
        cfgd_core::normalize_for_snapshot(&payload.to_string(), &[(&source, "<SOURCE>")]);
    let normalized = normalize_backup_timestamp(&normalized);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&normalized).unwrap(),
        serde_json::json!({
            "name": "docs",
            "snapshot": "notes.txt.<TIMESTAMP>",
            "restoredTo": "<SOURCE>",
            "restored": false,
            "declined": true,
        }),
        "a decline exits 0, so it must not claim `clean: false` — the key is absent entirely"
    );
}

//! Snapshot tests for cfgd sync — local repo pull, source iteration,
//! permission prompts, failure handling, bridge transition.

mod common;

use std::path::Path;

use cfgd::cli::output_types::{SourceSyncOutput, SyncOutput};
use cfgd::cli::sync::{build_sync_doc, cmd_sync};
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::test_helpers::EnvVarGuard;
use pretty_assertions::assert_eq;
use serial_test::serial;

use common::{
    cli_for, permission_change_source_setup, tiny_profile_setup, two_source_setup,
    unreachable_source_setup,
};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_output() -> SyncOutput {
    SyncOutput {
        local_pulled: false,
        sources: vec![
            SourceSyncOutput {
                name: "team-a".to_string(),
                status: "synced".to_string(),
                commit: Some("abc1234def56".to_string()),
            },
            SourceSyncOutput {
                name: "team-b".to_string(),
                status: "synced".to_string(),
                commit: Some("def56abc1234".to_string()),
            },
        ],
    }
}

fn normalize_tempdir_paths(raw: &str, config_dir: &Path) -> String {
    let cfg_file = config_dir.join("cfgd.yaml");
    cfgd_core::normalize_for_snapshot(
        raw,
        &[
            (&cfg_file, "<CONFIG_DIR>/cfgd.yaml"),
            (config_dir, "<CONFIG_DIR>"),
        ],
    )
}

/// Replace the commit short-hash (12 hex chars) with a stable placeholder so
/// goldens don't drift across runs.
fn normalize_commit_hashes(raw: &str) -> String {
    // A ref MOVEMENT renders two hashes on one line (`commit: <old> → <new>`),
    // so the scan anchors on the arrow as well as on the label. Each anchor
    // only folds what actually is a hash, so an arrow elsewhere is untouched.
    const NEEDLES: [&str; 2] = ["commit: ", "→ "];
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some((idx, needle)) = NEEDLES
        .iter()
        .filter_map(|n| rest.find(n).map(|i| (i, *n)))
        .min_by_key(|(i, _)| *i)
    {
        let after = idx + needle.len();
        out.push_str(&rest[..after]);
        let tail = &rest[after..];
        let hex_len = tail
            .chars()
            .take(12)
            .take_while(|c| c.is_ascii_hexdigit())
            .count();
        if hex_len == 12 {
            out.push_str("<COMMIT>");
            rest = &tail[12..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out.replace('\\', "/")
}

/// Two-source happy path: local pull + per-source spinners + sources updated status.
#[test]
#[serial]
fn sync_happy_human() {
    let _allow = EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let (_workspace, config_dir, state_dir, _branch_a, _branch_b) = two_source_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_sync(&cli, &printer).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path());
    let normalized = normalize_commit_hashes(&normalized);
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "sync/happy.txt", &stripped);
}

/// JSON payload roundtrip — SyncOutput shape via build_sync_doc + cap.json().
#[test]
fn sync_happy_json() {
    let output = happy_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_sync_doc(&output));
    drop(printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("sync doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(SyncOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "sync/happy.json");
}

/// No-sources path emits only the local pull section.
#[test]
#[serial]
fn sync_no_sources_human() {
    let (config_dir, state_dir, _target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_sync(&cli, &printer).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path());
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "sync/no_sources.txt", &stripped);
}

/// The closing `Modules` row names what the synced config RESOLVES to, not
/// what its profile declares: `editor` alone is in `spec.modules`, and the row
/// reads `core, editor`.
#[test]
#[serial]
fn sync_module_dependency_header_human() {
    let (config_dir, state_dir) = common::profile_with_module_dependency_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_sync(&cli, &printer).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path());
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "sync/module_dependency.txt",
        &stripped
    );
}

/// Permission-rejection path skips the source and prints a Skipped status.
#[test]
#[serial]
fn sync_perm_changes_rejection_human() {
    let _allow = EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let (_workspace, config_dir, state_dir, _branch) = permission_change_source_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    use cfgd_core::output::{PromptAnswer, Verbosity};
    let (printer, buf) = Printer::for_test_with_prompt_responses_at(
        vec![PromptAnswer::Confirm(false)],
        Verbosity::Normal,
    );

    cmd_sync(&cli, &printer).unwrap();
    printer.flush();
    drop(printer);

    let raw = buf.lock().unwrap().clone();
    let normalized = normalize_tempdir_paths(&raw, config_dir.path());
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "sync/perm_changes.txt", &stripped);
}

/// Permission-acceptance path emits the canonical "'X' synced" line after the prompt.
#[test]
#[serial]
fn sync_perm_changes_accept_human() {
    let _allow = EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");

    let (_workspace, config_dir, state_dir, _branch) = permission_change_source_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    use cfgd_core::output::{PromptAnswer, Verbosity};
    let (printer, buf) = Printer::for_test_with_prompt_responses_at(
        vec![PromptAnswer::Confirm(true)],
        Verbosity::Normal,
    );

    cmd_sync(&cli, &printer).unwrap();
    printer.flush();
    drop(printer);

    let raw = buf.lock().unwrap().clone();
    // The golden folds both hashes to one placeholder, so the claim that the
    // two ENDS of the movement differ can only be made here, on the capture
    // that still holds them.
    assert_movement_ends_differ(&strip_ansi(&raw));
    let normalized = normalize_tempdir_paths(&raw, config_dir.path());
    let normalized = normalize_commit_hashes(&normalized);
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "sync/perm_changes_accept.txt",
        &stripped,
    );
}

/// Assert the one `commit: <old> → <new>` line names two different commits.
fn assert_movement_ends_differ(human: &str) {
    let line = human
        .lines()
        .find(|l| l.contains("commit: "))
        .unwrap_or_else(|| panic!("no commit line in:\n{human}"));
    let detail = line.split("commit: ").nth(1).expect("commit detail");
    let ends: Vec<&str> = detail.split(" → ").collect();
    assert_eq!(
        ends.len(),
        2,
        "expected a two-ended movement, got: {detail}"
    );
    assert_ne!(
        ends[0], ends[1],
        "a ref that did not move must render one commit, never an arrow to itself"
    );
}

/// Failed source produces a "Failed to sync" status inside the Sources section.
#[test]
#[serial]
fn sync_source_failure_human() {
    let _disallow = EnvVarGuard::unset("CFGD_ALLOW_LOCAL_SOURCES");

    let (config_dir, state_dir) = unreachable_source_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_sync(&cli, &printer).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path());
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "sync/source_failure.txt",
        &stripped,
    );
}

/// Streaming section followed by buffered Doc produces exactly one blank line between.
#[test]
fn sync_bridge_one_blank_line() {
    let (printer, cap) = Printer::for_test_doc();

    printer.heading("Sync");
    {
        let repo_sec = printer.section("Local Repo");
        repo_sec.status(Role::Ok, "Already up to date");
    }

    let doc = Doc::new()
        .section("Source Commits", |s| s.bullet("team-a @ abc1234"))
        .with_data(happy_output());
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

    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "sync/bridge.txt", &captured);
}

#[test]
#[serial]
fn a_successful_sync_records_the_fetch_so_status_stops_saying_not_yet_fetched() {
    // `sync` is the command whose whole job is refreshing sources, but the
    // freshness ledger used to hear only from `source add` / `source update`,
    // so `cfgd status` right after a green sync still reported the source as
    // never fetched.
    let _allow = EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (_workspace, config_dir, state_dir, _target) = common::opted_in_script_source_setup(false);
    let cli = cli_for(config_dir.path(), state_dir.path());

    let (printer, _cap) = Printer::for_test_doc();
    cmd_sync(&cli, &printer).expect("the source must sync");
    drop(printer);

    let state =
        cfgd_core::state::StateStore::open_in_dir(state_dir.path()).expect("state store opens");
    let records = state.config_sources().expect("config_sources reads");
    let acme = records
        .iter()
        .find(|r| r.name == "acme")
        .expect("a synced source must leave a state record");
    assert!(
        acme.last_fetched.is_some() && acme.last_commit.is_some(),
        "the record must carry the fetch time and the resolved commit: {acme:?}"
    );

    // The declared catalog carries the columns the status payload does not,
    // so the shared `Sources` table has something to render.
    let declared = vec![cfgd::cli::output_types::SourceListEntry {
        name: "acme".to_string(),
        url: Some(acme.origin_url.clone()),
        priority: Some(100),
        version: acme.source_version.clone(),
        status: acme.status.clone(),
        last_fetched: acme.last_fetched.clone(),
        signed: None,
        require_signed_commits: Some(false),
        last_commit: acme.last_commit.clone(),
        drift_count: None,
    }];

    // The record is what keeps `status` off the "not yet fetched" branch.
    let output = cfgd::cli::status::StatusOutput {
        last_apply: None,
        drift: Vec::new(),
        sources: records,
        pending_decisions: Vec::new(),
        modules: Vec::new(),
        managed_resources: Vec::new(),
        warnings: Vec::new(),
        classification_degraded: false,
        classification_degraded_code: None,
        classification_degraded_reason: None,
        drift_checked_live: false,
        last_scan_at: None,
    };

    let (status_printer, status_cap) = Printer::for_test_doc();
    status_printer.emit(cfgd::cli::status::build_fleet_status_doc(
        &output,
        &[],
        &declared,
        Path::new("/tmp/cfgd.yaml"),
        "default",
        "2026-05-14T10:05:00Z",
        &Default::default(),
    ));
    drop(status_printer);
    let rendered = strip_ansi(&status_cap.human());
    assert!(
        !rendered.contains("not yet fetched"),
        "status must report the synced source, not claim it was never fetched: {rendered}"
    );
    assert!(
        rendered.contains("acme"),
        "the source must appear in the Config Sources table: {rendered}"
    );
}

/// Representative of the "missing else arm" shape at
/// `cli/sync.rs`'s per-source loop. The sibling arm this fix added — `Ok(())`
/// from `load_source` but the source absent from the cache — is structurally
/// unreachable through the real `SourceManager` (every success path inserts
/// into `self.sources` before returning `Ok`), so this proves the discipline
/// on the reachable sibling instead: the `Err(e)` arm right below it, which
/// settles the SAME spinner the same way (`finish_fail`, one line, no Drop).
/// Both arms share one shape by construction — `sp.finish_fail("sync
/// failed").detail(...)` — so proving one never leaks proves the other by
/// symmetry. Live capture (not `for_test_doc`) so a leaked Drop-interrupted
/// line would be visible rather than silently absorbed into a buffered Doc.
#[test]
#[serial]
fn sync_source_failure_settles_the_spinner_exactly_once_never_via_drop() {
    let _disallow = EnvVarGuard::unset("CFGD_ALLOW_LOCAL_SOURCES");

    let (config_dir, state_dir) = unreachable_source_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, buf) = Printer::for_test_live_scrollback();

    cmd_sync(&cli, &printer).unwrap();
    drop(printer);

    let out = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        out.contains("source:missing-team"),
        "the owner header must be committed via commit_header before the settle line: {out}"
    );
    assert!(
        out.contains("Sync failed — git error"),
        "the finish_fail line must be committed: {out}"
    );
    assert_eq!(
        out.matches("Sync failed — git error").count(),
        1,
        "the failure must settle exactly once, never twice: {out}"
    );
    assert!(
        !out.contains("(interrupted)"),
        "a spinner settled by finish_fail must never also settle via Drop: {out}"
    );
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

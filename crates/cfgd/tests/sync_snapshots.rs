//! Snapshot tests for cfgd sync — local repo pull, source iteration,
//! permission prompts, failure handling, bridge transition.

mod common;

use std::path::Path;

use cfgd::cli::output_types::{SourceSyncOutput, SyncOutput};
use cfgd::cli::sync::{build_sync_doc, cmd_sync};
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
    let mut out = raw.to_string();
    let cfg_file = config_dir.join("cfgd.yaml");
    out = out.replace(
        &cfg_file.to_string_lossy().to_string(),
        "<CONFIG_DIR>/cfgd.yaml",
    );
    out = out.replace(&config_dir.to_string_lossy().to_string(), "<CONFIG_DIR>");
    out.replace('\\', "/")
}

/// Replace the commit short-hash (12 hex chars) with a stable placeholder so
/// goldens don't drift across runs.
fn normalize_commit_hashes(raw: &str) -> String {
    let needle = "commit: ";
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(idx) = rest.find(needle) {
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
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "sync/happy.txt", &stripped);
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
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "sync/no_sources.txt", &stripped);
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
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "sync/perm_changes.txt", &stripped);
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
    let normalized = normalize_tempdir_paths(&raw, config_dir.path());
    let normalized = normalize_commit_hashes(&normalized);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "sync/perm_changes_accept.txt",
        &stripped,
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
    assert_snapshot(
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
        let repo_sec = printer.section("Local repo");
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

    assert_snapshot(Path::new(SNAPSHOT_ROOT), "sync/bridge.txt", &captured);
}

// ─────────────────────────────────────────────────────
// snapshot helpers
// ─────────────────────────────────────────────────────

fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();

    // Normalize CRLF→LF: windows captured output has \r\n; committed snapshot is LF.

    let actual_norm = actual.replace("\r\n", "\n");

    let expected_norm = expected.replace("\r\n", "\n");

    pretty_assertions::assert_eq!(actual_norm, expected_norm, "snapshot mismatch: {name}");
}

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

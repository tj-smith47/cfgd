//! Snapshot tests for `cfgd pull`.
//!
//! `pulled` and `up_to_date` cases drive the streaming + buffered shape
//! through the `render_pull` helper with stubbed `git_pull_sync` results —
//! standing up a fast-forwardable git remote in-tree is fixture-heavy and
//! out of proportion for a single-operation command. `failed` runs real
//! `cmd_pull` against a non-git config_dir so the error path is exercised
//! end-to-end. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test pull_snapshots

mod common;

use std::path::Path;

use cfgd::cli::output_types::PullOutput;
use cfgd::cli::pull::{build_pull_doc, cmd_pull, render_pull};
use cfgd_core::output::Printer;
use pretty_assertions::assert_eq;

use common::{cli_for, tiny_profile_setup};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn pulled_output() -> PullOutput {
    PullOutput {
        status: "pulled".to_string(),
        error: None,
    }
}

/// Stubbed `Ok(true)` — new commits were pulled.
#[test]
fn pull_pulled_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Pull");
    render_pull(&printer, &Ok(true));
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "pull/pulled.txt", &stripped);
}

/// JSON payload roundtrip — PullOutput shape via build_pull_doc + cap.json().
#[test]
fn pull_pulled_json() {
    let output = pulled_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_pull_doc(&output));
    drop(printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("pull doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(PullOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "pull/pulled.json");
}

/// Stubbed `Ok(false)` — remote was up to date, no fast-forward.
#[test]
fn pull_up_to_date_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Pull");
    render_pull(&printer, &Ok(false));
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "pull/up_to_date.txt", &stripped);
}

/// Real `cmd_pull` against a tempdir config_dir that is NOT a git repo —
/// `git_pull_sync` returns `Err("open repo: ...")`, which renders as the
/// `Pull failed` warn status with the libgit2 detail.
#[test]
fn pull_failed_human() {
    let (config_dir, state_dir, _target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_pull(&cli, &printer).unwrap();
    drop(printer);

    let normalized = normalize_libgit2_paths(&cap.human(), config_dir.path());
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "pull/failed.txt", &stripped);
}

// ─────────────────────────────────────────────────────
// snapshot helpers
// ─────────────────────────────────────────────────────

/// Replace tempdir-rooted paths and libgit2's error-message path with stable
/// placeholders so the failed-pull golden is host-stable.
fn normalize_libgit2_paths(raw: &str, config_dir: &Path) -> String {
    let mut out = raw.to_string();
    out = out.replace(&config_dir.to_string_lossy().to_string(), "<CONFIG_DIR>");
    out
}

fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    pretty_assertions::assert_eq!(actual, &expected, "snapshot mismatch: {name}");
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

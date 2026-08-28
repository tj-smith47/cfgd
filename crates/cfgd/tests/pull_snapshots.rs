//! Snapshot tests for `cfgd pull`.
//!
//! `pulled` and `up_to_date` cases drive the streaming + buffered shape
//! through the `render_pull` helper with stubbed `git_pull_sync` results —
//! standing up a fast-forwardable git remote in-tree is fixture-heavy and
//! out of proportion for a single-operation command. The refusal cases drive
//! the real `git_pull_sync` seam — against a directory that is no repository,
//! and against a repository with no `origin`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test pull_snapshots

mod common;

use std::path::Path;

use cfgd::cli::output_types::PullOutput;
use cfgd::cli::pull::{build_pull_doc, cmd_pull, render_pull};
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::daemon::PullOutcome;
use cfgd_core::daemon::RefMovement;
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

/// Stubbed fast-forward — new commits were pulled.
#[test]
fn pull_pulled_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Pull");
    render_pull(
        &printer,
        PullOutcome::Moved(RefMovement {
            from: "1111111111111111111111111111111111111111".to_string(),
            to: "2222222222222222222222222222222222222222".to_string(),
        }),
    );
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "pull/pulled.txt", &stripped);
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

/// Stubbed no-op — remote was up to date, no fast-forward.
#[test]
fn pull_up_to_date_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Pull");
    render_pull(&printer, PullOutcome::UpToDate);
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "pull/up_to_date.txt", &stripped);
}

/// Real `cmd_pull` against a tempdir config_dir that is NOT a git repo.
///
/// There is no remote to be out of date with, so this is not a failure: the
/// same verdict `cfgd sync`'s local-repo leg answers, from the same seam.
#[test]
fn pull_over_a_non_repo_says_there_is_nothing_to_pull() {
    let (config_dir, state_dir, _target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_pull(&cli, &printer).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cfgd_core::normalize_for_snapshot(&cap.human(), &[]));
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "pull/not_a_repository.txt",
        &stripped
    );
}

/// A real repository whose pull refuses: the cause is stated without
/// libgit2's `class=…; code=…` tail, and the hint names the fix for THIS
/// kind of refusal rather than "resolve it by hand".
#[test]
fn pull_failure_states_its_cause_without_libgit2_internals() {
    let (config_dir, _state_dir, _target) = tiny_profile_setup();
    seed_repository(config_dir.path());

    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Pull");
    render_pull(
        &printer,
        cfgd_core::daemon::git_pull_sync(config_dir.path()),
    );
    drop(printer);

    let human = strip_ansi(&cap.human());
    assert!(
        !human.contains("class="),
        "a result line must not carry libgit2 internals:\n{human}"
    );
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "pull/failed.txt", &human);
}

/// A worktree or a submodule keeps its `.git` as a FILE, and it is a
/// repository like any other — the probe asks whether the entry EXISTS.
#[test]
fn a_gitlink_config_dir_still_gets_its_pull() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    seed_repository(&repo_dir);

    let linked = tmp.path().join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", repo_dir.join(".git").display()),
    )
    .unwrap();

    assert!(
        cfgd_core::daemon::is_git_repository(&linked),
        "a gitlink file is a repository"
    );
    assert_ne!(
        cfgd_core::daemon::git_pull_sync(&linked),
        PullOutcome::NotARepository,
        "a gitlink config dir must still be pulled"
    );
}

/// A repository with one commit and no `origin`, so a pull refuses without
/// reaching a network and fails the same way on every host.
fn seed_repository(dir: &Path) {
    let repo = git2::Repository::init(dir).unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    let tree = {
        let mut index = repo.index().unwrap();
        let oid = index.write_tree().unwrap();
        repo.find_tree(oid).unwrap()
    };
    repo.commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
        .unwrap();
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

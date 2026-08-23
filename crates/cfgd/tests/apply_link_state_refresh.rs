//! `cfgd apply` re-records the content hash of a file deployed by symlink after
//! the user edits it THROUGH the link, and says nothing while doing it.
//!
//! Editing through a symlink writes the module's own source file, so link
//! identity still holds and no action is ever planned. The recorded
//! `managed_resources.last_hash` would otherwise describe bytes that are no
//! longer on disk, and the consumer asking "did the user hand-modify this?"
//! answers yes forever.

mod common;

use std::path::{Path, PathBuf};

use cfgd::cli::apply;
use cfgd_core::output::{Printer, Verbosity};
use cfgd_core::state::StateStore;
use cfgd_core::test_helpers::captured_text;

use common::{apply_args, cli_for};

/// Build a tempdir-backed profile deploying one file with `strategy`.
///
/// Returns `(config_dir, state_dir, source, target)`.
fn setup(strategy: &str) -> (tempfile::TempDir, tempfile::TempDir, PathBuf, PathBuf) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    let source = files_dir.join("hello.txt");
    std::fs::write(&source, "hello world").unwrap();

    let target = config_dir.path().join("out").join("hello.txt");
    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: {strategy}\n",
        target.display()
    );
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir, source, target)
}

/// Run one `cfgd apply` against the fixture, returning what it printed.
fn apply_once(config_dir: &Path, state_dir: &Path) -> String {
    let cli = cli_for(config_dir, state_dir);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    apply::cmd_apply(&cli, &printer, &apply_args()).unwrap();
    captured_text(&buf)
}

/// The recorded hash of the managed file at `target`, if cfgd tracks one.
fn recorded_hash(state_dir: &Path, target: &Path) -> Option<String> {
    let state = StateStore::open(&state_dir.join("state.db")).unwrap();
    let id = cfgd_core::to_posix_string(target);
    state
        .managed_resources()
        .unwrap()
        .into_iter()
        .find(|r| r.resource_type == "file" && r.resource_id == id)
        .and_then(|r| r.last_hash)
}

/// What the "did the user hand-modify the deployed file?" check compares
/// against: the hash of the bytes the target currently holds.
fn deployed_hash(target: &Path) -> String {
    cfgd_core::sha256_hex(&std::fs::read(target).unwrap())
}

#[cfg(unix)]
#[test]
fn apply_refreshes_the_recorded_hash_of_a_file_edited_through_its_symlink() {
    let (config_dir, state_dir, source, target) = setup("Symlink");

    let first = apply_once(config_dir.path(), state_dir.path());
    assert!(
        first.contains("hello.txt"),
        "the first apply deploys the file and names it: {first}"
    );
    assert!(target.is_symlink(), "deployed by symlink");

    // The user edits the deployed file. Through the link that IS the source
    // file, so the module already holds the new bytes.
    std::fs::write(&target, "edited through the link").unwrap();
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "edited through the link",
        "the edit landed on the module's own source file"
    );
    assert_ne!(
        recorded_hash(state_dir.path(), &target),
        Some(deployed_hash(&target)),
        "the recorded hash is stale before the next apply"
    );

    let second = apply_once(config_dir.path(), state_dir.path());
    assert!(
        second.contains("Nothing to do"),
        "an edit through the link is not drift: {second}"
    );
    assert!(
        !second.contains("hello.txt"),
        "the refresh is silent — nothing names the resource: {second}"
    );
    assert_eq!(
        recorded_hash(state_dir.path(), &target),
        Some(deployed_hash(&target)),
        "the recorded hash now describes the bytes on disk, so the \
         hand-modified check answers no"
    );

    // Idempotent: a third apply with no further edit leaves the row alone.
    let before = StateStore::open(&state_dir.path().join("state.db"))
        .unwrap()
        .managed_resources()
        .unwrap();
    let third = apply_once(config_dir.path(), state_dir.path());
    assert!(third.contains("Nothing to do"), "still converged: {third}");
    assert_eq!(
        recorded_hash(state_dir.path(), &target),
        Some(deployed_hash(&target))
    );
    assert_eq!(
        before.len(),
        StateStore::open(&state_dir.path().join("state.db"))
            .unwrap()
            .managed_resources()
            .unwrap()
            .len(),
        "no row minted by the refresh"
    );
}

#[test]
fn apply_does_not_refresh_the_recorded_hash_of_a_copy_deployed_file() {
    let (config_dir, state_dir, _source, target) = setup("Copy");

    apply_once(config_dir.path(), state_dir.path());
    assert!(!target.is_symlink(), "deployed by copy");

    // Under Copy the target is cfgd's own file, so editing it is drift.
    std::fs::write(&target, "edited at the target").unwrap();

    let second = apply_once(config_dir.path(), state_dir.path());
    assert!(
        second.contains("hello.txt"),
        "a content edit under Copy is drift and is repaired out loud: {second}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "hello world",
        "the apply rewrote the target from its source"
    );
    assert_ne!(
        recorded_hash(state_dir.path(), &target),
        Some(cfgd_core::sha256_hex(b"edited at the target")),
        "a Copy target's edit is never recorded as the new truth"
    );
}

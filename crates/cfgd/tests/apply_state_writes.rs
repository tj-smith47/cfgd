//! State-write regression test for `cfgd apply`.
//!
//! Guards against accidental "while we're here" side-effect rewrites
//! during the output-system migration. Verifies:
//!   1. cmd_apply against a tempdir with one file action increments the
//!      apply-log row count by exactly +1.
//!   2. The apply lock is released after the run (re-acquire succeeds).
//!
//! Test asserts on *count*, not output — output shape is covered by
//! `apply_snapshots.rs`.

mod common;

use cfgd::cli::apply;
use cfgd_core::state::StateStore;
use cfgd_core::test_helpers::test_printer;

use common::{apply_args, cli_for, tiny_profile_setup};

#[test]
fn cmd_apply_increments_apply_log_by_one() {
    let (config_dir, state_dir, target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let printer = test_printer();
    let args = apply_args();

    // Baseline: no applies recorded.
    let db_path = state_dir.path().join("state.db");
    {
        let state = StateStore::open(&db_path).unwrap();
        let before = state.history(100).unwrap();
        assert_eq!(before.len(), 0, "no applies before the test");
    }

    apply::cmd_apply(&cli, &printer, &args).unwrap();
    assert!(target.exists(), "target file was created");

    // Exactly one apply record now.
    let state = StateStore::open(&db_path).unwrap();
    let after = state.history(100).unwrap();
    assert_eq!(
        after.len(),
        1,
        "cmd_apply must add exactly one apply-log row, got {} ({:?})",
        after.len(),
        after
    );
}

#[test]
fn cmd_apply_releases_apply_lock() {
    let (config_dir, state_dir, _target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let printer = test_printer();
    let args = apply_args();

    apply::cmd_apply(&cli, &printer, &args).unwrap();

    // If the lock is released, re-acquiring must succeed.
    let guard = cfgd_core::acquire_apply_lock(state_dir.path())
        .expect("apply lock should be released after cmd_apply");
    drop(guard);
}

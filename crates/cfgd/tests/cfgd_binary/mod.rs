//! The one constructor every real-binary test in this crate spawns `cfgd`
//! through.

use std::path::PathBuf;
use std::process::Command;

use assert_cmd::cargo::{CargoError, CommandCargoExt};

thread_local! {
    // libtest runs every test on a thread of its own, so this is one state
    // store per test: shared by each invocation the test makes, and deleted
    // when the test's thread exits.
    static STATE_DIR: tempfile::TempDir =
        tempfile::tempdir().expect("create the calling test's state directory");
}

/// The binary under test. Assert on it through `assert_cmd::prelude`'s
/// `.assert()`, or spawn it and hold the child.
///
/// The startup update check is opted out: a fixture spawning the real binary
/// reaches GitHub over the network on every human-channel run otherwise, which
/// is no part of what any of these pins claims.
///
/// `CFGD_STATE_DIR` names the calling test's own directory ([`state_dir`]).
/// Every test process in a run shares one `HOME`, so a `cfgd` resolving the
/// default state directory opens the same `state.db` as every other test
/// running beside it. A `--state-dir` or `CFGD_STATE_DIR` the test sets itself
/// still wins.
#[allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x
pub fn cfgd_bin() -> Result<Command, CargoError> {
    let mut cmd = Command::cargo_bin("cfgd")?;
    cmd.env("CFGD_NO_UPDATE_CHECK", "1");
    cmd.env("CFGD_STATE_DIR", state_dir());
    Ok(cmd)
}

/// The state directory [`cfgd_bin`] hands the calling test's invocations.
pub fn state_dir() -> PathBuf {
    STATE_DIR.with(|dir| dir.path().to_path_buf())
}

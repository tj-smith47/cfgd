//! The one constructor every real-binary test in this crate spawns `cfgd`
//! through.

use std::process::Command;

use assert_cmd::cargo::{CargoError, CommandCargoExt};

mod isolated_env;
use isolated_env::{ISOLATED_ENV, REMOVED_ENV, WORKING_DIR};

thread_local! {
    // libtest runs every test on a thread of its own, so this is one scratch
    // tree per test: shared by each invocation the test makes, and deleted
    // when the test's thread exits.
    static ROOT: tempfile::TempDir = {
        let root = tempfile::tempdir().expect("create the calling test's directory");
        for (_, path) in ISOLATED_ENV {
            std::fs::create_dir_all(root.path().join(path))
                .expect("create the calling test's isolated directories");
        }
        std::fs::create_dir_all(root.path().join(WORKING_DIR))
            .expect("create the calling test's working directory");
        root
    };
}

/// The binary under test. Assert on it through `assert_cmd::prelude`'s
/// `.assert()`, or spawn it and hold the child.
///
/// The startup update check is opted out: a fixture spawning the real binary
/// reaches GitHub over the network on every human-channel run otherwise, which
/// is no part of what any of these pins claims.
///
/// Every `CFGD_*` variable the test process inherited is removed, and so is
/// every variable in `REMOVED_ENV`, then every
/// variable in `ISOLATED_ENV` and the working directory point into the
/// calling test's own directory. Every test process in a run shares one
/// `HOME` and starts in the crate's checkout, so a `cfgd` left to resolve its
/// defaults opens the same `state.db` as every test running beside it, reads
/// the developer's own config, and resolves project scope to the checkout; a
/// developer's exported `CFGD_*` would reach every spawn the same way. A
/// variable, `--state-dir` or working directory the test sets itself after
/// this still wins.
#[allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x
pub fn cfgd_bin() -> Result<Command, CargoError> {
    let mut cmd = Command::cargo_bin("cfgd")?;
    // Windows reads environment names case-insensitively, so `cfgd_yes`
    // there is `CFGD_YES`.
    for (var, _) in std::env::vars_os() {
        if var
            .as_encoded_bytes()
            .get(..5)
            .is_some_and(|p| p.eq_ignore_ascii_case(b"CFGD_"))
        {
            cmd.env_remove(var);
        }
    }
    for var in REMOVED_ENV {
        cmd.env_remove(var);
    }
    cmd.env("CFGD_NO_UPDATE_CHECK", "1");
    ROOT.with(|root| {
        for (var, path) in ISOLATED_ENV {
            cmd.env(var, root.path().join(path));
        }
        cmd.current_dir(root.path().join(WORKING_DIR));
    });
    Ok(cmd)
}

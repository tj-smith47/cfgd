#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

//! Exit-code regression test for `cfgd apply`.
//!
//! A partial or total apply failure must surface as a nonzero exit
//! (`ExitCode::ApplyFailed` == 7) so CI `&&` chains and the daemon don't
//! treat a broken apply as success. A fully-successful apply must exit 0.
//!
//! These run the real binary via `assert_cmd` because `cmd_apply` ends in
//! `std::process::exit` on failure — calling it in-process would kill the
//! test harness.

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

/// Write a config + profile with two managed file actions: one whose target
/// directory is normal (succeeds), and one whose target's parent is a regular
/// file (so the write hits ENOTDIR at apply time). One action succeeds, one
/// fails — a partial apply.
fn partial_failure_config(dir: &Path) {
    let files_dir = dir.join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("hello.txt"), "hello world").unwrap();
    std::fs::write(files_dir.join("world.txt"), "second").unwrap();

    let target_ok = dir.join("out").join("hello.txt");
    // Parent of target_fail is a regular FILE, so create_dir_all fails.
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, "i am a file, not a dir").unwrap();
    let target_fail = blocker.join("world.txt");

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n      - source: files/world.txt\n        target: {}\n        strategy: Copy\n",
        target_ok.display(),
        target_fail.display(),
    );
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(dir.join("cfgd.yaml"), config).unwrap();
}

/// Write a config + profile with a single managed file action whose target's
/// parent is a regular file (so the write hits ENOTDIR at apply time). The sole
/// action fails — a total apply failure.
fn total_failure_config(dir: &Path) {
    let files_dir = dir.join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("world.txt"), "second").unwrap();

    let blocker = dir.join("blocker");
    std::fs::write(&blocker, "i am a file, not a dir").unwrap();
    let target_fail = blocker.join("world.txt");

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/world.txt\n        target: {}\n        strategy: Copy\n",
        target_fail.display(),
    );
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(dir.join("cfgd.yaml"), config).unwrap();
}

/// Write a config + profile with a single managed file action that succeeds.
fn success_config(dir: &Path) {
    let files_dir = dir.join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("hello.txt"), "hello world").unwrap();

    let target = dir.join("out").join("hello.txt");
    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n",
        target.display(),
    );
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(dir.join("cfgd.yaml"), config).unwrap();
}

#[test]
fn apply_partial_failure_exits_with_apply_failed_code() {
    let config_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    partial_failure_config(config_tmp.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--yes"])
        .arg("--config")
        .arg(config_tmp.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_tmp.path())
        .assert()
        .code(7)
        .stderr(predicate::str::contains("action(s) failed"));
}

#[test]
fn apply_total_failure_exits_with_apply_failed_code() {
    let config_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    total_failure_config(config_tmp.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--yes"])
        .arg("--config")
        .arg(config_tmp.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_tmp.path())
        .assert()
        .code(7)
        .stderr(predicate::str::contains("action(s) failed"));
}

#[test]
fn apply_full_success_exits_zero() {
    let config_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    success_config(config_tmp.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--yes"])
        .arg("--config")
        .arg(config_tmp.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_tmp.path())
        .assert()
        .success();
}

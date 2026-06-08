#![cfg(unix)]
#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

//! Signal-abort regression test for `cfgd apply`.
//!
//! SIGINT during an apply is a cooperative cancellation: the in-flight atomic
//! action finishes, the reconciler stops before the next one (no torn file),
//! the apply-lock releases via RAII Drop, an `Aborted` run is journaled, and
//! the process exits 130 (128 + SIGINT). A second apply must then succeed,
//! proving the lock was freed.
//!
//! Runs the real binary as a child process so a real SIGINT can be delivered;
//! `cmd_apply` ends in `std::process::exit`, so an in-process call can't be
//! signalled without killing the harness.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;

/// A profile whose `preApply` script sleeps, so the apply is reliably in-flight
/// when we deliver the signal, plus a managed file action whose target must NOT
/// be written once the abort takes effect.
fn sleeping_apply_config(dir: &Path) -> std::path::PathBuf {
    let files_dir = dir.join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("hello.txt"), "hello world").unwrap();

    let target = dir.join("out").join("hello.txt");
    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  scripts:\n    preApply:\n      - run: \"sleep 5\"\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n",
        target.display(),
    );
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(dir.join("cfgd.yaml"), config).unwrap();
    target
}

fn send_sigint(pid: u32) {
    // SAFETY: `kill(2)` with a valid PID and SIGINT has no memory effects.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGINT);
    }
}

#[test]
fn apply_sigint_aborts_cleanly_releases_lock_and_exits_130() {
    let config_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    let target = sleeping_apply_config(config_tmp.path());

    let mut child = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--yes"])
        .arg("--config")
        .arg(config_tmp.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_tmp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cfgd apply");

    // Give the child time to acquire the lock and enter the sleeping preApply
    // script before delivering the signal.
    std::thread::sleep(Duration::from_millis(1500));
    send_sigint(child.id());

    // Wait (bounded) for the child to exit cooperatively.
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        match child.try_wait().unwrap() {
            Some(s) => break s,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("cfgd apply did not exit after SIGINT within deadline");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr);
    }

    // Exit code 130 = 128 + SIGINT.
    assert_eq!(
        status.code(),
        Some(130),
        "SIGINT must yield exit 130; stderr:\n{stderr}"
    );

    // Honest abort message surfaced to the user.
    assert!(
        stderr.contains("apply aborted by signal"),
        "abort message missing; stderr:\n{stderr}"
    );

    // The file action ran AFTER the sleeping preApply, so the abort stops before
    // it: target must not exist, and no torn temp file is left behind.
    assert!(
        !target.exists(),
        "target must not be written after a cooperative abort"
    );

    // The lock must have been released via Drop: a second apply succeeds rather
    // than failing with ApplyLockHeld. Use a non-sleeping config so it finishes.
    let config_tmp2 = tempfile::tempdir().unwrap();
    let target2 = {
        let files_dir = config_tmp2.path().join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        std::fs::write(files_dir.join("hello.txt"), "hi").unwrap();
        let tgt = config_tmp2.path().join("out").join("hello.txt");
        let profile = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n",
            tgt.display(),
        );
        let profiles_dir = config_tmp2.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();
        std::fs::write(
            config_tmp2.path().join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n",
        )
        .unwrap();
        tgt
    };

    let second = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--yes"])
        .arg("--config")
        .arg(config_tmp2.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_tmp.path())
        .output()
        .expect("second apply");
    assert!(
        second.status.success(),
        "second apply must succeed (lock freed); stderr:\n{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(target2.exists(), "second apply must write its target");
}

#[test]
fn apply_second_sigint_force_quits_via_default_disposition() {
    use std::os::unix::process::ExitStatusExt;

    let config_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    let _target = sleeping_apply_config(config_tmp.path());

    let mut child = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--yes"])
        .arg("--config")
        .arg(config_tmp.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_tmp.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cfgd apply");

    // Let the child enter the long preApply sleep, then deliver TWO signals.
    // The first requests cooperative cancellation (deferred until the in-flight
    // sleep finishes — 5s away); the second must force-quit immediately via the
    // default disposition, long before the sleep ends.
    std::thread::sleep(Duration::from_millis(1500));
    send_sigint(child.id());
    std::thread::sleep(Duration::from_millis(300));
    send_sigint(child.id());

    // It must die well before the 5s sleep would have completed.
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait().unwrap() {
            Some(s) => break s,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("second SIGINT did not force-quit before deadline");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    // Force-quit takes the default disposition: terminated BY the signal, not a
    // graceful exit code.
    assert_eq!(
        status.signal(),
        Some(libc::SIGINT),
        "second SIGINT must terminate via default disposition (killed by signal)"
    );
    assert_eq!(
        status.code(),
        None,
        "a signal-terminated process has no graceful exit code"
    );
}

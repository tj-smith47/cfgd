#![cfg(unix)]
#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

//! Exit-code contract for the two drift surfaces that take `--exit-code`.
//!
//! An erroring check (a system configurator whose drift probe itself fails)
//! is reported as its own row and escalates the exit to `ExitCode::Error`
//! (1) on BOTH surfaces, outranking `DriftDetected` (5): exiting 5 — or 0 —
//! over a probe that never answered reports a verdict the scan never
//! reached. Plain drift with every check answered still exits 5.
//!
//! These run the real binary via `assert_cmd` because both commands end in
//! `std::process::exit` — calling them in-process would kill the harness.

use std::path::Path;

use assert_cmd::Command;

/// A gpg stand-in that always fails, so the gpgKeys configurator's own
/// keyring probe errors (gpg exit codes other than 0/2 are probe errors).
fn failing_gpg(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let shim = dir.join("gpg-fails");
    std::fs::write(&shim, "#!/bin/sh\necho 'keyring unavailable' >&2\nexit 1\n").unwrap();
    let mut perms = std::fs::metadata(&shim).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).unwrap();
    shim
}

/// Config + profile demanding one gpg key (the check that will error) and,
/// when `tampered_file` is set, one managed file whose target holds other
/// bytes (ordinary drift).
fn write_config(dir: &Path, with_gpg_check: bool, tampered_file: bool) {
    let mut spec = String::new();
    if with_gpg_check {
        spec.push_str(
            "  system:\n    gpgKeys:\n      - name: sig\n        realName: Test User\n        email: sig@example.com\n",
        );
    }
    if tampered_file {
        let files_dir = dir.join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        std::fs::write(files_dir.join("managed.txt"), "desired\n").unwrap();
        let target = dir.join("deployed.txt");
        std::fs::write(&target, "tampered\n").unwrap();
        spec.push_str(&format!(
            "  files:\n    managed:\n      - source: files/managed.txt\n        target: {}\n        strategy: Copy\n",
            target.display()
        ));
    }
    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n{spec}"
    );
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n",
    )
    .unwrap();
}

/// One surface's run: the binary with its own throwaway HOME, the failing
/// gpg seam, and the fixture config/state dirs.
fn run(
    args: &[&str],
    config: &Path,
    state: &Path,
    home: &Path,
    gpg: Option<&Path>,
) -> std::process::Output {
    let mut cmd = Command::cargo_bin("cfgd").unwrap();
    cmd.args(args)
        .arg("--config")
        .arg(config.join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state)
        .env("HOME", home);
    if let Some(gpg) = gpg {
        cmd.env("CFGD_GPG_BIN", gpg);
    }
    cmd.output().unwrap()
}

#[test]
fn an_erroring_check_is_reported_and_escalates_on_every_exit_code_surface() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_config(config_tmp.path(), true, false);
    let gpg = failing_gpg(config_tmp.path());

    for args in [
        &["status", "--scan", "--exit-code"][..],
        &["diff", "--exit-code"][..],
    ] {
        let state_tmp = tempfile::tempdir().unwrap();
        let out = run(
            args,
            config_tmp.path(),
            state_tmp.path(),
            home_tmp.path(),
            Some(&gpg),
        );
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "cfgd {args:?}: a check that could not run outranks DriftDetected, got: {text}"
        );
        assert!(
            text.contains("gpgKeys") && text.contains("error checking drift"),
            "cfgd {args:?}: the failed check renders as its own row, got: {text}"
        );
    }
}

#[test]
fn drift_with_every_check_answered_still_exits_drift_detected() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_config(config_tmp.path(), false, true);

    for args in [
        &["status", "--scan", "--exit-code"][..],
        &["diff", "--exit-code"][..],
    ] {
        let state_tmp = tempfile::tempdir().unwrap();
        let out = run(
            args,
            config_tmp.path(),
            state_tmp.path(),
            home_tmp.path(),
            None,
        );
        assert_eq!(
            out.status.code(),
            Some(5),
            "cfgd {args:?}: plain drift keeps the DriftDetected exit, got: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

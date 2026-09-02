#![cfg(unix)]
#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation

//! Exit-code contract for every drift surface that takes `--exit-code`
//! (`diff`, `status`, `verify`, plus the `--module` flag-scoped variants of
//! the first two — `every_exit_code_surface_reports_an_erroring_check`
//! walks the clap population).
//!
//! An erroring check (a system configurator whose drift probe itself fails)
//! is reported as its own row and escalates the exit to `ExitCode::Error`
//! (1) on EVERY surface, outranking `DriftDetected` (5): exiting 5 — or 0 —
//! over a probe that never answered reports a verdict the scan never
//! reached. Plain drift with every check answered still exits 5.
//!
//! These run the real binary via `assert_cmd` because the commands end in
//! `std::process::exit` — calling them in-process would kill the harness.

use std::path::Path;

use assert_cmd::Command;

/// Every surface taking `--exit-code`, each spelled as the argv that arms it.
const EXIT_CODE_SURFACES: [&[&str]; 3] = [
    &["status", "--scan", "--exit-code"],
    &["diff", "--exit-code"],
    &["verify", "--exit-code"],
];

/// The flag-scoped variants of the same contract: `--module` narrows `status`
/// and `diff` to one module's chain, where the module-scoped env probe is the
/// check that can error (the primary managed env file exists but cannot be
/// read, so every env verdict of the scan is unknown). `verify --module`
/// deliberately evaluates no env half — a scoped run's composition is
/// module-only config (`cli/verify.rs`) — so it has no erroring-check cell.
const SCOPED_EXIT_CODE_SURFACES: [&[&str]; 2] = [
    &["status", "--module", "envmod", "--scan", "--exit-code"],
    &["diff", "--module", "envmod", "--exit-code"],
];

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

    for args in EXIT_CODE_SURFACES {
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
            "cfgd {args:?}: a check that could not run exits Error, got: {text}"
        );
        assert!(
            text.contains("gpgKeys") && text.contains("error checking drift"),
            "cfgd {args:?}: the failed check renders as its own row, got: {text}"
        );
    }
}

/// The cell that can tell `1` from `5`: with BOTH a failing check and real
/// drift on the machine, every surface must pick `Error` — the unknown
/// outranks the known. The error-only sibling above cannot distinguish the
/// two (any ordering exits 1 there), so this is the precedence proof.
#[test]
fn an_erroring_check_outranks_real_drift_on_every_exit_code_surface() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_config(config_tmp.path(), true, true);
    let gpg = failing_gpg(config_tmp.path());

    for args in EXIT_CODE_SURFACES {
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
            text.contains("error checking drift"),
            "cfgd {args:?}: the failed check still renders beside the drift, got: {text}"
        );
    }
}

/// An `apk` stand-in: it OFFERS `demo` at 3.0.0 (so the declaration's
/// `minVersion` resolves onto apk) while its installed listing carries no
/// versions at all — apk's real listing format, which is why apk has no
/// `list_with_versions` override. A pinned package this manager holds can
/// therefore be neither met nor missed, which is the check-error case.
fn versionless_apk(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let shim = dir.join("apk-versionless");
    std::fs::write(
        &shim,
        "#!/bin/sh\ncase \"$1\" in\n  list) echo 'demo-3.0.0-r0 x86_64 {demo} (MIT) [installed]' ;;\n  policy) printf 'demo policy:\\n  3.0.0:\\n    https://example.invalid/alpine/main\\n' ;;\nesac\nexit 0\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&shim).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).unwrap();
    shim
}

/// One module pinning a package's version onto the versionless manager above,
/// and a profile that resolves it.
fn write_pinned_package_config(dir: &Path, manager: &str) {
    let module_dir = dir.join("modules").join("pinned");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(
        module_dir.join("module.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: pinned\nspec:\n  packages:\n    - name: demo\n      minVersion: \"2\"\n      prefer: [{manager}]\n"
        ),
    )
    .unwrap();
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("tiny.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  modules:\n    - pinned\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n",
    )
    .unwrap();
}

/// The version half of the same contract: a package the machine HOLDS still
/// fails its check when the declaration pins a floor and the manager cannot
/// state what is installed. Presence alone would exit 0 here.
#[test]
fn a_pinned_package_whose_version_cannot_be_read_escalates_on_every_exit_code_surface() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_pinned_package_config(config_tmp.path(), "apk");
    let apk = versionless_apk(config_tmp.path());

    for args in EXIT_CODE_SURFACES {
        let state_tmp = tempfile::tempdir().unwrap();
        let mut cmd = Command::cargo_bin("cfgd").unwrap();
        let out = cmd
            .args(args)
            .arg("--config")
            .arg(config_tmp.path().join("cfgd.yaml"))
            .arg("--state-dir")
            .arg(state_tmp.path())
            .env("HOME", home_tmp.path())
            .env("CFGD_APK_BIN", &apk)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "cfgd {args:?}: an unreadable installed version exits Error, got: {text}"
        );
        assert!(
            text.contains("apk:demo") && text.contains("error checking drift"),
            "cfgd {args:?}: the unanswerable version check renders as its own row, got: {text}"
        );
    }
}

/// A `dnf`/`rpm` pair that OFFERS `demo` at 3.0.0 and reports 1.0.0 installed
/// — a machine holding a package whose declared floor it no longer meets.
fn below_floor_dnf(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let write = |name: &str, body: &str| {
        let shim = dir.join(name);
        std::fs::write(&shim, body).unwrap();
        let mut perms = std::fs::metadata(&shim).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shim, perms).unwrap();
        shim
    };
    let dnf = write(
        "dnf-offers-3",
        "#!/bin/sh\ncase \"$1\" in\n  info) printf 'Name : demo\\nVersion : 3.0.0\\n' ;;\n  list) echo 'demo.x86_64  1.0.0-1  @main' ;;\nesac\nexit 0\n",
    );
    let rpm = write(
        "rpm-holds-1",
        "#!/bin/sh\nprintf 'demo\\t1.0.0\\n'\nexit 0\n",
    );
    (dnf, rpm)
}

/// The version half's DRIFT cell: every check answered, one of them below its
/// declared floor, so every surface exits `DriftDetected` and names both
/// operands. Presence alone would exit 0 — the package IS installed.
#[test]
fn a_pinned_package_below_its_floor_exits_drift_detected_on_every_surface() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_pinned_package_config(config_tmp.path(), "dnf");
    let (dnf, rpm) = below_floor_dnf(config_tmp.path());

    for args in EXIT_CODE_SURFACES {
        let state_tmp = tempfile::tempdir().unwrap();
        let mut cmd = Command::cargo_bin("cfgd").unwrap();
        let out = cmd
            .args(args)
            .arg("--config")
            .arg(config_tmp.path().join("cfgd.yaml"))
            .arg("--state-dir")
            .arg(state_tmp.path())
            .env("HOME", home_tmp.path())
            .env("CFGD_DNF_BIN", &dnf)
            .env("CFGD_RPM_BIN", &rpm)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(5),
            "cfgd {args:?}: an installed package below its floor is drift, got: {text}"
        );
        // Each surface states the same finding in its own register: `status`
        // summarises the recorded operands as their terse cause, while `diff`
        // and `verify` print the pair. Both are the ONE finding, named by the
        // ONE id.
        assert!(
            text.contains("dnf:demo")
                && (text.contains("version mismatch")
                    || (text.contains("want: 2") && text.contains("have: 1.0.0"))),
            "cfgd {args:?}: the version finding names its package and its cause, got: {text}"
        );
    }
}

/// The scoped variants of the two version cells above. A `--module` surface
/// RESOLVES the rows it re-checks, so it must evaluate the same floors: one
/// that checked presence alone would both miss the drift and mark a standing
/// version row healed while the machine is still below its floor.
const SCOPED_PINNED_SURFACES: [&[&str]; 2] = [
    &["status", "--module", "pinned", "--scan", "--exit-code"],
    &["diff", "--module", "pinned", "--exit-code"],
];

#[test]
fn a_pinned_package_below_its_floor_is_drift_on_both_scoped_surfaces() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_pinned_package_config(config_tmp.path(), "dnf");
    let (dnf, rpm) = below_floor_dnf(config_tmp.path());

    for args in SCOPED_PINNED_SURFACES {
        let state_tmp = tempfile::tempdir().unwrap();
        let mut cmd = Command::cargo_bin("cfgd").unwrap();
        let out = cmd
            .args(args)
            .arg("--config")
            .arg(config_tmp.path().join("cfgd.yaml"))
            .arg("--state-dir")
            .arg(state_tmp.path())
            .env("HOME", home_tmp.path())
            .env("CFGD_DNF_BIN", &dnf)
            .env("CFGD_RPM_BIN", &rpm)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(5),
            "cfgd {args:?}: a scoped surface checks the floors it resolves, got: {text}"
        );
        assert!(
            text.contains("demo"),
            "cfgd {args:?}: the finding names its package, got: {text}"
        );
    }
}

#[test]
fn a_pinned_package_whose_version_cannot_be_read_escalates_on_both_scoped_surfaces() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_pinned_package_config(config_tmp.path(), "apk");
    let apk = versionless_apk(config_tmp.path());

    for args in SCOPED_PINNED_SURFACES {
        let state_tmp = tempfile::tempdir().unwrap();
        let mut cmd = Command::cargo_bin("cfgd").unwrap();
        let out = cmd
            .args(args)
            .arg("--config")
            .arg(config_tmp.path().join("cfgd.yaml"))
            .arg("--state-dir")
            .arg(state_tmp.path())
            .env("HOME", home_tmp.path())
            .env("CFGD_APK_BIN", &apk)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "cfgd {args:?}: an unanswerable floor exits Error on a scoped surface too, got: {text}"
        );
        assert!(
            text.contains("error checking drift"),
            "cfgd {args:?}: the unanswerable check renders as its own row, got: {text}"
        );
    }
}

/// The erasure guard: a scoped run resolves every key it re-checked and did not
/// re-find. A standing version-drift row therefore survives a `--module` run
/// over the very module that declared it — the machine is still below the
/// floor, and the scoped surface says so rather than healing it.
#[test]
fn a_scoped_run_does_not_heal_a_version_row_the_machine_still_holds() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    write_pinned_package_config(config_tmp.path(), "dnf");
    let (dnf, rpm) = below_floor_dnf(config_tmp.path());

    let run_one = |args: &[&str]| {
        let mut cmd = Command::cargo_bin("cfgd").unwrap();
        let out = cmd
            .args(args)
            .arg("--config")
            .arg(config_tmp.path().join("cfgd.yaml"))
            .arg("--state-dir")
            .arg(state_tmp.path())
            .env("HOME", home_tmp.path())
            .env("CFGD_DNF_BIN", &dnf)
            .env("CFGD_RPM_BIN", &rpm)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code(), text)
    };

    let (code, text) = run_one(&["diff", "--exit-code"]);
    assert_eq!(code, Some(5), "the full walk records the row: {text}");
    let (code, text) = run_one(&["diff", "--module", "pinned", "--exit-code"]);
    assert_eq!(code, Some(5), "the scoped run re-finds it: {text}");
    // The RECORDED read: no `--scan`, so this reports what the two runs above
    // left in the store.
    let (code, text) = run_one(&["status", "--exit-code"]);
    assert_eq!(
        code,
        Some(5),
        "the recorded row survived the scoped run: {text}"
    );
}

/// One module declaring an env var, so a scoped run's merged env is
/// non-empty and the env probe actually runs.
fn write_module_config(dir: &Path) {
    let module_dir = dir.join("modules").join("envmod");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(
        module_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: envmod\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n",
    )
    .unwrap();
    write_config(dir, false, false);
}

#[test]
fn a_scoped_env_probe_failure_is_reported_and_escalates_on_both_scoped_surfaces() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_module_config(config_tmp.path());
    // The primary managed env file EXISTS but cannot be read — a directory
    // at its path fails `read_to_string` with EISDIR on every host, root
    // included (a permission bit would not).
    std::fs::create_dir(home_tmp.path().join(".cfgd.env")).unwrap();

    for args in SCOPED_EXIT_CODE_SURFACES {
        let state_tmp = tempfile::tempdir().unwrap();
        let out = run(
            args,
            config_tmp.path(),
            state_tmp.path(),
            home_tmp.path(),
            None,
        );
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "cfgd {args:?}: a probe that could not run exits Error on the scoped surface too, got: {text}"
        );
        assert!(
            text.contains("error checking drift") && text.contains(".cfgd.env"),
            "cfgd {args:?}: the failed probe renders as its own row naming the file, got: {text}"
        );
    }
}

/// A recorded row the scan KEEPS standing is rendered and priced by the scan
/// that kept it.
///
/// The full check evaluates eight resource types; a `script` row is one no
/// pass of it can re-find, so the recorder deliberately leaves it unresolved.
/// Rendering only what the scan re-found made `status` and `status --scan`
/// disagree about the same untouched machine — Drifted, then Synced, then
/// Drifted — and let `status --scan --exit-code` wave a CI gate through over
/// drift the very same invocation had just re-affirmed as standing.
#[test]
fn a_row_the_scan_keeps_standing_is_rendered_and_priced_by_that_scan() {
    use cfgd_core::state::StateStore;

    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    write_config(config_tmp.path(), false, false);

    // Recorded exactly as a daemon tick records a planned script action.
    {
        let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
        state
            .record_drift("script", "echo hook", None, Some("drift detected"), "local")
            .unwrap();
    }

    let out = run(
        &["status", "--scan"],
        config_tmp.path(),
        state_tmp.path(),
        home_tmp.path(),
        None,
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("echo hook"),
        "the scan renders the row it left standing: {text}"
    );

    let out = run(
        &["status", "--scan", "--exit-code"],
        config_tmp.path(),
        state_tmp.path(),
        home_tmp.path(),
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(5),
        "the exit gate prices the standing row too: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Still standing afterwards: nothing about this scan healed it, which is
    // what makes rendering it the honest verdict.
    let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
    assert_eq!(
        state
            .unresolved_drift()
            .unwrap()
            .into_iter()
            .map(|e| (e.resource_type, e.resource_id))
            .collect::<Vec<_>>(),
        vec![("script".to_string(), "echo hook".to_string())],
    );
}

#[test]
fn drift_with_every_check_answered_still_exits_drift_detected() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_config(config_tmp.path(), false, true);

    for args in EXIT_CODE_SURFACES {
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

/// A `brew` stand-in for a converged machine: the formula is installed at
/// `0.12.5_1` — Homebrew's `<upstream>_<revision>` grammar — and offered at
/// `0.12.5`, so a declared `minVersion: 0.11` is met.
fn converged_brew(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let shim = dir.join("brew-holds-neovim");
    std::fs::write(
        &shim,
        "#!/bin/sh\ncase \"$*\" in\n  *--versions*) echo 'neovim 0.12.5_1' ;;\n  *info*) printf '{\"formulae\":[{\"versions\":{\"stable\":\"0.12.5\"}}]}' ;;\n  *list*) echo 'neovim' ;;\n  *) echo 'Homebrew 4.0.0' ;;\nesac\nexit 0\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&shim).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).unwrap();
    shim
}

/// One module pinning a floor the installed brew formula clears.
fn write_brew_pinned_config(dir: &Path) {
    let module_dir = dir.join("modules").join("pinned");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(
        module_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: pinned\nspec:\n  packages:\n    - name: neovim\n      minVersion: \"0.11\"\n      prefer: [brew]\n",
    )
    .unwrap();
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("tiny.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  modules:\n    - pinned\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n",
    )
    .unwrap();
}

/// The healed end state of the version class: a manager whose own grammar
/// carries a packaging suffix answers its floor instead of erroring it, and
/// the same comparator keeps the package out of the plan. A machine holding
/// `neovim 0.12.5_1` against `minVersion: 0.11` is converged on every surface
/// — no check error, no drift, and nothing left to install.
#[test]
fn a_brew_formula_clearing_its_floor_is_converged_on_every_surface() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_brew_pinned_config(config_tmp.path());
    let brew = converged_brew(config_tmp.path());

    for args in EXIT_CODE_SURFACES {
        let state_tmp = tempfile::tempdir().unwrap();
        let mut cmd = Command::cargo_bin("cfgd").unwrap();
        let out = cmd
            .args(args)
            .arg("--config")
            .arg(config_tmp.path().join("cfgd.yaml"))
            .arg("--state-dir")
            .arg(state_tmp.path())
            .env("HOME", home_tmp.path())
            .env("CFGD_BREW_BIN", &brew)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "cfgd {args:?}: a met floor is neither drift nor an erroring check, got: {text}"
        );
        assert!(
            !text.contains("error checking drift"),
            "cfgd {args:?}: the manager's own grammar is comparable, got: {text}"
        );
    }

    // The same comparator decides the plan: a package the machine holds above
    // its floor is elided, so a converged machine plans nothing at all.
    let state_tmp = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("cfgd").unwrap();
    let out = cmd
        .args(["plan"])
        .arg("--config")
        .arg(config_tmp.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_tmp.path())
        .env("HOME", home_tmp.path())
        .env("CFGD_BREW_BIN", &brew)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !text.contains("brew install neovim"),
        "a converged formula is never re-planned: {text}"
    );
    assert!(
        text.contains("Nothing to do"),
        "a converged machine reaches the up-to-date verdict: {text}"
    );
}

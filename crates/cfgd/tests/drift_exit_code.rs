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
use cfgd_core::test_helpers::{ShimArm, write_tool_shim};

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

/// A whole-module standing row's seeded operand, for a test asserting the
/// row itself rendered. Never the literal `drift detected`: that word is
/// also `drift_terse_cause`'s fallback for a row with NO operands at all,
/// and a substring of the clean "No drift detected" verdict, so it cannot
/// tell "the row rendered" apart from "the row was empty" or "the renderer
/// fell back to the clean verdict".
const STANDING_ROW_MARKER: &str = "standing-row-marker-7f2c";

/// A gpg stand-in that always fails, so the gpgKeys configurator's own
/// keyring probe errors (gpg exit codes other than 0/2 are probe errors).
fn failing_gpg(dir: &Path) -> std::path::PathBuf {
    write_tool_shim(
        dir,
        "gpg-fails",
        &[ShimArm::always("", "keyring unavailable\n", 1)],
    )
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
        .env("HOME", home)
        // Windows resolves `~` from USERPROFILE first, so a child left holding
        // the invoking account's profile would write to the real home.
        .env("USERPROFILE", home)
        // `directories` reads Windows' known folders rather than the env, so
        // nothing but this seam keeps a child's module cache out of the real profile.
        .env("CFGD_CACHE_DIR", home.join("cache"));
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
    write_tool_shim(
        dir,
        "apk-versionless",
        &[
            ShimArm::on("list", "demo-3.0.0-r0 x86_64 {demo} (MIT) [installed]\n"),
            ShimArm::on(
                "policy",
                "demo policy:\n  3.0.0:\n    https://example.invalid/alpine/main\n",
            ),
            ShimArm::always("", "", 0),
        ],
    )
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
            .env("USERPROFILE", home_tmp.path())
            .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
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
///
/// The three cells built on this pair are the file's only `#[cfg(unix)]` ones,
/// and the gate is the FIXTURE's, not the contract's: dnf's installed listing
/// is `rpm --query --all --queryformat "%{NAME}\t%{VERSION}\n"`, whose
/// trailing newline `std::process::Command` refuses to pass to a `.cmd` — a
/// newline truncates a `cmd.exe` command line, and a `.cmd` is what the
/// Windows arm of `write_tool_shim` has to be. (The `%` is not the problem;
/// std neutralizes those.) The distro grammar this pair carries — an
/// `<epoch>:<upstream>-<revision>` version compared on its upstream part — is
/// reachable nowhere else, so the trio stays here rather than moving to a
/// manager Windows can shim. What Windows loses is only the GRAMMAR: each of
/// the three cells has a brew twin proving the same outcome there — the
/// unscoped walk in
/// `a_brew_formula_below_its_floor_exits_drift_detected_on_every_surface`, the
/// scoped pass in `a_brew_formula_below_its_floor_is_drift_on_both_scoped_surfaces`,
/// and the non-heal in
/// `a_scoped_brew_run_does_not_heal_a_version_row_the_machine_still_holds`.
#[cfg(unix)]
fn below_floor_dnf(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let dnf = write_tool_shim(
        dir,
        "dnf-offers-3",
        &[
            ShimArm::on("info", "Name : demo\nVersion : 3.0.0\n"),
            ShimArm::on("list", "demo.x86_64  1.0.0-1  @main\n"),
            ShimArm::always("", "", 0),
        ],
    );
    let rpm = write_tool_shim(
        dir,
        "rpm-holds-1",
        &[ShimArm::always("demo\t1.0.0\n", "", 0)],
    );
    (dnf, rpm)
}

/// The version half's DRIFT cell: every check answered, one of them below its
/// declared floor, so every surface exits `DriftDetected` and names both
/// operands. Presence alone would exit 0 — the package IS installed.
#[test]
#[cfg(unix)] // see `below_floor_dnf`: rpm's newline-bearing argv is unshimmable on Windows
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
            .env("USERPROFILE", home_tmp.path())
            .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
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
#[cfg(unix)] // see `below_floor_dnf`: rpm's newline-bearing argv is unshimmable on Windows
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
            .env("USERPROFILE", home_tmp.path())
            .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
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
            .env("USERPROFILE", home_tmp.path())
            .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
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
#[cfg(unix)] // see `below_floor_dnf`: rpm's newline-bearing argv is unshimmable on Windows
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
            .env("USERPROFILE", home_tmp.path())
            .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
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

/// A module declaring no env, files or packages, so a scoped run's `checked`
/// set is empty and a seeded legacy `("module", <name>)` row stands untouched
/// — never confused with `write_module_config`'s own genuinely-drifted
/// `EDITOR` env var, which a scoped scan re-checks and (correctly) leaves
/// recorded as real drift rather than a standing row.
fn write_bare_module_config(dir: &Path) {
    let module_dir = dir.join("modules").join("envmod");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(
        module_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: envmod\nspec: {}\n",
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
    // at its path fails `read_to_string` on every host, root included (a
    // permission bit would not).
    let env_file = cfgd_core::reconciler::primary_env_file(home_tmp.path());
    std::fs::create_dir(&env_file).unwrap();
    let env_file_name = env_file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap()
        .to_string();

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
            text.contains("error checking drift") && text.contains(&env_file_name),
            "cfgd {args:?}: the failed probe renders as its own row naming the file, got: {text}"
        );
    }
}

/// A `prefer: [script]` package entry is invisible to live drift detection
/// by design (`action_drift_rows`'s own doc), so `checked` never names it —
/// exactly the shape that broke the OLD `effective_packages`, which was
/// derived from `checked` itself and could therefore never contain a
/// package this run had not just re-checked. The scope this run's own
/// resolved chain declares must attribute the row regardless.
fn write_script_preferred_package_config(dir: &Path) {
    let module_dir = dir.join("modules").join("scripted");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(
        module_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: scripted\nspec:\n  packages:\n    - name: demo\n      prefer: [script]\n      script: echo installed\n",
    )
    .unwrap();
    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("tiny.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  modules:\n    - scripted\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n",
    )
    .unwrap();
}

#[test]
fn a_scoped_scan_renders_and_prices_a_standing_script_preferred_package_row() {
    use cfgd_core::state::StateStore;

    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    write_script_preferred_package_config(config_tmp.path());
    {
        let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
        state
            .record_drift("package", "script:demo", None, Some("to remove"), "local")
            .unwrap();
    }

    let render_args = &["diff", "--module", "scripted"];
    let out = run(
        render_args,
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
        text.contains("demo"),
        "cfgd {render_args:?}: renders the script-preferred package row it left standing, got: {text}"
    );

    let mut json_args: Vec<&str> = render_args.to_vec();
    json_args.extend(["-o", "json"]);
    let out = run(
        &json_args,
        config_tmp.path(),
        state_tmp.path(),
        home_tmp.path(),
        None,
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "cfgd {json_args:?}: expected JSON on stdout, got {}: {e}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(
        payload["standing"][0]["resourceId"],
        serde_json::json!("script:demo"),
        "cfgd {json_args:?}: standing[0].resourceId, got: {payload}"
    );

    let out = run(
        &["diff", "--module", "scripted", "--exit-code"],
        config_tmp.path(),
        state_tmp.path(),
        home_tmp.path(),
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(5),
        "the standing script-preferred package row is priced as drift, got: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An `env-var`/`alias` standing row is rendered and priced by a scoped
/// scan exactly like a `package`/`module` one — the asymmetry this closes:
/// `row_attributable_to_module` used to answer `false` for both types
/// unconditionally, so a module's own declared env var recorded standing
/// while its probe could not run (the file exists but is unreadable) was
/// silently dropped by the scan path even though the recorded-fallback path
/// attributed it correctly.
#[test]
fn a_scoped_scan_renders_and_prices_a_standing_env_var_row() {
    use cfgd_core::state::StateStore;

    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    write_module_config(config_tmp.path());
    let env_file = cfgd_core::reconciler::primary_env_file(home_tmp.path());
    std::fs::create_dir(&env_file).unwrap();
    {
        let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
        state
            .record_drift(
                "env-var",
                "EDITOR",
                Some("EDITOR=vim"),
                Some("missing or changed"),
                "local",
            )
            .unwrap();
    }

    let render_args = &["diff", "--module", "envmod"];
    let out = run(
        render_args,
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
        text.contains("EDITOR"),
        "cfgd {render_args:?}: renders the env-var row it left standing, got: {text}"
    );

    let mut json_args: Vec<&str> = render_args.to_vec();
    json_args.extend(["-o", "json"]);
    let out = run(
        &json_args,
        config_tmp.path(),
        state_tmp.path(),
        home_tmp.path(),
        None,
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "cfgd {json_args:?}: expected JSON on stdout, got {}: {e}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let standing = payload["standing"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a standing array, got: {payload}"));
    assert!(
        standing
            .iter()
            .any(|e| e["resourceId"] == serde_json::json!("EDITOR")
                && e["resourceType"] == serde_json::json!("env-var")),
        "cfgd {json_args:?}: EDITOR must appear in standing, got: {payload}"
    );
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

/// The FULL-machine axis of the standing-row contract, over every surface
/// `EXIT_CODE_SURFACES` names: a `script` row (a type none of the full scan's
/// passes can re-find) stands after any of the three and is rendered and
/// priced by all of them the same way the single-surface pin above proved
/// for `status --scan`.
#[test]
fn every_full_exit_code_surface_renders_and_prices_a_standing_row() {
    use cfgd_core::state::StateStore;

    for surface in EXIT_CODE_SURFACES {
        let config_tmp = tempfile::tempdir().unwrap();
        let home_tmp = tempfile::tempdir().unwrap();
        let state_tmp = tempfile::tempdir().unwrap();
        write_config(config_tmp.path(), false, false);
        {
            let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
            state
                .record_drift("script", "echo hook", None, Some("drift detected"), "local")
                .unwrap();
        }

        // The render half: the same argv minus its trailing `--exit-code`.
        let render_args = &surface[..surface.len() - 1];
        let out = run(
            render_args,
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
            "cfgd {render_args:?}: renders the row it left standing, got: {text}"
        );
        // The human verdict and `--exit-code`'s `5` below must read one
        // set: a run with nothing but a standing row still says so, never
        // "No drift detected" over an exit code that disagrees.
        assert!(
            !text.contains("No drift detected"),
            "cfgd {render_args:?}: verdict must agree with the exit code below, got: {text}"
        );

        // The `-o json` twin: the same row under the payload's own
        // `standing` key, not just the terminal render.
        let mut json_args: Vec<&str> = render_args.to_vec();
        json_args.extend(["-o", "json"]);
        let out = run(
            &json_args,
            config_tmp.path(),
            state_tmp.path(),
            home_tmp.path(),
            None,
        );
        let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "cfgd {json_args:?}: expected JSON on stdout, got {}: {e}",
                String::from_utf8_lossy(&out.stdout)
            )
        });
        assert_eq!(
            payload["standing"][0]["resourceId"],
            serde_json::json!("echo hook"),
            "cfgd {json_args:?}: standing[0].resourceId, got: {payload}"
        );
        assert_eq!(
            payload["standing"][0]["resourceType"],
            serde_json::json!("script"),
            "cfgd {json_args:?}: standing[0].resourceType, got: {payload}"
        );

        let out = run(
            surface,
            config_tmp.path(),
            state_tmp.path(),
            home_tmp.path(),
            None,
        );
        assert_eq!(
            out.status.code(),
            Some(5),
            "cfgd {surface:?}: prices the standing row, got: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
        assert_eq!(
            state
                .unresolved_drift()
                .unwrap()
                .into_iter()
                .map(|e| (e.resource_type, e.resource_id))
                .collect::<Vec<_>>(),
            vec![("script".to_string(), "echo hook".to_string())],
            "cfgd {surface:?}: the standing row is never healed by the scan that renders it",
        );
    }
}

/// The SCOPED (`--module`) axis of the same contract. A bare legacy
/// whole-module id (the daemon's own action spelling) is attributable to the
/// named module's chain — [`cfgd_core::reconciler::row_attributable_to_module`]
/// says so by owner alone — but a scoped scan of a module declaring no files
/// or packages never re-checks it, so it stands. `verify --module` is
/// included here even though it carries no erroring-check cell in
/// `SCOPED_EXIT_CODE_SURFACES` above: that exclusion is about the env probe
/// specifically, not about this module-id row.
#[test]
fn every_scoped_exit_code_surface_renders_and_prices_a_standing_row() {
    use cfgd_core::state::StateStore;

    const SURFACES: [&[&str]; 3] = [
        &["diff", "--module", "envmod", "--exit-code"],
        &["verify", "--module", "envmod", "--exit-code"],
        &["status", "--module", "envmod", "--scan", "--exit-code"],
    ];

    for surface in SURFACES {
        let config_tmp = tempfile::tempdir().unwrap();
        let home_tmp = tempfile::tempdir().unwrap();
        let state_tmp = tempfile::tempdir().unwrap();
        write_bare_module_config(config_tmp.path());
        {
            let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
            state
                .record_drift("module", "envmod", None, Some(STANDING_ROW_MARKER), "local")
                .unwrap();
        }

        let render_args = &surface[..surface.len() - 1];
        let out = run(
            render_args,
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
        // `envmod` alone is not proof — every scoped surface's own header
        // names the module regardless of whether the standing row rendered.
        // Neither is the literal `drift detected`: it is also the terse
        // fallback `drift_terse_cause` renders for a row with NO operands at
        // all, and a substring of the clean "No drift detected" verdict, so
        // either a genuinely empty row or a renderer that fell back to the
        // clean verdict would satisfy it too. The seeded row's own operand
        // is a marker no other rendered path can produce, so its presence
        // proves the STANDING ROW ITSELF made it onto the screen.
        assert!(
            text.contains(STANDING_ROW_MARKER),
            "cfgd {render_args:?}: renders the row it left standing, got: {text}"
        );
        // The row's own subject beside its operand: the id as the drift-row
        // renderer spells a module row, which no header spells that way.
        let subject = cfgd_core::output::drift_item_subject("module", "envmod");
        assert!(
            text.contains(&subject),
            "cfgd {render_args:?}: the standing row names its id `{subject}`, got: {text}"
        );
        assert!(
            !text.contains("No drift detected"),
            "cfgd {render_args:?}: verdict must agree with the standing row above, got: {text}"
        );

        // The `-o json` twin: the same row under the payload's own
        // `standing` key, not just the terminal render.
        let mut json_args: Vec<&str> = render_args.to_vec();
        json_args.extend(["-o", "json"]);
        let out = run(
            &json_args,
            config_tmp.path(),
            state_tmp.path(),
            home_tmp.path(),
            None,
        );
        let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "cfgd {json_args:?}: expected JSON on stdout, got {}: {e}",
                String::from_utf8_lossy(&out.stdout)
            )
        });
        assert_eq!(
            payload["standing"][0]["resourceId"],
            serde_json::json!("envmod"),
            "cfgd {json_args:?}: standing[0].resourceId, got: {payload}"
        );
        assert_eq!(
            payload["standing"][0]["resourceType"],
            serde_json::json!("module"),
            "cfgd {json_args:?}: standing[0].resourceType, got: {payload}"
        );

        let out = run(
            surface,
            config_tmp.path(),
            state_tmp.path(),
            home_tmp.path(),
            None,
        );
        assert_eq!(
            out.status.code(),
            Some(5),
            "cfgd {surface:?}: prices the standing row, got: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
        assert_eq!(
            state
                .unresolved_drift()
                .unwrap()
                .into_iter()
                .map(|e| (e.resource_type, e.resource_id))
                .collect::<Vec<_>>(),
            vec![("module".to_string(), "envmod".to_string())],
            "cfgd {surface:?}: the standing row is never healed by the scan that renders it",
        );
    }
}

/// `status --module --scan` must render and price a `<module>:script` /
/// `<module>:skip` standing row exactly like the bare whole-module id above —
/// [`cfgd_core::reconciler::module_row_owner`] reads both grammars the same
/// way, up to the first `/` or `:`, and `classify_recorded_drift_for_chain`'s
/// "module" arm must ask it rather than comparing the row's full id against
/// the bare chain names.
#[test]
fn a_module_scoped_scan_renders_and_prices_a_script_shaped_standing_row() {
    use cfgd_core::state::StateStore;

    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    write_bare_module_config(config_tmp.path());
    {
        let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
        state
            .record_drift(
                "module",
                "envmod:script",
                None,
                Some(STANDING_ROW_MARKER),
                "local",
            )
            .unwrap();
    }

    let render_args = &["status", "--module", "envmod", "--scan"];
    let out = run(
        render_args,
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
    // The seeded marker, not the literal `drift detected`: that word is also
    // the terse fallback a NO-operand row renders and a substring of the
    // clean "No drift detected" verdict — see the sibling test above.
    assert!(
        text.contains(STANDING_ROW_MARKER),
        "cfgd {render_args:?}: renders the script-shaped row it left standing, got: {text}"
    );
    assert!(
        text.contains("envmod:script"),
        "cfgd {render_args:?}: the standing row names its own `:script` id, got: {text}"
    );
    assert!(
        !text.contains("No drift detected"),
        "cfgd {render_args:?}: verdict must agree with the standing row above, got: {text}"
    );

    let mut json_args: Vec<&str> = render_args.to_vec();
    json_args.extend(["-o", "json"]);
    let out = run(
        &json_args,
        config_tmp.path(),
        state_tmp.path(),
        home_tmp.path(),
        None,
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "cfgd {json_args:?}: expected JSON on stdout, got {}: {e}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(
        payload["standing"][0]["resourceId"],
        serde_json::json!("envmod:script"),
        "cfgd {json_args:?}: standing[0].resourceId, got: {payload}"
    );

    let out = run(
        &["status", "--module", "envmod", "--scan", "--exit-code"],
        config_tmp.path(),
        state_tmp.path(),
        home_tmp.path(),
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(5),
        "cfgd status --module envmod --scan --exit-code: prices the standing row, got: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let state = StateStore::open(&state_tmp.path().join("state.db")).unwrap();
    assert_eq!(
        state
            .unresolved_drift()
            .unwrap()
            .into_iter()
            .map(|e| (e.resource_type, e.resource_id))
            .collect::<Vec<_>>(),
        vec![("module".to_string(), "envmod:script".to_string())],
        "the standing row is never healed by the scan that renders it",
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
    write_tool_shim(
        dir,
        "brew-holds-neovim",
        &[
            ShimArm::on("--versions", "neovim 0.12.5_1\n"),
            ShimArm::on(
                "info",
                "{\"formulae\":[{\"versions\":{\"stable\":\"0.12.5\"}}]}",
            ),
            ShimArm::on("list", "neovim\n"),
            ShimArm::always("Homebrew 4.0.0\n", "", 0),
        ],
    )
}

/// The same shape as [`converged_brew`] one floor lower: the formula is
/// installed at `0.10.2_1` against a declared `minVersion: 0.11`. Every argv
/// brew is asked here is plain, so this is the below-floor fixture Windows can
/// reach — see [`below_floor_dnf`] for the one it cannot.
fn below_floor_brew(dir: &Path) -> std::path::PathBuf {
    write_tool_shim(
        dir,
        "brew-holds-old-neovim",
        &[
            ShimArm::on("--versions", "neovim 0.10.2_1\n"),
            ShimArm::on(
                "info",
                "{\"formulae\":[{\"versions\":{\"stable\":\"0.12.5\"}}]}",
            ),
            ShimArm::on("list", "neovim\n"),
            ShimArm::always("Homebrew 4.0.0\n", "", 0),
        ],
    )
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
            .env("USERPROFILE", home_tmp.path())
            .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
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
        .env("USERPROFILE", home_tmp.path())
        .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
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

/// The below-floor outcome on a manager every host can shim: every check is
/// answered, one of them below its declared floor, so every surface exits
/// `DriftDetected` and names the package. Presence alone would exit 0 — the
/// formula IS installed. The dnf twin proves the same outcome over the distro
/// version grammar; this one is what keeps the outcome itself from being
/// Unix-only.
#[test]
fn a_brew_formula_below_its_floor_exits_drift_detected_on_every_surface() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_brew_pinned_config(config_tmp.path());
    let brew = below_floor_brew(config_tmp.path());

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
            .env("USERPROFILE", home_tmp.path())
            .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
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
            Some(5),
            "cfgd {args:?}: an installed formula below its floor is drift, got: {text}"
        );
        assert!(
            !text.contains("error checking drift"),
            "cfgd {args:?}: brew's own grammar reads `0.10.2_1`, so this is drift and not an unanswered check, got: {text}"
        );
        // Each surface states the one finding in its own register: `diff` and
        // `verify` print both operands, `status` the terse cause. Brew's
        // `0.10.2_1` is a version only brew's own comparator reads, so a terse
        // cause derived by re-parsing the operands would render the bare value
        // here and say nothing.
        assert!(
            text.contains("brew:neovim")
                && (text.contains("version mismatch")
                    || (text.contains("want: 0.11") && text.contains("have: 0.10.2_1"))),
            "cfgd {args:?}: the version finding names its package and its cause, got: {text}"
        );
    }
}

/// The scoped twin of the cell above, on the same every-host manager: a
/// `--module` surface resolves its own floors through
/// `cli::live_drift::scoped_version_drift`, a different pass from the full
/// walk's, so the scoped path needs its own Windows-reachable cell or the
/// outcome is proven only where the dnf fixture runs.
#[test]
fn a_brew_formula_below_its_floor_is_drift_on_both_scoped_surfaces() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    write_brew_pinned_config(config_tmp.path());
    let brew = below_floor_brew(config_tmp.path());

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
            .env("USERPROFILE", home_tmp.path())
            .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
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
            Some(5),
            "cfgd {args:?}: a scoped surface checks the floors it resolves, got: {text}"
        );
        assert!(
            !text.contains("error checking drift"),
            "cfgd {args:?}: brew's own grammar reads `0.10.2_1`, so this is drift and not an unanswered check, got: {text}"
        );
        assert!(
            text.contains("neovim"),
            "cfgd {args:?}: the finding names its package, got: {text}"
        );
    }
}

/// The erasure guard on the every-host manager: `record_scoped_scan_findings`
/// resolves every key a scoped run re-checked and did not re-find, so a
/// standing version row must survive a `--module` run over the very module
/// that declared it while the machine is still below the floor.
#[test]
fn a_scoped_brew_run_does_not_heal_a_version_row_the_machine_still_holds() {
    let config_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    write_brew_pinned_config(config_tmp.path());
    let brew = below_floor_brew(config_tmp.path());

    let run_one = |args: &[&str]| {
        let mut cmd = Command::cargo_bin("cfgd").unwrap();
        let out = cmd
            .args(args)
            .arg("--config")
            .arg(config_tmp.path().join("cfgd.yaml"))
            .arg("--state-dir")
            .arg(state_tmp.path())
            .env("HOME", home_tmp.path())
            .env("USERPROFILE", home_tmp.path())
            .env("CFGD_CACHE_DIR", home_tmp.path().join("cache"))
            .env("CFGD_BREW_BIN", &brew)
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

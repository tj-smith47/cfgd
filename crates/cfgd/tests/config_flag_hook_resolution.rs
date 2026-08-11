#![cfg(unix)]
#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

//! I3 regression: the finding's exact mandated matrix — {absolute, relative}
//! `--config` × {no args, with args} — all four resolve and run.
//!
//! Reproduces the two independent triage symptoms together, each of which
//! alone produced exit 127 on v0.8.1:
//!
//! - A relative `--config` left `config_dir` relative to the process's own
//!   cwd; a `preApply` hook's process spawns with `cwd = $HOME`
//!   (`script_default_workdir`), so a relative `run:` script resolved
//!   against the wrong directory whenever cfgd wasn't invoked from the
//!   config directory itself. `main.rs`'s `absolutize_path` sweep fixes
//!   this by canonicalizing `--config` once at the CLI boundary.
//! - ANY trailing argument in a `run:` string defeated the (then
//!   unconditional) whole-string existence test and fell through to the
//!   shell verbatim, unresolved. `resolve_run_target` fixes this by
//!   resolving only the leading token when the whole string isn't itself a
//!   file.
//!
//! Every cell here spawns the real binary (never in-process — `cmd_apply`
//! ends in `std::process::exit`) from a cwd distinct from both the config
//! directory and `$HOME`, with `$HOME` also distinct from the config
//! directory — the exact shape that reproduces exit 127 if either fix
//! regresses.

use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;

/// Marker file the hook script writes, directly under the config directory
/// (the resolution base a profile script uses is the directory holding
/// `cfgd.yaml` itself, not a `scripts/` subdirectory —
/// `scripts_apply.rs::execute_script`'s `script_dir` parameter is the
/// reconciler's own `config_dir`).
const MARKER: &str = "marker.txt";

/// Lay out `cfgd.yaml` + `profiles/tiny.yaml` + `hook.sh` under `config_dir`,
/// with a `preApply` hook whose `run:` string is `hook.sh` (no args) or
/// `hook.sh hello` (with args). The script echoes `$1` into the marker file
/// so a with-args cell can assert the trailing argument actually reached
/// argv, not merely that the script ran.
fn write_hook_config(config_dir: &Path, with_args: bool) {
    std::fs::create_dir_all(config_dir).unwrap();
    let marker = config_dir.join(MARKER);
    let script_path = config_dir.join("hook.sh");
    std::fs::write(
        &script_path,
        format!("#!/bin/sh\necho \"ran:$1\" > '{}'\n", marker.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let run_str = if with_args {
        "hook.sh hello"
    } else {
        "hook.sh"
    };
    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  scripts:\n    preApply:\n      - run: \"{run_str}\"\n"
    );
    let profiles_dir = config_dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(config_dir.join("cfgd.yaml"), config).unwrap();
}

/// Run `cfgd apply --config <config_arg>` from `cwd`, with `$HOME` pinned to
/// `home` (distinct from both `cwd` and the config directory), and assert it
/// exits 0 and the hook actually ran — the with-args cells also assert the
/// trailing argument reached the script's `$1`.
fn run_matrix_cell(
    cwd: &Path,
    home: &Path,
    config_arg: impl AsRef<Path>,
    marker_path: &Path,
    with_args: bool,
) {
    let state_tmp = tempfile::tempdir().unwrap();

    let output = Command::cargo_bin("cfgd")
        .unwrap()
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(["apply", "--yes"])
        .arg("--config")
        .arg(config_arg.as_ref())
        .arg("--state-dir")
        .arg(state_tmp.path())
        .output()
        .expect("spawn cfgd apply");

    assert!(
        output.status.success(),
        "cfgd apply exited {:?} (stdout: {}, stderr: {})",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let marker_content =
        std::fs::read_to_string(marker_path).expect("preApply hook must have written the marker");
    if with_args {
        assert!(
            marker_content.contains("ran:hello"),
            "trailing arg must reach the script's argv: {marker_content:?}"
        );
    } else {
        assert!(
            marker_content.contains("ran:"),
            "hook must have run: {marker_content:?}"
        );
    }
}

#[test]
fn apply_absolute_config_no_args_resolves_and_runs() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("cfg");
    write_hook_config(&config_dir, false);
    let cwd = root.path().join("elsewhere");
    std::fs::create_dir_all(&cwd).unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    run_matrix_cell(
        &cwd,
        &home,
        config_dir.join("cfgd.yaml"),
        &config_dir.join(MARKER),
        false,
    );
}

#[test]
fn apply_absolute_config_with_args_resolves_and_runs() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("cfg");
    write_hook_config(&config_dir, true);
    let cwd = root.path().join("elsewhere");
    std::fs::create_dir_all(&cwd).unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    run_matrix_cell(
        &cwd,
        &home,
        config_dir.join("cfgd.yaml"),
        &config_dir.join(MARKER),
        true,
    );
}

#[test]
fn apply_relative_config_no_args_resolves_and_runs() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("cfg");
    write_hook_config(&config_dir, false);
    // `cwd` IS the parent of `config_dir`, so `--config cfg/cfgd.yaml` is a
    // relative path resolved against the invocation directory — never
    // against `$HOME`, which sits elsewhere entirely.
    let cwd = root.path().to_path_buf();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    run_matrix_cell(
        &cwd,
        &home,
        Path::new("cfg").join("cfgd.yaml"),
        &config_dir.join(MARKER),
        false,
    );
}

#[test]
fn apply_relative_config_with_args_resolves_and_runs() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("cfg");
    write_hook_config(&config_dir, true);
    let cwd = root.path().to_path_buf();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    run_matrix_cell(
        &cwd,
        &home,
        Path::new("cfg").join("cfgd.yaml"),
        &config_dir.join(MARKER),
        true,
    );
}

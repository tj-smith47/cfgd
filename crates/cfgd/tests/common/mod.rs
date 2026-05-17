//! Shared integration-test helpers.
//!
//! Each integration test file is its own crate, so any unused helper here
//! will trip `dead_code` when imported by a file that only uses some of
//! them. `#![allow(dead_code)]` is the standard Cargo idiom for this
//! shared-fixture pattern.

#![allow(dead_code)]

use std::path::PathBuf;

use cfgd::cli::{ApplyArgs, Cli, Command, OutputFormatArg, PlanArgs};
use cfgd_core::output;

/// Build a tempdir-backed profile with a single file action that will
/// succeed on apply.
///
/// Returns `(config_dir, state_dir, target)` — the tempdirs must outlive
/// the test (they own the on-disk directories) and `target` is the path
/// the action will create.
pub fn tiny_profile_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    // Source file the profile references.
    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("hello.txt"), "hello world").unwrap();

    let target = config_dir.path().join("out").join("hello.txt");
    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n",
        target.display()
    );
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir, target)
}

/// Build a tempdir-backed profile with two file actions: one whose target
/// directory exists and is writable (succeeds), and one whose target's
/// parent is a regular file (so `create_dir_all` errors at apply time —
/// partial failure, NOT a hard error).
///
/// Returns `(config_dir, state_dir, target_ok, target_fail)`.
pub fn profile_with_one_failure_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, PathBuf)
{
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    // Both source files exist (plan stage hard-errors on missing source
    // for non-private files; we need the failure to surface at apply time).
    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("hello.txt"), "hello world").unwrap();
    std::fs::write(files_dir.join("world.txt"), "second").unwrap();

    // target_ok lands in a normal directory.
    let target_ok = config_dir.path().join("out").join("hello.txt");
    // target_fail's parent is a regular FILE on disk — `create_dir_all`
    // returns ENOTDIR at apply time. One action succeeds, one fails.
    let blocker = config_dir.path().join("blocker");
    std::fs::write(&blocker, "i am a file, not a dir").unwrap();
    let target_fail = blocker.join("world.txt");

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/hello.txt\n        target: {}\n        strategy: Copy\n      - source: files/world.txt\n        target: {}\n        strategy: Copy\n",
        target_ok.display(),
        target_fail.display(),
    );
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir, target_ok, target_fail)
}

/// Build a `Cli` parameterised on tempdir locations. The `command` slot is
/// filled with a no-op `Status` because the dispatcher isn't invoked —
/// integration tests call command functions directly.
pub fn cli_for(config_dir: &std::path::Path, state_dir: &std::path::Path) -> Cli {
    Cli {
        config: config_dir.join("cfgd.yaml"),
        profile: None,
        no_color: true,
        verbose: 0,
        quiet: true,
        output: OutputFormatArg(output::OutputFormat::Table),
        jsonpath: None,
        state_dir: Some(state_dir.to_path_buf()),
        command: Some(Command::Status {
            module: None,
            exit_code: false,
        }),
    }
}

/// Default `ApplyArgs` for a non-dry-run `--yes` apply.
pub fn apply_args() -> ApplyArgs {
    ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    }
}

/// Default `ApplyArgs` for a `--dry-run --yes` apply.
pub fn apply_args_dry_run() -> ApplyArgs {
    ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    }
}

/// Build a tempdir-backed profile with zero managed files and zero modules.
/// `cmd_plan` against this fixture exercises the "nothing to do" branch.
///
/// Returns `(config_dir, state_dir)`.
pub fn empty_profile_setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n";
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("empty.yaml"), profile).unwrap();

    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    (config_dir, state_dir)
}

/// Like `tiny_profile_setup` but pre-records an unresolved pending decision in
/// the state DB so `display_plan_preview_v2` renders the pending-decisions
/// section.
///
/// Returns `(config_dir, state_dir, target)`.
pub fn state_with_pending_decision_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let (config_dir, state_dir, target) = tiny_profile_setup();

    // `cmd_plan` opens the state DB at `<state_dir>/cfgd.db` via
    // `open_state_store`; record the pending decision against the same path so
    // the subsequent `pending_decisions()` query inside `display_plan_preview_v2`
    // sees it.
    let store = cfgd_core::state::StateStore::open(&state_dir.path().join("cfgd.db")).unwrap();
    store
        .upsert_pending_decision(
            "team-config",
            "packages.brew.ripgrep",
            "permission",
            "add",
            "team-config wants to install ripgrep",
        )
        .unwrap();

    (config_dir, state_dir, target)
}

/// Default `PlanArgs` for a plan against the active profile.
pub fn plan_args() -> PlanArgs {
    PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    }
}

/// `PlanArgs` with a `--module` filter set.
pub fn plan_args_module(name: &str) -> PlanArgs {
    PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: Some(name.to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
    }
}

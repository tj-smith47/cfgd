//! State-write regression test for `cfgd apply`.
//!
//! Guards against accidental "while we're here" side-effect rewrites
//! during the output-system migration. Verifies:
//!   1. cmd_apply against a tempdir with one file action increments the
//!      apply-log row count by exactly +1.
//!   2. The apply lock is released after the run (re-acquire succeeds).
//!
//! Test asserts on *count*, not output — output shape is covered by
//! `apply_v2_snapshots.rs`.

use std::path::PathBuf;

use cfgd::cli::{ApplyArgs, Cli, Command, OutputFormatArg, apply};
use cfgd_core::output::{self, Printer as PrinterV1};
use cfgd_core::output_v2::Printer as PrinterV2;
use cfgd_core::state::StateStore;

fn tiny_profile_setup() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
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

fn cli_for(config_dir: &std::path::Path, state_dir: &std::path::Path) -> Cli {
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

fn apply_args() -> ApplyArgs {
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

#[test]
fn cmd_apply_increments_apply_log_by_one() {
    let (config_dir, state_dir, target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let printer = PrinterV1::new(output::Verbosity::Quiet);
    let v2_printer = PrinterV2::new(cfgd_core::output_v2::Verbosity::Quiet);
    let args = apply_args();

    // Baseline: no applies recorded.
    let db_path = state_dir.path().join("cfgd.db");
    {
        let state = StateStore::open(&db_path).unwrap();
        let before = state.history(100).unwrap();
        assert_eq!(before.len(), 0, "no applies before the test");
    }

    apply::cmd_apply(&cli, &printer, &v2_printer, &args).unwrap();
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
    let printer = PrinterV1::new(output::Verbosity::Quiet);
    let v2_printer = PrinterV2::new(cfgd_core::output_v2::Verbosity::Quiet);
    let args = apply_args();

    apply::cmd_apply(&cli, &printer, &v2_printer, &args).unwrap();

    // If the lock is released, re-acquiring must succeed.
    let guard = cfgd_core::acquire_apply_lock(state_dir.path())
        .expect("apply lock should be released after cmd_apply");
    drop(guard);
}

//! Snapshot tests for `cfgd profile create`.
//!
//! Cases:
//!   - `profile_create/happy.{txt,json}` — flag-driven create with no
//!     inherits and no modules; real `cmd_profile_create` against a
//!     tempdir fixture.
//!   - `profile_create/inherits.txt` — create with `--inherits a,b` and
//!     `--modules nvim`.
//!   - `profile_create/interactive.txt` — empty flags trigger interactive
//!     mode; the prompt-response queue answers both `prompt_text` calls
//!     with the empty string so no inherits/modules are added.
//!   - `profile_create/already_exists.txt` — re-creating a fixture-seeded
//!     profile triggers the emit-then-bail error Doc
//!     (`{error: "already_exists"}`).
//!
//! Goldens live under `tests/output_snapshots/profile_create/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_create_snapshots

mod common;

use std::path::Path;

use cfgd::cli::profile::cmd_profile_create;
use cfgd_core::output::{Printer, PromptAnswer};
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;

use common::{cli_for, normalize_profile_paths, profile_create_args, profile_test_config_setup};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for inner in chars.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn profile_create_happy_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = profile_create_args("newprof");
    // Non-empty packages list forces non-interactive mode without adding a
    // package: pass a flag that's a no-op against the default platform mgr.
    args.env = vec!["FOO=bar".to_string()];

    cmd_profile_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_create/happy.txt",
        &stripped,
    );
}

#[test]
fn profile_create_happy_json() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = profile_create_args("newprof");
    args.env = vec!["FOO=bar".to_string()];

    cmd_profile_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "newprof");
    assert!(json["path"].as_str().unwrap().ends_with("newprof.yaml"));
}

#[test]
fn profile_create_inherits_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = profile_create_args("child");
    args.inherits = vec!["default".to_string()];
    args.modules = vec!["nvim".to_string()];

    cmd_profile_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_create/inherits.txt",
        &stripped,
    );
}

#[test]
fn profile_create_interactive_human() {
    // Empty args list triggers interactive mode; the queue answers both
    // `prompt_text` invocations with the empty string so the profile is
    // created with no inherits and no modules.
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_prompt_responses(vec![
        PromptAnswer::Text(String::new()),
        PromptAnswer::Text(String::new()),
    ]);
    let args = profile_create_args("interactive_prof");

    cmd_profile_create(&cli, &printer, &args).unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_create/interactive.txt",
        &stripped,
    );
}

#[test]
fn profile_create_already_exists_human() {
    // `default` is seeded in the fixture, so re-creating it triggers the
    // emit-then-bail Doc before `anyhow::bail!` fires.
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = profile_create_args("default");
    args.env = vec!["FOO=bar".to_string()];

    let err = cmd_profile_create(&cli, &printer, &args)
        .expect_err("creating an existing profile must error");
    assert!(err.to_string().contains("already exists"));
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_create/already_exists.txt",
        &stripped,
    );
}

#[test]
fn profile_create_already_exists_json_payload() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let mut args = profile_create_args("default");
    args.env = vec!["FOO=bar".to_string()];

    let _ = cmd_profile_create(&cli, &printer, &args);
    drop(printer);

    let json = cap.json().expect("error path emits a Doc with payload");
    assert_eq!(json["error"], "already_exists");
    assert_eq!(json["name"], "default");
}

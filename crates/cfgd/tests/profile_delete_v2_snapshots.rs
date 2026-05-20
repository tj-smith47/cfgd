//! Snapshot tests for `cfgd profile delete`.
//!
//! Cases:
//!   - `profile_delete/happy.{txt,json}` — `--yes` path against a real
//!     tempdir profile: file removed, `Role::Ok "Deleted profile '<name>'"`
//!     emitted.
//!   - `profile_delete/cancelled.{txt,json}` — queued `Confirm(false)`
//!     takes the early-return arm; the cancelled Doc carries
//!     `{cancelled: true}`.
//!   - `profile_delete/active_profile_refused.txt` — deleting the active
//!     profile triggers the emit-then-bail error Doc.
//!   - `profile_delete/inheritor_refused.txt` — deleting a profile that
//!     other profiles inherit from triggers the emit-then-bail Doc.
//!
//! Goldens live under `tests/output_snapshots/profile_delete/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_delete_v2_snapshots

mod common;

use std::path::Path;

use cfgd::cli::profile::cmd_profile_delete;
use cfgd_core::output::{Printer, PromptAnswer};

use common::{cli_for, normalize_profile_paths, profile_test_config_setup};

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

fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    pretty_assertions::assert_eq!(actual, expected, "snapshot mismatch: {name}");
}

#[test]
fn profile_delete_happy_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    // Delete the inheritor first so we don't trip the active-profile refusal
    // (the fixture's active profile is `default`, so `work` is safely deletable).
    cmd_profile_delete(&cli, &printer, "work", true).unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_delete/happy.txt",
        &stripped,
    );
}

#[test]
fn profile_delete_happy_json() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_profile_delete(&cli, &printer, "work", true).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "work");
    assert_eq!(json["cancelled"], false);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_delete/happy.json");
}

#[test]
fn profile_delete_cancelled_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);

    cmd_profile_delete(&cli, &printer, "work", false).unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_delete/cancelled.txt",
        &stripped,
    );
}

#[test]
fn profile_delete_cancelled_json() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) =
        Printer::for_test_doc_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);

    cmd_profile_delete(&cli, &printer, "work", false).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["cancelled"], true);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_delete/cancelled.json");
}

#[test]
fn profile_delete_active_profile_refused_human() {
    // `default` is the active profile in the fixture; the safety check
    // emits the error Doc and bails.
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let err = cmd_profile_delete(&cli, &printer, "default", true)
        .expect_err("deleting the active profile must error");
    assert!(err.to_string().contains("active profile"));
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_delete/active_profile_refused.txt",
        &stripped,
    );
}

#[test]
fn profile_delete_inheritor_refused_human() {
    // Add a second profile that inherits from `work` so deleting `work`
    // hits the inheritor-refusal arm.
    let (config_dir, state_dir) = profile_test_config_setup();
    let child_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - work\n";
    std::fs::write(
        config_dir.path().join("profiles").join("child.yaml"),
        child_yaml,
    )
    .unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let err = cmd_profile_delete(&cli, &printer, "work", true)
        .expect_err("deleting an inherited profile must error");
    assert!(err.to_string().contains("inherited"));
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_delete/inheritor_refused.txt",
        &stripped,
    );
}

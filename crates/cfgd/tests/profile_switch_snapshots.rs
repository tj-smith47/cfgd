//! Snapshot tests for `cfgd profile switch`.
//!
//! Each case drives real `cmd_profile_switch` against a tempdir-backed fixture
//! and captures the rendered output through `Printer::for_test_doc()`. The
//! error paths (`not_found`) emit a `Role::Fail` Doc carrying `{error, name}`
//! before the `anyhow::bail!` fires, so structured consumers see a stable
//! shape on every code path.
//!
//! Goldens live under `tests/output_snapshots/profile_switch/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_switch_snapshots

mod common;

use std::path::Path;

use cfgd::cli::error::render_cli_error;
use cfgd::cli::profile::cmd_profile_switch;
use cfgd_core::output::Printer;
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;

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

#[test]
fn profile_switch_happy_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_profile_switch(&cli, "work", &printer).unwrap();
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_switch/happy.txt",
        &stripped,
    );
}

#[test]
fn profile_switch_happy_json() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_profile_switch(&cli, "work", &printer).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["from"], "default");
    assert_eq!(json["to"], "work");
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_switch/happy.json");
}

#[test]
fn profile_switch_not_found_json_payload_is_dir_oriented() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let err = cmd_profile_switch(&cli, "missing", &printer)
        .expect_err("switching to nonexistent profile must error");
    render_cli_error(&printer, &err);
    drop(printer);

    let json = cap.json().expect("error doc captured json");
    assert_eq!(json["error"], "not_found");
    assert_eq!(json["name"], "missing");
    let profiles_dir = json["profilesDir"]
        .as_str()
        .expect("payload names the probed profiles dir");
    assert!(
        profiles_dir.ends_with("/profiles") && !profiles_dir.contains('\\'),
        "profilesDir must be the posix profiles directory, got: {profiles_dir}"
    );
    assert!(
        json.get("profilePath").is_none(),
        "single-file profilePath key is replaced by profilesDir"
    );
    let available: Vec<&str> = json["available"]
        .as_array()
        .expect("available list present")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(available, vec!["default", "work"]);
}

#[test]
fn profile_switch_not_found_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let err = cmd_profile_switch(&cli, "missing", &printer)
        .expect_err("switching to nonexistent profile must error");
    assert!(err.to_string().contains("not found"));
    render_cli_error(&printer, &err);
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_switch/not_found.txt",
        &stripped,
    );
}

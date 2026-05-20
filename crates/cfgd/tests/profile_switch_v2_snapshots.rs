//! Snapshot tests for `cfgd profile switch`.
//!
//! Each case drives real `cmd_profile_switch` against a tempdir-backed fixture
//! and captures the rendered output through `Printer::for_test_doc()`. The
//! error paths (`not_found`) emit a `Role::Fail` Doc carrying `{error, name}`
//! before the `anyhow::bail!` fires, so structured consumers see a stable
//! shape on every code path.
//!
//! Goldens live under `tests/output_snapshots/profile_switch/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_switch_v2_snapshots

mod common;

use std::path::Path;

use cfgd::cli::profile::cmd_profile_switch;
use cfgd_core::output::Printer;

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
fn profile_switch_not_found_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let err = cmd_profile_switch(&cli, "missing", &printer)
        .expect_err("switching to nonexistent profile must error");
    assert!(err.to_string().contains("not found"));
    drop(printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_switch/not_found.txt",
        &stripped,
    );
}

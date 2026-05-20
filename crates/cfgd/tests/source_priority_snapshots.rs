//! Snapshot tests for `cfgd source priority`.
//!
//! Cases:
//!   - `source_priority/happy.{txt,json}` — real `cmd_source_priority` with
//!     a new priority value updates `cfgd.yaml` and emits the Doc.
//!   - `source_priority/view.txt` — view-only branch (no value passed).
//!   - `source_priority/not_found.txt` — error-path Doc for missing source.
//!
//! Goldens live under `tests/output_snapshots/source_priority/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_priority_snapshots

mod common;

use std::path::Path;

use cfgd::cli::source::cmd_source_priority;
use cfgd_core::output::Printer;

use common::{cli_for, source_test_config_with_source_setup};

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
fn source_priority_happy_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_priority(&cli, &printer, "team-config", Some(500)).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_priority/happy.txt",
        &stripped,
    );
}

#[test]
fn source_priority_happy_json() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_priority(&cli, &printer, "team-config", Some(500)).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "team-config");
    assert_eq!(json["priority"], 500);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_priority/happy.json");
}

#[test]
fn source_priority_view_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_priority(&cli, &printer, "team-config", None).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_priority/view.txt",
        &stripped,
    );
}

#[test]
fn source_priority_not_found_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let result = cmd_source_priority(&cli, &printer, "missing", None);
    assert!(result.is_err());
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_priority/not_found.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "not_found");
    assert_eq!(json["name"], "missing");
}

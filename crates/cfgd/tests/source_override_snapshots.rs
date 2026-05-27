//! Snapshot tests for `cfgd source override`.
//!
//! Cases:
//!   - `source_override/accept.{txt,json}` — `cmd_source_override Set` with a
//!     value.
//!   - `source_override/reject.txt` — `cmd_source_override Reject` for a path.
//!   - `source_override/not_found.txt` — error-path Doc for missing source.
//!
//! Goldens live under `tests/output_snapshots/source_override/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_override_snapshots

mod common;

use std::path::Path;

use cfgd::cli::SourceOverrideAction;
use cfgd::cli::source::cmd_source_override;
use cfgd_core::output::Printer;
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;

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

#[test]
fn source_override_accept_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_override(
        &cli,
        &printer,
        "team-config",
        SourceOverrideAction::Set,
        "packages.brew.ripgrep",
        Some("true"),
    )
    .unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_override/accept.txt",
        &stripped,
    );
}

#[test]
fn source_override_accept_json() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_override(
        &cli,
        &printer,
        "team-config",
        SourceOverrideAction::Set,
        "packages.brew.ripgrep",
        Some("true"),
    )
    .unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["sourceName"], "team-config");
    assert_eq!(json["action"], "set");
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_override/accept.json");
}

#[test]
fn source_override_reject_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_override(
        &cli,
        &printer,
        "team-config",
        SourceOverrideAction::Reject,
        "packages.brew.ripgrep",
        None,
    )
    .unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_override/reject.txt",
        &stripped,
    );
}

#[test]
fn source_override_not_found_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let result = cmd_source_override(
        &cli,
        &printer,
        "missing",
        SourceOverrideAction::Reject,
        "packages.brew",
        None,
    );
    assert!(result.is_err());
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_override/not_found.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "not_found");
    assert_eq!(json["name"], "missing");
}

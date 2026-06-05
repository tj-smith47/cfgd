//! Snapshot tests for `cfgd config get`.
//!
//! Cases:
//!   - `config_get/happy.{txt,json}` — fetches `profile`, which renders as
//!     the bare value on stdout and the structured Doc carries
//!     `{"key": "profile", "value": "default"}`.
//!   - `config_get/not_found.txt` — error-path Doc when the key path doesn't
//!     resolve under `spec`.

mod common;

use std::path::Path;

use cfgd::cli::config_cmd;
use cfgd::cli::error::render_cli_error;
use cfgd_core::output::{OutputFormat, Printer};
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;

use common::{cli_for, config_test_setup};

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
fn config_get_happy_human() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    config_cmd::cmd_config_get(&cli, &printer, "profile").unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "config_get/happy.txt", &stripped);
}

#[test]
fn config_get_happy_json() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);

    config_cmd::cmd_config_get(&cli, &printer, "profile").unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["key"], "profile");
    assert_eq!(json["value"], "default");
}

#[test]
fn config_get_not_found_human() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());

    // The handler returns the error; the central sink (render_cli_error) renders the
    // one ✗ line. Drive both through the SAME printer so the golden captures any handler
    // pre-error output plus the central failure line — exactly what the user sees.
    let (printer, cap) = Printer::for_test_doc();
    let err = config_cmd::cmd_config_get(&cli, &printer, "ghost.path")
        .expect_err("missing key must return Err");
    render_cli_error(&printer, &err);
    drop(printer);
    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "config_get/not_found.txt",
        &stripped,
    );

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "key_not_found");
    assert_eq!(meta.name, "ghost.path");
}

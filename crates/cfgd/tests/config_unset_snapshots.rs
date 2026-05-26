//! Snapshot tests for `cfgd config unset`.
//!
//! Cases:
//!   - `config_unset/happy.{txt,json}` — removes an existing key; Doc carries
//!     `previousValue` and `removed: true`.
//!   - `config_unset/not_found.txt` — error-path Doc when the key path doesn't
//!     resolve under `spec`.

mod common;

use std::path::Path;

use cfgd::cli::config_cmd;
use cfgd_core::output::{OutputFormat, Printer};

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

fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();

    // Normalize CRLF→LF: windows captured output has \r\n; committed snapshot is LF.

    let actual_norm = actual.replace("\r\n", "\n");

    let expected_norm = expected.replace("\r\n", "\n");

    pretty_assertions::assert_eq!(actual_norm, expected_norm, "snapshot mismatch: {name}");
}

#[test]
fn config_unset_happy_human() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    config_cmd::cmd_config_unset(&cli, &printer, "profile").unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "config_unset/happy.txt",
        &stripped,
    );
}

#[test]
fn config_unset_happy_json() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);

    config_cmd::cmd_config_unset(&cli, &printer, "profile").unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["key"], "profile");
    assert_eq!(json["previousValue"], "default");
    assert_eq!(json["removed"], true);
}

#[test]
fn config_unset_not_found_human() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let result = config_cmd::cmd_config_unset(&cli, &printer, "ghostKey");
    assert!(result.is_err());
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "config_unset/not_found.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "key_not_found");
    assert_eq!(json["name"], "ghostKey");
}

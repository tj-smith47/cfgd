//! Snapshot tests for `cfgd config set`.
//!
//! Cases:
//!   - `config_set/happy.{txt,json}` — overwrites an existing scalar; Doc
//!     carries `previousValue`.
//!   - `config_set/creates_new.txt` — sets a key that didn't exist before;
//!     `previousValue` is null.

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
    pretty_assertions::assert_eq!(actual, expected, "snapshot mismatch: {name}");
}

#[test]
fn config_set_happy_human() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    config_cmd::cmd_config_set(&cli, &v2_printer, "profile", "work").unwrap();
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "config_set/happy.txt", &stripped);
}

#[test]
fn config_set_happy_json() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);

    config_cmd::cmd_config_set(&cli, &v2_printer, "profile", "work").unwrap();
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["key"], "profile");
    assert_eq!(json["value"], "work");
    assert_eq!(json["previousValue"], "default");
}

#[test]
fn config_set_creates_new_human() {
    let (config_dir, state_dir) = config_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    config_cmd::cmd_config_set(&cli, &v2_printer, "daemon.enabled", "true").unwrap();
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "config_set/creates_new.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["previousValue"], serde_json::Value::Null);
    assert_eq!(json["value"], true);
}

//! Snapshot tests for `cfgd module push` / `pull`.
//!
//! Only error-paths are captured here — the happy paths shell out to a
//! live OCI registry, which is exercised in unit tests against mock
//! responses.

mod common;

use std::path::Path;

use cfgd::cli::module;
use cfgd_core::output::Printer;

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
fn module_push_missing_yaml_human() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, cap) = Printer::for_test_doc();

    let result = module::cmd_module_push(
        &printer,
        dir.path().to_str().unwrap(),
        "oci.example.com/test:v1",
        module::PushOptions {
            platform: None,
            apply: false,
            sign: false,
            key: None,
            attest: false,
        },
    );
    assert!(result.is_err());
    drop(printer);

    let stripped = strip_ansi(&cap.human()).replace(&dir.path().display().to_string(), "<DIR>");
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_push/missing_yaml.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "module_yaml_missing");
}

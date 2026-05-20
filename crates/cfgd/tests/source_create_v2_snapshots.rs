//! Snapshot tests for `cfgd source create`.
//!
//! Cases:
//!   - `source_create/happy.{txt,json}` — flag-driven create scaffolds
//!     `cfgd-source.yaml` and emits the success Doc.
//!   - `source_create/already_exists.txt` — error-path Doc when manifest is
//!     already on disk.
//!
//! Goldens live under `tests/output_snapshots/source_create/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_create_v2_snapshots

mod common;

use std::path::Path;

use cfgd::cli::source::cmd_source_create;
use cfgd_core::output::Printer;

use common::{cli_for, normalize_profile_paths, source_test_config_setup};

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
fn source_create_happy_human() {
    let (config_dir, state_dir) = source_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    cmd_source_create(
        &cli,
        &v2_printer,
        Some("my-source"),
        Some("Test source"),
        Some("1.0.0"),
    )
    .unwrap();
    drop(v2_printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_create/happy.txt",
        &stripped,
    );
}

#[test]
fn source_create_happy_json() {
    let (config_dir, state_dir) = source_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    cmd_source_create(
        &cli,
        &v2_printer,
        Some("my-source"),
        Some("Test source"),
        Some("1.0.0"),
    )
    .unwrap();
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "my-source");
    assert_eq!(json["version"], "1.0.0");
}

#[test]
fn source_create_already_exists_human() {
    let (config_dir, state_dir) = source_test_config_setup();
    std::fs::write(config_dir.path().join("cfgd-source.yaml"), "stub manifest").unwrap();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    let result = cmd_source_create(&cli, &v2_printer, Some("x"), Some("x"), Some("1.0"));
    assert!(result.is_err());
    drop(v2_printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_create/already_exists.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "already_exists");
}

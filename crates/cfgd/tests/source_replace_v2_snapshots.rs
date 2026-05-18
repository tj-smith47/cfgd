//! Snapshot tests for `cfgd source replace`.
//!
//! Cases:
//!   - `source_replace/not_found.txt` — error-path Doc when the old source
//!     doesn't exist (`cmd_source_remove` fails first).
//!
//! `cmd_source_replace` composes `cmd_source_remove` + `cmd_source_add`. The
//! happy path requires a live source clone (network-dependent in production);
//! the failure path is the most stable snapshot anchor.
//!
//! Goldens live under `tests/output_snapshots/source_replace/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_replace_v2_snapshots

mod common;

use std::path::Path;

use cfgd::cli::source::cmd_source_replace;
use cfgd_core::output::{Printer as PrinterV1, Verbosity};
use cfgd_core::output_v2::Printer;

use common::{cli_for, source_test_config_setup};

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
fn source_replace_not_found_human() {
    let (config_dir, state_dir) = source_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let v1_printer = PrinterV1::new(Verbosity::Quiet);
    let (v2_printer, cap) = Printer::for_test_doc();

    let result = cmd_source_replace(
        &cli,
        &v1_printer,
        &v2_printer,
        "missing",
        "https://github.com/team/new.git",
    );
    assert!(result.is_err());
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_replace/not_found.txt",
        &stripped,
    );
}

//! Snapshot tests for `cfgd source edit`.
//!
//! Cases:
//!   - `source_edit/no_config.txt` — error-path Doc when `cfgd-source.yaml`
//!     doesn't exist.
//!
//! The valid/invalid edit paths require driving an external editor + prompt
//! queue; those branches live in unit tests under `cli/tests.rs` where the
//! `EditorGuard` infrastructure is set up. The error-path snapshot pins the
//! emitted Doc shape consumers see when the manifest is missing.
//!
//! Goldens live under `tests/output_snapshots/source_edit/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_edit_v2_snapshots

mod common;

use std::path::Path;

use cfgd::cli::source::cmd_source_edit;
use cfgd_core::output_v2::Printer;

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
fn source_edit_no_config_human() {
    let (config_dir, state_dir) = source_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    let result = cmd_source_edit(&cli, &v2_printer);
    assert!(result.is_err());
    drop(v2_printer);

    let stripped = normalize_profile_paths(&strip_ansi(&cap.human()), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_edit/no_config.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "no_config");
}

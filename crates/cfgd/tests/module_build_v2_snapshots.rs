//! Snapshot tests for `cfgd module build`.
//!
//! Cases:
//!   - `module_build/missing_yaml.txt` — error-path Doc when the directory
//!     has no `module.yaml`. The happy path requires the OCI builder which
//!     is exercised in unit tests with controlled fixtures, not snapshot
//!     coverage here.
//!   - `module_build/bridge.txt` — §17.2 bridge invariant on the streaming
//!     build surface (multi-target spinners) → buffered summary Doc.
//!     Synthetic per the F3 README bridge-synthetic exception: production
//!     `cmd_module_build` requires the OCI builder + controlled fixtures
//!     (skopeo + docker layers), so we hand-roll the minimal
//!     streaming-then-buffered shape. The streaming-side status content is
//!     deterministic and may diverge from any specific real invocation;
//!     what's locked is the §17.2 invariant.

mod common;

use std::path::Path;

use cfgd::cli::module;
use cfgd_core::output_v2::{Doc, Printer, Role};

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
fn module_build_missing_yaml_human() {
    let dir = tempfile::tempdir().unwrap();
    let (v2_printer, cap) = Printer::for_test_doc();

    let result = module::cmd_module_build(
        &v2_printer,
        dir.path().to_str().unwrap(),
        None,
        None,
        None,
        false,
        None,
    );
    assert!(result.is_err());
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human()).replace(&dir.path().display().to_string(), "<DIR>");
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_build/missing_yaml.txt",
        &stripped,
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "module_yaml_missing");
}

#[test]
fn module_build_bridge_one_blank_line() {
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.heading("Build Module");
    {
        let sp = v2_printer.spinner("Building for linux/amd64...");
        sp.finish_ok("Built linux/amd64 to /tmp/build-out");
    }
    v2_printer.emit(
        Doc::new()
            .status(Role::Ok, "Built module")
            .with_data(serde_json::json!({
                "dir": ".",
                "targets": ["linux/amd64"],
                "outputArtifacts": ["/tmp/build-out"],
                "signed": false,
            })),
    );
    drop(v2_printer);

    let captured = strip_ansi(&cap.human());
    assert!(
        captured.contains("\n\n"),
        "bridge missing blank line:\n{captured}"
    );
    assert!(
        !captured.contains("\n\n\n"),
        "bridge has duplicate blank line:\n{captured}"
    );
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_build/bridge.txt",
        &captured,
    );
}

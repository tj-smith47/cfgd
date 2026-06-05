//! Snapshot tests for `cfgd module build`.
//!
//! Cases:
//!   - `module_build/missing_yaml.txt` — error-path Doc when the directory
//!     has no `module.yaml`. The happy path requires the OCI builder which
//!     is exercised in unit tests with controlled fixtures, not snapshot
//!     coverage here.
//!   - `module_build/bridge.txt` — bridge invariant on the streaming
//!     build surface (multi-target spinners) → buffered summary Doc.
//!     Synthetic under the bridge-synthetic exception: production
//!     `cmd_module_build` requires the OCI builder + controlled fixtures
//!     (skopeo + docker layers), so the minimal streaming-then-buffered
//!     shape is hand-rolled. The streaming-side status content is
//!     deterministic and may diverge from any specific real invocation;
//!     what's locked is the one-blank-line bridge invariant.

mod common;

use std::path::Path;

use cfgd::cli::error::render_cli_error;
use cfgd::cli::module;
use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;

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
fn module_build_missing_yaml_human() {
    let dir = tempfile::tempdir().unwrap();

    let (printer, cap) = Printer::for_test_doc();
    let err = module::cmd_module_build(
        &printer,
        dir.path().to_str().unwrap(),
        None,
        None,
        None,
        false,
        None,
    )
    .expect_err("missing module.yaml must return Err");
    render_cli_error(&printer, &err);
    drop(printer);

    let stripped =
        cfgd_core::normalize_for_snapshot(&strip_ansi(&cap.human()), &[(dir.path(), "<DIR>")]);
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_build/missing_yaml.txt",
        &stripped,
    );

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "module_yaml_missing");
}

#[test]
fn module_build_bridge_one_blank_line() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Build Module");
    {
        let sp = printer.spinner("Building for linux/amd64...");
        sp.finish_ok("Built linux/amd64 to /tmp/build-out");
    }
    printer.emit(
        Doc::new()
            .status(Role::Ok, "Built module")
            .with_data(serde_json::json!({
                "dir": ".",
                "targets": ["linux/amd64"],
                "outputArtifacts": ["/tmp/build-out"],
                "signed": false,
            })),
    );
    drop(printer);

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

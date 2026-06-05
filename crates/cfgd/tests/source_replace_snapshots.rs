//! Snapshot tests for `cfgd source replace`.
//!
//! Cases:
//!   - `source_replace/not_found.txt` — error-path Doc when the old source
//!     doesn't exist (`cmd_source_remove` fails first).
//!   - `source_replace/happy.{txt,json}` — `cmd_source_replace` swaps the
//!     subscribed URL from one local bare repo to another, walking the
//!     `cmd_source_remove` + `cmd_source_add` composition end-to-end. Drives
//!     `cmd_source_add` first to seed the initial subscription, then runs
//!     `cmd_source_replace` against a second `make_bare_source_repo`.
//!
//! `cmd_source_replace` composes `cmd_source_remove` + `cmd_source_add`. Both
//! `file://` fixtures rely on `CFGD_ALLOW_LOCAL_SOURCES=1`.
//!
//! Goldens live under `tests/output_snapshots/source_replace/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_replace_snapshots

mod common;

use std::path::Path;

use cfgd::cli::error::render_cli_error;
use cfgd::cli::source::{cmd_source_add, cmd_source_replace};
use cfgd_core::output::Printer;
use cfgd_core::test_helpers::assert_snapshot_golden as assert_snapshot;
use serial_test::serial;

use common::{cli_for, make_bare_source_repo, source_add_args, source_test_config_setup};

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

fn normalize_bare(raw: &str, bares: &[(&std::path::Path, &str)]) -> String {
    // Posixify text + path keys FIRST, then substitute — otherwise the
    // captured output (already in `/` form from libgit2 URL emission on
    // Windows) won't match a Windows `PathBuf`'s `\`-form `to_string_lossy`.
    let normalized = cfgd_core::normalize_for_snapshot(raw, bares);
    // `to_file_url` emits `file:///<absolute-posix-path>` on every OS; on
    // Windows the substituted path lacks a leading `/`, leaving the URL
    // prefix's third slash visible (`file:///<PLACEHOLDER>`). Fold to the
    // unix shape (`file://<PLACEHOLDER>`) so a single golden survives both.
    let folded = bares.iter().fold(normalized, |acc, (_, label)| {
        let win_form = ["file:///", label].concat();
        let unix_form = ["file://", label].concat();
        acc.replace(&win_form, &unix_form)
    });
    strip_spinner_duration(folded)
}

/// Strip non-deterministic spinner finish durations like ` (0.0s)` so goldens
/// survive runtime variance.
use cfgd_core::output::test_capture::strip_spinner_duration;

#[test]
#[serial]
fn source_replace_happy_human() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (config_dir, state_dir) = source_test_config_setup();
    let bare_root = tempfile::tempdir().unwrap();
    let bare_old = make_bare_source_repo(bare_root.path(), "replace-old", None);
    let bare_new = make_bare_source_repo(bare_root.path(), "replace-new", None);
    let url_old = cfgd_core::test_helpers::file_url(&bare_old);
    let url_new = cfgd_core::test_helpers::file_url(&bare_new);

    let cli = cli_for(config_dir.path(), state_dir.path());

    // Seed the initial subscription via cmd_source_add (matches add fixture).
    let (add_printer, _add_cap) = Printer::for_test_doc();
    let mut args = source_add_args(url_old);
    args.name = Some("replace-old".into());
    cmd_source_add(&cli, &add_printer, &args).expect("seed source");
    drop(add_printer);

    let (printer, cap) = Printer::for_test_doc();
    cmd_source_replace(&cli, &printer, "replace-old", &url_new).unwrap();
    drop(printer);

    let stripped = normalize_bare(
        &strip_ansi(&cap.human()),
        &[
            (&bare_old, "<BARE_OLD>"),
            (&bare_new, "<BARE_NEW>"),
            (bare_root.path(), "<BARE_ROOT>"),
            (config_dir.path(), "<CONFIG_DIR>"),
            (state_dir.path(), "<STATE_DIR>"),
        ],
    );
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_replace/happy.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["oldName"], "replace-old");
    assert_eq!(json["newUrl"], url_new);

    // Roll our own JSON snapshot — the captured payload carries the tempdir
    // path, so route it through the same normalize_bare helper used for
    // the human surface.
    let json_pretty = serde_json::to_string_pretty(&json).unwrap();
    let normalized_json = normalize_bare(
        &json_pretty,
        &[
            (&bare_old, "<BARE_OLD>"),
            (&bare_new, "<BARE_NEW>"),
            (bare_root.path(), "<BARE_ROOT>"),
        ],
    );
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_replace/happy.json",
        &normalized_json,
    );
}

#[test]
fn source_replace_not_found_human() {
    let (config_dir, state_dir) = source_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let err = cmd_source_replace(&cli, &printer, "missing", "https://github.com/team/new.git")
        .expect_err("missing old source must return Err");
    render_cli_error(&printer, &err);
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "source_replace/not_found.txt",
        &stripped,
    );

    let _meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
}

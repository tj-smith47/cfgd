//! Snapshot tests for `cfgd log`.
//!
//! Real `cmd_log` capture against tempdir state DBs seeded directly through
//! the public `StateStore` API. Bridge snapshot omitted: `cmd_log` is a
//! pure buffered surface (heading + table + emit) with no streaming side,
//! and `cmd_log_show_output`'s streaming-entries branch emits the streaming
//! "Entries" section without a buffered human surface afterwards (the
//! trailing Doc is payload-only). The `entries.is_empty()` branch is fully
//! buffered (heading + status + with_data) — also no streaming→buffered
//! transition. Follows the bridge-snapshot rule: surfaces with no
//! streaming→buffered transition do not get a bridge snapshot.
//!
//! Timestamps in the multi-row golden are normalised to a placeholder so
//! the snapshot is host-stable.
//!
//! Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test log_snapshots

mod common;

use std::path::Path;

use cfgd::cli::log::{build_log_doc, cmd_log};
use cfgd::cli::output_types::LogOutput;
use cfgd_core::output::Printer;
use cfgd_core::state::ApplyStatus;
use pretty_assertions::assert_eq;

use common::{log_history_setup, log_show_output_no_journal_setup, log_show_output_setup};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

/// Empty history — `cmd_log` emits the "No applies recorded yet" status.
#[test]
fn log_empty_human() {
    let (state_dir, _ids) = log_history_setup(&[]);

    let (printer, cap) = Printer::for_test_doc();

    cmd_log(&printer, 10, None, Some(state_dir.path())).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "log/empty.txt", &stripped);
}

/// JSON payload roundtrip — LogOutput shape via build_log_doc + cap.json().
#[test]
fn log_empty_json() {
    let output = LogOutput {
        entries: Vec::new(),
    };
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_log_doc(&output));
    drop(printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("log doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(LogOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "log/empty.json");
}

/// Three apply rows with mixed status — locks the table renderer's header
/// row, column ordering, and status-string mapping.
#[test]
fn log_multi_row_human() {
    let (state_dir, _ids) = log_history_setup(&[
        ("alpha", ApplyStatus::Success, Some("1 file applied")),
        ("beta", ApplyStatus::Partial, Some("2 ok, 1 failed")),
        ("gamma", ApplyStatus::Failed, None),
    ]);

    let (printer, cap) = Printer::for_test_doc();

    cmd_log(&printer, 10, None, Some(state_dir.path())).unwrap();
    drop(printer);

    let normalized = normalize_timestamps(&cap.human());
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "log/multi_row.txt", &stripped);
}

/// `--show-output <apply_id>` against an apply whose journal entries carry
/// `script_output` — locks the heading, per-entry section header, and the
/// streaming status_simple lines.
#[test]
fn log_show_output_happy_human() {
    let (state_dir, apply_id) = log_show_output_setup(&[
        (
            "scripts",
            "script",
            "script:pre:hello",
            Some("hello world\nsecond line"),
        ),
        (
            "scripts",
            "script",
            "script:post:bye",
            Some("goodbye world"),
        ),
    ]);

    let (printer, cap) = Printer::for_test_doc();

    cmd_log(&printer, 10, Some(apply_id), Some(state_dir.path())).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "log/show_output_happy.txt",
        &stripped,
    );
}

/// `--show-output <apply_id>` against an apply whose journal entries exist
/// but none carries `script_output` — locks the "No script output captured"
/// status emitted after the Entries section closes.
#[test]
fn log_show_output_empty_human() {
    let (state_dir, apply_id) = log_show_output_setup(&[
        ("files", "file", "file:create:/tmp/a", None),
        ("files", "file", "file:create:/tmp/b", None),
    ]);

    let (printer, cap) = Printer::for_test_doc();

    cmd_log(&printer, 10, Some(apply_id), Some(state_dir.path())).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "log/show_output_empty.txt",
        &stripped,
    );
}

/// `--show-output <apply_id>` against an apply that exists but has zero
/// journal entries (`journal_begin` was never called for this apply) —
/// locks the buffered `heading + status + with_data` Doc emitted on the
/// `entries.is_empty()` branch of `cmd_log_show_output`.
#[test]
fn log_show_output_no_journal_human() {
    let (state_dir, apply_id) = log_show_output_no_journal_setup();

    let (printer, cap) = Printer::for_test_doc();

    cmd_log(&printer, 10, Some(apply_id), Some(state_dir.path())).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "log/show_output_no_journal.txt",
        &stripped,
    );
}

/// `--show-output <apply_id>` happy path — locks the JSON shape of
/// `LogShowOutputOutput` carried via `Doc::with_data` on the final emit.
/// Captures from a real `cmd_log` run so the snapshot pins the runtime
/// `with_data` payload, not a hand-rolled struct.
#[test]
fn log_show_output_happy_json() {
    let (state_dir, apply_id) = log_show_output_setup(&[
        (
            "scripts",
            "script",
            "script:pre:hello",
            Some("hello world\nsecond line"),
        ),
        (
            "scripts",
            "script",
            "script:post:bye",
            Some("goodbye world"),
        ),
    ]);

    let (printer, cap) = Printer::for_test_doc();

    cmd_log(&printer, 10, Some(apply_id), Some(state_dir.path())).unwrap();
    drop(printer);

    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "log/show_output_happy.json");
}

// ─────────────────────────────────────────────────────
// snapshot helpers
// ─────────────────────────────────────────────────────

/// Replace ISO-8601 timestamps (e.g. `2026-05-17T14:23:01Z` —
/// `cfgd_core::utc_now_iso8601()` shape) with a stable placeholder so the
/// multi-row golden is host-stable. Implemented as a character-window match
/// to avoid pulling regex into the test crate.
fn normalize_timestamps(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        if i + 20 <= chars.len() && is_iso8601_window(&chars[i..i + 20]) {
            out.push_str("<TIMESTAMP>");
            i += 20;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Match `YYYY-MM-DDTHH:MM:SSZ` exactly.
fn is_iso8601_window(w: &[char]) -> bool {
    w.len() == 20
        && w[..4].iter().all(|c| c.is_ascii_digit())
        && w[4] == '-'
        && w[5..7].iter().all(|c| c.is_ascii_digit())
        && w[7] == '-'
        && w[8..10].iter().all(|c| c.is_ascii_digit())
        && w[10] == 'T'
        && w[11..13].iter().all(|c| c.is_ascii_digit())
        && w[13] == ':'
        && w[14..16].iter().all(|c| c.is_ascii_digit())
        && w[16] == ':'
        && w[17..19].iter().all(|c| c.is_ascii_digit())
        && w[19] == 'Z'
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

//! Snapshot tests for `cfgd checkin`.
//!
//! `cmd_checkin` posts to a real device gateway over HTTP, which is
//! disproportionate to fixture for the buffered Doc shape under test —
//! `build_checkin_doc` is invoked directly with synthetic `CheckinOutput`
//! payloads (`happy`, `drift_reported`, `no_drift`, `server_pushed_config`).
//! The bridge invariant uses synthetic content per the F3 README §
//! "Bridge synthetic exception". End-to-end client behavior is exercised
//! by existing tests in `crates/cfgd-core/src/server_client/`.
//! Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test checkin_v2_snapshots

use std::path::Path;

use cfgd::cli::checkin::build_checkin_doc;
use cfgd::cli::output_types::CheckinOutput;
use cfgd_core::output_v2::{Doc, Printer, Role};
use pretty_assertions::assert_eq;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_output() -> CheckinOutput {
    CheckinOutput {
        server_status: "ok".to_string(),
        config_changed: false,
        drift_count: 0,
        drift_status: "no_drift".to_string(),
        server_pushed_config: false,
    }
}

/// Buffered payload-only Doc for the happy path — no drift, no pushed config.
#[test]
fn checkin_happy_human() {
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.heading("Checkin");
    v2_printer.kv("Server status", "ok");
    v2_printer.kv("Config changed", "false");
    v2_printer.status_simple(Role::Info, "No drift to report");
    v2_printer.emit(build_checkin_doc(&happy_output()));
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "checkin/happy.txt", &stripped);
}

/// JSON payload roundtrip — CheckinOutput shape via build_checkin_doc + cap.json().
#[test]
fn checkin_happy_json() {
    let output = happy_output();
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.emit(build_checkin_doc(&output));
    drop(v2_printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("checkin doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(CheckinOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "checkin/happy.json");
}

/// Drift > 0, report succeeded — the streaming "Drift report" section closes
/// with an Ok status carrying the count.
#[test]
fn checkin_drift_reported_human() {
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.heading("Checkin");
    v2_printer.kv("Server status", "ok");
    v2_printer.kv("Config changed", "false");
    {
        let sp = v2_printer.spinner("Reporting drift");
        sp.finish_ok("3 drift items reported");
    }
    v2_printer.emit(build_checkin_doc(&CheckinOutput {
        server_status: "ok".to_string(),
        config_changed: false,
        drift_count: 3,
        drift_status: "drift_reported".to_string(),
        server_pushed_config: false,
    }));
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "checkin/drift_reported.txt",
        &stripped,
    );
}

/// Drift count zero — the no-drift branch emits `Role::Info`, not `Role::Ok`
/// (T6 Info-on-zero pattern).
#[test]
fn checkin_no_drift_human() {
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.heading("Checkin");
    v2_printer.kv("Server status", "ok");
    v2_printer.kv("Config changed", "false");
    v2_printer.status_simple(Role::Info, "No drift to report");
    v2_printer.emit(build_checkin_doc(&happy_output()));
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "checkin/no_drift.txt", &stripped);
}

/// Server pushed a desired config — `Role::Warn` status precedes the nested
/// "Server config" section so the urgency carries (T6 manual-review pattern).
#[test]
fn checkin_server_pushed_config_human() {
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.heading("Checkin");
    v2_printer.kv("Server status", "ok");
    v2_printer.kv("Config changed", "true");
    v2_printer.status_simple(Role::Warn, "Server pushed desired config");
    {
        let push_sec = v2_printer.section("Server config");
        push_sec.status_simple(Role::Ok, "Saved to <PATH>");
        push_sec.status_simple(
            Role::Info,
            "Run 'cfgd apply --dry-run' to preview changes, then 'cfgd apply'",
        );
    }
    v2_printer.status_simple(Role::Info, "No drift to report");
    v2_printer.emit(build_checkin_doc(&CheckinOutput {
        server_status: "ok".to_string(),
        config_changed: true,
        drift_count: 0,
        drift_status: "no_drift".to_string(),
        server_pushed_config: true,
    }));
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "checkin/server_pushed_config.txt",
        &stripped,
    );
}

/// Bridge invariant: streaming "Checkin" section drops, then a synthetic
/// buffered status emits — combined human surface contains exactly one
/// blank line at the transition. The bridge synthetic adds a status that
/// real `cmd_checkin` does not emit (production's buffered Doc is
/// payload-only); per the F3 README bridge-synthetic exception, deterministic
/// minimal content on both sides is preferred over matching the real shape.
#[test]
fn checkin_bridge_one_blank_line() {
    let (v2_printer, cap) = Printer::for_test_doc();

    v2_printer.heading("Checkin");
    {
        let net_sec = v2_printer.section("Checkin");
        net_sec.status_simple(Role::Ok, "server status: ok");
    }

    let doc = Doc::new()
        .status(Role::Ok, "Checkin complete")
        .with_data(&CheckinOutput {
            server_status: "ok".to_string(),
            config_changed: false,
            drift_count: 1,
            drift_status: "drift_reported".to_string(),
            server_pushed_config: false,
        });
    v2_printer.emit(doc);
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

    assert_snapshot(Path::new(SNAPSHOT_ROOT), "checkin/bridge.txt", &captured);
}

// ─────────────────────────────────────────────────────
// snapshot helpers
// ─────────────────────────────────────────────────────

fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    pretty_assertions::assert_eq!(actual, &expected, "snapshot mismatch: {name}");
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

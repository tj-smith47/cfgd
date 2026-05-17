//! Snapshot tests for `cfgd enroll`.
//!
//! Goldens live under `tests/output_snapshots/enroll/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test enroll_v2_snapshots
//!
//! `cmd_enroll` orchestrates an HTTP round-trip to the device gateway. The
//! live in-module tests under `cli/init/tests.rs::enroll_mockito` already
//! drive that orchestration via mockito; these snapshots exercise the pure
//! `build_enroll_final_doc` builder against typed fixtures so the human
//! rendering and JSON payload stay locked in without restanding a mock
//! server here.

use std::path::Path;

use cfgd::cli::init::{EnrollOutput, build_enroll_error_doc, build_enroll_final_doc};
use cfgd_core::output_v2::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_fixture() -> EnrollOutput {
    EnrollOutput {
        server_url: "https://gateway.example.com".into(),
        device_id: "host-abc-001".into(),
        username: "alice".into(),
        team: Some("platform".into()),
    }
}

fn no_team_fixture() -> EnrollOutput {
    EnrollOutput {
        server_url: "https://gateway.example.com".into(),
        device_id: "host-abc-001".into(),
        username: "alice".into(),
        team: None,
    }
}

#[test]
fn enroll_final_doc_human() {
    let output = happy_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_enroll_final_doc(&output));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "enroll/final.txt");
}

#[test]
fn enroll_next_steps_section_lists_four_commands() {
    // Pins the bullets — drift in the line set would silently degrade the
    // first-run UX without firing the human snapshot test (whose diff would
    // surface the change but not gate the line count).
    let output = happy_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_enroll_final_doc(&output));
    drop(printer);
    let human = cap.human();
    assert!(human.contains("Next Steps"), "got:\n{human}");
    for cmd in [
        "cfgd checkin",
        "cfgd apply --dry-run",
        "cfgd apply",
        "cfgd daemon install",
    ] {
        assert!(
            human.contains(cmd),
            "missing next-step `{cmd}` in:\n{human}"
        );
    }
}

#[test]
fn enroll_not_found_method_human() {
    // Pins the not-found Doc shape emitted when the server reports
    // bootstrap-token enrollment but the CLI was invoked without --token.
    // Mirrors the F1 not-found pattern: hint + with_data envelope, no
    // Role::Fail status (main.rs renders the error string).
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_enroll_error_doc(
        "method_mismatch",
        serde_json::json!({
            "serverUrl": "https://gateway.example.com",
            "serverMethod": "token",
        }),
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "enroll/not_found_method.txt");
}

#[test]
fn enroll_no_team_human() {
    let output = no_team_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_enroll_final_doc(&output));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "enroll/no_team.txt");
}

#[test]
fn enroll_json_payload_shape() {
    // -o json must surface exactly the EnrollOutput shape. Pins the schema
    // for structured consumers — `team` is omitted when None (serde
    // skip_serializing_if), included otherwise.
    let output = happy_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_enroll_final_doc(&output));
    drop(printer);
    let actual = cap.json().expect("doc carries an EnrollOutput payload");
    let expected = serde_json::to_value(&output).unwrap();
    pretty_assertions::assert_eq!(
        actual,
        expected,
        "enroll -o json must serialize exactly EnrollOutput"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "enroll/final.json");
}

#[test]
fn enroll_no_team_json_omits_team_key() {
    let output = no_team_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_enroll_final_doc(&output));
    drop(printer);
    let actual = cap.json().expect("doc carries payload");
    assert!(
        actual.get("team").is_none(),
        "team must be skipped when None, got: {actual}"
    );
}

//! Snapshot tests for `cfgd verify`.
//!
//! Goldens live under `tests/output_snapshots/verify/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test verify_v2_snapshots
//!
//! The live `cmd_verify` runs the reconciler against real package managers and
//! configurators. To keep snapshots stable across hosts, these tests drive
//! `build_verify_doc` with hand-crafted `VerifyOutput` fixtures.

use std::path::Path;

use cfgd::cli::verify::{VerifyOutput, build_verify_doc};
use cfgd_core::output_v2::Printer;
use cfgd_core::reconciler::VerifyResult;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn pkg_ok(name: &str) -> VerifyResult {
    VerifyResult {
        resource_type: "package".into(),
        resource_id: name.into(),
        expected: "installed".into(),
        actual: "installed".into(),
        matches: true,
    }
}

fn sysctl_drift(key: &str, want: &str, have: &str) -> VerifyResult {
    VerifyResult {
        resource_type: "sysctl".into(),
        resource_id: key.into(),
        expected: want.into(),
        actual: have.into(),
        matches: false,
    }
}

fn ok_fixture() -> VerifyOutput {
    let results = vec![pkg_ok("curl"), pkg_ok("ripgrep")];
    let pass_count = results.iter().filter(|r| r.matches).count();
    let fail_count = results.iter().filter(|r| !r.matches).count();
    VerifyOutput {
        results,
        pass_count,
        fail_count,
    }
}

fn drift_fixture() -> VerifyOutput {
    let results = vec![
        pkg_ok("curl"),
        sysctl_drift("net.ipv4.ip_forward", "1", "0"),
    ];
    let pass_count = results.iter().filter(|r| r.matches).count();
    let fail_count = results.iter().filter(|r| !r.matches).count();
    VerifyOutput {
        results,
        pass_count,
        fail_count,
    }
}

fn empty_fixture() -> VerifyOutput {
    VerifyOutput {
        results: Vec::new(),
        pass_count: 0,
        fail_count: 0,
    }
}

#[test]
fn verify_ok_human() {
    let output = ok_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_verify_doc(&output));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "verify/ok.txt");
}

#[test]
fn verify_ok_json() {
    let output = ok_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_verify_doc(&output));
    drop(printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("doc captured json");
    pretty_assertions::assert_eq!(
        actual,
        expected,
        "verify -o json must serialize exactly VerifyOutput (regression anchor)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "verify/ok.json");
}

#[test]
fn verify_drift_human() {
    let output = drift_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_verify_doc(&output));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "verify/drift.txt");
}

#[test]
fn verify_empty_human() {
    let output = empty_fixture();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_verify_doc(&output));
    drop(printer);
    let human = cap.human();
    assert!(
        human.contains("No managed resources to verify"),
        "empty output must include info status, got:\n{human}"
    );
    assert!(
        !human.contains("Resources"),
        "empty output must omit Resources section, got:\n{human}"
    );
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "verify/empty.txt");
}

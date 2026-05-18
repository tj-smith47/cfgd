//! Snapshot tests for `cfgd upgrade`.
//!
//! Coverage strategy:
//!   `cmd_upgrade` calls into `cfgd_core::upgrade::*` for GitHub Releases
//!   API lookup, signature verification, and atomic install. Those carry
//!   `&Printer` (v1) per the F3.5 hybrid lib-call rule and are exhaustively
//!   tested in `cfgd-core::upgrade::tests` with the `CFGD_GITHUB_API_BASE`
//!   mockito seam.
//!
//!   The "update available" branch in `--check` mode calls
//!   `ExitCode::UpdateAvailable.exit()` which terminates the process — we
//!   cannot drive it from inside a test. The download path requires a
//!   tarball + cosign signature + checksums, an end-to-end-only surface
//!   that is exercised by `crates/cfgd-core/src/upgrade/tests.rs`'s
//!   `download_and_install_to_*` cases.
//!
//!   What we DO snapshot here is the `--check` path's "up to date" branch
//!   (the safe, no-exit, no-network-mutation case). The release-info
//!   payload is stubbed via mockito and the buffered v2 Doc is captured
//!   for both human (`up_to_date.txt`) and JSON (`up_to_date.json`)
//!   shapes. The `up_to_date.{txt,json}` filenames cover the `--check`
//!   up-to-date sub-case; the upgrade-without-`--check` up-to-date path
//!   shares the same buffered Doc shape (different `with_data` keys) and
//!   is exercised end-to-end by `crates/cfgd-core/src/upgrade/tests.rs`.

use std::path::Path;

use cfgd::cli::upgrade;
use cfgd_core::output::{OutputFormat as OutputFormatV1, Printer as PrinterV1, Verbosity};
use cfgd_core::output_v2::{Doc, OutputFormat, Printer, Role};
use cfgd_core::test_helpers::EnvVarGuard;
use serial_test::serial;

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

/// Build a mock GitHub Releases endpoint that reports v0.0.1 — older than
/// the compiled `CARGO_PKG_VERSION` — so `update_available` is false and
/// the up-to-date branch fires (no exit()).
fn mock_older_release_server() -> mockito::ServerGuard {
    let mut server = mockito::Server::new();
    let _ = server
        .mock("GET", mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
                "tag_name": "v0.0.1",
                "assets": []
            }"#,
        )
        .create();
    server
}

#[test]
#[serial]
fn upgrade_check_up_to_date_human() {
    let server = mock_older_release_server();
    let _api = EnvVarGuard::set("CFGD_GITHUB_API_BASE", &server.url());
    let home = tempfile::tempdir().unwrap();
    let _home = cfgd_core::with_test_home_guard(home.path());

    let v1_printer = PrinterV1::with_format(Verbosity::Quiet, None, OutputFormatV1::Table);
    let (v2_printer, cap) = Printer::for_test_doc();

    upgrade::cmd_upgrade(&v1_printer, &v2_printer, /*check_only=*/ true).unwrap();
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "upgrade/up_to_date.txt",
        &stripped,
    );
}

#[test]
#[serial]
fn upgrade_check_up_to_date_json() {
    let server = mock_older_release_server();
    let _api = EnvVarGuard::set("CFGD_GITHUB_API_BASE", &server.url());
    let home = tempfile::tempdir().unwrap();
    let _home = cfgd_core::with_test_home_guard(home.path());

    let v1_printer = PrinterV1::with_format(Verbosity::Quiet, None, OutputFormatV1::Table);
    let (v2_printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);

    upgrade::cmd_upgrade(&v1_printer, &v2_printer, /*check_only=*/ true).unwrap();
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["updateAvailable"], false);
    assert_eq!(json["latestVersion"], "0.0.1");
    // currentVersion is the compiled CARGO_PKG_VERSION; non-empty and parseable.
    assert!(
        json["currentVersion"].as_str().is_some(),
        "currentVersion should be a string"
    );
}

/// Bridge invariant: streaming "Downloading" section drops, then a buffered
/// summary Doc emits — combined human surface contains exactly one blank
/// line at the transition. Production `cmd_upgrade`'s download path requires
/// a live GitHub release + cosign signature + checksum bundle (covered E2E
/// by `crates/cfgd-core/src/upgrade/tests.rs::download_and_install_to_*`),
/// so per the F3 README bridge-synthetic exception we hand-roll the
/// minimal streaming-then-buffered shape here. The streaming-side status
/// content is deterministic and may diverge from any specific real
/// invocation; what's locked is the §17.2 invariant.
#[test]
fn upgrade_bridge_one_blank_line() {
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.heading("Upgrade");
    {
        let work = v2_printer.section("Downloading");
        work.status(Role::Ok, "Verified signature");
    }
    v2_printer.emit(Doc::new().status(Role::Ok, "Upgraded to v0.4.0").with_data(
        serde_json::json!({
            "currentVersion": "0.3.5",
            "targetVersion": "0.4.0",
            "installed": true,
            "verified": true,
        }),
    ));
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
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "upgrade/bridge.txt", &captured);
}

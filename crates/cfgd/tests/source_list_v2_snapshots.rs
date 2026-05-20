//! Snapshot tests for `cfgd source list`.
//!
//! Cases:
//!   - `source_list/happy.txt` — real `cmd_source_list` against a tempdir
//!     fixture with one source entry.
//!   - `source_list/happy.json` — payload-roundtrip via `build_source_list_doc`.
//!   - `source_list/empty.txt` — `cmd_source_list` against a tempdir with no
//!     sources configured.
//!   - `source_list/no_config.txt` — `cmd_source_list` against a non-existent
//!     `cfgd.yaml`.
//!   - `source_list/wide.txt` — `--wide` table layout against the same fixture.
//!
//! Goldens live under `tests/output_snapshots/source_list/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test source_list_v2_snapshots

mod common;

use std::path::Path;

use cfgd::cli::output_types::SourceListEntry;
use cfgd::cli::source::cmd_source_list;
use cfgd::cli::source::list::{build_source_list_doc, build_source_list_no_config_doc};
use cfgd_core::output::Printer;

use common::{cli_for, source_test_config_setup, source_test_config_with_source_setup};

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

fn happy_entries() -> Vec<SourceListEntry> {
    vec![SourceListEntry {
        name: "team-config".into(),
        url: "https://github.com/team/config".into(),
        priority: 100,
        version: Some("1.0.0".into()),
        status: "synced".into(),
        last_fetched: Some("2026-05-14T10:00:00Z".into()),
    }]
}

#[test]
fn source_list_happy_human() {
    let (config_dir, state_dir) = source_test_config_with_source_setup(
        "team-config",
        "https://github.com/team/config",
        "main",
        100,
    );
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_list(&cli, &printer).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "source_list/happy.txt", &stripped);
}

#[test]
fn source_list_happy_json() {
    // Payload-roundtrip — build_source_list_doc serializes &[SourceListEntry]
    // directly; this exercises the JSON envelope without disk fixtures.
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_list_doc(&entries, false));
    drop(printer);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_list/happy.json");
}

#[test]
fn source_list_empty_human() {
    // cfgd.yaml present but spec.sources is empty: emits the Config Sources
    // heading + a Role::Info "No sources configured" line.
    let (config_dir, state_dir) = source_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_source_list(&cli, &printer).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "source_list/empty.txt", &stripped);
}

#[test]
fn source_list_no_config_human() {
    // cfgd.yaml absent: emits the Config Sources heading + "No config file
    // found" Info status. Hand-rolled because the missing-config branch
    // takes a different doc shape entirely (no payload entries).
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_list_no_config_doc());
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_list/no_config.txt");
}

#[test]
fn source_list_wide_human() {
    // `--wide` table layout: drive via build_source_list_doc with wide=true.
    // for_test_doc defaults to wide=false; the build helper is the seam.
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_list_doc(&entries, true));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "source_list/wide.txt");
}

#[test]
fn source_list_payload_carries_with_data() {
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_source_list_doc(&entries, false));
    drop(printer);
    let payload = cap.json().expect("doc captured json");
    let arr = payload.as_array().expect("array payload");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "team-config");
}

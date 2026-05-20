//! Snapshot tests for `cfgd profile list`.
//!
//! Cases:
//!   - `profile_list/happy.txt` — real `cmd_profile_list` against a tempdir
//!     fixture with two profiles (default active, work inactive).
//!   - `profile_list/happy.json` — payload-roundtrip via `build_profile_list_doc`.
//!   - `profile_list/empty.txt` — `cmd_profile_list` against an empty
//!     profiles directory (Role::Info "No profiles found").
//!   - `profile_list/no_dir.txt` — profiles directory absent (Role::Warn).
//!     Uses `build_profile_list_missing_doc` directly because the warning
//!     embeds an absolute path; reproducing the on-disk shape from a
//!     tempdir would force path normalization for a single warning line.
//!   - `profile_list/wide.txt` — `--wide` table layout against the same
//!     fixture.
//!
//! Goldens live under `tests/output_snapshots/profile_list/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_list_v2_snapshots

mod common;

use std::path::Path;

use cfgd::cli::output_types::ProfileListEntry;
use cfgd::cli::profile::cmd_profile_list;
use cfgd::cli::profile::list::{build_profile_list_doc, build_profile_list_missing_doc};
use cfgd_core::output::Printer;

use common::{cli_for, profile_test_config_setup};

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

fn happy_entries() -> Vec<ProfileListEntry> {
    vec![
        ProfileListEntry {
            name: "default".into(),
            active: true,
            inherits: None,
            module_count: 0,
        },
        ProfileListEntry {
            name: "work".into(),
            active: false,
            inherits: Some("default".into()),
            module_count: 0,
        },
    ]
}

#[test]
fn profile_list_happy_human() {
    let (config_dir, state_dir) = profile_test_config_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_profile_list(&cli, &printer).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_list/happy.txt",
        &stripped,
    );
}

#[test]
fn profile_list_happy_json() {
    // Payload-roundtrip: build_profile_list_doc serializes &[ProfileListEntry]
    // directly; this exercises the JSON envelope without standing up disk fixtures.
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_doc(&entries, false));
    drop(printer);
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_list/happy.json");
}

#[test]
fn profile_list_empty_human() {
    // Empty profiles directory: real cmd_profile_list emits Role::Info
    // "No profiles found".
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec: {}\n",
    )
    .unwrap();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    cmd_profile_list(&cli, &printer).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "profile_list/empty.txt",
        &stripped,
    );
}

#[test]
fn profile_list_no_dir_human() {
    // Hand-rolled because the Doc embeds an absolute path; reproducing
    // through cmd_profile_list would require path normalization for a
    // single warning line. The Doc shape is what's pinned.
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_missing_doc(Path::new(
        "/etc/cfgd/profiles",
    )));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_list/no_dir.txt");
}

#[test]
fn profile_list_wide_human() {
    // `--wide` table layout: drive cmd_profile_list with a printer whose
    // is_wide() returns true. The Printer's wide flag is part of OutputFormat
    // handling; the simplest route is to use the build_*_doc directly since
    // cmd_profile_list reads `printer.is_wide()`. for_test_doc defaults
    // to Wide=false, so use the build helper here.
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_doc(&entries, true));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_list/wide.txt");
}

#[test]
fn profile_list_no_dir_payload_is_empty_array() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_missing_doc(Path::new("/missing")));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert!(json.is_array(), "expected array, got: {json}");
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[test]
fn profile_list_payload_carries_with_data() {
    let entries = happy_entries();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_profile_list_doc(&entries, false));
    drop(printer);
    let payload = cap.json().expect("doc captured json");
    let arr = payload.as_array().expect("array payload");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"], "default");
    assert_eq!(arr[1]["name"], "work");
}

//! Snapshot tests for `cfgd module keys list/generate/rotate`.
//!
//! Only `cmd_module_keys_list` against an empty workspace is captured here —
//! `generate` and `rotate` shell out to `cosign`, which is exercised in
//! per-command unit tests under `crates/cfgd/src/cli/module/tests.rs` against
//! a fake-cosign shim.

mod common;

use std::path::Path;

use cfgd::cli::module;
use cfgd_core::output_v2::Printer;
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

#[test]
#[serial]
fn module_keys_list_empty_human() {
    // Resolve the snapshot path BEFORE changing the cwd so the "./cosign.*"
    // lookup misses (the relative SNAPSHOT_ROOT would otherwise dangle).
    let snap_root = std::env::current_dir().unwrap().join(SNAPSHOT_ROOT);

    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();

    // Use HOME override so ~/.cfgd lookup also misses.
    let home_guard =
        cfgd_core::test_helpers::EnvVarGuard::set("HOME", tmp.path().to_str().unwrap());

    let (v2_printer, cap) = Printer::for_test_doc();
    module::cmd_module_keys_list(&v2_printer).unwrap();
    drop(v2_printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(&snap_root, "module_keys/list_empty.txt", &stripped);

    let json = cap.json().expect("doc captured json");
    assert!(json.is_array(), "list payload is a Vec<KeyListEntry>");

    std::env::set_current_dir(original).unwrap();
    drop(home_guard);
}

#[test]
#[serial]
fn module_keys_list_empty_json() {
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let home_guard =
        cfgd_core::test_helpers::EnvVarGuard::set("HOME", tmp.path().to_str().unwrap());

    let (v2_printer, cap) = Printer::for_test_doc();
    module::cmd_module_keys_list(&v2_printer).unwrap();
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    let arr = json.as_array().expect("Vec payload");
    assert_eq!(arr.len(), 0, "no keys present in empty workspace");

    std::env::set_current_dir(original).unwrap();
    drop(home_guard);
}

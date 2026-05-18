//! Snapshot tests for `cfgd profile create`.
//!
//! Cases:
//!   - `profile_create/happy.{txt,json}` — flag-driven create with no
//!     inherits and no modules.
//!   - `profile_create/inherits.txt` — create with `--inherits a,b` and
//!     `--modules nvim`; both kvs render under the success status.
//!
//! `cmd_profile_create` builds its emitted Doc inline; these tests
//! reconstruct the same Doc shape so the renderer coverage is tested
//! without needing a real tempdir + file copy machinery. The
//! `cmd_profile_create` body itself is exercised by the in-crate unit
//! tests in `src/cli/profile/tests.rs`.
//!
//! Goldens live under `tests/output_snapshots/profile_create/`. Regenerate
//! with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test profile_create_v2_snapshots

use std::path::Path;

use cfgd_core::output_v2::{Doc, Printer, Role};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn create_doc(name: &str, path: &str, inherits: &[&str], modules: &[&str]) -> Doc {
    let mut doc = Doc::new().status(Role::Ok, format!("Created profile '{}' at {}", name, path));
    if !inherits.is_empty() {
        doc = doc.kv("Inherits", inherits.join(", "));
    }
    if !modules.is_empty() {
        doc = doc.kv("Modules", modules.join(", "));
    }
    doc.hint(format!("Activate with: cfgd profile switch {}", name))
        .with_data(serde_json::json!({
            "name": name,
            "path": path,
            "inherits": inherits,
            "modules": modules,
        }))
}

#[test]
fn profile_create_happy_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Create Profile: newprof");
    printer.emit(create_doc(
        "newprof",
        "/etc/cfgd/profiles/newprof.yaml",
        &[],
        &[],
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_create/happy.txt");
}

#[test]
fn profile_create_happy_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(create_doc(
        "newprof",
        "/etc/cfgd/profiles/newprof.yaml",
        &[],
        &[],
    ));
    drop(printer);
    let json = cap.json().expect("doc captured json");
    assert_eq!(json["name"], "newprof");
    assert_eq!(json["path"], "/etc/cfgd/profiles/newprof.yaml");
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_create/happy.json");
}

#[test]
fn profile_create_inherits_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.heading("Create Profile: child");
    printer.emit(create_doc(
        "child",
        "/etc/cfgd/profiles/child.yaml",
        &["default", "shared"],
        &["nvim"],
    ));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "profile_create/inherits.txt");
}

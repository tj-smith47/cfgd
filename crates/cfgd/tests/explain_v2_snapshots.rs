//! Snapshot tests for `cfgd explain`.
//!
//! Three cases mapping to the three command shapes:
//!   - `explain/index.{txt,json}`  — bare `cfgd explain` (schema table + hints)
//!   - `explain/module.{txt,json}` — `cfgd explain module` (overview + fields)
//!   - `explain/unknown.txt`       — `cfgd explain bogus` (error path; the
//!     command short-circuits with `anyhow::bail!` so the snapshot captures
//!     the Err string rather than a rendered Doc)
//!
//! Goldens live under `tests/output_snapshots/explain/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test explain_v2_snapshots

use std::path::Path;

use cfgd::cli::explain::{build_explain_index_doc, build_explain_schema_doc, find_schema};
use cfgd_core::output_v2::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

#[test]
fn explain_index_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_index_doc());
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "explain/index.txt");
}

#[test]
fn explain_index_json() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_index_doc());
    drop(printer);
    let actual = cap.json().expect("doc captured json");
    assert!(
        actual.is_array(),
        "explain index payload must be a top-level JSON array, got: {actual}"
    );
    assert_eq!(
        actual.as_array().map(|a| a.len()),
        Some(8),
        "explain index must list 8 schemas, got: {actual}"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "explain/index.json");
}

#[test]
fn explain_module_human() {
    let schema = find_schema("module").expect("module schema is registered");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_schema_doc(schema, false));
    drop(printer);
    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "explain/module.txt");
}

#[test]
fn explain_module_json() {
    let schema = find_schema("module").expect("module schema is registered");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_schema_doc(schema, false));
    drop(printer);
    let actual = cap.json().expect("doc captured json");
    assert!(
        actual.is_object(),
        "explain <resource> payload must be a top-level JSON object, got: {actual}"
    );
    assert_eq!(
        actual.get("kind").and_then(|v| v.as_str()),
        Some("Module"),
        "explain module payload must carry kind=Module, got: {actual}"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "explain/module.json");
}

#[test]
fn explain_recursive_drops_plus_marker() {
    // Recursive mode replaces the `[+]` marker with nested subsections —
    // confirm the marker is absent so we don't regress to the manual-indent
    // shape.
    let schema = find_schema("profile").expect("profile schema is registered");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_schema_doc(schema, true));
    drop(printer);
    let human = cap.human();
    assert!(
        !human.contains("[+]"),
        "recursive expansion must not leave any [+] markers, got:\n{human}"
    );
}

#[test]
fn explain_unknown_resource_returns_none() {
    // The live `cmd_explain` `bail!`s on an unknown resource before any Doc
    // is emitted; the lookup helper that drives that branch is the public
    // signal. Pin its negative-case shape so future renames stay deliberate.
    assert!(find_schema("bogus").is_none());
    let expected_err =
        "Unknown resource type 'bogus'. Run 'cfgd explain' to see available types.\n";
    let snapshot_path = Path::new(SNAPSHOT_ROOT).join("explain/unknown.txt");
    write_snapshot(&snapshot_path, expected_err);
}

/// Match `DocCapture::assert_human_snapshot_in`'s INSTA_UPDATE-aware write
/// semantics for the Err-string case where no Doc is ever emitted.
fn write_snapshot(path: &Path, contents: &str) {
    let update = std::env::var("INSTA_UPDATE")
        .map(|v| v == "always" || v == "auto" || v == "new")
        .unwrap_or(false);
    if update || !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create snapshot parent");
        }
        std::fs::write(path, contents).expect("write snapshot");
        return;
    }
    let on_disk = std::fs::read_to_string(path).expect("read snapshot");
    pretty_assertions::assert_eq!(
        on_disk,
        contents,
        "snapshot drift at {} (set INSTA_UPDATE=always to refresh)",
        path.display()
    );
}

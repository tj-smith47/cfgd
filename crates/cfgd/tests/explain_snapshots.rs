//! Snapshot tests for `cfgd explain`.
//!
//! Three cases mapping to the three command shapes:
//!   - `explain/index.{txt,json}`  — bare `cfgd explain` (schema table + hints)
//!   - `explain/module.{txt,json}` — `cfgd explain module` (overview + fields)
//!   - `explain/profile-packages-brew.{txt,json}` — `cfgd explain profile.spec.packages.brew`
//!     (a union field: the shapes under `Variants`, the one object shape's
//!     fields under `Fields`, a legend whose placeholder resolves, and the
//!     field page's own `Docs` row / `docs`+`docsUrl` pair)
//!   - `explain/unknown.txt`       — `cfgd explain bogus` (error path; the
//!     command short-circuits with `anyhow::bail!` so the snapshot captures
//!     the Err string rather than a rendered Doc)
//!
//! Goldens live under `tests/output_snapshots/explain/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test explain_snapshots

use std::path::Path;

use cfgd::cli::explain::{
    build_explain_drilldown_doc, build_explain_index_doc, build_explain_schema_doc, find_schema,
    resolve_field_path,
};
use cfgd_core::output::test_capture::assert_snapshot_at;
use cfgd_core::output::{DocCapture, Printer, strip_ansi};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

/// Assert a human golden with the running cfgd version folded to `<VERSION>`.
/// The `Docs` row carries a release-pinned URL, so an unfolded golden would
/// have to be re-cut on every version bump.
fn assert_human(cap: &DocCapture, name: &str) {
    let human = strip_ansi(&cap.human());
    let actual = cfgd_core::normalize_cfgd_version(&human, env!("CARGO_PKG_VERSION"));
    assert_snapshot_at(Path::new(SNAPSHOT_ROOT), name, &actual);
}

/// The same fold over the `-o json` payload, whose `docsUrl` is the same
/// release-pinned URL.
fn assert_json(cap: &DocCapture, name: &str) {
    let payload = serde_json::to_string_pretty(&cap.json().expect("doc captured json"))
        .expect("payload serializes");
    let actual = cfgd_core::normalize_cfgd_version(&payload, env!("CARGO_PKG_VERSION"));
    assert_snapshot_at(Path::new(SNAPSHOT_ROOT), name, &actual);
}

#[test]
fn explain_index_human() {
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_index_doc());
    drop(printer);
    assert_human(&cap, "explain/index.txt");
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
        Some(10),
        "explain index must list 10 schemas (9 registry kinds incl. Module CRD + TeamConfig), got: {actual}"
    );
    assert_json(&cap, "explain/index.json");
}

#[test]
fn explain_module_human() {
    let schema = find_schema("module").expect("module schema is registered");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_schema_doc(schema, false));
    drop(printer);
    assert_human(&cap, "explain/module.txt");
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
    assert_json(&cap, "explain/module.json");
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
fn explain_recursive_tree_human() {
    // Pin the full recursive (`--recursive`) field tree, not just the absence
    // of `[+]` markers: every nested level — `packages.brew.taps`, the deeply
    // nested override blocks — must render at a stable indent so a schemars
    // walk regression (a dropped child, a re-ordered field) shows up as a
    // diff. Profile is the representative case because its schema nests the
    // deepest. The walk is pure and `serde_json` keys sort, so this is
    // deterministic.
    let schema = find_schema("profile").expect("profile schema is registered");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_schema_doc(schema, true));
    drop(printer);
    assert_human(&cap, "explain/profile-recursive.txt");
}

#[test]
fn explain_recursive_tree_json() {
    let schema = find_schema("profile").expect("profile schema is registered");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_schema_doc(schema, true));
    drop(printer);
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual.get("kind").and_then(|v| v.as_str()),
        Some("Profile"),
        "recursive explain payload must carry kind=Profile, got: {actual}"
    );
    assert_json(&cap, "explain/profile-recursive.json");
}

#[test]
fn explain_union_drilldown_human() {
    let schema = find_schema("profile").expect("profile schema is registered");
    let path = ["packages", "brew"];
    let fields = resolve_field_path(&schema.fields, &path).expect("brew resolves");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_drilldown_doc(schema, &path, fields, false));
    drop(printer);
    assert_human(&cap, "explain/profile-packages-brew.txt");
}

#[test]
fn explain_union_drilldown_json() {
    let schema = find_schema("profile").expect("profile schema is registered");
    let path = ["packages", "brew"];
    let fields = resolve_field_path(&schema.fields, &path).expect("brew resolves");
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_explain_drilldown_doc(schema, &path, fields, false));
    drop(printer);
    let actual = cap.json().expect("doc captured json");
    assert_eq!(
        actual.get("path").and_then(|v| v.as_str()),
        Some("profile.spec.packages.brew"),
        "drilldown payload must carry path=profile.spec.packages.brew, got: {actual}"
    );
    assert!(
        actual.get("docsUrl").and_then(|v| v.as_str()).is_some(),
        "drilldown payload must carry the same docs/docsUrl pair a kind page carries, got: {actual}"
    );
    assert_json(&cap, "explain/profile-packages-brew.json");
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
    let on_disk = std::fs::read_to_string(path)
        .expect("read snapshot")
        .replace("\r\n", "\n");
    let contents = contents.replace("\r\n", "\n");
    pretty_assertions::assert_eq!(
        on_disk,
        contents,
        "snapshot drift at {} (set INSTA_UPDATE=always to refresh)",
        path.display()
    );
}

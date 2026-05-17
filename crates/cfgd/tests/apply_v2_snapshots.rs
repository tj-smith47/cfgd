//! Snapshot tests for `cfgd apply`.
//!
//! Pins the rendered output of every shape `cmd_apply` produces. Goldens
//! live under `tests/output_snapshots/apply/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test apply_v2_snapshots
//!
//! Cases:
//!   - `apply/happy.{txt,json}` — all phases succeed (streaming preview
//!     + per-phase results + buffered ApplyOutput).
//!   - `apply/dry_run.txt`      — `--dry-run` path through
//!     `display_plan_preview_v2`.
//!   - `apply/nothing_to_do.txt`— plan is empty.
//!   - `apply/with_failures.txt`— one action fails; the failure status
//!     renders INSIDE the phase section (the §1.8 anchor under real apply
//!     data — not at column 0).
//!   - `apply/bridge.txt`       — streaming section + buffered Doc on the
//!     same `Printer`; asserts the §17.2 one-blank-line invariant
//!     programmatically.

use std::collections::HashMap;
use std::path::Path;

use cfgd::cli::apply::build_apply_doc;
use cfgd::cli::output_types::ApplyOutput;
use cfgd_core::output_v2::{Doc, Printer, Role};
use pretty_assertions::assert_eq;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_output() -> ApplyOutput {
    let mut source_commits = HashMap::new();
    source_commits.insert("team-config".to_string(), "abc1234".to_string());
    ApplyOutput {
        status: "success".to_string(),
        apply_id: Some(42),
        succeeded: 3,
        failed: 0,
        source_commits,
    }
}

fn with_failures_output() -> ApplyOutput {
    ApplyOutput {
        status: "partial".to_string(),
        apply_id: Some(43),
        succeeded: 1,
        failed: 1,
        source_commits: HashMap::new(),
    }
}

#[test]
fn apply_happy_human() {
    let (v2_printer, cap) = Printer::for_test_doc();

    v2_printer.heading("Apply");
    v2_printer.kv_block([("Config", "/etc/cfgd.yaml"), ("Profile", "default")]);

    // Plan preview — mirrors cmd_apply's streaming section shape: each
    // phase opens a fresh nested section that drops before the next
    // phase opens, so phases are siblings, not nested.
    {
        let preview = v2_printer.section("Plan preview");
        {
            let files = preview.section("Files");
            files.bullet("create /etc/hosts");
        }
        {
            let packages = preview.section("Packages");
            packages.bullet("install nodejs");
            packages.bullet("install ripgrep");
        }
    }

    v2_printer.status_simple(Role::Ok, "Apply complete — 3 action(s) succeeded");
    v2_printer.emit(build_apply_doc(&happy_output()));
    drop(v2_printer);

    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "apply/happy.txt");
}

#[test]
fn apply_happy_json() {
    let output = happy_output();
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.emit(build_apply_doc(&output));
    drop(v2_printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("apply doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(ApplyOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "apply/happy.json");
}

#[test]
fn apply_dry_run_human() {
    // Mirrors the --dry-run path's shape after display_plan_preview_v2:
    // heading + plan-table sections + "N action(s) planned" status.
    let (v2_printer, cap) = Printer::for_test_doc();

    v2_printer.heading("Plan");
    v2_printer.kv_block([("Config", "/etc/cfgd.yaml"), ("Profile", "default")]);

    {
        let phase = v2_printer.section("Phase: Files");
        phase.bullet("create /etc/hosts");
    }
    {
        let phase = v2_printer.section("Phase: Packages");
        phase.bullet("install nodejs");
    }
    v2_printer.status_simple(Role::Info, "2 action(s) planned");
    drop(v2_printer);

    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "apply/dry_run.txt");
}

#[test]
fn apply_nothing_to_do_human() {
    let (v2_printer, cap) = Printer::for_test_doc();

    v2_printer.heading("Apply");
    v2_printer.kv_block([("Config", "/etc/cfgd.yaml"), ("Profile", "default")]);
    v2_printer.status_simple(Role::Ok, "Nothing to do — everything is up to date");
    v2_printer.emit(Doc::new().with_data(ApplyOutput::nothing_to_do()));
    drop(v2_printer);

    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "apply/nothing_to_do.txt");
}

#[test]
fn apply_with_failures_human() {
    // §1.8 anchor under real apply data: the failure Status renders INSIDE
    // the phase SectionGuard — at the phase's indent, NOT at column 0.
    // Bucket-g `apply_results_stay_indented` proves it at the renderer
    // level; this snapshot proves it under the shape `cmd_apply` produces.
    let (v2_printer, cap) = Printer::for_test_doc();

    v2_printer.heading("Apply");
    v2_printer.kv_block([("Config", "/etc/cfgd.yaml"), ("Profile", "default")]);

    {
        let preview = v2_printer.section("Plan preview");
        {
            let files = preview.section("Files");
            files.bullet("create /etc/hosts");
            files.bullet("create /tmp/foo");
        }
    }

    {
        let work = v2_printer.section("Files");
        work.status(Role::Ok, "Wrote /etc/hosts");
        work.status(Role::Fail, "Failed /tmp/foo")
            .detail("permission denied");
    }

    v2_printer.status_simple(
        Role::Warn,
        "Apply partially complete — 1 succeeded, 1 failed",
    );
    v2_printer.emit(build_apply_doc(&with_failures_output()));
    drop(v2_printer);

    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "apply/with_failures.txt");
}

#[test]
fn apply_bridge_one_blank_line() {
    // §17.2 bridge invariant: when the streaming SectionGuard drops, the
    // renderer auto-emits one blank line. The buffered Doc that follows
    // respects the no-leading-blank rule, so the combined human surface
    // has exactly one blank line at the transition between the last
    // streaming line and the first buffered line.
    let (v2_printer, cap) = Printer::for_test_doc();

    v2_printer.heading("Apply");
    {
        let work = v2_printer.section("Files");
        work.status(Role::Ok, "Wrote /etc/hosts");
    }
    v2_printer.status_simple(Role::Ok, "Apply complete — 1 action(s) succeeded");

    // Buffered Doc carrying both a human section and the ApplyOutput payload.
    // Combining both surfaces is what the bridge invariant guards.
    let doc = Doc::new()
        .section("Source Commits", |s| s.bullet("team-config @ abc1234"))
        .with_data(happy_output());
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

    assert_snapshot(Path::new(SNAPSHOT_ROOT), "apply/bridge.txt", &captured);
}

// ─────────────────────────────────────────────────────
// snapshot helpers — local to keep tests/output_snapshots/ self-contained
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

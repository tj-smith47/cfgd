//! Snapshot tests for `cfgd apply`.
//!
//! Pins the rendered output of every shape `cmd_apply` produces. Goldens
//! live under `tests/output_snapshots/apply/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test apply_snapshots
//!
//! Cases:
//!   - `apply/happy.{txt,json}` — all phases succeed. Snapshot captures
//!     real `cmd_apply` output (streaming preview + per-phase results +
//!     buffered ApplyOutput) against a tempdir-backed profile; the JSON
//!     case exercises `build_apply_doc` directly.
//!   - `apply/dry_run.txt`      — `--dry-run` path through real
//!     `cmd_apply` (so `display_plan_preview` drift is caught).
//!   - `apply/nothing_to_do.txt`— plan is empty.
//!   - `apply/with_failures.txt`— one file action fails (parent path is
//!     a regular file, so `create_dir_all` errors at apply time). The
//!     failure status renders INSIDE the phase section — the
//!     indent-invariant anchor under real apply data, not at column 0.
//!   - `apply/bridge.txt`       — streaming section + buffered Doc on the
//!     same `Printer`; asserts the bridge invariant (one blank line
//!     between streaming and buffered) programmatically.

mod common;

use std::collections::HashMap;
use std::path::Path;

use cfgd::cli::apply::{build_apply_doc, cmd_apply};
use cfgd::cli::output_types::ApplyOutput;
use cfgd_core::output::{Doc, Printer, Role};
use pretty_assertions::assert_eq;

use common::{
    apply_args, apply_args_dry_run, cli_for, profile_with_one_failure_setup, tiny_profile_setup,
};

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

/// Replace tempdir-rooted paths with stable placeholders so goldens are
/// host-stable. `cmd_apply` embeds the config-file path and target file
/// paths into its output (kv block + per-action lines).
fn normalize_tempdir_paths(raw: &str, config_dir: &Path, extra_paths: &[(&Path, &str)]) -> String {
    let mut out = raw.to_string();
    // Longest first so substring overlap doesn't swallow the shorter one.
    let cfg_file = config_dir.join("cfgd.yaml");
    out = out.replace(
        &cfg_file.to_string_lossy().to_string(),
        "<CONFIG_DIR>/cfgd.yaml",
    );
    for (path, placeholder) in extra_paths {
        out = out.replace(&path.to_string_lossy().to_string(), placeholder);
    }
    out = out.replace(&config_dir.to_string_lossy().to_string(), "<CONFIG_DIR>");
    out
}

/// Replace ` (N.Ns)` duration suffixes (rendered by StatusBuilder.duration())
/// with a stable placeholder so the apply-summary golden is host-stable.
fn normalize_duration(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(len) = duration_span(&chars[i..]) {
            out.push_str(" (XXs)");
            i += len;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Detect a ` (N.Ns)` or ` (NN.Ns)` (etc.) suffix and return its length in chars.
/// Renderer formats `{:.1}s` so there is always exactly one fractional digit.
fn duration_span(window: &[char]) -> Option<usize> {
    if window.len() < 7 || window[0] != ' ' || window[1] != '(' {
        return None;
    }
    let mut i = 2;
    let int_start = i;
    while i < window.len() && window[i].is_ascii_digit() {
        i += 1;
    }
    if i == int_start {
        return None;
    }
    if i + 3 >= window.len() {
        return None;
    }
    if window[i] != '.' || !window[i + 1].is_ascii_digit() {
        return None;
    }
    if window[i + 2] != 's' || window[i + 3] != ')' {
        return None;
    }
    Some(i + 4)
}

#[test]
fn apply_happy_human() {
    let (config_dir, state_dir, target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = apply_args();

    cmd_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    let normalized =
        normalize_tempdir_paths(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = normalize_duration(&strip_ansi(&normalized));
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "apply/happy.txt", &stripped);
}

#[test]
fn apply_happy_json() {
    // Pure data-roundtrip test on `build_apply_doc` — doesn't need to
    // stand up a reconciler.
    let output = happy_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(build_apply_doc(&output));
    drop(printer);

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
    // Real --dry-run path through cmd_apply → display_plan_preview.
    let (config_dir, state_dir, target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = apply_args_dry_run();

    cmd_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    assert!(!target.exists(), "dry-run must not create the target file");

    let normalized =
        normalize_tempdir_paths(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "apply/dry_run.txt", &stripped);
}

#[test]
fn apply_nothing_to_do_human() {
    let (printer, cap) = Printer::for_test_doc();

    printer.heading("Apply");
    printer.kv_block([("Config", "/etc/cfgd.yaml"), ("Profile", "default")]);
    printer.status_simple(Role::Ok, "Nothing to do — everything is up to date");
    printer.emit(Doc::new().with_data(ApplyOutput::nothing_to_do()));
    drop(printer);

    cap.assert_human_snapshot_in(Path::new(SNAPSHOT_ROOT), "apply/nothing_to_do.txt");
}

#[test]
fn apply_with_failures_human() {
    // Indent-invariant anchor under real apply data: the failure Status
    // renders INSIDE the phase SectionGuard — at the phase's indent, NOT
    // at column 0. The bucket-g `apply_results_stay_indented` renderer-level
    // anchor pins the invariant abstractly; this snapshot pins it under
    // cmd_apply's real shape.
    let (config_dir, state_dir, target_ok, target_fail) = profile_with_one_failure_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = apply_args();

    cmd_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    assert!(target_ok.exists(), "first file action must succeed");
    assert!(
        !target_fail.exists(),
        "second file action must fail (parent is a regular file)"
    );

    let normalized = normalize_tempdir_paths(
        &cap.human(),
        config_dir.path(),
        &[(&target_ok, "<TARGET_OK>"), (&target_fail, "<TARGET_FAIL>")],
    );
    let stripped = normalize_duration(&strip_ansi(&normalized));
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "apply/with_failures.txt",
        &stripped,
    );
}

#[test]
fn apply_bridge_one_blank_line() {
    // Bridge invariant: when the streaming SectionGuard drops, the
    // renderer auto-emits one blank line. The buffered Doc that follows
    // respects the no-leading-blank rule, so the combined human surface
    // has exactly one blank line at the transition between the last
    // streaming line and the first buffered line.
    let (printer, cap) = Printer::for_test_doc();

    printer.heading("Apply");
    {
        let work = printer.section("Files");
        work.status(Role::Ok, "Wrote /etc/hosts");
    }
    printer.status_simple(Role::Ok, "Apply complete — 1 action(s) succeeded");

    // Buffered Doc carrying both a human section and the ApplyOutput payload.
    // Combining both surfaces is what the bridge invariant guards.
    let doc = Doc::new()
        .section("Source Commits", |s| s.bullet("team-config @ abc1234"))
        .with_data(happy_output());
    printer.emit(doc);
    drop(printer);

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

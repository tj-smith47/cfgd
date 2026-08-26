//! Snapshot tests for `cfgd apply`.
//!
//! Pins the rendered output of every shape `cmd_apply` produces. Goldens
//! live under `tests/output_snapshots/apply/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test apply_snapshots
//!
//! Cases:
//!   - `apply/happy.{txt,json}` — all phases succeed. Snapshot captures
//!     real `cmd_apply` output (run header + per-phase results + buffered
//!     ApplyOutput) against a tempdir-backed profile; `--yes` carries no
//!     preview, so the header's `Actions` row is where the count lands. The
//!     JSON case exercises `build_apply_doc` directly.
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

use std::collections::BTreeMap;
use std::path::Path;

use cfgd::cli::apply::{build_apply_doc, cmd_apply, run_apply};
use cfgd::cli::output_types::ApplyOutput;
use cfgd::cli::plan::cmd_plan;
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::{Doc, Printer, Role};
use pretty_assertions::assert_eq;

#[cfg(unix)]
use common::profile_with_packages_setup;
use common::{
    apply_args, apply_args_dry_run, cli_for, plan_args, profile_with_one_failure_setup,
    tiny_profile_setup,
};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_output() -> ApplyOutput {
    let mut source_commits = BTreeMap::new();
    source_commits.insert("team-config".to_string(), "abc1234".to_string());
    ApplyOutput {
        status: "success".to_string(),
        apply_id: Some(42),
        succeeded: 3,
        failed: 0,
        source_commits,
        backups: vec![],
    }
}

/// Replace tempdir-rooted paths with stable placeholders so goldens are
/// host-stable. `cmd_apply` embeds the config-file path and target file
/// paths into its output (kv block + per-action lines). Delegates to
/// [`cfgd_core::normalize_for_snapshot`] which also folds CRLF → LF and
/// collapses OS-specific `(os error N)` tails.
fn normalize_tempdir_paths(raw: &str, config_dir: &Path, extra_paths: &[(&Path, &str)]) -> String {
    let cfg_file = config_dir.join("cfgd.yaml");
    let mut subs: Vec<(&Path, &str)> = Vec::with_capacity(extra_paths.len() + 2);
    subs.push((&cfg_file, "<CONFIG_DIR>/cfgd.yaml"));
    subs.extend(extra_paths.iter().copied());
    subs.push((config_dir, "<CONFIG_DIR>"));
    cfgd_core::normalize_for_snapshot(raw, &subs)
}

/// Replace ` (N.Ns)` duration suffixes (rendered by StatusBuilder.duration())
/// with a stable placeholder so the apply-summary golden is host-stable, and
/// fold any surviving `\` so a Windows capture matches the same golden.
fn normalize_duration(raw: &str) -> String {
    cfgd_core::normalize_snapshot_durations(raw).replace('\\', "/")
}

/// Collapse the alignment padding ahead of an action row's trailing column.
///
/// The report's column is measured on the REAL subjects, and a subject holding
/// a host temp path is substituted for a short placeholder only afterwards —
/// so the padding left beside the other rows encodes the length of THIS host's
/// temp dir. A golden pinning structure, order and labels must not also pin
/// that; the column itself is a renderer unit test
/// (`output::renderer::status`), where the subjects are literals.
fn collapse_alignment_padding(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.split_inclusive('\n') {
        let mut folded = line.to_string();
        for marker in ['(', '\u{2014}'] {
            let pair = format!("  {marker}");
            while let Some(idx) = folded.find(&pair) {
                folded.remove(idx);
            }
        }
        out.push_str(&folded);
    }
    out
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
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "apply/happy.txt", &stripped);
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
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "apply/dry_run.txt", &stripped);
}

/// `cfgd plan` and `cfgd apply --dry-run` are one surface with two spellings:
/// the only difference either is allowed to have is the title row. Comparing
/// the two goldens would only prove they were regenerated together, so both are
/// re-driven here against identical setups and diffed live.
#[test]
fn plan_and_dry_run_agree_below_the_title_row() {
    fn body(rendered: &str) -> String {
        rendered
            .split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default()
    }

    let (config_dir, state_dir, target) = tiny_profile_setup();
    let (printer, cap) = Printer::for_test_doc();
    cmd_plan(
        &cli_for(config_dir.path(), state_dir.path()),
        &printer,
        &plan_args(),
    )
    .unwrap();
    drop(printer);
    let planned = strip_ansi(&normalize_tempdir_paths(
        &cap.human(),
        config_dir.path(),
        &[(&target, "<TARGET>")],
    ));

    let (config_dir, state_dir, target) = tiny_profile_setup();
    let (printer, cap) = Printer::for_test_doc();
    cmd_apply(
        &cli_for(config_dir.path(), state_dir.path()),
        &printer,
        &apply_args_dry_run(),
    )
    .unwrap();
    drop(printer);
    let dry_run = strip_ansi(&normalize_tempdir_paths(
        &cap.human(),
        config_dir.path(),
        &[(&target, "<TARGET>")],
    ));

    assert!(
        planned.starts_with("Plan\n") && dry_run.starts_with("Plan\n"),
        "both surfaces title themselves Plan:\n{planned}\n---\n{dry_run}"
    );
    assert_eq!(
        body(&planned),
        body(&dry_run),
        "cfgd plan and cfgd apply --dry-run must render identically below the title row"
    );
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

    // `run_apply` renders the failure shape and returns the status without the
    // `process::exit` that `cmd_apply` performs on a partial apply — that exit
    // would abort the in-process snapshot capture (it is covered by the
    // subprocess test in `apply_exit_code.rs`).
    let outcome = run_apply(&cli, &printer, &args).unwrap();
    drop(printer);

    assert_eq!(
        outcome.status,
        cfgd_core::state::ApplyStatus::Partial,
        "one action succeeds and one fails — a partial apply"
    );
    assert_eq!(
        outcome.aborted_code, None,
        "a partial apply is not a signal abort"
    );
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
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "apply/with_failures.txt",
        &stripped,
    );
}

/// The phase tree at the CLI boundary: a `Prerequisites` phase whose one lane
/// group is labelled above its nodes, a `Packages` phase whose install renders
/// under the profile's own label, and a serial `Files` phase below both. The
/// golden pins structure, order and labels — a capture sink never wraps, so
/// the per-line alignment budget stays a renderer unit test.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn apply_phase_tree_human() {
    // Every brew invocation — availability probe, index refresh, install —
    // lands on a shim that exits 0 and says nothing, so the plan and the
    // transcript are the same on a host with brew and a host without.
    let _brew = cfgd_core::test_helpers::ToolShim::install("CFGD_BREW_BIN", 0, "", "");
    let (config_dir, state_dir, target) = profile_with_packages_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();

    let outcome = run_apply(&cli, &printer, &apply_args()).unwrap();
    drop(printer);

    assert_eq!(
        outcome.status,
        cfgd_core::state::ApplyStatus::Success,
        "every action in the fixture succeeds: {}",
        cap.human()
    );
    assert!(target.exists(), "the file action must write its target");

    let normalized =
        normalize_tempdir_paths(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = collapse_alignment_padding(&normalize_duration(&strip_ansi(&normalized)));
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "apply/phase_tree.txt", &stripped);
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
    printer.status_simple(Role::Ok, "Apply complete — 1 action succeeded");

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

    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "apply/bridge.txt", &captured);
}

// ─────────────────────────────────────────────────────
// snapshot helpers — local to keep tests/output_snapshots/ self-contained
// ─────────────────────────────────────────────────────

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

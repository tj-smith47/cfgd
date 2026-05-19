//! Snapshot tests for `cfgd plan`.
//!
//! Pins the rendered output of every shape `cmd_plan` produces. Goldens
//! live under `tests/output_snapshots/plan/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test plan_v2_snapshots
//!
//! Cases:
//!   - `plan/happy.{txt,json}`   — multi-phase plan via real `cmd_plan`
//!     against `tiny_profile_setup`. The JSON case roundtrips the
//!     `PlanOutput` payload directly through `Doc::with_data` — pure data,
//!     no human-surface capture needed.
//!   - `plan/empty.txt`          — `MSG_NOTHING_TO_DO` branch via an
//!     empty-profile fixture.
//!   - `plan/module_only.txt`    — `--module` filter with no profile loaded
//!     (the module-only fallback kv lines).
//!   - `plan/with_pending.txt`   — pending-decisions section: the state DB
//!     is pre-populated with an unresolved decision so
//!     `display_plan_preview_v2` renders the "Pending Decisions" section.

mod common;

use std::path::Path;

use cfgd::cli::output_types::{PlanActionOutput, PlanOutput, PlanPhaseOutput};
use cfgd::cli::plan::cmd_plan;
use cfgd_core::output_v2::{Doc, Printer};
use pretty_assertions::assert_eq;

use common::{
    cli_for, empty_profile_setup, plan_args, plan_args_module, state_with_pending_decision_setup,
    tiny_profile_setup,
};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn happy_plan_output() -> PlanOutput {
    PlanOutput {
        context: "apply".to_string(),
        phases: vec![PlanPhaseOutput {
            phase: "Files".to_string(),
            actions: vec![PlanActionOutput {
                description: "create /etc/hosts".to_string(),
                action_type: "file.create".to_string(),
            }],
        }],
        total_actions: 1,
        warnings: vec![],
    }
}

/// Replace tempdir-rooted paths with stable placeholders so goldens are
/// host-stable. `cmd_plan` embeds the config-file path and target file
/// paths into its output (kv block + per-action lines).
fn normalize_tempdir_paths(raw: &str, config_dir: &Path, extra_paths: &[(&Path, &str)]) -> String {
    let mut out = raw.to_string();
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

#[test]
fn plan_happy_human() {
    let (config_dir, state_dir, target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();
    let args = plan_args();

    cmd_plan(&cli, &v2_printer, &args).unwrap();
    drop(v2_printer);

    let normalized =
        normalize_tempdir_paths(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "plan/happy.txt", &stripped);
}

#[test]
fn plan_happy_json() {
    // Pure data-roundtrip test on `PlanOutput` — drives the JSON path
    // through `Doc::with_data` without standing up a reconciler.
    let output = happy_plan_output();
    let (v2_printer, cap) = Printer::for_test_doc();
    v2_printer.emit(Doc::new().with_data(&output));
    drop(v2_printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("plan doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(PlanOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "plan/happy.json");
}

#[test]
fn plan_empty_human() {
    // Empty profile: zero managed files, zero modules — exercises the
    // `MSG_NOTHING_TO_DO` branch of `display_plan_preview_v2`.
    let (config_dir, state_dir) = empty_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();
    let args = plan_args();

    cmd_plan(&cli, &v2_printer, &args).unwrap();
    drop(v2_printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path(), &[]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "plan/empty.txt", &stripped);
}

#[test]
fn plan_module_only_human() {
    // `--module` filter pointed at a config dir without a valid profile —
    // `cmd_plan` falls into the module-only branch and emits the
    // "Profile: (module-only)" kv. The module itself is unresolved (no
    // module repo configured), so `resolve_modules` returns an empty list
    // and the plan body has zero actions.
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    // Bare config — no `spec.profile`, no profiles dir. Forces the
    // `load_config_and_profile_v2` Err branch.
    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec: {}\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();
    let args = plan_args_module("nettools");

    cmd_plan(&cli, &v2_printer, &args).unwrap();
    drop(v2_printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path(), &[]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "plan/module_only.txt", &stripped);
}

#[test]
fn plan_with_pending_human() {
    // Pre-populated state DB with one unresolved pending decision.
    // `display_plan_preview_v2` renders the "Pending Decisions" section
    // ahead of the plan table.
    let (config_dir, state_dir, target) = state_with_pending_decision_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();
    let args = plan_args();

    cmd_plan(&cli, &v2_printer, &args).unwrap();
    drop(v2_printer);

    let normalized =
        normalize_tempdir_paths(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "plan/with_pending.txt", &stripped);
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

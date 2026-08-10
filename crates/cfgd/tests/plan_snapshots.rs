//! Snapshot tests for `cfgd plan`.
//!
//! Pins the rendered output of every shape `cmd_plan` produces. Goldens
//! live under `tests/output_snapshots/plan/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test plan_snapshots
//!
//! Cases:
//!   - `plan/happy.{txt,json}`   — multi-phase plan via real `cmd_plan`
//!     against `tiny_profile_setup`. The JSON case roundtrips the
//!     `PlanOutput` payload directly through `Doc::with_data` — pure data,
//!     no human-surface capture needed.
//!   - `plan/owner_groups.json` — the owner-axis payload: two groups in
//!     `Owner::sort_key` order inside one phase, with `owner`/`token`.
//!   - `plan/empty.txt`          — `MSG_NOTHING_TO_DO` branch via an
//!     empty-profile fixture.
//!   - `plan/module_only.txt`    — `--module` filter with no profile loaded
//!     (the module-only fallback kv lines).
//!   - `plan/with_inert_decision.txt` — a decision row belonging to a source
//!     the config does not subscribe to: it withholds nothing and is named
//!     nowhere, so the render is byte-identical to the plain plan.

mod common;

use std::path::Path;

use cfgd::cli::output_types::{PlanActionOutput, PlanGroupOutput, PlanOutput, PlanPhaseOutput};
use cfgd::cli::plan::cmd_plan;
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::{Doc, Printer};
use cfgd_core::reconciler::Owner;
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
            // `profile:tiny` mirrors the owner the human golden draws for the
            // same run, and `create` is what `action_type_str` returns for
            // `FileAction::Create` — the fixture describes a payload the
            // product can actually emit.
            groups: vec![PlanGroupOutput::new(
                Owner::profile("tiny"),
                vec![PlanActionOutput {
                    description: "create /etc/hosts".to_string(),
                    action_type: "create".to_string(),
                    targets: vec!["/etc/hosts".to_string()],
                    origin: None,
                }],
            )],
        }],
        total_actions: 1,
        warnings: vec![],
        pending_backups: vec![],
        pending_decisions: vec![],
        rejected_decisions: vec![],
    }
}

/// The owner-axis payload exactly as the redesign specifies it: two groups in
/// `Owner::sort_key` order (`profile:work` before `module:nvim`) inside one
/// phase, `origin` present only where a source delivered the body.
fn owner_groups_plan_output() -> PlanOutput {
    PlanOutput {
        context: "apply".to_string(),
        phases: vec![PlanPhaseOutput {
            phase: "Packages".to_string(),
            groups: vec![
                PlanGroupOutput::new(
                    Owner::profile("work"),
                    vec![PlanActionOutput {
                        description: "apt install sl, cowsay".to_string(),
                        action_type: "install".to_string(),
                        targets: vec!["sl".to_string(), "cowsay".to_string()],
                        origin: None,
                    }],
                ),
                PlanGroupOutput::new(
                    Owner::module("nvim"),
                    vec![PlanActionOutput {
                        description: "brew install neovim".to_string(),
                        action_type: "install".to_string(),
                        targets: vec!["neovim".to_string()],
                        origin: Some("team".to_string()),
                    }],
                ),
            ],
        }],
        total_actions: 2,
        warnings: vec![],
        pending_backups: vec![],
        pending_decisions: vec![],
        rejected_decisions: vec![],
    }
}

/// Replace tempdir-rooted paths with stable placeholders so goldens are
/// host-stable. `cmd_plan` embeds the config-file path and target file
/// paths into its output (kv block + per-action lines).
fn normalize_tempdir_paths(raw: &str, config_dir: &Path, extra_paths: &[(&Path, &str)]) -> String {
    let cfg_file = config_dir.join("cfgd.yaml");
    let mut subs: Vec<(&Path, &str)> = Vec::with_capacity(extra_paths.len() + 2);
    subs.push((&cfg_file, "<CONFIG_DIR>/cfgd.yaml"));
    subs.extend(extra_paths.iter().copied());
    subs.push((config_dir, "<CONFIG_DIR>"));
    cfgd_core::normalize_for_snapshot(raw, &subs)
}

#[test]
fn plan_happy_human() {
    let (config_dir, state_dir, target) = tiny_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = plan_args();

    cmd_plan(&cli, &printer, &args).unwrap();
    drop(printer);

    let normalized =
        normalize_tempdir_paths(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "plan/happy.txt", &stripped);
}

#[test]
fn plan_happy_json() {
    // Pure data-roundtrip test on `PlanOutput` — drives the JSON path
    // through `Doc::with_data` without standing up a reconciler.
    let output = happy_plan_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(Doc::new().with_data(&output));
    drop(printer);

    let expected = serde_json::to_value(&output).unwrap();
    let actual = cap.json().expect("plan doc carries a payload");
    assert_eq!(
        actual, expected,
        "emit -o json must match serde_json::to_value(PlanOutput)"
    );
    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "plan/happy.json");
}

#[test]
fn plan_json_owner_groups_payload() {
    // The owner-axis payload as a whole: group nesting, `owner`/`token`, the
    // `Owner::sort_key` group order (`profile:work` before `module:nvim`), the
    // alphabetical key order `serde_json`'s BTreeMap-backed `Map` produces, and
    // the absence — not emptiness — of `warnings`/`pendingBackups`.
    let output = owner_groups_plan_output();
    let (printer, cap) = Printer::for_test_doc();
    printer.emit(Doc::new().with_data(&output));
    drop(printer);

    cap.assert_json_snapshot_in(Path::new(SNAPSHOT_ROOT), "plan/owner_groups.json");
}

#[test]
fn plan_json_exposes_action_target_paths() {
    // End-to-end through real `cmd_plan` (not the hand-built fixture): the
    // managed-file action's structured `targets` must carry the absolute
    // destination, so `-o json` consumers (CI, blast-radius tooling) read the
    // target without scraping the human `description`.
    let (config_dir, state_dir, target) = tiny_profile_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    // A structured printer so `display_plan_preview` emits the data Doc
    // (`printer.is_structured()` gate) rather than human status lines.
    let (printer, cap) = Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    cmd_plan(&cli, &printer, &plan_args()).unwrap();
    drop(printer);

    let payload = cap.json().expect("plan doc carries a payload");
    let files_phase = payload["phases"]
        .as_array()
        .expect("phases array")
        .iter()
        .find(|p| p["phase"] == "Files")
        .expect("a Files phase is planned");
    let targets = files_phase["groups"][0]["actions"][0]["targets"]
        .as_array()
        .expect("file action exposes a targets array");
    assert_eq!(
        targets,
        &vec![serde_json::json!(target.display().to_string())],
        "structured targets must equal the managed file's absolute destination"
    );
}

#[test]
fn plan_empty_human() {
    // Empty profile: zero managed files, zero modules — exercises the
    // `MSG_NOTHING_TO_DO` branch of `display_plan_preview`.
    let (config_dir, state_dir) = empty_profile_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = plan_args();

    cmd_plan(&cli, &printer, &args).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path(), &[]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "plan/empty.txt", &stripped);
}

#[test]
fn plan_module_only_human() {
    // `--module` filter pointed at a config dir without a valid profile —
    // `cmd_plan` falls into the module-only branch and emits the
    // "Profile: (module-only)" kv. The module itself is unresolved (no
    // module repo configured), so `resolve_modules` returns an empty list.
    // The summary must name the unresolved module rather than claim
    // "everything is up to date" — a silent no-op would hide the miss.
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    // Bare config — no `spec.profile`, no profiles dir. Forces the
    // `load_config_and_profile` Err branch.
    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec: {}\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = plan_args_module("nettools");

    cmd_plan(&cli, &printer, &args).unwrap();
    drop(printer);

    let normalized = normalize_tempdir_paths(&cap.human(), config_dir.path(), &[]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(Path::new(SNAPSHOT_ROOT), "plan/module_only.txt", &stripped);
}

#[test]
fn plan_with_a_decision_from_an_unsubscribed_source_human() {
    // A decision row whose source the config no longer lists withholds
    // nothing and is named nowhere: it is a row the operator cannot answer —
    // `cfgd decide` would act against a source that is gone — so the plan
    // renders exactly as it would with no decision at all. The block for a row
    // that IS live is asserted in `cli/tests.rs`
    // (`plan_preview_excludes_the_resource_its_pending_block_names` and
    // `plan_preview_names_the_decision_that_declined_a_resource`), where the
    // fixture can subscribe to a real source without a git clone's timing
    // landing in a golden.
    let (config_dir, state_dir, target) = state_with_pending_decision_setup();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (printer, cap) = Printer::for_test_doc();
    let args = plan_args();

    cmd_plan(&cli, &printer, &args).unwrap();
    drop(printer);

    let normalized =
        normalize_tempdir_paths(&cap.human(), config_dir.path(), &[(&target, "<TARGET>")]);
    let stripped = strip_ansi(&normalized);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "plan/with_inert_decision.txt",
        &stripped
    );
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

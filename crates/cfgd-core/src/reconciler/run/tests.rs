use std::path::PathBuf;

use super::*;
use crate::config::FileStrategy;
use crate::config::ScriptEntry;
use crate::output::{Printer, PromptAnswer, Verbosity, strip_ansi};
use crate::providers::{FileAction, PackageAction};
use crate::reconciler::{
    Action, ActionResult, Owner, Phase, PhaseName, Plan, ScriptAction, ScriptPhase,
};

fn install(manager: &str, packages: &[&str]) -> Action {
    Action::Package(PackageAction::Install {
        manager: manager.to_string(),
        packages: packages.iter().map(|p| p.to_string()).collect(),
        origin: "local".to_string(),
    })
}

fn create(target: &str) -> Action {
    Action::File(FileAction::Create {
        source: PathBuf::from("/src").join(target),
        target: PathBuf::from(target),
        origin: "local".to_string(),
        strategy: FileStrategy::Copy,
        source_hash: None,
        patch: None,
    })
}

fn module_install(module: &str, manager: &str, package: &str) -> Action {
    Action::Module(crate::reconciler::ModuleAction::local(
        module,
        crate::reconciler::ModuleActionKind::InstallPackages {
            resolved: vec![crate::modules::ResolvedPackage {
                canonical_name: package.to_string(),
                resolved_name: package.to_string(),
                manager: manager.to_string(),
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
            }],
        },
    ))
}

fn script_run(body: &str, script_phase: ScriptPhase, origin: &str) -> Action {
    Action::Script(ScriptAction::Run {
        entry: ScriptEntry::Simple(body.to_string()),
        phase: script_phase,
        origin: origin.to_string(),
    })
}

fn phase(name: PhaseName, actions: Vec<Action>) -> Phase {
    Phase::from_actions(name, &Owner::profile("work"), actions)
}

fn plan_of(phases: Vec<Phase>) -> Plan {
    Plan {
        phases,
        warnings: Vec::new(),
    }
}

fn ctx(title: RunTitle) -> RunContext<'static> {
    RunContext {
        title,
        config_path: None,
        profile: Some("work"),
        modules: &[],
        trigger: None,
    }
}

fn action_result(success: bool) -> ActionResult {
    ActionResult {
        phase: "files".to_string(),
        description: "file:create:/tmp/x".to_string(),
        success,
        error: None,
        changed: true,
    }
}

fn apply_result(
    succeeded: usize,
    failed: usize,
    status: ApplyStatus,
    planned: usize,
) -> ApplyResult {
    let mut action_results = Vec::new();
    for _ in 0..succeeded {
        action_results.push(action_result(true));
    }
    for _ in 0..failed {
        action_results.push(action_result(false));
    }
    ApplyResult {
        action_results,
        status,
        apply_id: 1,
        aborted: None,
        planned_total: planned,
    }
}

/// A `RunExecutor` that returns a canned result and records the plan it saw.
struct StubExecutor {
    result: Option<ApplyResult>,
    calls: usize,
}

impl StubExecutor {
    fn new(result: ApplyResult) -> Self {
        Self {
            result: Some(result),
            calls: 0,
        }
    }
}

impl RunExecutor for StubExecutor {
    fn apply(&mut self, _plan: &Plan, _printer: &Printer) -> Result<ApplyResult> {
        self.calls += 1;
        Ok(self
            .result
            .take()
            .unwrap_or_else(|| apply_result(0, 0, ApplyStatus::Success, 0)))
    }
}

// --- rollup ---

/// The table test the restructure exists to make writable: every `ApplyStatus`
/// arm returns lines rather than one of them panicking, and the short arm's
/// extra line is `Role::Info` (`⊙`) rather than `Role::Pending` (`○`).
#[test]
fn rollup_lines_covers_every_apply_status() {
    let cases: Vec<(ApplyStatus, usize, Vec<Role>)> = vec![
        (ApplyStatus::Success, 1, vec![Role::Ok]),
        (ApplyStatus::Partial, 2, vec![Role::Ok, Role::Accent]),
        (ApplyStatus::Failed, 1, vec![Role::Fail]),
        (ApplyStatus::InProgress, 1, vec![Role::Warn]),
        (ApplyStatus::Aborted, 1, vec![Role::Warn]),
    ];
    for (status, count, roles) in cases {
        let tally = RunTally {
            succeeded: 2,
            failed: 1,
            planned_total: 3,
            status: status.clone(),
            aborted: None,
        };
        let lines = rollup_lines(&tally, RunTitle::Apply);
        assert_eq!(
            lines.len(),
            count,
            "{status:?} produced the wrong line count: {lines:?}"
        );
        assert_eq!(
            lines.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
            roles,
            "{status:?} produced the wrong roles"
        );
    }

    // The short tally: the extra line is the rollup's, and its role decides
    // whether the glyph is `⊙` or `○`.
    let short = RunTally {
        succeeded: 1,
        failed: 0,
        planned_total: 4,
        status: ApplyStatus::Success,
        aborted: None,
    };
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(&short, RunTitle::Apply, &printer, None);
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());
    assert!(
        out.contains("3 action(s) not attempted"),
        "shortfall line missing: {out:?}"
    );
    let glyph_line = out
        .lines()
        .find(|l| l.contains("not attempted"))
        .unwrap_or_default();
    assert!(
        glyph_line.contains('⊙'),
        "shortfall line must carry Role::Info's glyph, got: {glyph_line:?}"
    );
}

/// A completed run names itself, so the header and the rollup cannot disagree
/// about what ran. Only the `Success` arm is titled — `Partial` splits into two
/// bare count lines, which is the shape every mock of a partial run shows.
#[test]
fn a_completed_rollup_names_the_run_it_finished() {
    for (title, expected) in [
        (RunTitle::Apply, "Apply complete"),
        (RunTitle::Reconcile, "Reconcile complete"),
        (RunTitle::Backup, "Backup complete"),
    ] {
        let tally = RunTally {
            succeeded: 1,
            failed: 0,
            planned_total: 1,
            status: ApplyStatus::Success,
            aborted: None,
        };
        let lines = rollup_lines(&tally, title);
        assert_eq!(lines.len(), 1, "{title:?} success rollup is one line");
        assert!(
            lines[0].1.starts_with(expected),
            "{title:?} rollup reads {:?}",
            lines[0].1
        );
    }

    let partial = RunTally {
        succeeded: 1,
        failed: 1,
        planned_total: 2,
        status: ApplyStatus::Partial,
        aborted: None,
    };
    let lines = rollup_lines(&partial, RunTitle::Apply);
    assert_eq!(lines.len(), 2, "a partial rollup splits into two lines");
    assert!(
        lines.iter().all(|(_, line)| !line.contains("Apply")),
        "a partial rollup carries bare counts, not a titled verdict: {lines:?}"
    );
}

/// The abort sentence is the CLI's, verbatim and lowercase, and it is the only
/// abort wording in the tree.
#[test]
fn abort_rollup_keeps_the_lowercase_cli_sentence() {
    let tally = RunTally {
        succeeded: 2,
        failed: 0,
        planned_total: 5,
        status: ApplyStatus::Aborted,
        aborted: Some(130),
    };
    let lines = rollup_lines(&tally, RunTitle::Apply);
    assert_eq!(
        lines[0].1,
        "apply aborted by signal — 2 of 5 action(s) applied; no partial writes, rerun to converge"
    );
    assert_eq!(
        rollup_lines(&tally, RunTitle::Reconcile)[0].1,
        "reconcile aborted by signal — 2 of 5 action(s) applied; no partial writes, rerun to converge"
    );
}

#[test]
fn rollup_attaches_elapsed_to_the_last_line_emitted() {
    // Partial with no shortfall: the duration belongs to the failure line.
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(
        &RunTally {
            succeeded: 1,
            failed: 1,
            planned_total: 2,
            status: ApplyStatus::Partial,
            aborted: None,
        },
        RunTitle::Apply,
        &printer,
        Some(Duration::from_millis(400)),
    );
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());
    let failed_line = out
        .lines()
        .find(|l| l.contains("action(s) failed"))
        .unwrap_or_default();
    assert!(
        failed_line.contains("(0.4s)"),
        "duration must ride the failure line: {out:?}"
    );
    assert!(
        !out.lines()
            .find(|l| l.contains("action(s) succeeded"))
            .unwrap_or_default()
            .contains("(0.4s)"),
        "duration must not also ride the success line: {out:?}"
    );
}

#[test]
fn tally_merge_adds_counts_and_takes_the_worse_status() {
    let mut base = RunTally {
        succeeded: 3,
        failed: 0,
        planned_total: 3,
        status: ApplyStatus::Success,
        aborted: None,
    };
    base.merge(RunTally {
        succeeded: 1,
        failed: 1,
        planned_total: 3,
        status: ApplyStatus::Partial,
        aborted: None,
    });
    assert_eq!(base.succeeded, 4);
    assert_eq!(base.failed, 1);
    assert_eq!(base.planned_total, 6);
    assert_eq!(base.status, ApplyStatus::Partial);

    // A lesser status never masks a higher-severity one.
    let mut failed = RunTally {
        succeeded: 0,
        failed: 2,
        planned_total: 2,
        status: ApplyStatus::Failed,
        aborted: None,
    };
    failed.merge(RunTally {
        succeeded: 1,
        failed: 0,
        planned_total: 1,
        status: ApplyStatus::Partial,
        aborted: None,
    });
    assert_eq!(failed.status, ApplyStatus::Failed);
}

#[test]
fn apply_result_tally_reads_the_reconcilers_planned_total() {
    let result = apply_result(2, 1, ApplyStatus::Partial, 5);
    let tally = result.tally();
    assert_eq!(tally.succeeded, 2);
    assert_eq!(tally.failed, 1);
    assert_eq!(tally.planned_total, 5);
    assert_eq!(tally.status, ApplyStatus::Partial);
}

// --- alignment ---

/// The column is per PHASE, not per owner group: one long subject in the first
/// group moves the second group's column too.
#[test]
fn align_width_is_phase_wide() {
    let wide = phase(
        PhaseName::Packages,
        vec![
            install("apt", &["a-very-long-package-name-indeed"]),
            module_install("nvim", "brew", "neovim"),
        ],
    );
    let narrow = phase(
        PhaseName::Packages,
        vec![
            install("apt", &["sl"]),
            module_install("nvim", "brew", "neovim"),
        ],
    );

    let wide_width = align_width(&wide);
    let narrow_width = align_width(&narrow);
    assert!(
        wide_width > narrow_width,
        "the long subject in group A must widen the phase column: {wide_width} vs {narrow_width}"
    );
    // And the widened column is the long subject's own width, so group B pads
    // out to a column group B alone would never have produced.
    assert_eq!(
        wide_width,
        crate::output::measure_width("apt install a-very-long-package-name-indeed")
    );
}

/// The unfiltered rule: whether an action will carry trailing content is not
/// knowable from the plan, so the widest action sets the column even when it
/// carries nothing itself.
#[test]
fn align_width_counts_actions_without_trailing_content() {
    let subjects = ["short", "the-widest-subject-here", "mid"];
    assert_eq!(
        align_width_of(subjects.into_iter()),
        crate::output::measure_width("the-widest-subject-here")
    );
    // Measured, not byte-counted: a multi-byte glyph is one column.
    assert_eq!(align_width_of(["✓✓✓"].into_iter()), 3);
    assert_eq!(align_width_of(std::iter::empty()), 0);
}

// --- header ---

/// The header's count is the same in-scope predicate `Reconciler::apply` uses
/// for its own `planned_total`, both unfiltered and under `--phase`.
#[test]
fn header_action_count_matches_planned_total() {
    let plan = plan_of(vec![
        phase(PhaseName::Packages, vec![install("apt", &["sl", "cowsay"])]),
        phase(
            PhaseName::Files,
            vec![create("/tmp/one"), create("/tmp/two")],
        ),
    ]);

    let render = |run: &ApplyRun<'_>| {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        run.header(&printer);
        drop(printer);
        strip_ansi(&buf.lock().unwrap())
    };

    let unfiltered = ApplyRun::new(ctx(RunTitle::Apply), &plan);
    let out = render(&unfiltered);
    assert!(
        out.contains("Actions  3 planned"),
        "unfiltered header count wrong: {out:?}"
    );
    assert_eq!(
        unfiltered.in_scope_action_count(),
        reconciler_planned_total(&plan, None),
        "header and reconciler disagree on the unfiltered total"
    );

    let filter = PhaseFilter::Phase(PhaseName::Files);
    let filtered = ApplyRun::new(ctx(RunTitle::Apply), &plan).with_filter(Some(&filter));
    let out = render(&filtered);
    assert!(
        out.contains("Actions  2 planned"),
        "--phase-filtered header count wrong: {out:?}"
    );
    assert_eq!(
        filtered.in_scope_action_count(),
        reconciler_planned_total(&plan, Some(&filter)),
        "header and reconciler disagree under --phase"
    );
}

/// `Reconciler::apply`'s own `planned_total` computation, re-stated here as the
/// oracle the header is checked against.
fn reconciler_planned_total(plan: &Plan, filter: Option<&PhaseFilter>) -> usize {
    plan.phases
        .iter()
        .map(|p| match filter {
            Some(f) => p
                .owned_actions()
                .filter(|(owner, a)| {
                    crate::reconciler::action_matches_phase_filter(&p.name, owner, a, f)
                })
                .count(),
            None => p.action_count(),
        })
        .sum()
}

/// One carrier per run, judged at the header — the only carrier this crate
/// renders. An executing run states the count in its header, a preview-only run
/// does not (its caller's verdict line owns it), and a run with no in-scope work
/// states it nowhere. That the other carrier appears exactly when this one does
/// not is pinned end-to-end by `plan/happy.txt` (footer, no `Actions` row) and
/// `apply/happy.txt` (`Actions` row, no footer).
#[test]
fn the_header_carries_the_planned_count_only_for_an_executing_run() {
    let plan = plan_of(vec![phase(PhaseName::Files, vec![create("/tmp/one")])]);
    let render = |run: ApplyRun<'_>| {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        run.header(&printer);
        drop(printer);
        strip_ansi(&buf.lock().unwrap())
    };

    let executing = render(ApplyRun::new(ctx(RunTitle::Apply), &plan));
    assert_eq!(
        executing.matches("planned").count(),
        1,
        "executing run must state the count exactly once: {executing:?}"
    );

    let previewing = render(ApplyRun::new(ctx(RunTitle::Plan), &plan).preview_only());
    assert!(
        !previewing.contains("planned"),
        "preview-only run must not carry an Actions row: {previewing:?}"
    );

    let empty = plan_of(Vec::new());
    let nothing = render(ApplyRun::new(ctx(RunTitle::Apply), &empty));
    assert!(
        !nothing.contains("planned") && !nothing.contains("Phases"),
        "a run with no in-scope work carries neither row: {nothing:?}"
    );
    assert!(
        nothing.contains("Apply"),
        "the title row survives an empty run: {nothing:?}"
    );
}

#[test]
fn header_omits_every_empty_row_and_skips_the_modules_phase() {
    let plan = plan_of(vec![
        phase(
            PhaseName::Modules,
            vec![Action::Module(crate::reconciler::ModuleAction::local(
                "nvim",
                crate::reconciler::ModuleActionKind::Skip {
                    reason: "not for this host".to_string(),
                },
            ))],
        ),
        phase(PhaseName::Files, vec![create("/tmp/one")]),
    ]);
    let modules = vec!["nvim".to_string()];
    let run = ApplyRun::new(
        RunContext {
            title: RunTitle::Apply,
            config_path: None,
            profile: None,
            modules: &modules,
            trigger: Some("drift (3 resources)"),
        },
        &plan,
    );
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    run.header(&printer);
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    assert!(!out.contains("Config"), "no config path, no row: {out:?}");
    assert!(!out.contains("Profile"), "no profile, no row: {out:?}");
    assert!(
        out.contains("Modules  nvim"),
        "modules row missing: {out:?}"
    );
    assert!(
        out.contains("Trigger  drift (3 resources)"),
        "trigger row missing: {out:?}"
    );
    assert!(
        out.contains("Phases   Files"),
        "phases row must list only phases that render: {out:?}"
    );
    assert!(
        !out.contains("Phases   Modules") && !out.contains("Modules, Files"),
        "the Modules phase must never appear in the Phases row: {out:?}"
    );
}

/// Warnings live in the header, at row depth, so a `--yes` run that renders no
/// preview still shows them.
#[test]
fn header_renders_plan_warnings_at_row_depth() {
    let plan = Plan {
        phases: vec![phase(PhaseName::Files, vec![create("/tmp/one")])],
        warnings: vec!["EDITOR is set before the cfgd source line".to_string()],
    };
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    ApplyRun::new(ctx(RunTitle::Apply), &plan).header(&printer);
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());
    assert!(
        out.contains("\n  ⚠ EDITOR is set before the cfgd source line\n"),
        "warning must render at the header rows' indent: {out:?}"
    );
}

// --- preview ---

#[test]
fn preview_renders_phase_owner_action_and_sorts_profile_first() {
    let plan = plan_of(vec![phase(
        PhaseName::Packages,
        vec![
            module_install("nvim", "brew", "neovim"),
            install("apt", &["sl", "cowsay"]),
        ],
    )]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    ApplyRun::new(ctx(RunTitle::Apply), &plan).preview(&printer);
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    assert!(
        out.contains("Phase: Packages\n"),
        "phase heading missing: {out:?}"
    );
    assert!(
        out.contains("\n  profile:work\n    - apt install sl, cowsay\n"),
        "profile group shape wrong: {out:?}"
    );
    assert!(
        out.contains("\n  module:nvim\n    - brew install neovim\n"),
        "module group shape wrong: {out:?}"
    );
    let profile_at = out.find("profile:work").unwrap_or(usize::MAX);
    let module_at = out.find("module:nvim").unwrap_or(0);
    assert!(
        profile_at < module_at,
        "profile group must sort first: {out:?}"
    );
}

#[test]
fn preview_renders_only_in_scope_phases_and_never_the_modules_phase() {
    let plan = plan_of(vec![
        phase(
            PhaseName::Modules,
            vec![Action::Module(crate::reconciler::ModuleAction::local(
                "nvim",
                crate::reconciler::ModuleActionKind::Skip {
                    reason: "not for this host".to_string(),
                },
            ))],
        ),
        phase(PhaseName::Packages, vec![install("apt", &["sl"])]),
        phase(PhaseName::Files, vec![create("/tmp/one")]),
    ]);
    let filter = PhaseFilter::Phase(PhaseName::Files);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    ApplyRun::new(ctx(RunTitle::Plan), &plan)
        .with_filter(Some(&filter))
        .preview(&printer);
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    assert!(
        out.contains("Phase: Files"),
        "in-scope phase missing: {out:?}"
    );
    assert!(
        !out.contains("Phase: Packages"),
        "filtered-out phase must not render: {out:?}"
    );
    assert!(
        !out.contains("Modules"),
        "the Modules phase renders no heading: {out:?}"
    );
}

// The preview bullet, the string `align_width` measures and the subject the
// execution renders are ONE derivation (`action_display_subject`). A sourced
// script carries a ` <- <origin>` suffix on its preview line, so an execution
// subject deriving itself independently would both rename the action and pad
// every trailing field in the phase against a column nothing reaches.
#[test]
fn preview_bullet_matches_the_execution_subject_for_a_sourced_script() {
    let body = "echo hello";
    let plan = plan_of(vec![phase(
        PhaseName::PostScripts,
        vec![script_run(body, ScriptPhase::PostApply, "team-config")],
    )]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    ApplyRun::new(ctx(RunTitle::Apply), &plan).preview(&printer);
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    // The subject `scripts_apply` hands the status renderer, from the parts it
    // holds at execution time.
    let executed =
        crate::reconciler::script_run_subject(body, &ScriptPhase::PostApply, "team-config")
            .to_string();
    assert!(
        executed.ends_with(" <- team-config"),
        "execution subject must keep the preview's provenance suffix: {executed:?}"
    );
    assert!(
        out.contains(&format!("\n    - {executed}\n")),
        "preview bullet must be the execution subject verbatim: {out:?}"
    );
    assert_eq!(
        align_width(&plan.phases[0]),
        measure_width(&executed),
        "the alignment column must measure the subject the execution renders"
    );
}

// The condensing half of the same contract: a multi-line body is condensed for
// display, and the condensed form is what all three sites use — measuring the
// raw body would pad the phase against a width no line reaches, and a preview
// bullet naming the raw body would embed a newline.
#[test]
fn preview_bullet_matches_the_execution_subject_for_a_condensed_script() {
    // Long enough that condensing the WHOLE plan line — subject marker, body
    // and suffix as one string — truncates the suffix away.
    let body = format!("echo {}", "very-long-argument ".repeat(6));
    let plan = plan_of(vec![phase(
        PhaseName::PreScripts,
        vec![script_run(&body, ScriptPhase::PreApply, "team-config")],
    )]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    ApplyRun::new(ctx(RunTitle::Apply), &plan).preview(&printer);
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    let executed =
        crate::reconciler::script_run_subject(&body, &ScriptPhase::PreApply, "team-config")
            .to_string();
    assert!(
        executed.contains('\u{2026}') && crate::output::measure_width(&executed) < body.len(),
        "execution subject must be condensed: {executed:?}"
    );
    assert!(
        executed.ends_with(" <- team-config"),
        "condensing must not truncate away the provenance suffix: {executed:?}"
    );
    assert!(
        out.contains(&format!("\n    - {executed}\n")),
        "preview bullet must be the condensed execution subject verbatim: {out:?}"
    );
    assert_eq!(
        align_width(&plan.phases[0]),
        measure_width(&executed),
        "the alignment column must measure the condensed subject"
    );
}

// --- execute ---

#[test]
fn execute_skips_the_preview_when_confirmation_is_skipped() {
    let plan = plan_of(vec![phase(PhaseName::Files, vec![create("/tmp/one")])]);
    let run = ApplyRun::new(ctx(RunTitle::Apply), &plan);
    let mut exec = StubExecutor::new(apply_result(1, 0, ApplyStatus::Success, 1));
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let disposition = run.execute(&printer, Confirm::Skip, &mut exec).unwrap();
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    assert!(matches!(disposition, RunDisposition::Applied { .. }));
    assert_eq!(exec.calls, 1);
    assert!(
        !out.contains("Phase: Files"),
        "a run that confirms nothing renders no preview: {out:?}"
    );
    assert!(
        out.contains("Actions  1 planned"),
        "the header still states the count: {out:?}"
    );
    assert!(
        out.contains("Apply complete — 1 action(s) succeeded"),
        "rollup missing: {out:?}"
    );
}

#[test]
fn execute_previews_then_prompts_and_a_declined_prompt_runs_nothing() {
    let plan = plan_of(vec![phase(PhaseName::Files, vec![create("/tmp/one")])]);
    let run = ApplyRun::new(ctx(RunTitle::Apply), &plan);
    let mut exec = StubExecutor::new(apply_result(1, 0, ApplyStatus::Success, 1));
    let (printer, buf) = Printer::for_test_with_prompt_responses_at(
        vec![PromptAnswer::Confirm(false)],
        Verbosity::Normal,
    );
    let disposition = run
        .execute(&printer, Confirm::Ask("Apply these changes?"), &mut exec)
        .unwrap();
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    assert!(matches!(disposition, RunDisposition::Declined));
    assert_eq!(exec.calls, 0, "a declined run must execute nothing");
    assert!(
        out.contains("Phase: Files"),
        "the confirmation gate renders the preview: {out:?}"
    );
    assert!(
        !out.contains("Apply complete"),
        "a declined run has no rollup: {out:?}"
    );
}

#[test]
fn execute_on_a_preview_only_run_renders_the_tree_and_executes_nothing() {
    let plan = plan_of(vec![phase(PhaseName::Files, vec![create("/tmp/one")])]);
    let run = ApplyRun::new(ctx(RunTitle::Plan), &plan).preview_only();
    let mut exec = StubExecutor::new(apply_result(1, 0, ApplyStatus::Success, 1));
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let disposition = run.execute(&printer, Confirm::Skip, &mut exec).unwrap();
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    assert!(matches!(disposition, RunDisposition::Previewed));
    assert_eq!(exec.calls, 0);
    assert!(out.contains("Phase: Files"), "preview missing: {out:?}");
    assert!(
        !out.contains("Apply complete") && !out.contains("Plan complete"),
        "a preview-only run has no rollup: {out:?}"
    );
}

/// The `plan: None` arm: a backups run with no units has nothing to do, and no
/// plan is synthesized to give it a verdict.
#[test]
fn a_backups_run_with_no_units_does_nothing() {
    let store = crate::state::StateStore::open_in_memory().unwrap();
    let units: Vec<crate::backup::BackupUnit<'_>> = Vec::new();
    let run = ApplyRun::backups(ctx(RunTitle::Backup), &units, &store);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let (status, _reports) = run.execute_backups(&printer).unwrap();
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    assert_eq!(status, ApplyStatus::Success);
    assert!(
        !out.contains(BACKUPS_PHASE_LABEL),
        "no units, no pseudo-phase: {out:?}"
    );
    assert!(
        !out.contains("planned"),
        "no units, no Actions row: {out:?}"
    );
}

/// `execute` on a `backups()` run has no plan action to run, but it is not a
/// run that did nothing — its disposition has to say which.
#[test]
fn execute_on_a_backups_run_reports_the_work_it_did() {
    let store = crate::state::StateStore::open_in_memory().unwrap();
    let units: Vec<crate::backup::BackupUnit<'_>> = Vec::new();
    let run = ApplyRun::backups(ctx(RunTitle::Backup), &units, &store);
    let (printer, _buf) = Printer::for_test_at(Verbosity::Normal);
    let mut exec = StubExecutor::new(apply_result(9, 0, ApplyStatus::Success, 9));
    let disposition = run.execute(&printer, Confirm::Skip, &mut exec).unwrap();
    drop(printer);

    assert!(
        matches!(
            disposition,
            RunDisposition::BackupsApplied {
                status: ApplyStatus::Success,
                ..
            }
        ),
        "a backups run that executed must not report NothingToDo"
    );
    assert_eq!(exec.calls, 0, "a backups run runs no plan action");
}

// --- pseudo-phase ---

#[test]
fn pseudo_phase_renders_a_bare_heading_with_owner_groups_under_it() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    {
        let phase = pseudo_phase(&printer, BACKUPS_PHASE_LABEL);
        let group = phase.owner(&Owner::backup("docs"), 20);
        group.status_simple(Role::Ok, "snapshot notes.txt");
    }
    drop(printer);
    let out = strip_ansi(&buf.lock().unwrap());

    assert!(
        out.starts_with("Backups\n"),
        "a pseudo-phase heading renders bare, with no `Phase: ` prefix: {out:?}"
    );
    assert!(
        out.contains("\n  backup:docs\n    ✓ snapshot notes.txt\n"),
        "owner group shape wrong: {out:?}"
    );
}

#[test]
fn hooks_and_backups_labels_are_distinct_and_carry_no_phase_prefix() {
    assert_eq!(HOOKS_PHASE_LABEL, "Drift Hooks");
    assert_eq!(BACKUPS_PHASE_LABEL, "Backups");
    for label in [HOOKS_PHASE_LABEL, BACKUPS_PHASE_LABEL] {
        assert!(
            !label.starts_with("Phase: "),
            "{label} must not wear the PhaseName prefix"
        );
    }
}

#[test]
fn phase_coverage_decides_only_whether_the_modules_phase_is_walked() {
    // The one axis on which the payload's walk and the tree's differ. Every
    // other phase, group and action is yielded identically, which is what lets
    // both surfaces share this function instead of filtering twice.
    let skip = Action::Module(crate::reconciler::ModuleAction::local(
        "wsl-tools",
        crate::reconciler::ModuleActionKind::Skip {
            reason: "platform not matched (requires: windows)".to_string(),
        },
    ));
    let plan = plan_of(vec![
        phase(PhaseName::Modules, vec![skip]),
        phase(PhaseName::Packages, vec![install("brew", &["rg"])]),
    ]);

    let walked = |coverage| {
        in_scope_tree(&plan, None, coverage)
            .into_iter()
            .map(|(p, _)| p.name.display_name())
            .collect::<Vec<_>>()
    };
    assert_eq!(walked(PhaseCoverage::Complete), vec!["Modules", "Packages"]);
    assert_eq!(walked(PhaseCoverage::Rendered), vec!["Packages"]);
}

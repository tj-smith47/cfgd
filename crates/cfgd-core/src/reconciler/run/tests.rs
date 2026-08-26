use std::path::PathBuf;

use super::*;
use crate::config::FileStrategy;
use crate::config::ScriptEntry;
use crate::output::{Printer, PromptAnswer, Verbosity};
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
                min_version: None,
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
        sources: &[],
        modules: &[],
        trigger: None,
        subject: None,
    }
}

fn action_result(success: bool) -> ActionResult {
    ActionResult {
        phase: "files".to_string(),
        description: "file:create:/tmp/x".to_string(),
        success,
        error: None,
        changed: true,
        skipped: false,
        not_attempted: None,
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
        caveats: Vec::new(),
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
/// extra line is `Role::Info` (`◉`) rather than `Role::Pending` (`○`).
#[test]
fn rollup_lines_covers_every_apply_status() {
    let cases: Vec<(ApplyStatus, usize, Vec<Role>)> = vec![
        (ApplyStatus::Success, 1, vec![Role::Ok]),
        (
            ApplyStatus::Partial,
            3,
            vec![Role::Warn, Role::Ok, Role::Accent],
        ),
        (ApplyStatus::Failed, 1, vec![Role::Fail]),
        (ApplyStatus::InProgress, 1, vec![Role::Warn]),
        (ApplyStatus::Aborted, 1, vec![Role::Warn]),
    ];
    for (status, count, roles) in cases {
        let tally = RunTally {
            succeeded: 2,
            skipped: 0,
            not_attempted: Vec::new(),
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
            lines.iter().map(|(r, _, _)| *r).collect::<Vec<_>>(),
            roles,
            "{status:?} produced the wrong roles"
        );
    }

    // The short tally: the extra line is the rollup's, and its role decides
    // whether the glyph is `◉` or `○`.
    let short = RunTally {
        succeeded: 1,
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 0,
        planned_total: 4,
        status: ApplyStatus::Success,
        aborted: None,
    };
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(&short, RunTitle::Apply, &printer, None);
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    assert!(
        out.contains("3 actions not attempted"),
        "shortfall line missing: {out:?}"
    );
    let glyph_line = out
        .lines()
        .find(|l| l.contains("not attempted"))
        .unwrap_or_default();
    assert!(
        glyph_line.contains('◉'),
        "shortfall line must carry Role::Info's glyph, got: {glyph_line:?}"
    );
}

/// A run that planned work and reached none of it did not complete. `cfgd
/// backup run` refused by another holder of the unit's lock exits 1, and used
/// to close with `✓ Backup complete — 0 actions succeeded` above the
/// shortfall — the tick and the exit code were the only two things on screen
/// saying what happened, and they disagreed.
#[test]
fn a_run_that_attempted_nothing_says_so_instead_of_completing() {
    let nothing = RunTally {
        succeeded: 0,
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 0,
        planned_total: 3,
        status: ApplyStatus::Success,
        aborted: None,
    };

    let lines = rollup_lines(&nothing, RunTitle::Backup);
    assert_eq!(
        lines,
        vec![(
            Role::Skipped,
            "Backup did not run".to_string(),
            Some("3 actions not attempted".to_string())
        )]
    );

    // And the shortfall line is not repeated underneath it.
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(&nothing, RunTitle::Backup, &printer, None);
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    assert_eq!(
        out.matches("not attempted").count(),
        1,
        "the shortfall is named once, not twice: {out:?}"
    );

    // A run that planned nothing is untouched: it completed, having nothing to
    // do, and must keep saying so.
    assert_eq!(
        rollup_lines(&RunTally::empty(), RunTitle::Apply),
        vec![(
            Role::Ok,
            "Apply complete".to_string(),
            Some("0 actions succeeded".to_string())
        )]
    );
}

/// A completed run names itself, so the header and the rollup cannot disagree
/// about what ran. `Partial` names itself too, in a leading `Role::Warn`
/// verdict, and keeps the two count lines below it.
#[test]
fn a_completed_rollup_names_the_run_it_finished() {
    for (title, expected) in [
        (RunTitle::Apply, "Apply complete"),
        (RunTitle::Reconcile, "Reconcile complete"),
        (RunTitle::Backup, "Backup complete"),
    ] {
        let tally = RunTally {
            succeeded: 1,
            skipped: 0,
            not_attempted: Vec::new(),
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
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 1,
        planned_total: 2,
        status: ApplyStatus::Partial,
        aborted: None,
    };
    let lines = rollup_lines(&partial, RunTitle::Apply);
    assert_eq!(
        lines,
        vec![
            (
                Role::Warn,
                "Apply partial".to_string(),
                Some("1 of 2 applied".to_string())
            ),
            (Role::Ok, "1 action succeeded".to_string(), None),
            (Role::Accent, "1 action failed".to_string(), None),
        ],
        "a partial rollup leads with its own verdict and keeps both counts"
    );
    // The verdict names the run that was partial: a partially-applied backup
    // must not report itself as an apply.
    let backup_partial = &rollup_lines(&partial, RunTitle::Backup)[0];
    assert_eq!(backup_partial.1, "Backup partial");
    assert_eq!(backup_partial.2.as_deref(), Some("1 of 2 applied"));
}

/// A run that failed actions must not OPEN on a tick. The two count lines are
/// deliberately split so `9 succeeded, 1 failed` and `1 succeeded, 9 failed`
/// do not read the same colour — but with the success count first, the first
/// line of the closing block was `✓ N actions succeeded` for both, and a
/// reader who takes the first line as the verdict reads a failed run as a
/// clean one. Every rollup that carries a failure now leads with its verdict.
#[test]
fn a_rollup_carrying_failures_does_not_lead_with_a_tick() {
    for (status, succeeded, failed) in [
        (ApplyStatus::Partial, 9, 1),
        (ApplyStatus::Partial, 1, 9),
        (ApplyStatus::Failed, 0, 3),
    ] {
        let tally = RunTally {
            succeeded,
            skipped: 0,
            not_attempted: Vec::new(),
            failed,
            planned_total: succeeded + failed,
            status: status.clone(),
            aborted: None,
        };
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        render_run_rollup(&tally, RunTitle::Apply, &printer, None);
        drop(printer);
        let out = crate::test_helpers::captured_text(&buf);
        let first = out.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        assert!(
            !first.contains('✓'),
            "{status:?} ({succeeded}/{failed}) opened its rollup on a tick: {out:?}"
        );
        assert!(
            first.contains('⚠') || first.contains('✗'),
            "{status:?} ({succeeded}/{failed}) must open on its verdict: {out:?}"
        );
    }
}

/// The abort sentence is the CLI's, verbatim and lowercase, and it is the only
/// abort wording in the tree.
#[test]
fn abort_rollup_keeps_the_lowercase_cli_sentence() {
    let tally = RunTally {
        succeeded: 2,
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 0,
        planned_total: 5,
        status: ApplyStatus::Aborted,
        aborted: Some(130),
    };
    let lines = rollup_lines(&tally, RunTitle::Apply);
    assert_eq!(lines[0].1, "apply aborted by signal");
    assert_eq!(
        lines[0].2.as_deref(),
        Some("2 of 5 actions applied; no partial writes, rerun to converge")
    );
    let reconcile = rollup_lines(&tally, RunTitle::Reconcile);
    assert_eq!(reconcile[0].1, "reconcile aborted by signal");
    assert_eq!(
        reconcile[0].2.as_deref(),
        Some("2 of 5 actions applied; no partial writes, rerun to converge")
    );
}

#[test]
fn an_abort_that_killed_an_action_names_the_failure_too() {
    // The signal reaches the child: `brew install` dies with the run. Without
    // the failure clause that action is in neither the applied count nor the
    // not-attempted line, and the closing line reads as a clean stop.
    let tally = RunTally {
        succeeded: 2,
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 1,
        planned_total: 3,
        status: ApplyStatus::Aborted,
        aborted: Some(130),
    };
    let lines = rollup_lines(&tally, RunTitle::Apply);
    assert_eq!(lines[0].1, "apply aborted by signal");
    assert_eq!(
        lines[0].2.as_deref(),
        Some("2 of 3 actions applied, 1 failed; no partial writes, rerun to converge")
    );
}

#[test]
fn rollup_attaches_elapsed_to_the_last_line_emitted() {
    // Partial with no shortfall: the duration belongs to the failure line.
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(
        &RunTally {
            succeeded: 1,
            skipped: 0,
            not_attempted: Vec::new(),
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
    let out = crate::test_helpers::captured_text(&buf);
    let failed_line = out
        .lines()
        .find(|l| l.contains("1 action failed"))
        .unwrap_or_default();
    assert!(
        failed_line.contains("(0.4s)"),
        "duration must ride the failure line: {out:?}"
    );
    assert!(
        !out.lines()
            .find(|l| l.contains("1 action succeeded"))
            .unwrap_or_default()
            .contains("(0.4s)"),
        "duration must not also ride the success line: {out:?}"
    );
}

#[test]
fn tally_merge_adds_counts_and_takes_the_worse_status() {
    let mut base = RunTally {
        succeeded: 3,
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 0,
        planned_total: 3,
        status: ApplyStatus::Success,
        aborted: None,
    };
    base.merge(RunTally {
        succeeded: 1,
        skipped: 0,
        not_attempted: Vec::new(),
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
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 2,
        planned_total: 2,
        status: ApplyStatus::Failed,
        aborted: None,
    };
    failed.merge(RunTally {
        succeeded: 1,
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 0,
        planned_total: 1,
        status: ApplyStatus::Partial,
        aborted: None,
    });
    assert_eq!(failed.status, ApplyStatus::Failed);
}

/// One predicate prices both ends of a run. `Action::pre_skip_reason` keeps a
/// withheld action out of `Actions N planned`, so the tally keeps it out of
/// the counted rollup too: a two-action run closed on `2 actions succeeded, 1
/// skipped` under a header promising two, because the apply dispatched the
/// pre-skipped publish and filed its outcome as a skip that ran. The row keeps
/// its reason; the closing line names the count only in its parenthetical, and
/// `succeeded + skipped + failed` reconciles against the header with no
/// shortfall line for an action the run never promised.
#[test]
fn a_pre_skipped_action_is_priced_outside_the_counted_rollup() {
    let mut result = apply_result(2, 0, ApplyStatus::Success, 3);
    let mut skipped_that_ran = action_result(true);
    skipped_that_ran.changed = false;
    skipped_that_ran.skipped = true;
    result.action_results.push(skipped_that_ran);
    result.action_results.push(ActionResult {
        phase: "prerequisites".to_string(),
        description: "env:refresh".to_string(),
        success: true,
        error: None,
        changed: false,
        skipped: false,
        not_attempted: Some(crate::NO_SESSION_MANAGER.to_string()),
    });

    let tally = result.tally();
    assert_eq!(
        (tally.succeeded, tally.skipped, tally.failed),
        (2, 1, 0),
        "a withheld action is neither a success nor a skip that ran"
    );
    assert_eq!(
        tally.succeeded + tally.skipped + tally.failed,
        tally.planned_total,
        "the header's count is the counted rollup's count"
    );
    assert_eq!(
        tally.not_attempted,
        vec![crate::NO_SESSION_MANAGER.to_string()]
    );
    assert_eq!(
        outcome_counts(&tally),
        "2 actions succeeded, 1 skipped (1 not attempted — no session manager)"
    );

    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(&tally, RunTitle::Apply, &printer, None);
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    assert!(
        out.contains(
            "Apply complete — 2 actions succeeded, 1 skipped (1 not attempted — no session manager)"
        ),
        "the verdict prices the withheld action in its parenthetical only: {out:?}"
    );
    assert_eq!(
        out.matches("not attempted").count(),
        1,
        "no shortfall line for an action the header never promised: {out:?}"
    );

    // A second reason is a second clause, not a second count; one reason twice
    // is one clause over a count of two.
    tally_with_reasons(&["a", "a", "b"], |counts| {
        assert_eq!(counts, "2 actions succeeded (3 not attempted — a, b)");
    });
}

fn tally_with_reasons(reasons: &[&str], check: impl FnOnce(String)) {
    let tally = RunTally {
        succeeded: 2,
        skipped: 0,
        not_attempted: reasons.iter().map(|r| r.to_string()).collect(),
        failed: 0,
        planned_total: 2,
        status: ApplyStatus::Success,
        aborted: None,
    };
    check(outcome_counts(&tally));
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

/// The column is per REPORT, not per phase and not per owner group: one long
/// subject in the FIRST phase moves the column the second phase pads to.
#[test]
fn report_align_width_spans_every_phase() {
    let wide = plan_of(vec![
        phase(
            PhaseName::Prerequisites,
            vec![install("apt", &["a-very-long-package-name-indeed"])],
        ),
        phase(
            PhaseName::Packages,
            vec![module_install("nvim", "brew", "neovim")],
        ),
    ]);
    let narrow = plan_of(vec![
        phase(PhaseName::Prerequisites, vec![install("apt", &["sl"])]),
        phase(
            PhaseName::Packages,
            vec![module_install("nvim", "brew", "neovim")],
        ),
    ]);

    let wide_width = report_align_width(&wide, None);
    let narrow_width = report_align_width(&narrow, None);
    assert!(
        wide_width > narrow_width,
        "the long subject in the first phase must widen the report column: \
         {wide_width} vs {narrow_width}"
    );
    // And the widened column is the long subject's own width, so the SECOND
    // phase pads out to a column its own actions would never have produced.
    assert_eq!(
        wide_width,
        crate::output::measure_width("apt install a-very-long-package-name-indeed")
    );
}

/// A phase the filter empties prints no row, so it cannot widen the column the
/// rows that DO print pad to.
#[test]
fn report_align_width_ignores_a_filtered_out_phase() {
    let plan = plan_of(vec![
        phase(
            PhaseName::Prerequisites,
            vec![install("apt", &["a-very-long-package-name-indeed"])],
        ),
        phase(PhaseName::Files, vec![create("dotfile")]),
    ]);
    let filter = PhaseFilter::Phase(PhaseName::Files);
    assert_eq!(
        report_align_width(&plan, Some(&filter)),
        report_align_width(
            &plan_of(vec![phase(PhaseName::Files, vec![create("dotfile")])]),
            None
        ),
        "an excluded phase's widest action must not pad the phase that renders"
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

/// A run with no [`Plan`] and no backup units still states every row it holds:
/// the unit in the title, the config it composed under, the sources it drew
/// from, and the work it set out to do.
///
/// `cfgd backup restore` renders its own body and took only the rollup, so the
/// verb that overwrites live data was the one that never named its config.
#[test]
fn an_unplanned_run_heads_itself_with_every_row_it_can_state() {
    let sources = vec![ComposedSource {
        name: "team".to_string(),
        profile: Some("shared".to_string()),
    }];
    let config = std::path::Path::new("/home/me/.config/cfgd/cfgd.yaml");
    let run = ApplyRun::unplanned(
        RunContext {
            title: RunTitle::Restore,
            config_path: Some(config),
            profile: Some("work"),
            sources: &sources,
            modules: &[],
            trigger: None,
            subject: Some("notes"),
        },
        crate::backup::RESTORE_ACTION_COUNT,
    );

    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    run.header(&printer);
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);

    assert!(
        out.starts_with("Restore: notes"),
        "the unit belongs in the title, not in a row: {out:?}"
    );
    for row in [
        "Config   /home/me/.config/cfgd/cfgd.yaml",
        "Profile  work",
        "Sources  team (profile shared)",
        "Actions  1 planned",
    ] {
        assert!(out.contains(row), "missing {row:?} in: {out:?}");
    }
}

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
        crate::test_helpers::captured_text(&buf)
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
        crate::test_helpers::captured_text(&buf)
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
            sources: &[],
            modules: &modules,
            trigger: Some("drift (3 resources)"),
            subject: None,
        },
        &plan,
    );
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    run.header(&printer);
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);

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
    let out = crate::test_helpers::captured_text(&buf);
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
    let out = crate::test_helpers::captured_text(&buf);

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
    let out = crate::test_helpers::captured_text(&buf);

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
    let out = crate::test_helpers::captured_text(&buf);

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
        report_align_width(&plan, None),
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
    let out = crate::test_helpers::captured_text(&buf);

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
        report_align_width(&plan, None),
        measure_width(&executed),
        "the alignment column must measure the condensed subject"
    );
}

/// A planned script's marker (`run PostApply script:`) carries the same
/// `Role::Accent` styling in the preview tree that `StatusBuilder::marker`
/// gives it once the script actually runs — the two must read as the same
/// kind of thing before and after execution, not plain text in the preview
/// and styled only once the run starts.
#[test]
#[serial_test::serial]
fn preview_bullet_styles_a_scripts_marker() {
    use crate::output::{Role, Theme};

    let body = "echo hello";
    let plan = plan_of(vec![phase(
        PhaseName::PostScripts,
        vec![script_run(body, ScriptPhase::PostApply, "local")],
    )]);
    let theme = Theme::from_preset("dracula").with_colors(true);
    let (printer, buf) = Printer::for_test_with_theme_colored(theme.clone(), Verbosity::Normal);
    ApplyRun::new(ctx(RunTitle::Apply), &plan).preview(&printer);
    drop(printer);
    // raw-capture-ok: asserting the marker's exact styled run reaches the renderer unrestyled — captured_text would strip the ANSI this test exists to check
    let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let (_, accent) = crate::output::renderer::role_glyph(&theme, Role::Accent);
    let styled_marker = accent.apply_to("run postApply script:").to_string();
    assert!(
        raw.contains(&styled_marker),
        "the marker must carry Role::Accent styling: {raw:?}"
    );
    // The body itself stays unstyled — only the marker is coloured.
    assert!(
        !raw.contains(&accent.apply_to(body).to_string()),
        "the script body must not be styled like the marker: {raw:?}"
    );

    let plain = crate::output::strip_ansi(&raw);
    assert!(
        plain.contains("run postApply script: echo hello"),
        "the stripped text must match the execution subject: {plain:?}"
    );
}

/// One styling for a withheld action row, whichever tree draws it.
///
/// The plan lists an action the host has already refused and the apply settles
/// the same one a beat later; when the plan dimmed the subject and brightened
/// the reason while the apply did the reverse, a viewer flipping between the
/// two frames read a change that never happened. Both settle through
/// `SectionGuard::action_status` now, so the bytes match.
#[test]
#[serial_test::serial]
fn both_trees_paint_a_withheld_row_with_the_same_bytes() {
    use crate::output::{Role, Theme};

    let subject = "publish 3 vars to the session manager";
    let reason = crate::NO_SESSION_MANAGER;
    let theme = Theme::from_preset("dracula").with_colors(true);

    let line = |render: &dyn Fn(&Printer)| -> String {
        let (printer, buf) = Printer::for_test_with_theme_colored(theme.clone(), Verbosity::Normal);
        render(&printer);
        drop(printer);
        // raw-capture-ok: the claim IS that the two renders carry the same escapes — captured_text would strip exactly what is being compared
        buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
    };

    // The plan tree's arm, reached the way `render_plan_tree` reaches it.
    let planned = line(&|printer| {
        let section = printer.section_phase(&PhaseName::Prerequisites.section_label());
        let owner = section.section_owner(&OwnerLabel::new("cfgd", "env"));
        owner.action_status(Role::Skipped, subject).detail(reason);
    });
    // The apply tree's arm: the outcome `settle_action` records for the same
    // action, through the writer that commits every settled line.
    let settled = line(&|printer| {
        let section = printer.section_phase(&PhaseName::Prerequisites.section_label());
        let owner = section.section_owner(&OwnerLabel::new("cfgd", "env"));
        super::super::apply::emit_action_line(
            printer,
            &owner,
            &super::super::apply::ActionOutcome::for_test_settled(subject, Role::Skipped, reason),
        );
    });

    assert_eq!(
        planned, settled,
        "the plan tree and the apply tree must paint a withheld row identically"
    );
    // Anti-vacuity: the row really is styled, and the emphasis really is
    // subject-dim / reason-bright rather than two unstyled strings matching.
    let muted_subject = theme.muted.apply_to(subject).to_string();
    assert!(
        planned.contains(&muted_subject),
        "a withheld subject keeps its dim role style: {planned:?}"
    );
    assert!(
        !planned.contains(&theme.muted.apply_to(reason).to_string()),
        "a withheld row's reason is the information on it, and renders bright: {planned:?}"
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
    let out = crate::test_helpers::captured_text(&buf);

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
        out.contains("Apply complete — 1 action succeeded"),
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
    let out = crate::test_helpers::captured_text(&buf);

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
    let out = crate::test_helpers::captured_text(&buf);

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
    let out = crate::test_helpers::captured_text(&buf);

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
fn pseudo_phase_renders_the_same_phase_heading_treatment_as_a_real_phase() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    {
        let phase = pseudo_phase(&printer, BACKUPS_PHASE_LABEL);
        let group = phase.owner(&Owner::backup("docs"), 20);
        group.status_simple(Role::Ok, "snapshot notes.txt");
    }
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);

    assert!(
        out.starts_with("Phase: Backups\n"),
        "a pseudo-phase heading renders through PhaseLabel, exactly like a \
         planned reconciler phase: {out:?}"
    );
    assert!(
        out.contains("\n  backup:docs\n    ✓ snapshot notes.txt\n"),
        "owner group shape wrong: {out:?}"
    );
}

/// The run's only phase renders no phase row: the owner group opens at the
/// run's own depth, exactly where `backup restore` already puts it, so the two
/// verbs of one command no longer show one owner group at two depths.
#[test]
fn sole_phase_renders_its_owner_groups_at_the_run_depth() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    {
        let phase = sole_phase(&printer);
        let group = phase.owner(&Owner::backup("docs"), 20);
        group.status_simple(Role::Ok, "snapshot notes.txt");
    }
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);

    assert!(
        !out.contains("Phase:"),
        "no phase row on a sole phase: {out:?}"
    );
    assert!(
        out.starts_with("backup:docs\n  ✓ snapshot notes.txt\n"),
        "owner group opens at the run depth: {out:?}"
    );
}

/// The raw constants stay bare names, not pre-formatted headings — `PhaseLabel`
/// is what adds the `Phase: ` prefix at render time (see the test above), so a
/// constant that baked the prefix in would double it.
#[test]
fn hooks_and_backups_labels_are_distinct_bare_names() {
    assert_eq!(HOOKS_PHASE_LABEL, "Drift Hooks");
    assert_eq!(BACKUPS_PHASE_LABEL, "Backups");
    for label in [HOOKS_PHASE_LABEL, BACKUPS_PHASE_LABEL] {
        assert!(
            !label.starts_with("Phase: "),
            "{label} must be the bare name PhaseLabel::new(...) takes, not a \
             pre-formatted heading"
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

// --- composed sources ---

fn layer(source: &str, profile_name: &str) -> crate::config::ProfileLayer {
    crate::config::ProfileLayer {
        source: source.to_string(),
        profile_name: profile_name.to_string(),
        priority: 0,
        policy: crate::config::LayerPolicy::Required,
        spec: crate::config::ProfileSpec::default(),
    }
}

#[test]
fn composed_sources_skip_the_local_layer_and_dedup_by_source() {
    let layers = vec![
        layer(crate::config::LOCAL_LAYER, "work"),
        layer("team", "team"),
        layer("team", "team"),
        layer("infra", ""),
    ];
    let sources = ComposedSource::from_profile_layers(&layers);
    assert_eq!(
        sources,
        vec![
            ComposedSource {
                name: "team".to_string(),
                profile: Some("team".to_string()),
            },
            ComposedSource {
                name: "infra".to_string(),
                profile: None,
            },
        ],
        "the operator's own layer is not a source, and one source is announced once"
    );
}

#[test]
fn header_names_the_sources_a_run_composed() {
    let plan = plan_of(vec![phase(PhaseName::Files, vec![create("/tmp/one")])]);
    let sources = ComposedSource::from_profile_layers(&[layer("team", "team"), layer("infra", "")]);
    let run = ApplyRun::new(
        RunContext {
            title: RunTitle::Apply,
            config_path: None,
            profile: Some("work"),
            sources: &sources,
            modules: &[],
            trigger: None,
            subject: None,
        },
        &plan,
    );
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    run.header(&printer);
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    assert!(
        out.contains("Sources  team (profile team), infra"),
        "sources row missing: {out:?}"
    );
    let profile_at = out.find("Profile").expect("profile row");
    let sources_at = out.find("Sources").expect("sources row");
    assert!(
        profile_at < sources_at,
        "the sources row states what layered ON the profile, so it follows it: {out:?}"
    );
}

#[test]
fn header_omits_the_sources_row_when_nothing_composed() {
    let plan = plan_of(vec![phase(PhaseName::Files, vec![create("/tmp/one")])]);
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    ApplyRun::new(ctx(RunTitle::Apply), &plan).header(&printer);
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    assert!(
        !out.contains("Sources"),
        "a purely local run must not claim a source: {out:?}"
    );
}

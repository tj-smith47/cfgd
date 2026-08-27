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
        unit_source: None,
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
        installed: None,
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
            vec![Role::Warn, Role::Ok, Role::Fail],
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
            (Role::Fail, "1 action failed".to_string(), None),
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
        Some("2 of 5 actions applied; no partial writes")
    );
    let reconcile = rollup_lines(&tally, RunTitle::Reconcile);
    assert_eq!(reconcile[0].1, "reconcile aborted by signal");
    assert_eq!(
        reconcile[0].2.as_deref(),
        Some("2 of 5 actions applied; no partial writes")
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
        Some("2 of 3 actions applied, 1 failed; no partial writes")
    );
}

/// The wall total measures the RUN, so it belongs to the line that names the
/// run. Hanging it off whichever line came last fused it to that line's own
/// count: a partial run closed on `2 actions failed (274.0s wall)`, which
/// reads as the failures having burned four and a half minutes.
#[test]
fn the_rollups_elapsed_hangs_off_the_line_that_names_the_run() {
    for (status, first) in [
        (ApplyStatus::Partial, "Apply partial"),
        (ApplyStatus::Failed, "Apply failed"),
        (ApplyStatus::Success, "Apply complete"),
    ] {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        render_run_rollup(
            &RunTally {
                succeeded: 1,
                skipped: 0,
                not_attempted: Vec::new(),
                failed: if status == ApplyStatus::Success { 0 } else { 1 },
                planned_total: if status == ApplyStatus::Success { 1 } else { 2 },
                status: status.clone(),
                aborted: None,
            },
            RunTitle::Apply,
            &printer,
            Some(Duration::from_millis(400)),
        );
        drop(printer);
        let out = crate::test_helpers::captured_text(&buf);
        let mut lines = out.lines().filter(|l| !l.trim().is_empty());
        let head = lines.next().unwrap_or_default();
        assert!(
            head.contains(first) && head.contains("(0.4s wall)"),
            "{status:?}: the total must ride the line naming the run: {out:?}"
        );
        for line in lines {
            assert!(
                !line.contains("wall"),
                "{status:?}: only the run's own line carries the total: {out:?}"
            );
        }
    }
}

/// One outcome CLASS per line. `outcome_counts` fused every count into one
/// `Role::Ok` sentence, so `✓ 20 actions succeeded, 1 not attempted: no session
/// manager` painted work that never happened under a green tick — and invited
/// a reader to sum `20 + 1 + 2` against a header promising `Actions 22
/// planned`, of which the withheld count is deliberately no part.
#[test]
fn every_outcome_class_in_a_rollup_carries_its_own_role() {
    // The word each class states itself with, and the role its own action rows
    // wear. A settled skip and a pre-skip both paint `Role::Skipped`, so their
    // lines do too — what neither may do is share a line with another class.
    //
    // The withheld clause is keyed on its colon: it always carries the reason
    // the row above gave, which is what separates it from the SHORTFALL line
    // (`N actions not attempted`) — a different class, for work the run
    // planned and never reached, which already has a line and a role of its
    // own and is checked below.
    let classes = [
        ("succeeded", Role::Ok),
        ("failed", Role::Fail),
        ("skipped", Role::Skipped),
        ("not attempted:", Role::Skipped),
    ];
    let theme = crate::output::Theme::default();
    let mut seen: Vec<&str> = Vec::new();
    for status in [
        ApplyStatus::Success,
        ApplyStatus::Partial,
        ApplyStatus::Failed,
        ApplyStatus::InProgress,
        ApplyStatus::Aborted,
    ] {
        // Every class nonzero at once, which is the shape that fused them.
        let tally = RunTally {
            succeeded: 20,
            skipped: 1,
            not_attempted: vec![crate::NO_SESSION_MANAGER.to_string()],
            failed: 2,
            planned_total: 23,
            status: status.clone(),
            aborted: None,
        };
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        render_run_rollup(&tally, RunTitle::Apply, &printer, None);
        drop(printer);
        let out = crate::test_helpers::captured_text(&buf);
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            // The abort line is a SENTENCE about an interrupted run, not a
            // count list: it accounts for everything on the one line a reader
            // keeps, and it is `Role::Warn`, so it paints no non-success count
            // as a success.
            if line.contains("aborted by signal") {
                continue;
            }
            // The block's closing next step names the failure it is about; it
            // is an instruction, not one of the outcome classes.
            if line.starts_with(theme.icon_arrow.as_str()) {
                continue;
            }
            let named: Vec<&str> = classes
                .iter()
                .map(|(word, _)| *word)
                .filter(|word| line.contains(word))
                .collect();
            assert!(
                named.len() <= 1,
                "{status:?}: one line states {named:?} — two outcome classes \
                 under one role: {out:?}"
            );
            let Some(word) = named.first() else { continue };
            seen.push(word);
            let (_, role) = classes
                .iter()
                .find(|(w, _)| w == word)
                .copied()
                .unwrap_or(("", Role::Info));
            let (glyph, _) = crate::output::renderer::role_glyph(&theme, role);
            assert!(
                glyph.is_some_and(|g| line.starts_with(g)),
                "{status:?}: the {word:?} line must wear {role:?}'s glyph \
                 ({glyph:?}): {line:?}"
            );
        }
    }
    for (word, _) in classes {
        assert!(
            seen.contains(&word),
            "the walk never rendered a {word:?} line, so it proved nothing about it"
        );
    }

    // The fifth class, on the tally shape that produces it: work the run
    // planned and never reached. Its own line, its own role, and never fused
    // into a sentence with any of the four above.
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(
        &RunTally {
            succeeded: 1,
            skipped: 0,
            not_attempted: Vec::new(),
            failed: 0,
            planned_total: 4,
            status: ApplyStatus::Success,
            aborted: None,
        },
        RunTitle::Apply,
        &printer,
        None,
    );
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    let shortfall = out
        .lines()
        .find(|l| l.contains("not attempted"))
        .unwrap_or_else(|| panic!("the shortfall must state itself: {out:?}"));
    let (glyph, _) = crate::output::renderer::role_glyph(&theme, Role::Info);
    assert!(
        glyph.is_some_and(|g| shortfall.starts_with(g)),
        "the shortfall wears its own role, not a success glyph: {shortfall:?}"
    );
    assert!(
        !shortfall.contains("succeeded"),
        "the shortfall shares no line with the success count: {out:?}"
    );
}

/// A rollup is a block of STATUS lines, and every status line reserves the
/// glyph column. The partial arm's failure count took `Role::Accent`, which
/// reserves none, so the one line counting the failures hung a column left of
/// the two above it — unmarked, in a report where every failed action row
/// carries a red glyph.
#[test]
fn every_rollup_line_reserves_the_glyph_column() {
    let theme = crate::output::Theme::default();
    let statuses = [
        ApplyStatus::Success,
        ApplyStatus::Partial,
        ApplyStatus::Failed,
        ApplyStatus::InProgress,
        ApplyStatus::Aborted,
    ];
    let titles = [
        RunTitle::Plan,
        RunTitle::Apply,
        RunTitle::Reconcile,
        RunTitle::Backup,
        RunTitle::Restore,
    ];
    // Two shapes per arm: one that reached what it planned, and one that fell
    // short — the shortfall line is pushed by the renderer, outside
    // `rollup_lines`, and the `nothing_attempted` arm only exists in the second.
    let shapes = [(2usize, 1usize, 3usize), (0, 0, 3)];
    let mut checked = 0usize;
    for status in &statuses {
        for title in titles {
            for (succeeded, failed, planned_total) in shapes {
                let tally = RunTally {
                    succeeded,
                    skipped: 0,
                    not_attempted: Vec::new(),
                    failed,
                    planned_total,
                    status: status.clone(),
                    aborted: None,
                };
                let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
                render_run_rollup(&tally, title, &printer, None);
                drop(printer);
                let out = crate::test_helpers::captured_text(&buf);
                for line in out.lines().filter(|l| !l.trim().is_empty()) {
                    let glyph = line.chars().next().unwrap_or(' ');
                    assert!(
                        [
                            theme.icon_ok.as_str(),
                            theme.icon_warn.as_str(),
                            theme.icon_fail.as_str(),
                            theme.icon_pending.as_str(),
                            theme.icon_running.as_str(),
                            theme.icon_skipped.as_str(),
                            theme.icon_info.as_str(),
                            // The block's closing next step reserves the same
                            // column with the hint marker.
                            theme.icon_arrow.as_str(),
                        ]
                        .iter()
                        .any(|icon| icon.starts_with(glyph)),
                        "{status:?}/{title:?}: rollup line opens on {glyph:?}, \
                         not a glyph: {out:?}"
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(
        checked >= 50,
        "the walk rendered almost nothing ({checked} lines) — it cannot pass vacuously"
    );

    // The role table the render above reads, stated directly: a role with no
    // glyph is what a rollup line may never take.
    for role in [Role::Accent, Role::Secondary] {
        assert!(
            crate::output::renderer::role_glyph(&theme, role)
                .0
                .is_none(),
            "{role:?} is the class this test exists to keep out of a rollup"
        );
    }
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
        installed: None,
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
        "2 actions succeeded, 1 skipped, 1 not attempted: no session manager"
    );

    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(
        &tally,
        RunTitle::Apply,
        &printer,
        Some(Duration::from_millis(278_200)),
    );
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    // One outcome class per line, each in the role its own action rows wore:
    // fused into the tick's detail, "1 not attempted" rendered under a green
    // glyph and invited a reader to sum four numbers against a header that
    // counts two of them.
    assert_eq!(
        out.lines()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>(),
        vec![
            "\u{2713} Apply complete — 2 actions succeeded (278.2s wall)",
            "\u{2205} 1 skipped",
            "\u{2205} 1 not attempted: no session manager",
        ],
        "the verdict states each outcome on its own line: {out:?}"
    );
    assert_eq!(
        out.matches("not attempted").count(),
        1,
        "no shortfall line for an action the header never promised: {out:?}"
    );

    // A second reason is a second clause, not a second count; one reason twice
    // is one clause over a count of two.
    tally_with_reasons(&["a", "a", "b"], |counts| {
        assert_eq!(counts, "2 actions succeeded, 3 not attempted: a, b");
    });
}

/// The closing line carries ONE em-dash — the title's join to its detail — and
/// ONE trailing parenthetical, the elapsed. The withheld clause used to bring a
/// second of each: `(1 not attempted — no session manager) (278.2s)` nested an
/// em-dash under the detail's and set a caveat's parenthetical beside a
/// measurement's, on the line a reader stops on.
#[test]
fn the_closing_line_holds_one_em_dash_and_one_trailing_parenthetical() {
    let tally = RunTally {
        succeeded: 21,
        skipped: 0,
        not_attempted: vec![crate::NO_SESSION_MANAGER.to_string()],
        failed: 0,
        planned_total: 21,
        status: ApplyStatus::Success,
        aborted: None,
    };
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(
        &tally,
        RunTitle::Apply,
        &printer,
        Some(Duration::from_millis(278_200)),
    );
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    let line = out
        .lines()
        .find(|l| l.contains("complete"))
        .unwrap_or_default();
    assert_eq!(line.matches(" — ").count(), 1, "one em-dash: {line:?}");
    assert_eq!(line.matches('(').count(), 1, "one parenthetical: {line:?}");
    assert!(
        !line.contains(") ("),
        "never two parentheticals back to back: {line:?}"
    );
    assert!(
        line.ends_with(" wall)"),
        "the one parenthetical is the wall-clock total: {line:?}"
    );
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

/// `render_plan_tree` claims the report column and then renders a produced
/// count as a BULLET's trailing detail, so the bullet has to pad to the same
/// column a status row does — otherwise a report whose two row shapes are
/// interleaved puts its em-dashes in two places, and neither matches the apply
/// that settles the same actions a beat later.
#[test]
fn every_detail_bearing_row_of_a_report_lands_in_the_reports_one_column() {
    let write_env = Action::Env(super::super::types::EnvAction::WriteEnvFile {
        path: PathBuf::from("/home/u/.cfgd.env"),
        content: String::new(),
        vars: 3,
        aliases: 3,
    });
    let subject = action_display_subject(&write_env).body.clone();
    let detail = super::super::action_produced_detail(&write_env, None)
        .expect("a write of 3 vars and 3 aliases states what it produced");
    // A far longer sibling in the SAME report, so the column the short row
    // pads to is one its own width would never have produced.
    let plan = plan_of(vec![phase(
        PhaseName::Prerequisites,
        vec![
            write_env,
            install(
                "apt",
                &["a-very-long-package-name-indeed", "and-another-one"],
            ),
        ],
    )]);
    let width = report_align_width(&plan, None);

    let dash_column = |render: &dyn Fn(&Printer)| -> usize {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        render(&printer);
        drop(printer);
        let out = crate::test_helpers::captured_text(&buf);
        let line = out
            .lines()
            .find(|l| l.contains(&subject) && l.contains(" — "))
            .unwrap_or_else(|| panic!("no detail-bearing row for {subject:?} in:\n{out}"))
            .to_string();
        crate::output::measure_width(
            line.split_once(" — ")
                .unwrap_or_else(|| panic!("row carries the detail separator: {line:?}"))
                .0,
        )
    };

    let previewed = dash_column(&|printer| render_plan_tree(&plan, None, printer));
    // The apply's arm: the same action, settled through the writer every
    // executed line commits, under a report that claimed the same column.
    let settled = dash_column(&|printer| {
        let _column = printer.report_column(width);
        let phase_section = printer.section_phase(&PhaseName::Prerequisites.section_label());
        let owner = phase_section.section_owner(&OwnerLabel::new("cfgd", "env"));
        owner.live_column(width);
        super::super::apply::emit_action_line(
            printer,
            &owner,
            &super::super::apply::ActionOutcome::for_test_settled(
                &subject,
                crate::output::Role::Ok,
                &detail,
            ),
        );
    });

    assert_eq!(
        previewed, settled,
        "a produced count must land at the report's one column on both trees"
    );
    // Anti-vacuity: the row really was padded, rather than both trees gluing
    // the detail straight onto a subject that happened to be equally short.
    assert!(
        previewed > crate::output::measure_width(&subject),
        "the short subject pads out to the report column ({previewed} vs its own \
         width {}), not to its own end",
        crate::output::measure_width(&subject)
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
            unit_source: Some("~/notes.md"),
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
    // `Source` is the unit's declared path — a header fact, the counterpart of
    // the target the action row names — and sits with the other facts about
    // the run, above the count of what it will do.
    for row in [
        "Config   /home/me/.config/cfgd/cfgd.yaml",
        "Profile  work",
        "Sources  team (profile shared)",
        "Source   ~/notes.md",
        "Actions  1 planned",
    ] {
        assert!(out.contains(row), "missing {row:?} in: {out:?}");
    }
    let source_at = out.find("Source   ").expect("Source row");
    let actions_at = out.find("Actions  ").expect("Actions row");
    assert!(
        source_at < actions_at,
        "the unit's source is a fact about the run, stated before its count: {out:?}"
    );
}

/// A run acting on no one unit renders no `Source` row: `backup run` over
/// every declared unit, an apply, a plan.
#[test]
fn a_run_with_no_unit_source_prints_no_source_row() {
    let run = ApplyRun::unplanned(
        RunContext {
            title: RunTitle::Backup,
            config_path: None,
            profile: Some("work"),
            sources: &[],
            modules: &[],
            trigger: None,
            subject: None,
            unit_source: None,
        },
        1,
    );
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    run.header(&printer);
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    assert!(!out.contains("Source "), "{out:?}");
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
            unit_source: None,
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
            unit_source: None,
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

/// A count an action produces is the row's DETAIL on the plan tree too, so
/// the preview bullet and the apply's status row state one fact in one slot.
/// `deploy a, b (6 files)` baked the count into its subject while the env
/// write beside it said `— 3 vars, 3 aliases`; both now read the ONE producer.
#[test]
fn the_plan_tree_hangs_a_produced_count_off_the_bullet_not_the_subject() {
    let files: Vec<crate::modules::ResolvedFile> = (0..5)
        .map(|i| crate::modules::ResolvedFile {
            source: PathBuf::from(format!("/cache/mod/f{i}")),
            target: PathBuf::from(format!("/home/u/.f{i}")),
            is_git_source: false,
            strategy: None,
            encryption: None,
            permissions: None,
            patch: None,
        })
        .collect();
    let deploy = Action::Module(crate::reconciler::ModuleAction::local(
        "big",
        crate::reconciler::ModuleActionKind::DeployFiles {
            declared_total: files.len(),
            files,
        },
    ));
    let env = Action::Env(super::super::types::EnvAction::WriteEnvFile {
        path: PathBuf::from("/home/u/.cfgd.env"),
        content: String::new(),
        vars: 3,
        aliases: 1,
    });
    let plan = plan_of(vec![
        phase(PhaseName::Prerequisites, vec![env]),
        phase(PhaseName::Files, vec![deploy]),
    ]);

    let (printer, cap) = Printer::for_test_doc();
    render_plan_tree(&plan, None, &printer);
    drop(printer);
    // Padding collapsed: a bullet's detail pads out to the report's one column
    // (`every_detail_bearing_row_of_a_report_lands_in_the_reports_one_column`),
    // and the claim here is about which SLOT the count sits in.
    let out = crate::output::strip_ansi(&cap.human())
        .split('\n')
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        out.contains("- deploy /home/u/.f0, /home/u/.f1 — 5 files"),
        "the count sits after the em-dash, beside the subject: {out}"
    );
    assert!(
        !out.contains("files)"),
        "no subject carries a parenthesised count: {out}"
    );
    assert!(
        out.contains("- write /home/u/.cfgd.env — 3 vars, 1 alias"),
        "the env write states its produced counts in the same slot: {out}"
    );
}

/// Every verdict a run can close on, and whether it leaves the reader with
/// something to do. A run that did not converge is the only closing line in
/// the CLI family that ever shipped without one — a drift verdict has
/// `heal_drift_hint`, a refused source `source_failure_next_step`, a mutating
/// verb `success_next_step`, a decisions section `answer_decisions_hint`, a
/// no-op run `nothing_to_do_verdict` — and the env-file reminder that happened
/// to be on screen is about a different subject.
#[test]
fn every_unfinished_verdict_closes_on_the_one_next_step() {
    const TITLES: &[RunTitle] = &[
        RunTitle::Plan,
        RunTitle::Apply,
        RunTitle::Reconcile,
        RunTitle::Backup,
        RunTitle::Restore,
    ];
    let converged = |status: ApplyStatus| RunTally {
        succeeded: 2,
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 1,
        planned_total: 3,
        status,
        aborted: None,
    };
    let withheld = RunTally {
        succeeded: 0,
        skipped: 0,
        not_attempted: vec!["no session manager".to_string()],
        failed: 0,
        planned_total: 1,
        status: ApplyStatus::Success,
        aborted: None,
    };
    for title in TITLES {
        for tally in [
            converged(ApplyStatus::Partial),
            converged(ApplyStatus::Failed),
            converged(ApplyStatus::Aborted),
            converged(ApplyStatus::InProgress),
            withheld.clone(),
        ] {
            let next = super::run_next_step(&tally, *title).unwrap_or_else(|| {
                panic!(
                    "{:?} on a {title:?} run leaves the reader nothing to do",
                    tally.status
                )
            });
            assert!(
                next.contains('`') && next.contains("cfgd "),
                "a closing hint names the command that comes next, in backticks: {next:?}"
            );
            assert!(
                !next.contains("<name>") || matches!(title, RunTitle::Backup | RunTitle::Restore),
                "only a run over one declared unit has a unit to name: {next:?}"
            );
            // The verdict lines state facts; the instruction is the hint's.
            for (_, subject, detail) in super::rollup_lines(&tally, *title) {
                let line = format!("{subject} {}", detail.unwrap_or_default());
                assert!(
                    !line.contains("rerun") && !line.contains("run `"),
                    "an instruction baked into a verdict line cannot be found by a \
                     sweep over the hint composers: {line:?}"
                );
            }
        }
        // A run that converged has nothing left to say; the surfaces that do
        // (a withheld decision, a written env file) say it themselves.
        assert_eq!(
            super::run_next_step(&converged(ApplyStatus::Success), *title),
            None,
            "a converged {title:?} run must not close on a rerun instruction"
        );
    }
}

#[test]
fn a_failed_run_renders_its_next_step_under_the_verdict() {
    let tally = RunTally {
        succeeded: 21,
        skipped: 0,
        not_attempted: Vec::new(),
        failed: 1,
        planned_total: 22,
        status: ApplyStatus::Partial,
        aborted: None,
    };
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    render_run_rollup(&tally, RunTitle::Apply, &printer, None);
    drop(printer);
    let out = crate::test_helpers::captured_text(&buf);
    let hint = out
        .lines()
        .position(|l| l.contains("Fix what failed, then run `cfgd apply` again"))
        .unwrap_or_else(|| panic!("the partial verdict must close on its next step: {out:?}"));
    let failed = out
        .lines()
        .position(|l| l.contains("1 action failed"))
        .expect("the failure count is on screen");
    assert!(
        failed < hint,
        "the instruction follows the counts it is about: {out:?}"
    );
}

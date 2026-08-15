//! The one run skeleton: header block, plan preview, execution, rollup.
//!
//! It lives in core rather than in the CLI because the daemon renders the same
//! run — a reconcile tick differs from `cfgd apply` in its title and its
//! trigger row and in nothing else. A skeleton owned by the CLI would be a
//! shape the daemon had to re-implement, and the two would drift the first time
//! either grew a row.

use std::time::{Duration, Instant};

use crate::backup::BackupUnit;
use crate::errors::Result;
use crate::output::{OwnerLabel, Printer, Role, SectionGuard, measure_width};
use crate::state::{ApplyStatus, StateStore};

use super::apply::action_matches_phase_filter;
use super::format::action_display_subject;
use super::types::{
    Action, ApplyResult, Owner, OwnerGroup, Phase, PhaseFilter, PhaseName, Plan, SystemAction,
};

/// Heading for a hook group that runs around a reconcile but is not part of
/// the plan. Rendered through the same section primitive as a phase so the
/// tree stays coherent; deliberately not a [`PhaseName`].
pub const HOOKS_PHASE_LABEL: &str = "Drift Hooks";

/// The verdict a run prints when it found nothing to do. One string for every
/// surface that can reach it — `apply`, `plan`, `init --apply`, `module create
/// --apply` — because two spellings of "already converged" read as two
/// different outcomes to anyone comparing two commands' transcripts.
pub const MSG_NOTHING_TO_DO: &str = "Nothing to do — everything is up to date";

/// Heading for `spec.backups[]` work. Also not a [`PhaseName`]: backups are
/// declared work with their own hooks and record, but nothing plans them into
/// a [`Plan`] and nothing journals them into `apply_journal`.
pub const BACKUPS_PHASE_LABEL: &str = "Backups";

/// What a run calls itself — the heading it prints and the noun its rollup
/// lines are built from, so the two cannot disagree about what ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTitle {
    Plan,
    Apply,
    Reconcile,
    Backup,
}

impl RunTitle {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunTitle::Plan => "Plan",
            RunTitle::Apply => "Apply",
            RunTitle::Reconcile => "Reconcile",
            RunTitle::Backup => "Backup",
        }
    }
}

/// The context rows a run's header block carries. Every optional field is a row
/// that is omitted when it has no value, so a module-only run prints no
/// `Profile` row rather than an empty one.
pub struct RunContext<'a> {
    pub title: RunTitle,
    pub config_path: Option<&'a std::path::Path>,
    pub profile: Option<&'a str>,
    pub modules: &'a [String],
    /// What woke this run — the daemon's only extra row (`drift (3 resources)`,
    /// `schedule (daily)`).
    pub trigger: Option<&'a str>,
}

/// Whether a run asks before it acts.
pub enum Confirm {
    Skip,
    Ask(&'static str),
}

/// How a run ended.
///
/// NOT `RunOutcome`: `backup/mod.rs` already declares a private `struct
/// RunOutcome` that `record_run` consumes, and two meanings for one name inside
/// one crate is the drift the naming rules exist to stop.
pub enum RunDisposition {
    NothingToDo,
    Previewed,
    Declined,
    Applied {
        result: ApplyResult,
        /// What the run's `Backups` pseudo-phase produced, one report per unit
        /// and empty for a run that carried none. Returned rather than
        /// rendered-and-discarded because the caller's `-o json` payload names
        /// each unit's outcome, and re-deriving it from the state store would
        /// be a second answer to a question the run already answered.
        backups: Vec<crate::backup::BackupRunReport>,
    },
    /// A run built by [`ApplyRun::backups`] did its work. Nothing it ran was a
    /// plan action, so there is no [`ApplyResult`] to carry — only the status
    /// the `Backups` pseudo-phase rolled up, and the reports behind it.
    BackupsApplied {
        status: ApplyStatus,
        backups: Vec<crate::backup::BackupRunReport>,
    },
}

/// What [`ApplyRun::execute`] delegates the actual work to. One method, so the
/// CLI's reconciler call and the daemon's are the same shape to the skeleton.
pub trait RunExecutor {
    fn apply(&mut self, plan: &Plan, printer: &Printer) -> Result<ApplyResult>;
}

/// Everything a rollup reads, and nothing else.
///
/// [`ApplyResult`] produces one; so does a set of backup units, which has no
/// [`Plan`], no `apply_id` and no `ActionResult`s. Having the rollup take this
/// instead of an `&ApplyResult` is what makes `Backup complete — 2 action(s)
/// succeeded` producible without forging an apply row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTally {
    pub succeeded: usize,
    pub failed: usize,
    /// What the run set out to do. The `Actions  N planned` header row and the
    /// `⊙ N action(s) not attempted` shortfall line read the same field.
    pub planned_total: usize,
    pub status: ApplyStatus,
    pub aborted: Option<u8>,
}

impl RunTally {
    /// A run that set out to do nothing and did nothing.
    pub fn empty() -> Self {
        Self {
            succeeded: 0,
            failed: 0,
            planned_total: 0,
            status: ApplyStatus::Success,
            aborted: None,
        }
    }

    /// Fold in work that is not in the plan (the `Backups` pseudo-phase).
    ///
    /// Adds to `planned_total` as well as to the counts, so the shortfall
    /// arithmetic stays honest, and takes the worse `status` of the two.
    pub fn merge(&mut self, other: RunTally) {
        self.succeeded += other.succeeded;
        self.failed += other.failed;
        self.planned_total += other.planned_total;
        if status_severity(&other.status) > status_severity(&self.status) {
            self.status = other.status;
        }
        self.aborted = self.aborted.or(other.aborted);
    }

    /// The actions the run set out to do and never reached. `saturating_sub`
    /// because a debug build panics on underflow and cfgd-core may not carry a
    /// panic path.
    fn shortfall(&self) -> usize {
        self.planned_total
            .saturating_sub(self.succeeded + self.failed)
    }

    /// The run planned work and reached none of it — every action was withheld
    /// or skipped before it could be attempted.
    ///
    /// Distinct from a run that planned nothing: `Backup complete — 0 action(s)
    /// succeeded` read as a clean finish on a `cfgd backup run` that was
    /// refused by another holder of the unit's lock and exited 1, so the only
    /// two things on screen that could tell the user what happened — the ✓ and
    /// the exit code — disagreed.
    fn nothing_attempted(&self) -> bool {
        self.planned_total > 0 && self.succeeded == 0 && self.failed == 0
    }
}

/// Severity ordering for [`RunTally::merge`]. A backup unit's trouble must
/// never overwrite a higher-severity reconcile outcome with a lesser one, so
/// the merge takes the max rather than the last writer.
fn status_severity(status: &ApplyStatus) -> u8 {
    match status {
        ApplyStatus::Success => 0,
        ApplyStatus::InProgress => 1,
        ApplyStatus::Partial => 2,
        ApplyStatus::Aborted => 3,
        ApplyStatus::Failed => 4,
    }
}

impl ApplyResult {
    /// The rollup's view of an apply. The ONE place the conversion happens:
    /// every `ActionResult` lands in exactly one of the two counts, so the
    /// tally's shortfall is arithmetically `planned_total - action_results.len()`.
    pub fn tally(&self) -> RunTally {
        RunTally {
            succeeded: self.succeeded(),
            failed: self.failed(),
            planned_total: self.planned_total,
            status: self.status.clone(),
            aborted: self.aborted,
        }
    }
}

/// The `spec.backups[]` units a run must back up, and the store their records
/// go to. Units rather than names, because the pseudo-phase's alignment column
/// is derived from the unit before its first hook runs, which a `&[String]`
/// cannot supply.
struct PendingBackups<'a> {
    units: &'a [BackupUnit<'a>],
    store: &'a StateStore,
}

/// One run, from its header to its rollup.
pub struct ApplyRun<'a> {
    ctx: RunContext<'a>,
    plan: Option<&'a Plan>,
    filter: Option<&'a PhaseFilter>,
    preview_only: bool,
    backups: Option<PendingBackups<'a>>,
    withheld: Option<&'a super::WithheldDecisions>,
    decide_answerable: bool,
}

impl<'a> ApplyRun<'a> {
    pub fn new(ctx: RunContext<'a>, plan: &'a Plan) -> Self {
        Self {
            ctx,
            plan: Some(plan),
            filter: None,
            preview_only: false,
            backups: None,
            withheld: None,
            decide_answerable: true,
        }
    }

    pub fn with_filter(mut self, f: Option<&'a PhaseFilter>) -> Self {
        self.filter = f;
        self
    }

    /// The source decisions this run's plan was pruned with, so the header can
    /// name what is missing from it. Every path that prunes with them passes
    /// them here.
    pub fn with_withheld(mut self, withheld: &'a super::WithheldDecisions) -> Self {
        self.withheld = Some(withheld);
        self
    }

    /// Whether `cfgd decide` on this run's config can record an answer for a
    /// classified-but-unrecorded item — false when the config does not own the
    /// decision store ([`super::owns_decision_store`]). Only the `id == 0`
    /// rows change their instruction on it: resolving a RECORDED row is not a
    /// mint, so its instruction holds on every config.
    pub fn decisions_answerable(mut self, answerable: bool) -> Self {
        self.decide_answerable = answerable;
        self
    }

    /// The units this run must back up, and the store their records go to.
    pub fn with_pending_backups(
        mut self,
        units: &'a [BackupUnit<'a>],
        store: &'a StateStore,
    ) -> Self {
        self.backups = Some(PendingBackups { units, store });
        self
    }

    /// A run whose only work is `spec.backups[]` — `cfgd backup run` and the
    /// daemon's scheduled fire. There is no [`Plan`] and none is synthesized,
    /// which is also what suppresses the nothing-to-do verdict an empty plan
    /// would otherwise produce.
    pub fn backups(
        ctx: RunContext<'a>,
        units: &'a [BackupUnit<'a>],
        store: &'a StateStore,
    ) -> Self {
        Self {
            ctx,
            plan: None,
            filter: None,
            preview_only: false,
            backups: Some(PendingBackups { units, store }),
            withheld: None,
            decide_answerable: true,
        }
    }

    /// Mark this run as preview-only (`cfgd plan`, `--dry-run`, a notify-only
    /// tick): suppresses [`ApplyRun::header`]'s `Actions` row, because such a
    /// run's count belongs to its closing verdict line instead. One carrier per
    /// run, decided once here rather than per call site.
    pub fn preview_only(mut self) -> Self {
        self.preview_only = true;
        self
    }

    /// Title + context rows, then the plan's warnings as `Role::Warn` lines at
    /// row depth.
    ///
    /// Omits every empty row (a run with no in-scope work has no `Phases` and
    /// no `Actions` row); `Phases` lists only phases holding in-scope work
    /// **that renders**, and `Actions` renders `{n} planned` unless the run is
    /// preview-only. Warnings live here, not in the preview, so they survive
    /// `--yes`.
    ///
    /// `n` is computed here, before the run, from whichever source this run has
    /// — never from `ApplyResult.planned_total`, which does not exist yet. With
    /// a plan it is the in-scope predicate over the plan and the filter, the
    /// same predicate `Reconciler::apply` uses for its own `planned_total`,
    /// which is what makes the two reconcilable afterwards, plus one per
    /// pending backup item. With no plan it is the pending backup items alone.
    /// A backup item is one hook entry or one unit's snapshot; see
    /// [`ApplyRun::pending_backup_count`] for what the engine can enumerate
    /// ahead of the run.
    pub fn header(&self, printer: &Printer) {
        let mut rows: Vec<(String, String)> = Vec::new();
        if let Some(path) = self.ctx.config_path {
            rows.push(("Config".to_string(), crate::to_posix_string(path)));
        }
        if let Some(profile) = self.ctx.profile {
            rows.push(("Profile".to_string(), profile.to_string()));
        }
        // A platform-gated module contributed no work, so it leaves the name
        // list and returns as an annotation on it. That annotation IS the
        // render of `PhaseName::Modules`, which prints no block of its own.
        let skips = platform_skips(self.plan);
        let names: Vec<&str> = self
            .ctx
            .modules
            .iter()
            .map(String::as_str)
            .filter(|name| !skips.iter().any(|(skipped, _)| skipped == name))
            .collect();
        let annotation = skips
            .iter()
            .map(|(name, reason)| format!("{name} skipped: {reason}"))
            .collect::<Vec<_>>()
            .join(", ");
        let modules_row = match (names.is_empty(), annotation.is_empty()) {
            (_, true) => names.join(", "),
            // Every module gated off: the parentheses would enclose the whole
            // value, so the annotation stands alone as the row.
            (true, false) => printer.muted(&annotation),
            (false, false) => format!(
                "{} {}",
                names.join(", "),
                printer.muted(&format!("({annotation})"))
            ),
        };
        if !modules_row.is_empty() {
            rows.push(("Modules".to_string(), modules_row));
        }
        if let Some(trigger) = self.ctx.trigger {
            rows.push(("Trigger".to_string(), trigger.to_string()));
        }
        // The `Phases` row names exactly the blocks the tree will print, so it
        // is read off the tree rather than recomputed from the plan.
        let phases: Vec<&str> = self
            .plan
            .map(|plan| in_scope_tree(plan, self.filter, PhaseCoverage::Rendered))
            .unwrap_or_default()
            .into_iter()
            .map(|(phase, _)| phase.name.display_name())
            .collect();
        if !phases.is_empty() {
            rows.push(("Phases".to_string(), phases.join(", ")));
        }
        if !self.preview_only {
            let planned = self.planned_count();
            if planned > 0 {
                rows.push(("Actions".to_string(), format!("{planned} planned")));
            }
        }

        let warnings: &[String] = self.plan.map_or(&[], |p| p.warnings.as_slice());
        if rows.is_empty() && warnings.is_empty() {
            printer.heading(self.ctx.title.as_str());
        } else {
            // One section rather than a heading plus a top-level kv block: the
            // warnings belong to the header block and have to land at the same
            // indent as the rows they follow, which only a section's depth
            // gives.
            let head = printer.section(self.ctx.title.as_str());
            head.kv_block(rows);
            for warning in warnings {
                head.status_simple(Role::Warn, warning);
            }
        }
        self.render_withheld(printer);
    }

    /// The decisions that took resources out of this run, named directly under
    /// the header.
    ///
    /// `docs/sources.md` promises that an item missing from a plan is always
    /// explained by a decision the operator can see, and both withholding
    /// states have to keep it: a row still awaiting an answer and a row already
    /// declined remove work identically, so a plan that named only the first
    /// would leave the second as an unexplained absence. They are separate
    /// blocks because the answer differs — one wants a decision, the other
    /// already has one and would need reversing.
    ///
    /// It renders from the header rather than from the preview so every path
    /// that shows a run shows it in the same place: the tree below, the
    /// confirmation prompt an interactive apply raises, and the `-o json`
    /// payload's `pendingDecisions` / `rejectedDecisions` all describe one set.
    fn render_withheld(&self, printer: &Printer) {
        let Some(withheld) = self.withheld else {
            return;
        };
        let row = |d: &crate::state::PendingDecision, suffix: &str| {
            format!(
                "{} {} — {} by {} ({suffix})",
                d.tier, d.resource, d.action, d.source
            )
        };
        if !withheld.pending.is_empty() {
            let section = printer.section("Pending Decisions (not included in this plan)");
            for d in &withheld.pending {
                // An unrecorded item (`id` 0) is answerable only where `cfgd
                // decide` can mint its row. On a run whose config does not own
                // the store, the usual instruction names a command that will
                // refuse — so say what is true instead. Recorded rows resolve
                // without a mint and keep the instruction everywhere.
                let suffix = if d.id == 0 && !self.decide_answerable {
                    "not yet recorded; decide from the machine's own config, or with --state-dir"
                } else {
                    "run `cfgd decide accept/reject`"
                };
                section.status_simple(Role::Info, row(d, suffix));
            }
        }
        if !withheld.rejected.is_empty() {
            let section = printer.section("Declined Decisions (not included in this plan)");
            for d in &withheld.rejected {
                section.status_simple(
                    Role::Skipped,
                    row(d, "declined; run `cfgd decide accept` to include"),
                );
            }
        }
    }

    /// The phase → owner → action tree, as bullets.
    ///
    /// No footer of its own — the count is the header's `Actions` row for an
    /// executing run and part of the caller's verdict line for a preview-only
    /// one, so a tree rendered above a confirmation prompt carries no count at
    /// all. The run's view over [`render_plan_tree`].
    pub fn preview(&self, printer: &Printer) {
        if let Some(plan) = self.plan {
            render_plan_tree(plan, self.filter, printer);
        }
    }

    /// header → (preview + confirm, when [`Confirm::Ask`] and the plan has
    /// work) → execute → `Backups` pseudo-phase → rollup. Never exits, and
    /// never prompts on [`Confirm::Skip`].
    pub fn execute(
        &self,
        printer: &Printer,
        confirm: Confirm,
        exec: &mut dyn RunExecutor,
    ) -> Result<RunDisposition> {
        self.header(printer);
        let Some(plan) = self.plan else {
            // A run built by `backups()` has no plan action to execute; its
            // whole body is the pseudo-phase.
            return match &self.backups {
                Some(_) => {
                    let (status, backups) = self.execute_backups_after_header(printer)?;
                    Ok(RunDisposition::BackupsApplied { status, backups })
                }
                None => Ok(RunDisposition::NothingToDo),
            };
        };
        if self.preview_only {
            self.preview(printer);
            return Ok(RunDisposition::Previewed);
        }
        if let Confirm::Ask(prompt) = confirm
            && self.in_scope_action_count() > 0
        {
            self.preview(printer);
            if !printer.prompt_confirm(prompt).unwrap_or(false) {
                return Ok(RunDisposition::Declined);
            }
        }

        let started = Instant::now();
        let result = exec.apply(plan, printer)?;
        let mut tally = result.tally();
        let (backup_tally, backups) = self.render_backups(printer)?;
        tally.merge(backup_tally);
        render_run_rollup(&tally, self.ctx.title, printer, Some(started.elapsed()));
        Ok(RunDisposition::Applied { result, backups })
    }

    /// header → `Backups` pseudo-phase → rollup, for a run built by
    /// [`ApplyRun::backups`]. No preview, no confirm, no [`RunExecutor`] —
    /// nothing here is a plan action, and [`RunDisposition`] has no arm that
    /// would be true.
    pub fn execute_backups(
        &self,
        printer: &Printer,
    ) -> Result<(ApplyStatus, Vec<crate::backup::BackupRunReport>)> {
        self.header(printer);
        self.execute_backups_after_header(printer)
    }

    fn execute_backups_after_header(
        &self,
        printer: &Printer,
    ) -> Result<(ApplyStatus, Vec<crate::backup::BackupRunReport>)> {
        let started = Instant::now();
        let (tally, backups) = self.render_backups(printer)?;
        let status = render_run_rollup(&tally, self.ctx.title, printer, Some(started.elapsed()));
        Ok((status, backups))
    }

    /// The `Backups` pseudo-phase: one owner group per unit, each carrying that
    /// unit's outcome. Emits nothing and tallies nothing when the run carries
    /// no units, which is every run that did not ask for them.
    fn render_backups(
        &self,
        printer: &Printer,
    ) -> Result<(RunTally, Vec<crate::backup::BackupRunReport>)> {
        let Some(pending) = &self.backups else {
            return Ok((RunTally::empty(), Vec::new()));
        };
        if pending.units.is_empty() {
            return Ok((RunTally::empty(), Vec::new()));
        }
        // Derived ONCE, before the first hook runs: a live stream cannot buffer
        // to find its own column, and the widest label — the snapshot's — is
        // not minted until mid-run.
        let labels: Vec<Vec<String>> = pending
            .units
            .iter()
            .map(crate::backup::backup_unit_labels)
            .collect();
        let width = align_width_of(labels.iter().flatten().map(String::as_str));

        let phase = pseudo_phase(printer, BACKUPS_PHASE_LABEL);
        let mut tally = RunTally::empty();
        let mut reports = Vec::with_capacity(pending.units.len());
        for (unit, planned) in pending.units.iter().zip(&labels) {
            let report =
                crate::backup::run_backup_group(unit, pending.store, &phase, width, printer);
            tally.merge(backup_report_tally(&report, planned.len()));
            reports.push(report);
        }
        Ok((tally, reports))
    }

    /// The same in-scope predicate `Reconciler::apply` counts its own
    /// `planned_total` with, so the header's number and the rollup's are
    /// reconcilable rather than two independent guesses.
    ///
    /// Deliberately its own walk rather than a `len()` over [`in_scope_tree`]:
    /// the tree drops `PhaseName::Modules` because that phase renders no block,
    /// while `Reconciler::apply` counts its skips like any other action. Taking
    /// the count off the tree would under-report every run holding a
    /// platform-gated module skip, and the rollup would then always report a
    /// shortfall that did not happen.
    fn in_scope_action_count(&self) -> usize {
        let Some(plan) = self.plan else {
            return 0;
        };
        plan.phases
            .iter()
            .map(|phase| match self.filter {
                Some(filter) => phase
                    .owned_actions()
                    .filter(|(owner, action)| {
                        action_matches_phase_filter(&phase.name, owner, action, filter)
                    })
                    .count(),
                None => phase.action_count(),
            })
            .sum()
    }

    /// The `Backups` pseudo-phase's item count: one per hook entry plus one
    /// snapshot per unit.
    ///
    /// The same enumeration [`crate::backup::backup_unit_labels`] measures the
    /// alignment column over, so the header's `Actions N planned` and the
    /// rollup's counts reconcile against one list rather than two: a unit whose
    /// `preBackup` hook fails renders one line fewer than this and the
    /// difference surfaces as the run's `⊙ N action(s) not attempted`.
    fn pending_backup_count(&self) -> usize {
        self.backups.as_ref().map_or(0, |p| {
            p.units
                .iter()
                .map(|unit| crate::backup::backup_unit_labels(unit).len())
                .sum()
        })
    }

    fn planned_count(&self) -> usize {
        self.in_scope_action_count() + self.pending_backup_count()
    }
}

/// One owner group's in-scope actions, in group order. Each renders under the
/// subject [`action_display_subject`] derives from it alone, so no positional
/// pairing back into a per-group line vector is needed.
pub type ScopedGroup<'p> = (&'p OwnerGroup, Vec<&'p Action>);

/// One phase's renderable block: the phase, and every group in it holding
/// in-scope work.
pub type ScopedPhase<'p> = (&'p Phase, Vec<ScopedGroup<'p>>);

/// Which phases an [`in_scope_tree`] walk yields.
///
/// The one axis on which the human tree and the `-o json` payload disagree, so
/// it is a parameter of the ONE walk rather than a second walk: everything else
/// — the per-action filter, the empty-group prune, the empty-phase prune, the
/// group order — is shared, and membership cannot drift between the two
/// surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseCoverage {
    /// Every phase the plan holds. The complete inventory a structured
    /// consumer reads, `PhaseName::Modules` included.
    Complete,
    /// `PhaseName::Modules` omitted: it holds platform-gated skips, which the
    /// header's `Modules` row annotates rather than the tree drawing them as a
    /// block of their own.
    Rendered,
}

/// The platform-gated module skips a plan carries, as `(module, reason)` in
/// plan order.
///
/// The reason is the action's own field rather than a re-derivation from the
/// module, so the header row and the `-o json` payload carry the same string.
fn platform_skips(plan: Option<&Plan>) -> Vec<(&str, &str)> {
    let Some(plan) = plan else {
        return Vec::new();
    };
    plan.phases
        .iter()
        .filter(|phase| phase.name == PhaseName::Modules)
        .flat_map(|phase| phase.owned_actions())
        .filter_map(|(_, action)| match action {
            Action::Module(crate::reconciler::ModuleAction {
                module_name,
                kind: crate::reconciler::ModuleActionKind::Skip { reason },
                ..
            }) => Some((module_name.as_str(), reason.as_str())),
            _ => None,
        })
        .collect()
}

/// The phases and groups that hold in-scope work, in plan order. The ONE
/// in-scope walk, shared by the human tree and by the `-o json` payload.
///
/// Filtering per action rather than per phase is what keeps a preview's scope
/// equal to the run's: `--phase modules` selects module-owned work wherever its
/// kind routed it, and `--phase post-scripts` reaches module lifecycle scripts.
/// A group the filter empties is dropped, then a phase left with no group, so
/// neither surface renders an owner header over nothing.
///
/// Group order is [`Owner::sort_key`]'s by construction — [`Phase`] can hold
/// its groups in no other order — so nothing here sorts.
pub fn in_scope_tree<'p>(
    plan: &'p Plan,
    filter: Option<&PhaseFilter>,
    coverage: PhaseCoverage,
) -> Vec<ScopedPhase<'p>> {
    let mut tree = Vec::new();
    for phase in &plan.phases {
        if coverage == PhaseCoverage::Rendered && phase.name == PhaseName::Modules {
            continue;
        }
        let mut groups = Vec::new();
        for group in phase.groups() {
            let actions: Vec<&Action> = group
                .actions
                .iter()
                .filter(|action| match filter {
                    Some(f) => action_matches_phase_filter(&phase.name, &group.owner, action, f),
                    None => true,
                })
                .collect();
            if !actions.is_empty() {
                groups.push((group, actions));
            }
        }
        if !groups.is_empty() {
            tree.push((phase, groups));
        }
    }
    tree
}

/// The phase → owner → action tree, as bullets. The ONE tree renderer.
///
/// Free rather than an [`ApplyRun`] method so a caller holding nothing but a
/// plan reaches it without inventing a [`RunContext`] whose every row would be
/// empty — a fabricated context is a header waiting to be printed by accident.
pub fn render_plan_tree(plan: &Plan, filter: Option<&PhaseFilter>, printer: &Printer) {
    for (phase, groups) in in_scope_tree(plan, filter, PhaseCoverage::Rendered) {
        let phase_section = printer.section_phase(&phase.name.section_label());
        for (group, actions) in groups {
            let label = OwnerLabel::new(group.owner.kind.as_str(), &group.owner.name);
            let owner_section = phase_section.section_owner(&label);
            for action in actions {
                let item = action_display_subject(action).to_string();
                // An unknown system key is almost always a typo, so it keeps
                // its warning role instead of reading as ordinary planned work.
                if matches!(
                    action,
                    Action::System(SystemAction::Skip { unknown: true, .. })
                ) {
                    owner_section.status_simple(Role::Warn, item);
                } else {
                    owner_section.bullet(item);
                }
            }
        }
    }
}

/// One backup unit's contribution to the run's rollup: `succeeded`/`failed`
/// counted from the lines its group actually emitted, `planned_total` from
/// `planned` — the same per-unit enumeration the header counted and the
/// alignment column was measured over.
///
/// The two counts are deliberately different sources. A unit whose `preBackup`
/// hook list aborted never ran the hooks after the failure, so it emits fewer
/// lines than it planned, and that difference is the run's
/// `⊙ N action(s) not attempted`. A `Busy` skip contributes no item at all —
/// the unit IS being backed up, just not here — so its one `Role::Skipped` line
/// moves neither count nor exit code and the unit surfaces only as the
/// shortfall.
fn backup_report_tally(report: &crate::backup::BackupRunReport, planned: usize) -> RunTally {
    let succeeded = report.items.iter().filter(|item| item.ok).count();
    RunTally {
        succeeded,
        failed: report.items.len() - succeeded,
        planned_total: planned,
        // A skip is not a partial run of this unit; it is no run of it, so it
        // leaves the run's status alone.
        status: match (&report.skipped, report.is_clean()) {
            (Some(_), _) | (None, true) => ApplyStatus::Success,
            (None, false) => ApplyStatus::Partial,
        },
        aborted: None,
    }
}

/// A pseudo-phase heading held open across execution, so each item's status
/// lands under its owner.
pub struct PseudoPhase<'a> {
    section: SectionGuard<'a>,
    /// Work reached from inside a pseudo-phase renders at the phase's depth
    /// rather than tripping the top-level structural assert.
    _inherit: crate::output::renderer::DepthInheritGuard<'a>,
}

impl PseudoPhase<'_> {
    /// Owner group inside the pseudo-phase. `width` is the alignment column
    /// every group in this pseudo-phase shares; the caller derives it with
    /// [`align_width_of`] before the first item runs, because a live stream
    /// cannot buffer to find it.
    #[must_use = "the group closes when the SectionGuard is dropped; bind it"]
    pub fn owner(&self, owner: &Owner, width: usize) -> SectionGuard<'_> {
        let label = OwnerLabel::new(owner.kind.as_str(), &owner.name);
        let group = self.section.section_owner(&label);
        group.live_column(width);
        group
    }
}

/// Open a pseudo-phase heading ([`HOOKS_PHASE_LABEL`], [`BACKUPS_PHASE_LABEL`])
/// as a section, for work that surrounds a run without being planned. Held
/// across execution so each item's status lands under its owner.
///
/// A free function, not an [`ApplyRun`] associated function: the daemon's two
/// `onDrift` arms open a `Drift Hooks` phase around a tick that constructs no
/// `ApplyRun` at all, and naming a type to reach a function that takes no
/// `self` is exactly the kind of false coupling that gets copied.
#[must_use = "the pseudo-phase closes when the PseudoPhase is dropped; bind it"]
pub fn pseudo_phase<'p>(printer: &'p Printer, label: &str) -> PseudoPhase<'p> {
    PseudoPhase {
        section: printer.section(label),
        _inherit: printer.depth_inheritance(),
    }
}

/// Alignment column for a set of subjects: the max rendered width, unfiltered.
///
/// Unfiltered is the one semantic difference from the buffered column a section
/// close computes: whether an action will carry a duration, a detail or a
/// target is not knowable from the plan, so a phase whose widest action ends up
/// carrying nothing still pads the others against it.
pub fn align_width_of<'s>(labels: impl Iterator<Item = &'s str>) -> usize {
    labels.map(measure_width).max().unwrap_or(0)
}

/// The plan-phase view over [`align_width_of`]: the max over the DISPLAY
/// SUBJECT of **every** action in the phase, so every owner group in it opens
/// with the same column.
///
/// The subject, not the raw plan string: a condensed script body or a marker
/// the execution renders shorter than the payload would pad every trailing
/// field in the phase against a column nothing reaches.
pub fn align_width(phase: &Phase) -> usize {
    let items: Vec<String> = phase
        .groups()
        .iter()
        .flat_map(|group| group.actions.iter())
        .map(|action| action_display_subject(action).to_string())
        .collect();
    align_width_of(items.iter().map(String::as_str))
}

/// Every arm returns; `Partial` returns three lines. No path panics, so the
/// function is safe in core and testable without a `Printer` — and it reads a
/// [`RunTally`], so a backup run reaches it without an [`ApplyResult`].
fn rollup_lines(tally: &RunTally, title: RunTitle) -> Vec<(Role, String)> {
    match tally.status {
        // Partial leads with a Warn title line naming the outcome, because the
        // block below it opens on a ✓ and a reader who takes the first line as
        // the verdict reads a run that failed actions as a run that succeeded.
        //
        // The two counts stay split below it so the success count and the
        // failure count read as distinct outcomes — fusing them into one Warn
        // line makes a "9 succeeded, 1 failed" run look the same colour as a
        // "1 succeeded, 9 failed" run.
        ApplyStatus::Partial => vec![
            (
                Role::Warn,
                format!(
                    "{} partial — {} of {} applied",
                    title.as_str(),
                    tally.succeeded,
                    tally.planned_total
                ),
            ),
            (Role::Ok, format!("{} action(s) succeeded", tally.succeeded)),
            (Role::Accent, format!("{} action(s) failed", tally.failed)),
        ],
        // A run that reached none of what it planned did not complete, whatever
        // its status says. One line, not two: the shortfall it would otherwise
        // carry below is exactly the count named here.
        ApplyStatus::Success if tally.nothing_attempted() => vec![(
            Role::Skipped,
            format!(
                "{} did not run — {} action(s) not attempted",
                title.as_str(),
                tally.planned_total
            ),
        )],
        ApplyStatus::Success => vec![(
            Role::Ok,
            format!(
                "{} complete — {} action(s) succeeded",
                title.as_str(),
                tally.succeeded
            ),
        )],
        ApplyStatus::Failed => vec![(
            Role::Fail,
            format!(
                "{} failed — {} action(s) failed",
                title.as_str(),
                tally.failed
            ),
        )],
        ApplyStatus::InProgress => vec![(
            Role::Warn,
            format!("{} still in progress (unexpected state)", title.as_str()),
        )],
        // The one line that folds the title's case: it is pinned as a lowercase
        // sentence by `tests/apply_signal_abort.rs` and by the sample in
        // `docs/safety.md`, and it reads correctly for every other title.
        //
        // A signal reaches the child process too, so an abort can carry a
        // failure: the install that was in flight dies with it. Naming only
        // what was applied and what was never attempted leaves that action in
        // neither count, and the closing line — the one a reader keeps —
        // accounts for every planned action or it accounts for none.
        ApplyStatus::Aborted => vec![(
            Role::Warn,
            format!(
                "{} aborted by signal — {} of {} action(s) applied{}; no partial writes, rerun to converge",
                title.as_str().to_ascii_lowercase(),
                tally.succeeded,
                tally.planned_total,
                if tally.failed > 0 {
                    format!(", {} failed", tally.failed)
                } else {
                    String::new()
                }
            ),
        )],
    }
}

/// The run's closing rollup: one or two status lines naming what happened, plus
/// the shortfall line when fewer actions ran than the run set out to do.
///
/// `elapsed` lands on the LAST line emitted, so a `Partial` run wears it on its
/// failure line and a short run wears it on the shortfall.
pub fn render_run_rollup(
    tally: &RunTally,
    title: RunTitle,
    printer: &Printer,
    elapsed: Option<Duration>,
) -> ApplyStatus {
    let mut lines = rollup_lines(tally, title);
    let shortfall = tally.shortfall();
    // The `did not run` arm already names the whole shortfall in its own line.
    if shortfall > 0 && !(tally.status == ApplyStatus::Success && tally.nothing_attempted()) {
        // `Role::Info` and not `Role::Pending`: this is a final count, and
        // nothing it names is still going to happen.
        lines.push((Role::Info, format!("{shortfall} action(s) not attempted")));
    }
    let last = lines.len().saturating_sub(1);
    for (index, (role, subject)) in lines.into_iter().enumerate() {
        match elapsed {
            Some(d) if index == last => {
                printer.status(role, subject).duration(d);
            }
            _ => printer.status_simple(role, subject),
        }
    }
    tally.status.clone()
}

/// The [`ApplyResult`]-shaped view over [`render_run_rollup`], and the name
/// every plan-running call site keeps.
pub fn render_apply_result(
    result: &ApplyResult,
    title: RunTitle,
    printer: &Printer,
    elapsed: Option<Duration>,
) -> ApplyStatus {
    render_run_rollup(&result.tally(), title, printer, elapsed)
}

#[cfg(test)]
mod tests;

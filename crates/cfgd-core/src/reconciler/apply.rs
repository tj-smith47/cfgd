use crate::AbortFlag;
use crate::PathDisplayExt;
use crate::config::{LOCAL_LAYER, ResolvedProfile, ScriptShell};
use crate::errors::{ConfigError, Result};
use crate::modules::ResolvedModule;
use crate::output::{OwnerLabel, Printer, Role, SectionGuard, collapse_to_subject_line};
use crate::state::ApplyStatus;

use super::format::{
    action_display_subject, condense_action_desc_for_display, format_action_description,
    parse_package_description, parse_resource_from_description,
};
use super::restore::action_target_path;
use super::run::align_width;
use super::scripts::{
    MODULE_SCRIPT_TIMEOUT, ScriptEnvContext, ScriptReport, ScriptSubject, build_module_script_env,
    build_script_env, effective_continue_on_error, execute_script, script_default_workdir,
};
use super::types::{
    Action, ActionResult, ApplyResult, MANAGER_RESOURCE_TYPE, ManagerAction, ModuleAction,
    ModuleActionKind, Owner, OwnerKind, PhaseFilter, PhaseName, Plan, ReconcileContext,
    ScriptAction, ScriptPhase, SystemAction,
};
use crate::providers::{ActionNote, FileAction, NoteSink, PackageAction, SecretAction};

/// One action's line in the execution tree, resolved where the outcome is known
/// and written either immediately (a streaming phase) or at phase close
/// (`Packages`, whose dispatch order is not its reading order).
struct ActionOutcome {
    /// The action's display subject, resolved once at record time so the
    /// streaming writer and the deferred one cannot disagree about it.
    subject: String,
    role: Role,
    detail: Option<String>,
    /// The detail was derived from the plan or from the action's own no-op
    /// status rather than from what happened, so it renders muted. A collapsed
    /// error never does — it is the thing the reader has to act on.
    detail_muted: bool,
    duration: Option<std::time::Duration>,
    notes: Vec<ActionNote>,
    /// The child output a concurrent lane captured instead of streaming, laid
    /// out beneath this line when the phase's tree is written. Always empty in
    /// a sequential phase, where the output window already showed it live.
    body: Vec<String>,
}

/// A planned action that is a no-op by construction. Its subject already states
/// why nothing happened, so the tree renders it at the role that text implies
/// and attaches no `unchanged` detail.
///
/// An unknown system key keeps `Role::Warn` — it is almost always a typo, and
/// `format_plan_items` branches on the same flag, so the warning-versus-neutral
/// distinction the deleted bespoke lines carried survives as the action's role.
fn declared_noop_role(action: &Action) -> Option<Role> {
    match action {
        Action::System(SystemAction::Skip { unknown: true, .. }) => Some(Role::Warn),
        Action::System(SystemAction::Skip { .. })
        | Action::File(FileAction::Skip { .. })
        | Action::Package(PackageAction::Skip { .. })
        | Action::Secret(SecretAction::Skip { .. })
        | Action::Module(ModuleAction {
            kind: ModuleActionKind::Skip { .. },
            ..
        }) => Some(Role::Skipped),
        _ => None,
    }
}

/// `execute_script` emits a script action's one status line itself, so the tree
/// must not add a second for the two action shapes that reach it.
fn action_reports_its_own_status(action: &Action) -> bool {
    matches!(action, Action::Script(_))
        || matches!(
            action,
            Action::Module(ModuleAction {
                kind: ModuleActionKind::RunScript { .. },
                ..
            })
        )
}

/// Elapsed times below this read as noise on a line whose subject is the point.
const MIN_REPORTED_DURATION: std::time::Duration = std::time::Duration::from_secs(1);

/// The per-phase display inputs every settled action line is built from.
struct PhaseLedger<'p> {
    phase_name: PhaseName,
    /// The subject every action renders under, in both trees: the same string
    /// the preview bullet printed and `align_width` measured.
    subjects: &'p std::collections::HashMap<usize, String>,
    bootstrap_owners: &'p std::collections::HashMap<&'p str, String>,
}

/// One finished action, as its collection point hands it over.
struct SettleInput<'p, 'r> {
    action: &'p Action,
    journal_id: Option<i64>,
    result: Result<(String, bool, Option<String>)>,
    elapsed: std::time::Duration,
    notes: Vec<ActionNote>,
    body: Vec<String>,
    finished: usize,
    ledger: &'p PhaseLedger<'p>,
    results: &'r mut Vec<ActionResult>,
}

/// What settling produced: the description the journal and the result row
/// carry, whether the run must stop here, and the line the tree will render.
struct Settled {
    desc: String,
    should_abort: bool,
    /// `None` for the two script shapes, whose one line `execute_script`
    /// already emitted; their notes are in `notes` instead.
    outcome: Option<ActionOutcome>,
    notes: Vec<ActionNote>,
}

/// The run's monotonic count of finishes, ticked wherever an action is
/// collected — which is always the coordinator thread, sequential phase and
/// concurrent one alike.
///
/// Separate from the plan-position counter because lanes make the two orders
/// differ, and the wall clock cannot substitute: `utc_now_iso8601` truncates to
/// whole seconds, so two lanes finishing inside one second are unordered by
/// `completed_at`.
#[derive(Default)]
struct Completions(usize);

impl Completions {
    fn next(&mut self) -> usize {
        let index = self.0;
        self.0 += 1;
        index
    }
}

/// Who declared the manager a `PackageAction::Bootstrap` installs, as the
/// `for <owner token>, …` detail its line carries. `None` when nothing declared
/// it, which renders the line bare.
///
/// A union of DECLARATIONS, deliberately not `effective_desired_packages`: that
/// view resolves package CLAIMS, so a profile whose packages under this manager
/// are all also declared by a module would contribute no row and the line would
/// omit the very owner whose declaration planned the bootstrap.
fn bootstrap_attribution(
    manager: &str,
    profile_name: &str,
    resolved: &ResolvedProfile,
    module_actions: &[ResolvedModule],
) -> Option<String> {
    let mut owners: Vec<Owner> = Vec::new();
    // The profile arm is `plan_packages`' own `desired_for(m)` with `modules:
    // &[]`, so the profile appears exactly when its declaration is what planned
    // this bootstrap. It is total over `spec.packages.custom` too, which is why
    // a custom manager needs no second lookup.
    if !crate::config::desired_packages_for_spec(manager, &resolved.merged.packages).is_empty() {
        owners.push(Owner::profile(profile_name));
    }
    for module in module_actions {
        if module.packages.iter().any(|p| p.manager == manager) {
            owners.push(Owner::module(&module.name));
        }
    }
    Owner::order(&mut owners);
    let tokens: Vec<String> = owners.iter().map(Owner::token).collect();
    (!tokens.is_empty()).then(|| format!("for {}", tokens.join(", ")))
}

/// The notes a provider produced during one action, under the status that
/// action just emitted — whoever emitted it. The ONE render path for a note,
/// package-manager caveat and system-configurator narration alike.
///
/// Public so a per-configurator snapshot bridge renders its capture through the
/// same call the reconciler makes: a golden assembled from a test's own
/// `attached_status` loop would keep passing after this derivation moved, and
/// would then be pinning a shape cfgd no longer emits.
pub fn emit_action_notes(section: &SectionGuard<'_>, notes: &[ActionNote]) {
    for note in notes {
        section.attached_status(note.role, note.body());
    }
}

fn emit_action_line(printer: &Printer, section: &SectionGuard<'_>, outcome: &ActionOutcome) {
    {
        let mut builder = section.action_status(outcome.role, &outcome.subject);
        if outcome.detail_muted {
            builder = builder.detail_muted_opt(outcome.detail.as_deref());
        } else {
            builder = builder.detail_opt(outcome.detail.as_deref());
        }
        if let Some(d) = outcome.duration {
            builder = builder.duration(d);
        }
        drop(builder);
    }
    emit_action_notes(section, &outcome.notes);
    // Held back rather than streamed: two lanes streaming into one log
    // interleave line by line, so a concurrent phase captures its children's
    // output and lays each action's out under the line it belongs to. Empty
    // whenever a live window already showed it, and under `Verbosity::Quiet`.
    if !outcome.body.is_empty() {
        crate::output::OutputWindow::dump_below(printer, section.depth, &outcome.body);
    }
}

/// Identity of one planned action, for correlating the plan-order walk with the
/// dispatch-order walk over the same `OwnerGroup::actions` storage.
///
/// Value equality is wrong here: two groups can plan byte-identical actions
/// (the same package declared by two modules), and those are distinct rows in
/// the journal.
fn action_key(action: &Action) -> usize {
    action as *const Action as usize
}

fn hash_sorted_parts(mut parts: Vec<String>) -> String {
    parts.sort();
    crate::sha256_hex(parts.join("|").as_bytes())
}

/// Whether `action` (owned by `owner`, residing in `phase_name`) should execute
/// under `filter`.
///
/// `--phase modules` is an OWNER filter: module work applies in the phase whose
/// kind it is, so every module-owned action matches it wherever it landed.
///
/// `--phase post-scripts` / `--phase pre-scripts` are intentionally inclusive
/// across plan phases: a module lifecycle script is
/// `Action::Module(RunScript { phase: PostApply | ... })`, which a naive
/// `phase.name == filter` test would drop, making
/// `cfgd apply --module nvim --phase post-scripts` a no-op even when failed
/// module scripts need re-attempting. Other filters keep strict
/// phase-equality semantics.
pub fn action_matches_phase_filter(
    phase_name: &PhaseName,
    owner: &Owner,
    action: &Action,
    filter: &PhaseFilter,
) -> bool {
    let filter_phase = match filter {
        PhaseFilter::ModuleOwners => return owner.kind == OwnerKind::Module,
        PhaseFilter::Phase(name) => name,
    };
    if phase_name == filter_phase {
        return true;
    }
    match filter_phase {
        PhaseName::PostScripts => is_post_apply_script(action),
        PhaseName::PreScripts => is_pre_apply_script(action),
        _ => false,
    }
}

/// Suffix `apply_env_action` appends to a description when the surface was
/// already correct and nothing was written.
///
/// Reading it back out of a description is not a general description sniff: the
/// suffix is confined to env-action descriptions, and every reader below gets
/// its string straight from `apply_env_action`, so the shape is the producer's
/// own data. A converged surface also has to be stripped of it before the
/// description becomes a `managed_resources` id, or one resource owns two rows
/// that alternate by run and neither matches the id the planner derives.
pub(super) const ENV_SKIPPED_SUFFIX: &str = ":skipped";

fn env_result_key(description: &str) -> &str {
    description
        .strip_suffix(ENV_SKIPPED_SUFFIX)
        .unwrap_or(description)
}

/// Fold a late env regeneration into the result the Env phase already recorded
/// for the same surface.
///
/// The regeneration rewrites files the Env phase may have written earlier in the
/// same apply. Appending a second result would report one `~/.cfgd.env` twice and
/// push `results.len()` past the planned-action count every caller compares it
/// against. A prior *failure* is left standing as its own row: a failed attempt
/// and a later successful one are distinct events and collapsing them would hide
/// the error.
fn merge_env_result(results: &mut Vec<ActionResult>, description: String, changed: bool) {
    let key = env_result_key(&description);
    if let Some(prev) = results
        .iter_mut()
        .find(|r| r.success && env_result_key(&r.description) == key)
    {
        prev.changed = prev.changed || changed;
        prev.description = if prev.changed {
            key.to_string()
        } else {
            description
        };
        return;
    }
    results.push(ActionResult {
        // These are env actions no matter which late input triggered them, and a
        // caller filtering results by phase must find them where every other
        // `env:write:`/`env:inject:` result sits.
        phase: PhaseName::Prerequisites.as_str().to_string(),
        description,
        success: true,
        error: None,
        changed,
    });
}

fn is_post_apply_script(action: &Action) -> bool {
    matches!(
        action,
        Action::Script(ScriptAction::Run {
            phase: ScriptPhase::PostApply | ScriptPhase::PostReconcile,
            ..
        }) | Action::Module(ModuleAction {
            kind: ModuleActionKind::RunScript {
                phase: ScriptPhase::PostApply | ScriptPhase::PostReconcile,
                ..
            },
            ..
        })
    )
}

fn is_pre_apply_script(action: &Action) -> bool {
    matches!(
        action,
        Action::Script(ScriptAction::Run {
            phase: ScriptPhase::PreApply | ScriptPhase::PreReconcile,
            ..
        }) | Action::Module(ModuleAction {
            kind: ModuleActionKind::RunScript {
                phase: ScriptPhase::PreApply | ScriptPhase::PreReconcile,
                ..
            },
            ..
        })
    )
}

impl<'a> super::Reconciler<'a> {
    /// Update module state in state.db after a successful apply.
    fn update_module_state(
        &self,
        modules: &[ResolvedModule],
        apply_id: i64,
        results: &[ActionResult],
    ) -> Result<()> {
        for module in modules {
            // Check if any module action for this module failed
            let module_prefix = format!("module:{}:", module.name);
            let any_failed = results
                .iter()
                .any(|r| r.description.starts_with(&module_prefix) && !r.success);
            let status = if any_failed { "error" } else { "installed" };

            let packages_hash = hash_sorted_parts(
                module
                    .packages
                    .iter()
                    .map(|p| {
                        format!(
                            "{}:{}:{}",
                            p.manager,
                            p.resolved_name,
                            p.version.as_deref().unwrap_or("")
                        )
                    })
                    .collect(),
            );

            let files_hash = hash_sorted_parts(
                module
                    .files
                    .iter()
                    .map(|f| format!("{}:{}", f.source.display(), f.target.display()))
                    .collect(),
            );

            // Collect git source info
            let git_sources: Vec<serde_json::Value> = module
                .files
                .iter()
                .filter(|f| f.is_git_source)
                .map(|f| {
                    serde_json::json!({
                        "source": f.source.display().to_string(),
                        "target": f.target.display().to_string(),
                    })
                })
                .collect();
            let git_sources_json = if git_sources.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&git_sources).unwrap_or_default())
            };

            self.state.upsert_module_state(
                &module.name,
                Some(apply_id),
                &packages_hash,
                &files_hash,
                git_sources_json.as_deref(),
                status,
            )?;
        }
        Ok(())
    }

    /// Apply a plan, executing each phase in order.
    /// Failed actions are logged and skipped — they don't abort the entire apply.
    ///
    /// `shell_override` forces every inline lifecycle script to run under the
    /// supplied interpreter, ignoring entries' `shell:` field. Set by
    /// `cfgd apply --shell <shell>` for debugging. File/shebang scripts are
    /// unaffected.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        plan: &Plan,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        phase_filter: Option<&PhaseFilter>,
        module_actions: &[ResolvedModule],
        context: ReconcileContext,
        skip_scripts: bool,
        shell_override: Option<ScriptShell>,
        abort: &AbortFlag,
    ) -> Result<ApplyResult> {
        // Record apply up front as "in-progress" so the journal can reference it
        let plan_hash = crate::state::plan_hash(&plan.to_hash_string());
        let profile_name = resolved
            .layers
            .last()
            .map(|l| l.profile_name.as_str())
            .unwrap_or("unknown");
        let apply_id =
            self.state
                .record_apply(profile_name, &plan_hash, ApplyStatus::InProgress, None)?;

        // Filter-aware count of the actions this run intends to execute, using
        // the SAME predicate as the loop below — so an aborted run reports
        // "{applied} of {planned_total}" against only the in-scope actions, not
        // the whole plan.
        let planned_total: usize = plan
            .phases
            .iter()
            .map(|phase| match phase_filter {
                Some(filter) => phase
                    .owned_actions()
                    .filter(|(owner, a)| action_matches_phase_filter(&phase.name, owner, a, filter))
                    .count(),
                None => phase.action_count(),
            })
            .sum();

        let mut results = Vec::new();
        // Running base of the plan-position counter: `action_index` is dense
        // from 0 across the whole run, over the actions that survive
        // `phase_filter`.
        let mut plan_index_base: usize = 0;
        let mut completions = Completions::default();
        let mut secret_env_collector: Vec<(String, String)> = Vec::new();
        // The PATH directories the Env phase's planned content was built from.
        // Compared against the post-run set below to detect a manager this run
        // bootstrapped, whose directories could not have been known that early.
        let path_dirs_at_plan =
            super::env::recorded_manager_path_dirs(self.state, &resolved.merged, module_actions);
        // Set when a signal requested cooperative cancellation. Stopping happens BEFORE
        // the next action — the previous one already completed atomically, so no
        // file is left torn.
        let mut aborted_code: Option<u8> = None;
        // Who declared each manager a planned bootstrap installs. Derived here
        // rather than in `plan_packages`, which every executing call site
        // passes `modules: &[]` and whose view is therefore profile-only.
        let bootstrap_owners: std::collections::HashMap<&str, String> = plan
            .phases
            .iter()
            .flat_map(|phase| phase.owned_actions())
            .filter_map(|(_, action)| match action {
                Action::Package(PackageAction::Bootstrap { manager, .. }) => {
                    let detail =
                        bootstrap_attribution(manager, profile_name, resolved, module_actions)?;
                    Some((manager.as_str(), detail))
                }
                _ => None,
            })
            .collect();
        // Post-install notes a manager produced during ONE action, drained
        // after that action's status line so they render attached to it.
        let notes = NoteSink::default();
        // Library code reached from inside a phase or owner section renders at
        // that section's depth for the whole run: package-manager output
        // windows, script windows and every status they collapse into.
        let _inherit = printer.depth_inheritance();

        'phases: for phase in &plan.phases {
            // Plan positions of the actions in this phase that survive
            // `phase_filter`, in `Phase::actions()` order. `action_index` is
            // documented as "where this action sits in the plan", and Rule P
            // dispatches `Packages` out of plan order, so the value is read from
            // this map rather than counted at dispatch. Address identity is the
            // key because both walks borrow the same `OwnerGroup::actions`
            // storage, so a reference from either one names the same slot.
            let mut plan_positions: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::new();
            for (owner, action) in phase.owned_actions() {
                if let Some(filter) = phase_filter
                    && !action_matches_phase_filter(&phase.name, owner, action, filter)
                {
                    continue;
                }
                let next = plan_positions.len();
                plan_positions.insert(action_key(action), next);
            }

            if plan_positions.is_empty() {
                continue;
            }

            let total = plan_positions.len();
            // Rule P's three-tier partition (`0 → B → 1`), which is plain plan
            // order outside `Packages`. Membership in `plan_positions` is the
            // `phase_filter` test: the map was built from the same predicate
            // over the same actions, so an item missing from it is exactly one
            // the filter excluded.
            let dispatch: Vec<(&Owner, &Action, usize)> = phase
                .dispatch_order()
                .filter_map(|(owner, action)| {
                    plan_positions
                        .get(&action_key(action))
                        .map(|pos| (owner, action, *pos))
                })
                .collect();

            // The subject every action renders under, in both trees: the same
            // string the preview bullet printed and `align_width` measured.
            let subjects: std::collections::HashMap<usize, String> = phase
                .groups()
                .iter()
                .flat_map(|group| group.actions.iter())
                .map(|action| {
                    (
                        action_key(action),
                        action_display_subject(action).to_string(),
                    )
                })
                .collect();
            let width = align_width(phase);
            let ledger = PhaseLedger {
                phase_name: phase.name.clone(),
                subjects: &subjects,
                bootstrap_owners: &bootstrap_owners,
            };
            // `Packages` writes its tree at phase close so the groups read in
            // `Owner::sort_key` order while Rule P dispatches `0 -> B -> 1`;
            // every other phase streams, because there its dispatch order and
            // its reading order are the same walk.
            let deferred = phase.name == PhaseName::Packages;
            let mut recorded: std::collections::HashMap<usize, ActionOutcome> =
                std::collections::HashMap::new();
            // Platform-gated skips are the header's `Modules`-row annotation,
            // so the phase holding them opens no block at all.
            let phase_section = (phase.name != PhaseName::Modules)
                .then(|| printer.section(phase.name.section_title()));
            // The flat dispatch stream converted back to the nested render
            // shape: a new group guard opens whenever the owner changes, and
            // the previous one closes first. Outside `Packages` an owner's
            // actions are contiguous in the stream by construction, so this is
            // one guard per group, in group order.
            let mut owner_section: Option<SectionGuard<'_>> = None;
            let mut owner_open: Option<&Owner> = None;
            // Why the action loop stopped, resolved after the phase's tree is
            // written — a deferred phase must still render what it completed.
            let mut abort_stop: Option<u8> = None;
            let mut pre_script_stop: Option<String> = None;

            if deferred {
                // The concurrent dispatcher owns this phase: it opens each
                // action's journal row at its dispatch point, runs the work in
                // a per-manager lane, and hands every finish back HERE, on this
                // thread, in completion order. No status line streams — the
                // tree below is written once the phase closes.
                let run = super::lanes::PackageRun {
                    printer,
                    apply_id,
                    config_dir,
                    resolved,
                    module_actions,
                    context,
                    shell_override,
                    abort,
                    plan_index_base,
                    action_depth: phase_section.as_ref().map_or(0, |s| s.depth + 1),
                };
                let mut collect = |action: &Action, collected: super::lanes::LaneCollected| {
                    let finished = completions.next();
                    let settled = self.settle_action(SettleInput {
                        action,
                        journal_id: collected.journal_id,
                        result: collected
                            .result
                            .map(|(desc, changed)| (desc, changed, None)),
                        elapsed: collected.elapsed,
                        notes: collected.notes,
                        body: collected.body,
                        finished,
                        ledger: &ledger,
                        results: &mut results,
                    });
                    match settled.outcome {
                        Some(outcome) => {
                            recorded.insert(action_key(action), outcome);
                        }
                        // An action that reported its own status carries its
                        // notes beside the outcome rather than inside it, and a
                        // deferred phase has no line open to attach them under.
                        // Unreachable: only the two script shapes self-report,
                        // and neither is ever planned into `Packages`.
                        None => debug_assert!(
                            settled.notes.is_empty(),
                            "a self-reporting action reached a lane carrying notes"
                        ),
                    }
                };
                abort_stop = self.dispatch_package_lanes(&dispatch, &run, &mut collect);
            } else {
                for (owner, action, plan_index) in dispatch.into_iter() {
                    let action_index = plan_index_base + plan_index;
                    // Cooperative cancellation: a signal flips the abort flag, and
                    // the loop stops before beginning the next atomic action.
                    if let Some(code) = abort.aborted() {
                        abort_stop = Some(code);
                        break;
                    }
                    if !deferred
                        && let Some(section) = phase_section.as_ref()
                        && owner_open != Some(owner)
                    {
                        // Explicit close before the next open: assigning over the
                        // binding would build the new guard first and unwind the
                        // renderer's section stack out of order.
                        drop(owner_section.take());
                        let group = section.section_owner(&OwnerLabel::new(
                            owner.kind.as_str(),
                            owner.name.as_str(),
                        ));
                        group.live_column(width);
                        owner_section = Some(group);
                        owner_open = Some(owner);
                    }

                    let desc_for_journal = format_action_description(action);
                    let (action_type, resource_id) =
                        parse_resource_from_description(&desc_for_journal);

                    // Capture file state before overwrite (for backup). A target
                    // that does not yet exist (a CREATE) gets an absent marker so
                    // rollback removes it rather than restoring a later apply's
                    // post-apply snapshot.
                    if let Some(backup) = action_target_path(action) {
                        let path = &backup.path;
                        // Backup key, not display: every writer of
                        // `file_backups.file_path` folds with `to_posix_fs_key` so a
                        // rollback lookup finds the row a Windows apply wrote — and
                        // so the row a rollback reopens still names the file that
                        // was backed up.
                        let path_str = crate::to_posix_fs_key(path);
                        let captured = if backup.follow_symlink {
                            crate::capture_file_resolved_state(path)
                        } else {
                            crate::capture_file_state(path)
                        };
                        match captured {
                            Ok(Some(file_state)) => {
                                if let Err(e) =
                                    self.state
                                        .store_file_backup(apply_id, &path_str, &file_state)
                                {
                                    tracing::warn!(
                                        "failed to store file backup for {}: {}",
                                        path.posix(),
                                        e
                                    );
                                }
                            }
                            Ok(None) => {
                                if let Err(e) = self.state.store_absent_backup(apply_id, &path_str)
                                {
                                    tracing::warn!(
                                        "failed to store absent marker for {}: {}",
                                        path.posix(),
                                        e
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "failed to capture file state for backup of {}: {}",
                                    path.posix(),
                                    e
                                );
                            }
                        }
                    }

                    // Journal: record action start
                    let journal_id = self
                        .state
                        .journal_begin(
                            apply_id,
                            action_index,
                            phase.name.as_str(),
                            &action_type,
                            &resource_id,
                            None,
                        )
                        .ok();

                    let started = std::time::Instant::now();
                    let result = self.apply_action(
                        action,
                        resolved,
                        config_dir,
                        printer,
                        apply_id,
                        context,
                        module_actions,
                        &mut secret_env_collector,
                        shell_override,
                        abort,
                        &notes,
                    );
                    let elapsed = started.elapsed();
                    let finished = completions.next();
                    // Drained unconditionally: a note left in the sink would
                    // attach to whichever action drains next, which is not the one
                    // that produced it.
                    let drained = notes.take();
                    let settled = self.settle_action(SettleInput {
                        action,
                        journal_id,
                        result,
                        elapsed,
                        notes: drained,
                        body: Vec::new(),
                        finished,
                        ledger: &ledger,
                        results: &mut results,
                    });
                    let should_abort = settled.should_abort;
                    let desc = settled.desc;
                    match settled.outcome {
                        // One status line per plan action, always — except the two
                        // script shapes, whose line `execute_script` already
                        // emitted. Their notes still belong under it.
                        None => {
                            if let Some(section) = owner_section.as_ref() {
                                emit_action_notes(section, &settled.notes);
                            }
                        }
                        Some(outcome) => match (deferred, owner_section.as_ref()) {
                            (true, _) => {
                                recorded.insert(action_key(action), outcome);
                            }
                            (false, Some(section)) => emit_action_line(printer, section, &outcome),
                            // `PhaseName::Modules` opens no block: its only actions
                            // are platform-gated skips, which the header's
                            // `Modules` row already annotates.
                            (false, None) => {}
                        },
                    }

                    // If a signal arrived while the action was running, the execute_script
                    // poll loop already killed the child and returned an error. Treat this
                    // as a cooperative abort (not a script failure) so the correct exit
                    // code (130 for SIGINT) and "aborted" DB row are recorded.
                    if let Some(code) = abort.aborted() {
                        abort_stop = Some(code);
                        break;
                    }

                    // If a pre-script failed without continueOnError, abort
                    let is_pre_script = matches!(
                        action,
                        Action::Script(ScriptAction::Run { phase: sp, .. })
                            if matches!(sp, ScriptPhase::PreApply | ScriptPhase::PreReconcile)
                    ) || matches!(
                        action,
                        Action::Module(ModuleAction {
                            kind: ModuleActionKind::RunScript { phase: sp, .. },
                            ..
                        }) if matches!(sp, ScriptPhase::PreApply | ScriptPhase::PreReconcile)
                    );
                    if should_abort && is_pre_script {
                        pre_script_stop = Some(condense_action_desc_for_display(action, &desc));
                        break;
                    }
                }
            }

            // The deferred tree, written against recorded outcomes: the same
            // group/`live_column`/status walk a streaming phase runs live, in
            // `Owner::sort_key` order. A group an abort reached before any of
            // its actions were ever considered for dispatch produces nothing
            // — the shortfall is the rollup's to name, not an empty heading's.
            // A STALLED action (`dispatch_package_lanes` ran out of runnable
            // work with this one still `Waiting`) is different: it was
            // handed to `collect` as a synthetic failure before dispatch
            // returned, so it IS in `recorded` and renders here like any
            // other failed action — never dispatching is itself the failure
            // being reported, not a reason to say nothing.
            if deferred && let Some(section) = phase_section.as_ref() {
                for group in phase.groups() {
                    let mut group_section: Option<SectionGuard<'_>> = None;
                    for action in &group.actions {
                        let Some(outcome) = recorded.remove(&action_key(action)) else {
                            continue;
                        };
                        let group_section = group_section.get_or_insert_with(|| {
                            let opened = section.section_owner(&OwnerLabel::new(
                                group.owner.kind.as_str(),
                                group.owner.name.as_str(),
                            ));
                            opened.live_column(width);
                            opened
                        });
                        emit_action_line(printer, group_section, &outcome);
                    }
                }
            }

            // Guards close in declaration order's reverse, so the phase's tree
            // is complete before the run unwinds past it.
            drop(owner_section);
            drop(phase_section);

            plan_index_base += total;
            if let Some(display_desc) = pre_script_stop {
                return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                    message: format!("pre-script failed, aborting apply: {}", display_desc),
                }));
            }
            if let Some(code) = abort_stop {
                aborted_code = Some(code);
                break 'phases;
            }
        }

        // Cooperative abort: a signal stopped us between actions. Skip the
        // follow-up hooks (secret-env regen, onChange) — those represent a
        // completed apply — but still persist the managed-resource bookkeeping
        // for the actions that did run, then record an `Aborted` marker and
        // return the signal exit code. The lock releases via the caller's Drop.
        if let Some(code) = aborted_code {
            self.record_managed_resources(apply_id, &results)?;
            self.update_module_state(module_actions, apply_id, &results)?;
            let succeeded = results.iter().filter(|r| r.success).count();
            let summary = serde_json::json!({
                "total": results.len(),
                "succeeded": succeeded,
                "failed": results.len() - succeeded,
                "aborted": true,
            })
            .to_string();
            self.state
                .update_apply_status(apply_id, ApplyStatus::Aborted, Some(&summary))?;
            return Ok(ApplyResult {
                action_results: results,
                status: ApplyStatus::Aborted,
                apply_id,
                aborted: Some(code),
                planned_total,
            });
        }

        // --- Env regeneration: fold in inputs that only exist once the phases ran ---
        // Two inputs land too late for the Env phase, which by `PhaseName` order
        // runs before both Modules and Packages: a secret's resolved value, and
        // the PATH directories of a manager bootstrapped during this very run.
        // Regenerating once here, with both present, converges the file inside
        // the same apply instead of leaving it right only from the next one on.
        let path_dirs_now =
            super::env::recorded_manager_path_dirs(self.state, &resolved.merged, module_actions);
        // A phase-scoped run must stay inside the phase the caller asked for.
        // `--phase modules` bootstrapping a manager would otherwise reach out
        // and rewrite `~/.cfgd.env` plus the source lines in `~/.bashrc` —
        // surfaces the Env phase owns. The bootstrap record is durable either
        // way, so the next unfiltered apply still converges the file.
        let path_dirs_changed = phase_filter.is_none() && path_dirs_now != path_dirs_at_plan;
        // The regeneration reads the DECLARED env, not the plan, so a caller
        // that pruned the env actions out of its plan would still see the
        // surface written here. `withhold_env_surface` is that caller saying
        // the surface is not this run's to touch; the inputs that would have
        // triggered the regeneration are durable, so the run that stops
        // withholding still converges it.
        if self.withhold_env_surface {
            tracing::info!("env surface withheld: skipping post-phase regeneration");
        } else if !secret_env_collector.is_empty() || path_dirs_changed {
            let (env_actions, _) = self.plan_env(
                &resolved.merged.env,
                &resolved.merged.aliases,
                resolved.merged.env_scope,
                module_actions,
                &secret_env_collector,
                &path_dirs_now,
                &super::env::recorded_managed_env_files(self.state),
            );
            for env_action in &env_actions {
                if let Action::Env(ea) = env_action {
                    // No phase section is open here and nothing will drain a
                    // sink, so a session-refresh warning settles on its own line
                    // — beside the failure line below it, which does the same.
                    match Self::apply_env_action(ea, printer, NoteSink::discarded()) {
                        Ok(desc) => {
                            let changed = !desc.contains(ENV_SKIPPED_SUFFIX);
                            merge_env_result(&mut results, desc, changed);
                        }
                        Err(e) => {
                            printer.status_simple(
                                Role::Fail,
                                format!("Failed to regenerate shell env files: {}", e),
                            );
                            results.push(ActionResult {
                                phase: PhaseName::Prerequisites.as_str().to_string(),
                                description: "env:write:regenerate".to_string(),
                                success: false,
                                error: Some(e.to_string()),
                                changed: false,
                            });
                        }
                    }
                }
            }
        }

        // --- onChange detection: run profile onChange scripts if anything changed ---
        let any_changed = results.iter().any(|r| r.changed);
        if any_changed && !skip_scripts && !resolved.merged.scripts.on_change.is_empty() {
            let profile_name = resolved
                .layers
                .last()
                .map(|l| l.profile_name.as_str())
                .unwrap_or("unknown");
            let env_vars = build_script_env(&ScriptEnvContext {
                config_dir,
                profile_name,
                context,
                phase: &ScriptPhase::OnChange,
                module_name: None,
                module_dir: None,
                path_dirs: &super::all_recorded_path_dirs(self.state),
            });
            let working = script_default_workdir(config_dir);
            for entry in &resolved.merged.scripts.on_change {
                match execute_script(
                    entry,
                    config_dir,
                    &working,
                    &env_vars,
                    crate::PROFILE_SCRIPT_TIMEOUT,
                    printer,
                    shell_override,
                    Some(abort),
                    ScriptReport {
                        subject: ScriptSubject::Hook(ScriptPhase::OnChange.display_name()),
                        non_fatal: effective_continue_on_error(entry, &ScriptPhase::OnChange),
                        ..ScriptReport::default()
                    },
                ) {
                    Ok((desc, changed, _)) => {
                        results.push(ActionResult {
                            phase: "post-scripts".to_string(),
                            description: desc,
                            success: true,
                            error: None,
                            changed,
                        });
                    }
                    Err(e) => {
                        let continue_on_err =
                            effective_continue_on_error(entry, &ScriptPhase::OnChange);
                        results.push(ActionResult {
                            phase: "post-scripts".to_string(),
                            description: format!("onChange: {}", entry.run_str()),
                            success: false,
                            error: Some(format!("{}", e)),
                            changed: false,
                        });
                        if !continue_on_err {
                            return Err(e);
                        }
                    }
                }
            }
        }

        // --- Module-level onChange: run per-module onChange scripts if that module had changes ---
        if any_changed && !skip_scripts {
            let profile_name = resolved
                .layers
                .last()
                .map(|l| l.profile_name.as_str())
                .unwrap_or("unknown");
            let path_dirs = super::all_recorded_path_dirs(self.state);
            for module in module_actions {
                if module.on_change_scripts.is_empty() {
                    continue;
                }
                let prefix = format!("module:{}:", module.name);
                let module_changed = results
                    .iter()
                    .any(|r| r.changed && r.description.starts_with(&prefix));
                if !module_changed {
                    continue;
                }
                let env_vars = build_module_script_env(
                    &ScriptEnvContext {
                        config_dir,
                        profile_name,
                        context,
                        phase: &ScriptPhase::OnChange,
                        module_name: Some(&module.name),
                        module_dir: Some(&module.dir),
                        path_dirs: &path_dirs,
                    },
                    &module.env,
                );
                let working = script_default_workdir(config_dir);
                for entry in &module.on_change_scripts {
                    match execute_script(
                        entry,
                        &module.dir,
                        &working,
                        &env_vars,
                        MODULE_SCRIPT_TIMEOUT,
                        printer,
                        shell_override,
                        Some(abort),
                        ScriptReport {
                            subject: ScriptSubject::Hook(ScriptPhase::OnChange.display_name()),
                            non_fatal: effective_continue_on_error(entry, &ScriptPhase::OnChange),
                            ..ScriptReport::default()
                        },
                    ) {
                        Ok((desc, changed, _)) => {
                            results.push(ActionResult {
                                phase: "modules".to_string(),
                                description: desc,
                                success: true,
                                error: None,
                                changed,
                            });
                        }
                        Err(e) => {
                            let continue_on_err =
                                effective_continue_on_error(entry, &ScriptPhase::OnChange);
                            results.push(ActionResult {
                                phase: "modules".to_string(),
                                description: format!(
                                    "module:{}:onChange: {}",
                                    module.name,
                                    entry.run_str()
                                ),
                                success: false,
                                error: Some(format!("{}", e)),
                                changed: false,
                            });
                            if !continue_on_err {
                                return Err(e);
                            }
                        }
                    }
                }
            }
        }

        let total = results.len();
        let failed = results.iter().filter(|r| !r.success).count();
        let status = if failed == 0 {
            ApplyStatus::Success
        } else if failed == total {
            ApplyStatus::Failed
        } else {
            ApplyStatus::Partial
        };

        // Update apply status from "in-progress" placeholder to final
        let summary = serde_json::json!({
            "total": total,
            "succeeded": total - failed,
            "failed": failed,
        })
        .to_string();
        self.state
            .update_apply_status(apply_id, status.clone(), Some(&summary))?;

        self.record_managed_resources(apply_id, &results)?;

        // Update module state and file manifests for successfully applied modules
        self.update_module_state(module_actions, apply_id, &results)?;

        // Post-apply snapshot: capture the resolved content of all managed file
        // targets (following symlinks). This ensures rollback can restore the
        // exact content visible at this point, even for symlink-deployed files
        // where the source may be modified in-place between applies.
        let mut snapshot_paths = std::collections::HashSet::new();
        for managed in &resolved.merged.files.managed {
            let target = crate::expand_tilde(&managed.target);
            let key = crate::to_posix_fs_key(&target);
            if snapshot_paths.contains(&key) {
                continue;
            }
            snapshot_paths.insert(key.clone());
            if let Ok(Some(state)) = crate::capture_file_resolved_state(&target)
                && let Err(e) = self.state.store_file_backup(apply_id, &key, &state)
            {
                tracing::debug!("post-apply snapshot for {}: {}", key, e);
            }
        }
        for module in module_actions {
            for file in &module.files {
                let target = crate::expand_tilde(&file.target);
                let key = crate::to_posix_fs_key(&target);
                if snapshot_paths.contains(&key) {
                    continue;
                }
                snapshot_paths.insert(key.clone());
                if let Ok(Some(state)) = crate::capture_file_resolved_state(&target)
                    && let Err(e) = self.state.store_file_backup(apply_id, &key, &state)
                {
                    tracing::debug!("post-apply snapshot for {}: {}", key, e);
                }
            }
        }

        Ok(ApplyResult {
            action_results: results,
            status,
            apply_id,
            aborted: None,
            planned_total,
        })
    }

    /// Persist managed-resource tracking rows for the successfully-applied
    /// actions in `results`. Shared by the normal completion path and the
    /// cooperative-abort path, which both need state to reflect exactly the
    /// resources that actually changed.
    fn record_managed_resources(&self, apply_id: i64, results: &[ActionResult]) -> Result<()> {
        for result in results {
            if !result.success {
                continue;
            }

            // Packages track per-resolved-name under "package"/"<mgr>/<pkg>" so the
            // set is usable for declarative prune. The generic parser is lossy for
            // multi-package installs and embeds the verb, so handle them explicitly:
            // install adds a tracking row per package, uninstall deletes it.
            if let Some((manager, verb, packages)) = parse_package_description(&result.description)
            {
                for pkg in &packages {
                    let rid = format!("{manager}/{pkg}");
                    match verb.as_str() {
                        "install" => {
                            // Persist the scripted uninstall command (Some only for
                            // custom managers) so the package can still be pruned
                            // after its manager block leaves the config.
                            let uninstall_cmd = self
                                .registry
                                .package_managers
                                .iter()
                                .find(|m| m.name() == manager)
                                .and_then(|m| m.persisted_uninstall());
                            self.state.upsert_package_resource(
                                &rid,
                                LOCAL_LAYER,
                                Some(apply_id),
                                uninstall_cmd.as_deref(),
                            )?;
                            self.state.resolve_drift(apply_id, "package", &rid)?;
                        }
                        "uninstall" => {
                            self.state.remove_managed_resource("package", &rid)?;
                            self.state.resolve_drift(apply_id, "package", &rid)?;
                        }
                        _ => {}
                    }
                }
                continue;
            }

            let description = result
                .description
                .strip_suffix(ENV_SKIPPED_SUFFIX)
                .unwrap_or(&result.description);
            let (rtype, rid) = parse_resource_from_description(description);
            // A manager node is cfgd's own scaffolding, never a resource the
            // user declared: a refreshed index, a provisioned manager and a
            // tool a cascade shelled out to are none of them things cfgd
            // prunes, restores or reports under `cfgd status`. The journal
            // still records the work; `managed_resources` does not.
            if rtype == MANAGER_RESOURCE_TYPE {
                continue;
            }
            self.state
                .upsert_managed_resource(&rtype, &rid, LOCAL_LAYER, None, Some(apply_id))?;
            self.state.resolve_drift(apply_id, &rtype, &rid)?;
        }
        Ok(())
    }

    /// Turn one finished action into its journal completion, its result row and
    /// the line the phase's tree will render.
    ///
    /// The ONE settle, shared by the sequential walk and the concurrent
    /// dispatcher, because the two orders differ and the LINE must not: a
    /// deferred tree written from a second derivation would disagree with a
    /// streaming one about role, detail or duration on the same action.
    fn settle_action(&self, input: SettleInput<'_, '_>) -> Settled {
        let SettleInput {
            action,
            journal_id,
            result,
            elapsed,
            notes,
            body,
            finished,
            ledger,
            results,
        } = input;
        // Set by the `Err` arm below so the tree renders the failure on
        // the action's own line instead of a bespoke one above it.
        let mut failure_detail: Option<(String, bool)> = None;

        let (desc, success, action_changed, error, should_abort) = match result {
            Ok((desc, action_changed, script_output)) => {
                if let Some(jid) = journal_id
                    && let Err(e) =
                        self.state
                            .journal_complete(jid, finished, None, script_output.as_deref())
                {
                    tracing::warn!("failed to record journal completion: {e}");
                }
                (desc, true, action_changed, None, false)
            }
            Err(e) => {
                let desc = format_action_description(action);

                // Check if this is a script action with continueOnError
                let continue_on_err = if let Action::Script(ScriptAction::Run {
                    entry,
                    phase: script_phase,
                    ..
                }) = action
                {
                    effective_continue_on_error(entry, script_phase)
                } else {
                    false
                };

                failure_detail = Some((collapse_to_subject_line(&e), continue_on_err));
                if let Some(jid) = journal_id
                    && let Err(je) = self.state.journal_fail(jid, finished, &e.to_string())
                {
                    tracing::warn!("failed to record journal failure: {je}");
                }
                (desc, false, false, Some(e.to_string()), !continue_on_err)
            }
        };

        let changed = success && action_changed;
        results.push(ActionResult {
            phase: ledger.phase_name.as_str().to_string(),
            description: desc.clone(),
            success,
            error,
            changed,
        });

        if action_reports_its_own_status(action) {
            return Settled {
                desc,
                should_abort,
                outcome: None,
                notes,
            };
        }

        let subject = ledger
            .subjects
            .get(&action_key(action))
            .cloned()
            .unwrap_or_else(|| condense_action_desc_for_display(action, &desc));
        let outcome = match &failure_detail {
            Some((message, continue_on_err)) => ActionOutcome {
                subject,
                role: if *continue_on_err {
                    Role::Warn
                } else {
                    Role::Fail
                },
                // A refusal's subject IS its reason by construction
                // (`cannot provision <m> — <reason>`), and the error it
                // settles through restates that reason for the journal. On
                // the line the two are one sentence printed twice.
                detail: (!matches!(action, Action::Manager(ManagerAction::Refuse { .. })))
                    .then(|| message.clone()),
                detail_muted: false,
                duration: None,
                notes,
                body,
            },
            None => {
                let bootstrap = match action {
                    Action::Package(PackageAction::Bootstrap { manager, .. }) => {
                        Some(manager.as_str())
                    }
                    _ => None,
                };
                let noop = declared_noop_role(action);
                let role = match (noop, action_changed) {
                    (Some(role), _) => role,
                    (None, true) => Role::Ok,
                    (None, false) => Role::Skipped,
                };
                // A bootstrap's detail is the only plan-derived one in the
                // tree, and it always carries its duration: a `(0.0s)` is the
                // reader's evidence that the manager was already installed by
                // an earlier tier.
                let (detail, duration) = match bootstrap {
                    Some(manager) => (ledger.bootstrap_owners.get(manager).cloned(), Some(elapsed)),
                    None => (
                        (noop.is_none() && !action_changed).then(|| "unchanged".to_string()),
                        (elapsed >= MIN_REPORTED_DURATION).then_some(elapsed),
                    ),
                };
                ActionOutcome {
                    subject,
                    role,
                    detail,
                    detail_muted: true,
                    duration,
                    notes,
                    body,
                }
            }
        };
        Settled {
            desc,
            should_abort,
            outcome: Some(outcome),
            notes: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_action(
        &self,
        action: &Action,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        apply_id: i64,
        context: ReconcileContext,
        module_actions: &[ResolvedModule],
        secret_env_collector: &mut Vec<(String, String)>,
        shell_override: Option<ScriptShell>,
        abort: &AbortFlag,
        notes: &NoteSink,
    ) -> Result<(String, bool, Option<String>)> {
        match action {
            Action::System(sys) => self
                .apply_system_action(sys, &resolved.merged, module_actions, printer, notes)
                .map(|d| (d, true, None)),
            Action::Package(pkg) => self
                .apply_package_action(pkg, printer, notes)
                .map(|d| (d, true, None)),
            Action::File(file) => self
                .apply_file_action(file, resolved.profile_name(), config_dir, printer)
                .map(|d| (d, true, None)),
            Action::Secret(secret) => self
                .apply_secret_action(secret, config_dir, secret_env_collector)
                .map(|d| (d, true, None)),
            Action::Script(script) => self.apply_script_action(
                script,
                resolved,
                config_dir,
                printer,
                context,
                shell_override,
                abort,
            ),
            Action::Module(module) => self
                .apply_module_action(
                    module,
                    config_dir,
                    printer,
                    apply_id,
                    context,
                    resolved,
                    module_actions,
                    shell_override,
                    abort,
                    notes,
                )
                .map(|(d, c)| (d, c, None)),
            Action::Manager(manager) => self
                .apply_manager_action(manager, printer, notes)
                .map(|(desc, changed)| (desc, changed, None)),
            Action::Env(env) => Self::apply_env_action(env, printer, notes).map(|d| {
                let changed = !d.contains(ENV_SKIPPED_SUFFIX);
                (d, changed, None)
            }),
        }
    }
}

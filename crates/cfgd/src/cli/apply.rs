use super::*;

use cfgd_core::output::{Doc, Role};

/// Terminal outcome of an apply run: the final status plus, when a signal
/// cooperatively aborted the apply, the conventional exit code (`130` SIGINT /
/// `143` SIGTERM) so `cmd_apply` can exit with it.
#[derive(Debug)]
pub struct ApplyOutcome {
    pub status: cfgd_core::state::ApplyStatus,
    pub aborted_code: Option<u8>,
}

impl ApplyOutcome {
    /// A non-action terminal path (dry-run, nothing-to-do, declined-confirm):
    /// success, no abort.
    fn success() -> Self {
        Self {
            status: cfgd_core::state::ApplyStatus::Success,
            aborted_code: None,
        }
    }
}

/// Downgrade a running apply status to `Partial` for an unclean/failed
/// backup unit — but only from `Success`. A prior file/package/module phase
/// may have already set `status` to `Failed`; a backup unit's own trouble
/// must never silently mask that higher severity by overwriting it with the
/// lesser `Partial`.
fn downgrade_to_partial(status: &mut cfgd_core::state::ApplyStatus) {
    if matches!(status, cfgd_core::state::ApplyStatus::Success) {
        *status = cfgd_core::state::ApplyStatus::Partial;
    }
}

/// The CLI's binding of the run skeleton to `Reconciler::apply`.
///
/// The apply lock is taken here rather than around the run because the run
/// prompts for confirmation before it calls this: holding an exclusive lock
/// across an interactive prompt would block a concurrent apply for as long as
/// the user takes to answer. The guard is stored on the executor so it lives as
/// long as the borrow does — through the `spec.backups[]` units that run after
/// the reconciler returns.
pub(in crate::cli) struct ReconcilerExecutor<'a> {
    reconciler: &'a Reconciler<'a>,
    resolved: &'a ResolvedProfile,
    config_dir: &'a std::path::Path,
    phase_filter: Option<&'a PhaseFilter>,
    modules: &'a [modules::ResolvedModule],
    context: ReconcileContext,
    skip_scripts: bool,
    shell_override: Option<cfgd_core::config::ScriptShell>,
    abort: &'a cfgd_core::AbortFlag,
    lock_dir: PathBuf,
    lock: Option<cfgd_core::FileLockGuard>,
}

impl<'a> ReconcilerExecutor<'a> {
    /// The executor a caller with no scoping flags builds — `cfgd init --apply`
    /// and `cfgd module create --apply`, which apply exactly what they just
    /// scaffolded: no phase filter, no `--skip-scripts`, no shell override.
    pub(in crate::cli) fn unscoped(
        reconciler: &'a Reconciler<'a>,
        resolved: &'a ResolvedProfile,
        config_dir: &'a std::path::Path,
        modules: &'a [modules::ResolvedModule],
        abort: &'a cfgd_core::AbortFlag,
        lock_dir: PathBuf,
    ) -> Self {
        Self {
            reconciler,
            resolved,
            config_dir,
            phase_filter: None,
            modules,
            context: ReconcileContext::Apply,
            skip_scripts: false,
            shell_override: None,
            abort,
            lock_dir,
            lock: None,
        }
    }
}

impl reconciler::RunExecutor for ReconcilerExecutor<'_> {
    fn apply(
        &mut self,
        plan: &reconciler::Plan,
        printer: &cfgd_core::output::Printer,
    ) -> cfgd_core::errors::Result<cfgd_core::reconciler::ApplyResult> {
        // Prevent concurrent applies (see helpers::apply_lock_dir).
        self.lock = Some(cfgd_core::acquire_apply_lock(&self.lock_dir)?);
        self.reconciler.apply(
            plan,
            self.resolved,
            self.config_dir,
            printer,
            self.phase_filter,
            self.modules,
            self.context,
            self.skip_scripts,
            self.shell_override,
            self.abort,
        )
    }
}

pub fn cmd_apply(
    cli: &Cli,
    printer: &cfgd_core::output::Printer,
    args: &ApplyArgs,
) -> anyhow::Result<()> {
    let outcome = run_apply(cli, printer, args)?;

    // A graceful signal abort is NOT an error, but it must NOT exit 0: exit with
    // the signal-conventional code so wrappers see the interruption. The abort
    // message + structured payload are already flushed by `run_apply`.
    if let Some(code) = outcome.aborted_code {
        std::process::exit(code as i32);
    }

    // A partial or total apply failure must surface as a nonzero exit so CI `&&`
    // chains and the daemon don't treat a broken apply as success. The structured
    // payload is already flushed by `run_apply`; exit directly (mirrors
    // status/diff/upgrade) so render_cli_error doesn't double-print a failure line.
    if matches!(
        outcome.status,
        cfgd_core::state::ApplyStatus::Partial | cfgd_core::state::ApplyStatus::Failed
    ) {
        cfgd_core::exit::ExitCode::ApplyFailed.exit();
    }

    Ok(())
}

/// Drive a full apply (or dry-run) and return the resulting [`ApplyOutcome`]
/// so the caller can map a partial/total failure to a nonzero process exit and
/// a signal abort to its conventional exit code.
///
/// Non-apply terminal paths (dry-run, aborted-confirmation, nothing-to-do)
/// report [`ApplyStatus::Success`] with no abort code — they did not run
/// actions, so they never warrant a failure exit. Keeping the exit decision in
/// `cmd_apply` lets in-process tests capture the rendered failure shape without
/// `process::exit` aborting the harness.
pub fn run_apply(
    cli: &Cli,
    printer: &cfgd_core::output::Printer,
    args: &ApplyArgs,
) -> anyhow::Result<ApplyOutcome> {
    // Parse --context (mirrors PlanArgs::context).
    let reconcile_context = match args.context.as_str() {
        "apply" => ReconcileContext::Apply,
        "reconcile" => ReconcileContext::Reconcile,
        other => {
            anyhow::bail!(
                "Unknown context '{}'. Valid values: apply, reconcile",
                other
            );
        }
    };

    // --from: clone from git source or use local path as config directory.
    // When --config points to a non-default path, use its parent as the clone target
    // so the cloned config ends up where the user expects.
    if let Some(from) = &args.from {
        let cli_config_dir = cli.config.parent().map(|p| p.to_path_buf());
        let default_dir = cfgd_core::default_config_dir();
        let target = if let Some(ref dir) = cli_config_dir {
            if *dir != default_dir && !cli.config.exists() {
                Some(dir.as_path())
            } else {
                None
            }
        } else {
            None
        };
        init::resolve_from(from, target, "master", printer)?;
    }

    let dry_run = args.dry_run;
    let yes = args.yes;
    let skip = &args.skip;
    let only = &args.only;
    let module_filter = args.module.as_deref();

    let config_dir = config_dir(cli);

    // When --module is set, try loading profile but fall back to empty if none
    // configured. The header these rows belong to is rendered once the plan is
    // final — it states the phase and action counts — so the profile label is
    // carried down rather than printed here. A module-only run resolved no
    // profile, so it carries none and the header omits the row.
    let (cfg, resolved, profile_label, config_parsed) = if let Some(mod_name) = module_filter {
        match load_config_and_profile(cli) {
            Ok((cfg, profile_name, resolved)) => (cfg, resolved, Some(profile_name), true),
            Err(e) => {
                tracing::debug!("profile load failed, using module-only mode: {}", e);
                // `minimal_config()` subscribes to nothing, and that fabricated
                // empty list must never reach the decision sweep below: it
                // would read as "no source is subscribed any more" and delete
                // every decision row on the machine, turning "awaiting your
                // answer" into "applies silently" with nothing to recover from.
                let (cfg, config_parsed) = match config::load_config(&cli.config) {
                    Ok(c) => (c, true),
                    Err(_) => (config::minimal_config(), false),
                };
                let resolved =
                    empty_resolved_profile(mod_name, &active_profile_name(cli, Some(&cfg)));
                (cfg, resolved, None, config_parsed)
            }
        }
    } else {
        let (cfg, profile_name, resolved) = load_config_and_profile(cli)?;
        (cfg, resolved, Some(profile_name), true)
    };

    // Open state only after config discovery so a missing config (or an
    // unresolvable home) surfaces before any state.db is created — otherwise a
    // NoConfig exit would leave an orphan state directory behind.
    let state = open_state_store(cli.state_dir.as_deref())?;

    let mut registry = build_registry_with_config(Some(&cfg));
    registry.set_system_config_dir(&config_dir);

    // `ApplyPhase` (clap ValueEnum) is already validated at parse time.
    let phase_filter: Option<PhaseFilter> = args.phase.map(apply_phase_to_filter);

    // Compose with sources (network refresh) and resolve modules through the one
    // desired-state resolver every command shares, so apply and the read paths
    // compute an identical effective module set for the same config.
    let desired = resolve_desired_state(
        cli,
        &cfg,
        &resolved,
        module_filter,
        printer,
        true,
        composition::ConstraintMode::Enforce,
    )?;
    let source_env = desired.source_env;
    let source_commits = desired.source_commits;
    let resolved_modules = desired.modules;
    let mut effective_resolved = desired.resolved;

    // Resolve manifest files (Brewfile, package.json, etc.) into package lists
    packages::resolve_manifest_packages(&mut effective_resolved.merged.packages, &config_dir)?;

    // Extend registry with custom package managers from config
    registry.package_managers.extend(packages::custom_managers(
        &effective_resolved.merged.packages.custom,
    ));

    // If --module is set, skip profile-level packages/files
    let module_only = module_filter.is_some();

    // Declarative prune (and the post-apply tracking-table GC below) reconcile
    // removals, which is only safe on a FULL, unscoped run: it needs the
    // complete desired set to know a package has truly left the config. A scoped
    // apply (--phase / --only / --skip / --skip-scripts) or a module-only run
    // sees a partial picture, so prune/GC are suppressed there — a
    // not-applied-this-run consumer might still need the package.
    let scope_restricted =
        phase_filter.is_some() || !skip.is_empty() || !only.is_empty() || args.skip_scripts;
    let prune_eligible = !module_only && !scope_restricted;

    // In dry-run mode we don't need secret providers wired up — just plan files for display.
    // In apply mode we wire up the full file manager with secret providers.
    let (pkg_actions, file_actions, dry_run_fm) = if module_only {
        (Vec::new(), Vec::new(), None)
    } else {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let cfgd_installed = if prune_eligible {
            cfgd_installed_packages(&state)?
        } else {
            std::collections::HashSet::new()
        };
        // Profile-scoped: module packages are added separately by
        // `reconciler.plan` as `Action::Module`, so this planner must stay
        // profile-only to avoid double-handling them.
        let pkg_cx = cfgd_core::providers::PackageContext::new(printer, &state);
        let pkg = packages::plan_packages(
            &effective_resolved.merged,
            &[],
            &all_managers,
            &cfgd_installed,
            &pkg_cx,
        )?;

        let mut fm = CfgdFileManager::new(&config_dir, &effective_resolved)?;
        fm.set_global_strategy(cfg.spec.file_strategy);
        if !source_env.is_empty() {
            fm.set_source_env(&source_env);
        }

        if !dry_run {
            let (backend_name, age_key_path) = secret_backend_from_config(Some(&cfg));
            fm.set_secret_providers(
                Some(secrets::build_secret_backend(
                    &backend_name,
                    age_key_path,
                    Some(&config_dir),
                )),
                secrets::build_secret_providers(),
            );
        }

        let fa = fm.plan(&effective_resolved.merged)?;

        if dry_run {
            // Keep fm around for diff display but don't register it
            (pkg, fa, Some(fm))
        } else {
            // Register the file manager so the reconciler delegates through the trait
            registry.file_manager = Some(Box::new(fm));
            (pkg, fa, None)
        }
    };

    let module_names: Vec<String> = resolved_modules.iter().map(|m| m.name.clone()).collect();

    // A resource awaiting (or declined by) a source decision is not this run's
    // to touch, in any mode: the confirm prompt, `--yes` and `--dry-run` all
    // act on the plan below. The env arm withholds its surface as a unit and
    // apply rebuilds that surface from the DECLARED set after the phases run,
    // so the reconciler carries the flag as well — pruning alone would leave
    // an undecided variable reaching the machine through the regeneration.
    // Source gone, items gone: a decision the operator can no longer answer is
    // dropped rather than left to sit in `cfgd status` forever. Only a real
    // apply whose config actually parsed sweeps — a dry run reports and writes
    // nothing, and a module-only fallback knows no subscription list to judge
    // the rows against. The rows are inert either way (the gate below admits
    // only subscribed sources), so this is cleanup, not enforcement.
    // A config named with `--config` while the state dir stays the default is
    // not authoritative over that store either: its subscription list belongs
    // to a different machine picture, and the rows it would delete are another
    // config's, unrecoverably. Withholding is unaffected — the gate below still
    // refuses rows this run has no source for.
    let owns_the_store =
        reconciler::owns_decision_store(cli.config_explicit, cli.state_dir.is_some());
    let store_writes = !dry_run && config_parsed && owns_the_store;
    if store_writes {
        let subscribed: Vec<String> = cfg.spec.sources.iter().map(|s| s.name.clone()).collect();
        if let Err(e) = state.discard_decisions_not_in(&subscribed) {
            tracing::warn!(error = %e, "failed to discard decisions of removed sources");
        }
    }

    // A run that owns the store also RECORDS what the policy classified, so an
    // item this apply refuses to install is one `cfgd decide` can answer now
    // rather than after the daemon's next tick. The same three conditions gate
    // it as gate the sweep: a dry run changes nothing, a module-only fallback
    // knows no subscription list, and a `--config` naming someone else's store
    // does not write rows into it.
    let writes = if store_writes {
        plan_ops::DecisionWrites::Mint
    } else {
        plan_ops::DecisionWrites::ReadOnly
    };
    let withheld = plan_ops::withheld_for_run(
        &state,
        &cfg,
        &effective_resolved,
        &config_dir,
        config_parsed,
        writes,
    )?;
    let exclusions = reconciler::DecisionExclusions::from_withheld(&withheld);
    let reconciler = Reconciler::new(&registry, &state)
        .withholding_env_surface(exclusions.withholds_env_surface());
    let mut plan = reconciler.plan(
        &effective_resolved,
        file_actions,
        pkg_actions,
        resolved_modules.clone(),
        reconcile_context,
    )?;
    reconciler::withhold_from_plan(&mut plan, &exclusions);

    // Snapshot scope before --skip/--only prune the plan, so a zero-action
    // outcome distinguishes "in sync" from "a filter excluded pending work".
    let filter_active =
        phase_filter.is_some() || !skip.is_empty() || !only.is_empty() || args.skip_scripts;
    let module_miss = module_filter
        .filter(|_| resolved_modules.is_empty())
        .map(str::to_string);
    let scope = ScopeReport::capture(&plan, filter_active, module_miss);

    // Apply --skip / --only filters
    filter_plan(&mut plan, skip, only, printer, &registry);

    // Strip script phases when --skip-scripts is set
    if args.skip_scripts {
        strip_scripts_from_plan(&mut plan);
    }

    // Computed once so the dry-run preview and the real run below use the exact
    // same list.
    let pending_backup_specs = pending_backups(&effective_resolved.merged);
    let pending_backups: Vec<String> = pending_backup_specs
        .iter()
        .map(|b| b.name.clone())
        .collect();

    // The rows every path below prints above its own body. Built once so a dry
    // run, an executing run and a no-work run cannot describe the same
    // invocation differently.
    let run_ctx = |title| reconciler::RunContext {
        title,
        config_path: Some(cli.config.as_path()),
        profile: profile_label.as_deref(),
        modules: &module_names,
        trigger: None,
    };

    if dry_run {
        let run = reconciler::ApplyRun::new(run_ctx(reconciler::RunTitle::Plan), &plan)
            .with_filter(phase_filter.as_ref())
            .with_withheld(&withheld)
            .preview_only();
        display_plan_preview(
            &run,
            &plan,
            printer,
            &PlanPreviewArgs {
                context: &args.context,
                phase_filter: phase_filter.as_ref(),
                dry_run_fm: dry_run_fm.as_ref(),
                scope: &scope,
                pending_backups: &pending_backups,
                withheld: &withheld,
            },
        );
        // Preview orphaned custom-manager packages a real apply would prune
        // (read-only — execute nothing here). Same gating + query as the apply
        // path so the preview matches the action.
        if prune_eligible {
            preview_orphaned_custom_packages(&state, &registry, printer);
        }
        return Ok(ApplyOutcome::success());
    }

    // --- Apply mode ---

    // Handle unmanaged file targets: if a target exists as a non-cfgd file, prompt to
    // adopt (proceed), backup (rename to .cfgd-backup), or skip.
    handle_unmanaged_file_targets(&mut plan, &config_dir, &state, printer, yes)?;

    // Self-heal the package-tracking table on a full unscoped apply, BEFORE the
    // no-op early-return: a row whose package vanished (partial-uninstall
    // failure or out-of-band removal) produces no plan action, so it would never
    // be reached after the `has_actions` gate. Best-effort.
    if prune_eligible {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        gc_stale_package_tracking(&state, &all_managers, printer);
        gc_orphaned_custom_packages(&state, &registry, printer);
    }

    // Check if filtered plan has actions
    let has_actions = if let Some(ref pf) = phase_filter {
        plan.phases.iter().any(|p| {
            p.owned_actions()
                .any(|(owner, a)| reconciler::action_matches_phase_filter(&p.name, owner, a, pf))
        })
    } else {
        !plan.is_empty()
    };

    // A schedule-less backup runs on every apply regardless of reconciler
    // diff, so a converged machine (the common case once a fleet is settled)
    // must not short-circuit here — that would silently starve backups of
    // the cadence "every apply" promises.
    let run = reconciler::ApplyRun::new(run_ctx(reconciler::RunTitle::Apply), &plan)
        .with_filter(phase_filter.as_ref())
        .with_withheld(&withheld);

    if !has_actions && pending_backups.is_empty() {
        run.header(printer);
        report_plan_verdict(printer, 0, Some(&scope));
        printer.emit(Doc::new().with_data(ApplyOutput::nothing_to_do()));
        return Ok(ApplyOutcome::success());
    }

    // Register cooperative-cancellation handlers for the duration of the apply.
    // SIGINT/SIGTERM flip the shared flag (the reconciler checks it between
    // atomic actions); the in-flight action still finishes, so no file is torn.
    let abort = cfgd_core::AbortFlag::new();
    register_abort_handlers(&abort);

    // The confirmation gate is about the reconciler's file/package/module diff.
    // A backup-only apply (has_actions == false, pending_backups non-empty) has
    // no diff to confirm, and `ApplyRun::execute` skips the prompt for exactly
    // that case — prompting "Apply these changes?" over nothing would confuse
    // the one case this exists to serve.
    let confirm = if yes {
        reconciler::Confirm::Skip
    } else {
        // Closed-TTY / non-interactive defaults to "no" — apply is destructive
        // and silence is treated as decline, not as approval.
        reconciler::Confirm::Ask("Apply these changes?")
    };
    let mut exec = ReconcilerExecutor {
        reconciler: &reconciler,
        resolved: &effective_resolved,
        config_dir: &config_dir,
        phase_filter: phase_filter.as_ref(),
        modules: &resolved_modules,
        context: reconcile_context,
        skip_scripts: args.skip_scripts,
        shell_override: args.shell.map(super::apply_shell_to_script_shell),
        abort: &abort,
        lock_dir: apply_lock_dir(cli.state_dir.as_deref(), cli.scope())?,
        lock: None,
    };
    let result = match run.execute(printer, confirm, &mut exec)? {
        reconciler::RunDisposition::Applied(result) => result,
        reconciler::RunDisposition::Declined => {
            printer.status_simple(Role::Info, "Aborted");
            printer.emit(Doc::new().with_data(ApplyOutput::aborted()));
            return Ok(ApplyOutcome::success());
        }
        // None of these are reachable for a run carrying a plan and no
        // `preview_only`, and none of them ran a plan action, which is what
        // the nothing-to-do exit already reports.
        reconciler::RunDisposition::NothingToDo
        | reconciler::RunDisposition::Previewed
        | reconciler::RunDisposition::BackupsApplied(_) => {
            return Ok(ApplyOutcome::success());
        }
    };

    if let Some(code) = result.aborted {
        let signal = if code == 143 { "SIGTERM" } else { "SIGINT" };
        // Filter-aware planned count (computed by the reconciler with the same
        // predicate the apply loop uses), so "{applied} of {total}" reflects
        // only the in-scope actions under --phase/--skip/--only, not the whole
        // plan. The sentence itself is the rollup's `Aborted` arm; this `Doc`
        // carries data only, so structured consumers see the payload and the
        // human surface sees exactly one abort line.
        printer.emit(Doc::new().with_data(AbortOutput {
            aborted: true,
            signal: signal.to_string(),
            applied: result.succeeded(),
            total: result.planned_total,
        }));
        // An aborted run can still have completed the Env phase, so the user's
        // shell is just as stale as after a full apply.
        print_shell_env_reminder(&result, printer);
        return Ok(ApplyOutcome {
            status: result.status,
            aborted_code: Some(code),
        });
    }

    let mut status = result.status.clone();
    print_shell_env_reminder(&result, printer);

    // Link source commits to this apply for provenance tracking
    if !source_commits.is_empty() {
        for (source_name, commit_hash) in &source_commits {
            if let Err(e) = state.record_source_apply(source_name, result.apply_id, commit_hash) {
                tracing::warn!(
                    source = %source_name,
                    commit = %commit_hash,
                    error = %e,
                    "failed to record source apply"
                );
            }
        }
    }

    // Prune old rollback backups (per-apply pre-image snapshots, distinct from
    // `spec.backups[]` below) — keep last 10 applies' worth. Best-effort:
    // failures here (SQLite locked, disk full, permission denied) are
    // surfaced as a warn-level log so unbounded growth on a stuck filesystem
    // is observable instead of silent.
    if let Err(e) = state.prune_old_backups(10) {
        tracing::warn!(error = %e, "failed to prune old backups");
    }

    // Run every schedule-less `spec.backups[]` entry. Not a reconciler action
    // (no diff against desired state — they always run), and independent of
    // the reconcile outcome above, so they run here unconditionally rather
    // than being folded into `plan`/`result`. A dirty run (good snapshot, but
    // a failed post-hook) is user-declared work, so — unlike the best-effort
    // pruning above — it downgrades a `Success` apply to `Partial` and drives
    // the process exit code the same way a failed reconciler action would.
    let backup_outputs = run_pending_backups(
        &pending_backup_specs,
        &PendingBackupCtx {
            cli,
            cfg: &cfg,
            config_dir: &config_dir,
            state: &state,
            printer,
            abort: &abort,
        },
        &mut status,
    )?;

    let output = ApplyOutput {
        status: status.display_str().to_string(),
        apply_id: Some(result.apply_id),
        succeeded: result.succeeded(),
        failed: result.failed(),
        source_commits,
        backups: backup_outputs,
    };
    printer.emit(Doc::new().with_data(&output));

    Ok(ApplyOutcome {
        status,
        aborted_code: None,
    })
}

/// What [`run_pending_backups`] needs from the apply it runs inside.
struct PendingBackupCtx<'a> {
    cli: &'a Cli,
    cfg: &'a CfgdConfig,
    config_dir: &'a std::path::Path,
    state: &'a cfgd_core::state::StateStore,
    printer: &'a cfgd_core::output::Printer,
    abort: &'a cfgd_core::AbortFlag,
}

/// Run the apply's schedule-less `spec.backups[]` units, downgrading `status`
/// for any unit whose work did not complete cleanly.
fn run_pending_backups(
    specs: &[&cfgd_core::config::BackupSpec],
    ctx: &PendingBackupCtx<'_>,
    status: &mut cfgd_core::state::ApplyStatus,
) -> anyhow::Result<Vec<BackupRunOutput>> {
    let mut outputs = Vec::with_capacity(specs.len());
    if specs.is_empty() {
        return Ok(outputs);
    }
    let printer = ctx.printer;
    let profile_name = active_profile_name(ctx.cli, Some(ctx.cfg));
    let state_dir = cfgd_core::resolve_state_dir(ctx.cli.state_dir.as_deref(), ctx.cli.scope())?;
    for spec in specs {
        let backup_name = &spec.name;
        let unit =
            cfgd_core::backup::BackupUnit::new(spec, ctx.config_dir, &profile_name, &state_dir)
                .with_abort(ctx.abort);
        // Best-effort, matching the neighboring `record_source_apply` call in
        // the caller: `run_backup` only returns `Err` on a state-store write
        // failure (ordinary snapshot/hook failures are captured into the
        // returned record, not propagated), so a `?` here would abort every
        // remaining backup AND the rest of apply over what's really a single
        // unit's storage problem. Warn, count the unit as failed for
        // exit-code purposes, and keep going.
        let record = match cfgd_core::backup::run_backup(&unit, ctx.state, printer) {
            Ok(record) => record,
            // Another surface (a daemon timer fire, a hand-run) holds this
            // unit's lock, so the unit IS being backed up right now — just
            // not by us. Skipping is the correct outcome of the engine's
            // one-writer-per-unit rule, not an apply failure, so it does
            // not move the exit code; the structured payload still carries
            // the skip so a script can see the run did not come from here.
            Err(cfgd_core::errors::CfgdError::Backup(cfgd_core::errors::BackupError::Busy {
                holder,
                ..
            })) => {
                printer
                    .status(Role::Skipped, format!("backup '{backup_name}'"))
                    .detail(format!("already running ({holder})"));
                tracing::info!(
                    backup = %backup_name,
                    holder = %holder,
                    "backup already running elsewhere; skipped"
                );
                outputs.push(BackupRunOutput {
                    name: backup_name.clone(),
                    status: "skipped".to_string(),
                    destination_path: None,
                    clean: false,
                    error: Some(format!("already running ({holder})")),
                });
                continue;
            }
            Err(e) => {
                printer
                    .status(Role::Fail, format!("backup '{backup_name}'"))
                    .detail(cfgd_core::output::collapse_to_subject_line(&e));
                tracing::warn!(
                    backup = %backup_name,
                    error = %e,
                    "backup run failed; continuing with remaining backups"
                );
                downgrade_to_partial(status);
                outputs.push(BackupRunOutput {
                    name: backup_name.clone(),
                    status: cfgd_core::state::BackupRunStatus::Failed
                        .as_str()
                        .to_string(),
                    destination_path: None,
                    clean: false,
                    error: Some(e.to_string()),
                });
                continue;
            }
        };
        cfgd_core::backup::report_backup_record(printer, &record);
        if !record.is_clean() {
            downgrade_to_partial(status);
        }
        outputs.push(BackupRunOutput::from(&record));
    }
    Ok(outputs)
}

/// Structured `-o json` payload emitted when an apply is cooperatively aborted
/// by a signal.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AbortOutput {
    aborted: bool,
    signal: String,
    applied: usize,
    total: usize,
}

/// Register SIGINT (→130) / SIGTERM (→143) handlers for the running apply.
///
/// The FIRST delivery of a signal flips `abort` (the reconciler stops
/// cooperatively between atomic actions). A SECOND delivery of the same signal
/// force-quits via the OS default disposition, so a user hammering Ctrl-C is
/// never stuck waiting on cleanup.
///
/// Both the flag store and the default-disposition emulation are async-signal-
/// safe (`AtomicUsize::store` / `signal_hook::low_level::emulate_default_handler`).
/// Best-effort — a registration failure is logged and the apply proceeds
/// without cooperative cancellation (the OS default still terminates on signal).
#[cfg(unix)]
fn register_abort_handlers(abort: &cfgd_core::AbortFlag) {
    use std::sync::atomic::Ordering;

    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::low_level;

    // 128 + signum, the POSIX shell convention for signal-terminated processes.
    for (sig, code) in [(SIGINT, 130usize), (SIGTERM, 143usize)] {
        let flag = abort.raw();
        // SAFETY: the action only performs async-signal-safe operations — an
        // atomic load/store and `emulate_default_handler` (documented
        // async-signal-safe). No allocation, locking, or reentrant I/O.
        let res = unsafe {
            low_level::register(sig, move || {
                if flag.load(Ordering::SeqCst) == 0 {
                    // First delivery: request cooperative cancellation.
                    flag.store(code, Ordering::SeqCst);
                } else {
                    // Second delivery: force-quit with the default disposition.
                    let _ = low_level::emulate_default_handler(sig);
                }
            })
        };
        if let Err(e) = res {
            tracing::warn!(signal = sig, error = %e, "failed to register apply abort handler");
        }
    }
}

/// Windows lacks `signal_hook`'s POSIX flag API; cooperative abort is a no-op
/// here and Ctrl-C falls back to the OS default disposition. Logged so the
/// degraded behavior is observable.
#[cfg(windows)]
fn register_abort_handlers(_abort: &cfgd_core::AbortFlag) {
    tracing::debug!("cooperative apply abort handler not available on this platform");
}

/// Remove package-tracking rows whose package is no longer installed (stale
/// after a partial-uninstall failure or an out-of-band removal). Best-effort:
/// any failure is logged, never propagated, so it can't fail an apply.
fn gc_stale_package_tracking(
    state: &cfgd_core::state::StateStore,
    managers: &[&dyn cfgd_core::providers::PackageManager],
    printer: &cfgd_core::output::Printer,
) {
    let tracked = match cfgd_installed_packages(state) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read tracked packages for GC");
            return;
        }
    };
    let cx = cfgd_core::providers::PackageContext::new(printer, state);
    match cfgd_core::reconciler::stale_tracked_packages(managers, &tracked, &cx) {
        Ok(stale) => {
            for (mgr, id) in stale {
                let rid = format!("{mgr}/{id}");
                if let Err(e) = state.remove_managed_resource("package", &rid) {
                    tracing::warn!(resource = %rid, error = %e, "failed to GC stale package tracking row");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to compute stale package tracking rows"),
    }
}

/// Run persisted uninstall scripts for orphaned custom-manager packages, then
/// drop the rows that were removed successfully. Best-effort: store errors are
/// logged, never propagated, so the GC can't fail an apply. Mirrors
/// [`gc_stale_package_tracking`].
fn gc_orphaned_custom_packages(
    state: &cfgd_core::state::StateStore,
    registry: &cfgd_core::providers::ProviderRegistry,
    printer: &cfgd_core::output::Printer,
) {
    let known = registry.manager_names();
    let orphans = match state.orphaned_package_resources(&known) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read orphaned package rows for GC");
            return;
        }
    };
    if orphans.is_empty() {
        return;
    }
    let cx = cfgd_core::providers::PackageContext::new(printer, state);
    for (mgr, pkg) in packages::prune_orphaned_packages(&orphans, &cx) {
        let rid = format!("{mgr}/{pkg}");
        if let Err(e) = state.remove_managed_resource("package", &rid) {
            tracing::warn!(resource = %rid, error = %e, "failed to GC orphaned package tracking row");
        }
    }
}

/// Preview, without executing, which orphaned custom-manager packages a real
/// apply would prune via their persisted uninstall script — and which lack one
/// and need manual removal. Read-only; runs only on the dry-run path.
pub(in crate::cli) fn preview_orphaned_custom_packages(
    state: &cfgd_core::state::StateStore,
    registry: &cfgd_core::providers::ProviderRegistry,
    printer: &cfgd_core::output::Printer,
) {
    let known = registry.manager_names();
    let orphans = match state.orphaned_package_resources(&known) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read orphaned package rows for preview");
            return;
        }
    };
    for orphan in orphans {
        match orphan.uninstall_cmd {
            Some(_) => printer.status_simple(
                Role::Accent,
                format!(
                    "would uninstall orphaned {}/{} via persisted script",
                    orphan.manager, orphan.package
                ),
            ),
            None => printer.status_simple(
                Role::Warn,
                format!(
                    "orphaned {}/{} — no persisted uninstall; manual removal needed",
                    orphan.manager, orphan.package
                ),
            ),
        }
    }
}
/// Build the buffered `Doc` that carries the final `ApplyOutput` payload.
/// Pure function so snapshot tests can drive the JSON path without standing
/// up a reconciler.
pub fn build_apply_doc(output: &ApplyOutput) -> Doc {
    Doc::new().with_data(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::{Printer, Verbosity, strip_ansi};

    #[test]
    fn preview_orphaned_custom_packages_pins_both_contract_strings_and_executes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("preview-should-not-run");

        let state = cfgd_core::state::StateStore::open_in_memory().unwrap();
        // Orphan WITH a persisted script: would create the marker file if the
        // preview ever executed it (it must not).
        state
            .upsert_package_resource(
                "widgetmgr/widget",
                "local",
                None,
                Some(&format!("touch {}", marker.display())),
            )
            .unwrap();
        // Orphan with NO persisted script (legacy row).
        state
            .upsert_package_resource("legacymgr/legacypkg", "local", None, None)
            .unwrap();
        // A package under a registered (built-in) manager — NOT orphaned.
        state
            .upsert_package_resource("cargo/bat", "local", None, None)
            .unwrap();

        // Registry contains only built-in managers, so cargo is "known" but
        // widgetmgr / legacymgr are not — exactly the orphan condition.
        let mut registry = cfgd_core::providers::ProviderRegistry::new();
        registry.package_managers = crate::packages::all_package_managers();

        // Normal verbosity: Accent/Warn status lines are suppressed under Quiet
        // (the default for `for_test`).
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        preview_orphaned_custom_packages(&state, &registry, &printer);
        drop(printer);
        let out = strip_ansi(&buf.lock().unwrap());

        assert!(
            out.contains("would uninstall orphaned widgetmgr/widget via persisted script"),
            "persisted-script preview line missing, got: {out}"
        );
        assert!(
            out.contains(
                "orphaned legacymgr/legacypkg — no persisted uninstall; manual removal needed"
            ),
            "no-persisted-script preview line missing, got: {out}"
        );
        assert!(
            !out.contains("cargo/bat"),
            "a package under a registered manager must not be previewed as orphaned, got: {out}"
        );

        // Preview is read-only: the script was never executed and no row was
        // removed.
        assert!(
            !marker.exists(),
            "preview must not execute any uninstall script"
        );
        let known = registry.manager_names();
        let still_orphaned = state.orphaned_package_resources(&known).unwrap();
        assert_eq!(
            still_orphaned.len(),
            2,
            "preview must not remove any tracking rows"
        );
    }
}

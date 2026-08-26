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
        // Prevent concurrent applies (see helpers::run_state_dir).
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
/// report [`cfgd_core::state::ApplyStatus::Success`] with no abort code — they did not run
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
    let module_filter: &[String] = &args.module;
    let with_profile = args.with_profile;

    // `--with-profile` opts a `--module` run INTO composing with the full
    // profile; with no module named, there is nothing for it to compose
    // with — reject rather than silently behaving like a plain `cfgd apply`.
    if with_profile && module_filter.is_empty() {
        anyhow::bail!(
            "--with-profile requires --module (it composes the named module(s) with the full profile; without --module there is nothing to add)"
        );
    }

    let config_dir = config_dir(cli);

    // `--module` without `--with-profile` isolates unconditionally — a
    // profile is never even resolved, so a profile that DOES resolve can no
    // longer leak into an isolated run. The header these rows belong to is
    // rendered once the plan is final — it states the phase and action
    // counts — so the profile label is carried down rather than printed
    // here. An isolated run resolved no profile, so it carries none and the
    // header omits the row.
    let (cfg, resolved, profile_label, config_parsed) =
        load_config_and_profile_module_scoped(cli, printer, module_filter, with_profile)?;

    let ctx = RunContext::new(cli, printer);

    // Open state only after config discovery so a missing config (or an
    // unresolvable home) surfaces before any state.db is created — otherwise a
    // NoConfig exit would leave an orphan state directory behind.
    let state = ctx.state()?;

    // Compose with sources (network refresh) and resolve modules through the one
    // desired-state resolver every command shares, so apply and the read paths
    // compute an identical effective module set for the same config.
    let mut desired = resolve_desired_state(
        &ctx,
        &cfg,
        &resolved,
        module_filter,
        with_profile,
        printer,
        true,
        composition::ConstraintMode::Enforce,
    )?;
    // Taken before the other fields, because a partial move out of `desired`
    // would block the `&mut self` this accessor needs.
    // Built from the same config and composed packages this path would have
    // used, custom managers included.
    let mut registry = desired.take_registry(&cfg);
    let source_env = desired.source_env;
    let composed_sources = desired.sources;
    let source_commits = desired.source_commits;
    let mut resolved_modules = desired.modules;
    let mut effective_resolved = desired.resolved;
    registry.set_system_config_dir(&config_dir);

    // Resolve manifest files (Brewfile, package.json, etc.) into package lists
    ctx.resolve_manifest_packages(&mut effective_resolved.merged.packages)?;

    // `PhaseArg`'s base phase is clap-validated; a selector combined with
    // `--phase modules` is the one combination `resolve_phase_filter` still
    // has to reject at runtime (see its doc comment). Resolved only now that
    // `registry` carries every custom manager too, so a selector naming one
    // validates against the same vocabulary the plan itself will match.
    let phase_filter: Option<PhaseFilter> =
        resolve_phase_filter(args.phase.clone(), &registry, printer)?;

    // Isolated (--module without --with-profile): skip profile-level
    // packages/files — everything else profile-owned is already zeroed by
    // `resolve_desired_state`'s isolation (`effective_resolved`).
    let module_only = !module_filter.is_empty() && !with_profile;

    // Declarative prune (and the post-apply tracking-table GC below) reconcile
    // removals, which is only safe on a FULL, unscoped run: it needs the
    // complete desired set to know a package has truly left the config. A scoped
    // apply (--phase / --only / --skip / --skip-scripts) or a module-only run
    // sees a partial picture, so prune/GC are suppressed there — a
    // not-applied-this-run consumer might still need the package.
    let scope_restricted =
        phase_filter.is_some() || !skip.is_empty() || !only.is_empty() || args.skip_scripts;
    let prune_eligible = !module_only && !scope_restricted;

    // Declared here rather than inside the planning block below because the
    // tracking-table GC further down diffs against the same installed state:
    // both run before a single action executes, so one enumeration per manager
    // answers both. Anything the apply itself installs or removes retires the
    // memo, so nothing downstream of an action can read a stale set.
    let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);

    // In dry-run mode we don't need secret providers wired up — just plan files for display.
    // In apply mode we wire up the full file manager with secret providers.
    let (pkg_actions, file_actions, dry_run_fm, actual_packages) = if module_only {
        (
            Vec::new(),
            Vec::new(),
            None,
            cfgd_core::reconciler::ActualPackages::default(),
        )
    } else {
        printer.narrate("Planning", |sp| -> anyhow::Result<_> {
            sp.set_message("Planning Packages");
            let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
                .package_managers()
                .iter()
                .map(|m| m.as_ref())
                .collect();
            let cfgd_installed = if prune_eligible {
                cfgd_installed_packages(state)?
            } else {
                std::collections::HashSet::new()
            };
            // Profile-scoped: module packages are added separately by
            // `reconciler.plan` as `Action::Module`, so this planner must stay
            // profile-only to avoid double-handling them.
            let (pkg, actual) = packages::plan_packages_observed(
                &effective_resolved.merged,
                &[],
                &all_managers,
                &cfgd_installed,
                &pkg_cx,
            )?;

            sp.set_message("Planning Files");
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

            Ok(if dry_run {
                // Keep fm around for diff display but don't register it
                (pkg, fa, Some(fm), actual)
            } else {
                // Register the file manager so the reconciler delegates through the trait
                registry.file_manager = Some(Box::new(fm));
                (pkg, fa, None, actual)
            })
        })?
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
    // A FOREIGN config while the state dir stays the default is not
    // authoritative over that store either: its subscription list belongs to a
    // different machine picture, and the rows it would delete are another
    // config's, unrecoverably. Ownership follows the resolved path, not the
    // spelling — `--config` naming the default config file is still the
    // machine's own config. Withholding is unaffected — the gate below still
    // refuses rows this run has no source for.
    let owns_the_store =
        reconciler::owns_decision_store(&cli.config, cli.state_dir.is_some(), cli.scope());
    let store_writes = !dry_run && config_parsed && owns_the_store;
    if store_writes {
        let subscribed: Vec<String> = cfg.spec.sources.iter().map(|s| s.name.clone()).collect();
        if let Err(e) = state.discard_decisions_not_in(&subscribed) {
            tracing::warn!(error = %e, "failed to discard decisions of removed sources");
        }
    }

    // A run that owns the store also RECORDS what the policy classified, so an
    // item this apply refuses to install is one `cfgd decide` can answer now
    // rather than after the daemon's next tick — but only AFTER the operator
    // lets the run proceed: the mint is a store write like the others, so it
    // waits behind the confirm gate below (the preview withholds and names the
    // item either way, through `with_unrecorded`). The same three conditions
    // gate it as gate the sweep: a dry run changes nothing, a module-only
    // fallback knows no subscription list, and a foreign config naming someone
    // else's store does not write rows into it.
    let (withheld, review) = plan_ops::withheld_for_run(
        &ctx,
        state,
        &cfg,
        plan_ops::DesiredOwnership {
            resolved: &effective_resolved,
            entry_owners: &reconciler::merged_entry_owners(&effective_resolved, &resolved_modules),
        },
        config_parsed,
        plan_ops::DecisionWrites::ReadOnly,
        &actual_packages,
    )?;
    let exclusions = reconciler::DecisionExclusions::from_withheld(&withheld);
    let reconciler = Reconciler::new(&registry, state)
        .with_config_dir(&config_dir)
        .withholding_env_surface(exclusions.withholds_env_surface())
        .diffing_installed(&pkg_cx)
        // What the recorded apply says this run was scoped to. An isolated
        // module run resolved no profile, so it names the modules instead of
        // inheriting the placeholder `active_profile_name` falls back to; a run
        // whose profile is genuinely underivable records nothing at all, and
        // every reader omits the row rather than printing a stand-in.
        .recording_scope(if module_only {
            module_filter
                .iter()
                .map(|m| reconciler::Owner::module(m).token())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            profile_label.clone().unwrap_or_default()
        });
    let mut plan = printer.narrate("Planning", |sp| {
        // Apply's plan preview reads `brew install neovim (0.10.2)`, and the
        // same string is the persisted action description and the module's
        // recorded packages hash — priced survivor-gated (a package the
        // machine already holds is elided and never queried), and under this
        // bar so the wait is narrated, not dead air.
        sp.set_message("Resolving package versions");
        reconciler.fill_planned_versions(&mut resolved_modules, &registry.manager_map());
        reconciler.plan_observed(
            &effective_resolved,
            file_actions,
            pkg_actions,
            resolved_modules.clone(),
            reconcile_context,
            &mut |phase| sp.set_message(format!("Planning {}", phase.display_name())),
        )
    })?;
    // Snapshot BEFORE `withhold_from_plan` and every filter below prunes the
    // plan: a module is converged only when the RECONCILER found nothing to
    // do, never when a filter emptied a plan that still held real work.
    let plan_was_converged = plan.is_empty();
    reconciler::withhold_from_plan(&mut plan, &exclusions);

    // Snapshot scope before --skip/--only prune the plan, so a zero-action
    // outcome distinguishes "in sync" from "a filter excluded pending work".
    let filter_active =
        phase_filter.is_some() || !skip.is_empty() || !only.is_empty() || args.skip_scripts;
    let mut scope = ScopeReport::capture(&plan, filter_active);

    // Apply --skip / --only filters. `known_module_names` reads the module
    // tree and lockfile ONCE, only when a filter is actually active — a
    // filter-less run (the common case) never pays that I/O.
    let known_modules = if skip.is_empty() && only.is_empty() {
        std::collections::HashSet::new()
    } else {
        known_module_names(&config_dir)
    };
    scope.filter_miss = filter_plan(
        &mut plan,
        skip,
        only,
        phase_filter.as_ref(),
        printer,
        &registry,
        &known_modules,
    );

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
        sources: &composed_sources,
        modules: &module_names,
        trigger: None,
        subject: None,
    };

    if dry_run {
        let run = reconciler::ApplyRun::new(run_ctx(reconciler::RunTitle::Plan), &plan)
            .with_filter(phase_filter.as_ref())
            .with_withheld(&withheld)
            .decisions_answerable(owns_the_store)
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
            preview_orphaned_custom_packages(state, &registry, printer);
        }
        return Ok(ApplyOutcome::success());
    }

    // --- Apply mode ---

    // Handle unmanaged file targets: a target that already holds a file cfgd
    // never wrote is settled by `--on-conflict` before anything is applied.
    // The copies themselves are deferred to the actions that displace their
    // targets, so `backed up to …` rides as a DETAIL on the row of the write it
    // protects rather than standing as a line of its own above the run's header.
    let reconciler = reconciler.backing_up(handle_unmanaged_file_targets(
        &mut plan,
        &config_dir,
        state,
        printer,
        yes,
        args.on_conflict,
        &cfgd_core::effective::effective_file_strategies(
            &effective_resolved.merged,
            &resolved_modules,
            &config_dir,
            registry.default_file_strategy,
        ),
    )?);

    // Self-heal the package-tracking table on a full unscoped apply, BEFORE the
    // no-op early-return: a row whose package vanished (partial-uninstall
    // failure or out-of-band removal) produces no plan action, so it would never
    // be reached after the `has_actions` gate. Best-effort.
    if prune_eligible {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers()
            .iter()
            .map(|m| m.as_ref())
            .collect();
        gc_stale_package_tracking(state, &all_managers, &pkg_cx);
        gc_orphaned_custom_packages(state, &registry, printer);
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
    // Register cooperative-cancellation handlers for the duration of the apply.
    // SIGINT/SIGTERM flip the shared flag (the reconciler checks it between
    // atomic actions); the in-flight action still finishes, so no file is torn.
    // The flag itself is built here rather than after the no-work gate because
    // the backup units below borrow it, and they are part of the run this
    // gate decides about.
    let abort = cfgd_core::AbortFlag::new();
    let state_dir = cfgd_core::resolve_state_dir(cli.state_dir.as_deref(), cli.scope())?;
    let backup_profile = active_profile_name(cli, Some(&cfg));
    // The units the run's `Backups` pseudo-phase will render. Built before the
    // run so the header's `Actions N planned` can count their hooks and
    // snapshots, which is the same enumeration the rollup reconciles against.
    let backup_units: Vec<cfgd_core::backup::BackupUnit<'_>> = pending_backup_specs
        .iter()
        .map(|spec| {
            cfgd_core::backup::BackupUnit::new(spec, &config_dir, &backup_profile, &state_dir)
                .with_abort(&abort)
        })
        .collect();

    let run = reconciler::ApplyRun::new(run_ctx(reconciler::RunTitle::Apply), &plan)
        .with_filter(phase_filter.as_ref())
        .with_withheld(&withheld)
        .decisions_answerable(owns_the_store)
        .with_pending_backups(&backup_units, state);

    if !has_actions && pending_backups.is_empty() {
        run.header(printer);
        // The header above NAMED any withheld items, and this run proceeded —
        // there was just nothing else for it to do. It still records what the
        // policy classified, or a converged machine (the common case once a
        // fleet settles) could never mint the rows its own plan keeps naming.
        // No confirm gate exists on this path: nothing destructive follows.
        if store_writes {
            reconciler::mint_decisions(state, &review);
            // A module whose packages the machine already holds contributes no
            // action, so `Reconciler::apply` — the only writer of
            // `module_state` — never runs for it. Recorded here or the module
            // reads "not applied" forever on a machine where it is converged,
            // and its `packages_hash` keeps describing a set that has moved.
            // Gated on `plan_was_converged`, the pre-filter snapshot, not on
            // `plan.is_empty()` here: by this point `--skip`/`--only`/
            // `--skip-scripts`/a withheld decision may have pruned a plan
            // that held real work, and "installed" is a claim about all of a
            // module's packages, not about what a filter happened to spare.
            if plan_was_converged
                && let Err(e) = reconciler.record_converged_modules(&resolved_modules)
            {
                tracing::warn!(error = %e, "failed to record converged module state");
            }
            refresh_link_deployed_hashes(
                &reconciler,
                &registry,
                &effective_resolved,
                &resolved_modules,
            );
        }
        report_plan_verdict(printer, 0, Some(&scope), withheld.pending.len());
        printer.emit(Doc::new().with_data(ApplyOutput::nothing_to_do()));
        return Ok(ApplyOutcome::success());
    }

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
        lock_dir: run_state_dir(cli.state_dir.as_deref(), cli.scope())?,
        lock: None,
    };
    let disposition = run.execute(printer, confirm, &mut exec)?;
    // The operator let the run proceed (or `--yes` did), so apply's deferred
    // store write lands now: the rows the policy classified are recorded and
    // `cfgd decide` can answer them without waiting for a daemon tick. A
    // declined run skips this — refusing the apply refuses its writes.
    if store_writes && !matches!(disposition, reconciler::RunDisposition::Declined) {
        reconciler::mint_decisions(state, &review);
        refresh_link_deployed_hashes(
            &reconciler,
            &registry,
            &effective_resolved,
            &resolved_modules,
        );
    }
    let (result, backup_reports) = match disposition {
        reconciler::RunDisposition::Applied { result, backups } => (result, backups),
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
        | reconciler::RunDisposition::BackupsApplied { .. } => {
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
            failed: result.failed(),
            total: result.planned_total,
        }));
        // An aborted run can still have completed the Env phase, so the user's
        // shell is just as stale as after a full apply.
        print_caveats(&result, printer);
        return Ok(ApplyOutcome {
            status: result.status,
            aborted_code: Some(code),
        });
    }

    let mut status = result.status.clone();
    print_caveats(&result, printer);

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

    // Every schedule-less `spec.backups[]` entry ran inside the run above, as
    // the `Backups` pseudo-phase before the rollup. What is left here is the
    // half the run cannot decide: user-declared work that did not complete
    // cleanly downgrades a `Success` apply to `Partial` and drives the process
    // exit code the same way a failed reconciler action would. A unit another
    // writer held is NOT that — it is the engine's one-writer rule working, so
    // it leaves the exit code alone.
    // One report per pending unit, in unit order — `render_backups` pushes them
    // as it walks the same slice. A silent `zip` truncation here would drop
    // both the payload entry AND the status downgrade for units that did run.
    debug_assert_eq!(
        pending_backup_specs.len(),
        backup_reports.len(),
        "one report per pending backup unit"
    );
    let backup_outputs: Vec<BackupRunOutput> = pending_backup_specs
        .iter()
        .zip(&backup_reports)
        .map(|(spec, report)| {
            if report.skipped.is_none() && !report.is_clean() {
                downgrade_to_partial(&mut status);
            }
            BackupRunOutput::from_report(&spec.name, report)
        })
        .collect();

    let output = ApplyOutput {
        status: status.display_str().to_string(),
        apply_id: Some(result.apply_id),
        succeeded: result.succeeded(),
        skipped: result.skipped(),
        failed: result.failed(),
        not_attempted: result.not_attempted().len(),
        // `ApplyOutput.source_commits` is a `BTreeMap` so `-o json`/`-o yaml`
        // serialize its keys in a fixed order; `DesiredState.source_commits`
        // stays a `HashMap` internally since nothing else reads its
        // iteration order.
        source_commits: source_commits.into_iter().collect(),
        backups: backup_outputs,
    };
    printer.emit(Doc::new().with_data(&output));

    Ok(ApplyOutcome {
        status,
        aborted_code: None,
    })
}

/// Structured `-o json` payload emitted when an apply is cooperatively aborted
/// by a signal.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AbortOutput {
    aborted: bool,
    signal: String,
    applied: usize,
    /// Actions the abort caught mid-flight: the signal reaches the child
    /// process too, so an interrupted install dies with the run. Without it a
    /// consumer differencing `total - applied` reads a killed action as one
    /// that never started.
    failed: usize,
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

/// Re-record the content hash of every link-deployed file whose recorded value
/// has gone stale — an edit made THROUGH a symlink is the source changing,
/// which is never drift, so no action ever revisits the row. Best-effort and
/// silent: a failure is logged, never propagated, because a bookkeeping
/// correction must not fail an apply that otherwise succeeded.
///
/// The file manager is optional so a `--module` run, which registers none,
/// still refreshes its modules' aggregate rows.
fn refresh_link_deployed_hashes(
    reconciler: &cfgd_core::reconciler::Reconciler<'_>,
    registry: &cfgd_core::providers::ProviderRegistry,
    resolved: &cfgd_core::config::ResolvedProfile,
    modules: &[cfgd_core::modules::ResolvedModule],
) {
    if let Err(e) =
        reconciler.refresh_link_deployed_hashes(registry.file_manager.as_deref(), resolved, modules)
    {
        tracing::warn!(error = %e, "failed to refresh recorded file hashes");
    }
}

/// Remove package-tracking rows whose package is no longer installed (stale
/// after a partial-uninstall failure or an out-of-band removal). Best-effort:
/// any failure is logged, never propagated, so it can't fail an apply.
fn gc_stale_package_tracking(
    state: &cfgd_core::state::StateStore,
    managers: &[&dyn cfgd_core::providers::PackageManager],
    cx: &cfgd_core::providers::PackageContext<'_>,
) {
    let tracked = match cfgd_installed_packages(state) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read tracked packages for GC");
            return;
        }
    };
    match cfgd_core::reconciler::stale_tracked_packages(managers, &tracked, cx) {
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
            None => {
                printer
                    .status(
                        Role::Warn,
                        format!("Orphaned {}/{}", orphan.manager, orphan.package),
                    )
                    .detail("no persisted uninstall; manual removal needed");
            }
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
    use cfgd_core::output::{Printer, Verbosity};

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
        registry.set_package_managers(crate::packages::all_package_managers());

        // Normal verbosity: Accent/Warn status lines are suppressed under Quiet
        // (the default for `for_test`).
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        preview_orphaned_custom_packages(&state, &registry, &printer);
        drop(printer);
        let out = cfgd_core::test_helpers::captured_text(&buf);

        assert!(
            out.contains("would uninstall orphaned widgetmgr/widget via persisted script"),
            "persisted-script preview line missing, got: {out}"
        );
        assert!(
            out.contains(
                "Orphaned legacymgr/legacypkg — no persisted uninstall; manual removal needed"
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

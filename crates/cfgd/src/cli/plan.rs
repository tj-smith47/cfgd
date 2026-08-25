use super::*;

pub fn cmd_plan(
    cli: &Cli,
    printer: &cfgd_core::output::Printer,
    args: &PlanArgs,
) -> anyhow::Result<()> {
    // Parse --context
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

    // --from: mirror cmd_apply so `plan` can be pointed at a git source or local path.
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

    let config_dir = config_dir(cli);
    let ctx = RunContext::new(cli, printer);
    let state = ctx.state()?;
    let module_filter: &[String] = &args.module;
    let with_profile = args.with_profile;

    // `--with-profile` opts a `--module` run INTO composing with the full
    // profile; with no module named, there is nothing for it to compose
    // with — reject rather than silently behaving like a plain `cfgd plan`.
    if with_profile && module_filter.is_empty() {
        anyhow::bail!(
            "--with-profile requires --module (it composes the named module(s) with the full profile; without --module there is nothing to add)"
        );
    }

    // Load config and profile — same pattern as cmd_apply. The header these
    // rows belong to is rendered once the plan is final, so the profile label
    // is carried down rather than printed here. An isolated run resolved no
    // profile, so it carries none and the header omits the row.
    let (cfg, resolved, profile_label, config_parsed) =
        load_config_and_profile_module_scoped(cli, printer, module_filter, with_profile)?;

    // Compose with sources (network refresh) and resolve modules through the one
    // shared desired-state resolver — same path apply takes.
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

    // ONE installed-state read for the whole command: the profile planner below
    // diffs against it, and `Reconciler::plan` diffs a module's declared
    // packages against the same enumeration, so a converged host asks each
    // manager once rather than once per surface.
    let pkg_cx = cfgd_core::providers::PackageContext::new(printer, state);

    let module_names: Vec<String> = resolved_modules.iter().map(|m| m.name.clone()).collect();
    let reconciler = Reconciler::new(&registry, state)
        .with_config_dir(&config_dir)
        .diffing_installed(&pkg_cx);

    // ONE bar for the whole planning wait. Two adjacent `narrate("Planning")`
    // calls read as one label but are not one bar: the first retires and the
    // second redraws, so a phase name can appear twice across the seam.
    let (dry_run_fm, actual_packages, mut plan) =
        printer.narrate("Planning", |sp| -> anyhow::Result<_> {
            // Plan-only mode: no secret providers needed
            let (pkg_actions, file_actions, dry_run_fm, actual_packages) = if module_only {
                (
                    Vec::new(),
                    Vec::new(),
                    None,
                    cfgd_core::reconciler::ActualPackages::default(),
                )
            } else {
                sp.set_message("Planning Packages");
                let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
                    .package_managers()
                    .iter()
                    .map(|m| m.as_ref())
                    .collect();
                // Mirror apply's prune guard so the preview matches what a real run does:
                // a scoped plan (--phase / --only / --skip / --skip-scripts) sees a
                // partial picture, so suppress prune previews with an empty tracked set.
                let scope_restricted = phase_filter.is_some()
                    || !args.skip.is_empty()
                    || !args.only.is_empty()
                    || args.skip_scripts;
                let cfgd_installed = if scope_restricted {
                    std::collections::HashSet::new()
                } else {
                    cfgd_installed_packages(state)?
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

                let fa = fm.plan(&effective_resolved.merged)?;
                (pkg, fa, Some(fm), actual)
            };

            // The preview renders `brew install neovim (0.10.2)`, so this is a
            // path that consumes a version — priced survivor-gated (a package
            // the machine already holds is elided and never queried), and
            // under this bar so the wait is narrated, not dead air.
            sp.set_message("Resolving package versions");
            reconciler.fill_planned_versions(&mut resolved_modules, &registry.manager_map());

            let plan = reconciler.plan_observed(
                &effective_resolved,
                file_actions,
                pkg_actions,
                resolved_modules,
                reconcile_context,
                &mut |phase| sp.set_message(format!("Planning {}", phase.display_name())),
            )?;
            Ok((dry_run_fm, actual_packages, plan))
        })?;

    // A resource awaiting (or declined by) a source decision is not this run's
    // to plan. Pruned before the scope snapshot below, so the preview, the
    // counts and the payload all describe the set an apply would execute —
    // `apply` prunes with the same set, through the same gate. A preview writes
    // nothing, so an item classified but not yet recorded is withheld and
    // listed without a row being minted for it; the row lands when `cfgd
    // decide` answers it, or once an apply/tick proceeds.
    let (withheld, _review) = plan_ops::withheld_for_run(
        &ctx,
        state,
        &cfg,
        &effective_resolved,
        config_parsed,
        plan_ops::DecisionWrites::ReadOnly,
        &actual_packages,
    )?;
    reconciler::withhold_from_plan(
        &mut plan,
        &reconciler::DecisionExclusions::from_withheld(&withheld),
    );

    // Snapshot scope before --skip/--only prune the plan, so a zero-action
    // preview distinguishes "in sync" from "a filter excluded pending work".
    let filter_active = phase_filter.is_some()
        || !args.skip.is_empty()
        || !args.only.is_empty()
        || args.skip_scripts;
    let mut scope = ScopeReport::capture(&plan, filter_active);

    // Apply --skip / --only filters. `known_module_names` reads the module
    // tree and lockfile ONCE, only when a filter is actually active — a
    // filter-less run (the common case) never pays that I/O.
    let known_modules = if args.skip.is_empty() && args.only.is_empty() {
        std::collections::HashSet::new()
    } else {
        known_module_names(&config_dir)
    };
    scope.filter_miss = filter_plan(
        &mut plan,
        &args.skip,
        &args.only,
        phase_filter.as_ref(),
        printer,
        &registry,
        &known_modules,
    );

    // Strip script phases when --skip-scripts is set
    if args.skip_scripts {
        strip_scripts_from_plan(&mut plan);
    }

    // Surfaced in the preview so `plan` never omits work a real apply would do.
    let pending_backups: Vec<String> = pending_backups(&effective_resolved.merged)
        .iter()
        .map(|b| b.name.clone())
        .collect();

    let run = reconciler::ApplyRun::new(
        reconciler::RunContext {
            title: reconciler::RunTitle::Plan,
            config_path: Some(&cli.config),
            profile: profile_label.as_deref(),
            sources: &composed_sources,
            modules: &module_names,
            trigger: None,
        },
        &plan,
    )
    .with_filter(phase_filter.as_ref())
    .with_withheld(&withheld)
    .decisions_answerable(reconciler::owns_decision_store(
        &cli.config,
        cli.state_dir.is_some(),
        cli.scope(),
    ))
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

    Ok(())
}

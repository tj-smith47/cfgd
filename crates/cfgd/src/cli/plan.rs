use super::*;

use cfgd_core::PathDisplayExt;

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

    printer.heading("Plan");

    let config_dir = config_dir(cli);
    let state = open_state_store(cli.state_dir.as_deref())?;
    let module_filter = args.module.as_deref();

    // Load config and profile — same pattern as cmd_apply
    let (cfg, resolved) = if let Some(mod_name) = module_filter {
        match load_config_and_profile(cli) {
            Ok((cfg, profile_name, resolved)) => {
                printer.kv_block([
                    ("Config".to_string(), cli.config.display_posix()),
                    ("Profile".to_string(), profile_name),
                ]);
                (cfg, resolved)
            }
            Err(e) => {
                tracing::debug!("profile load failed, using module-only mode: {}", e);
                let cfg =
                    config::load_config(&cli.config).unwrap_or_else(|_| config::minimal_config());
                let resolved = empty_resolved_profile(mod_name);
                printer.kv_block([
                    ("Config".to_string(), cli.config.display_posix()),
                    ("Profile".to_string(), "(module-only)".to_string()),
                ]);
                (cfg, resolved)
            }
        }
    } else {
        let (cfg, profile_name, resolved) = load_config_and_profile(cli)?;
        printer.kv_block([
            ("Config".to_string(), cli.config.display_posix()),
            ("Profile".to_string(), profile_name),
        ]);
        (cfg, resolved)
    };

    let mut registry = build_registry_with_config(Some(&cfg));
    registry.set_system_config_dir(&config_dir);

    // `ApplyPhase` (clap ValueEnum) is already validated at parse time.
    let phase_filter: Option<PhaseName> = args.phase.map(apply_phase_to_phase_name);

    // Compose with sources (network refresh) and resolve modules through the one
    // shared desired-state resolver — same path apply takes.
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
    let resolved_modules = desired.modules;
    let mut effective_resolved = desired.resolved;

    // Resolve manifest files (Brewfile, package.json, etc.) into package lists
    packages::resolve_manifest_packages(&mut effective_resolved.merged.packages, &config_dir)?;

    // Extend registry with custom package managers from config
    registry.package_managers.extend(packages::custom_managers(
        &effective_resolved.merged.packages.custom,
    ));

    let module_only = module_filter.is_some();

    // `--module <name>` that resolved to nothing (typo / not found) — captured
    // before `resolved_modules` is consumed by planning below.
    let module_miss = module_filter
        .filter(|_| resolved_modules.is_empty())
        .map(str::to_string);

    // Plan-only mode: no secret providers needed
    let (pkg_actions, file_actions, dry_run_fm) = if module_only {
        (Vec::new(), Vec::new(), None)
    } else {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
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
            cfgd_installed_packages(&state)?
        };
        // Profile-scoped: module packages are added separately by
        // `reconciler.plan` as `Action::Module`, so this planner must stay
        // profile-only to avoid double-handling them.
        let pkg = packages::plan_packages(
            &effective_resolved.merged,
            &[],
            &all_managers,
            &cfgd_installed,
        )?;

        let mut fm = CfgdFileManager::new(&config_dir, &effective_resolved)?;
        fm.set_global_strategy(cfg.spec.file_strategy);
        if !source_env.is_empty() {
            fm.set_source_env(&source_env);
        }

        let fa = fm.plan(&effective_resolved.merged)?;
        (pkg, fa, Some(fm))
    };

    let reconciler = Reconciler::new(&registry, &state);
    let mut plan = reconciler.plan(
        &effective_resolved,
        file_actions,
        pkg_actions,
        resolved_modules,
        reconcile_context,
    )?;

    // Snapshot scope before --skip/--only prune the plan, so a zero-action
    // preview distinguishes "in sync" from "a filter excluded pending work".
    let filter_active = phase_filter.is_some()
        || !args.skip.is_empty()
        || !args.only.is_empty()
        || args.skip_scripts;
    let scope = ScopeReport::capture(&plan, filter_active, module_miss);

    // Apply --skip / --only filters
    filter_plan(&mut plan, &args.skip, &args.only);

    // Strip script phases when --skip-scripts is set
    if args.skip_scripts {
        strip_scripts_from_plan(&mut plan);
    }

    display_plan_preview(
        &plan,
        printer,
        &state,
        &args.context,
        phase_filter.as_ref(),
        dry_run_fm.as_ref(),
        &scope,
    );

    Ok(())
}

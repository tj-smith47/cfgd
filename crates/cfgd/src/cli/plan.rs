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

    // `ApplyPhase` (clap ValueEnum) is already validated at parse time.
    let phase_filter: Option<PhaseName> = args.phase.map(apply_phase_to_phase_name);

    // Compose with sources if configured
    let source_env = if !cfg.spec.sources.is_empty() {
        let composition_result = compose_with_sources(cli, &cfg, &resolved, printer)?;
        let se = composition_result.source_env;
        (Some(composition_result.resolved), se)
    } else {
        (None, std::collections::HashMap::new())
    };
    let mut effective_resolved = source_env.0.unwrap_or(resolved);
    let source_env = source_env.1;

    // Resolve manifest files (Brewfile, package.json, etc.) into package lists
    packages::resolve_manifest_packages(&mut effective_resolved.merged.packages, &config_dir)?;

    // Extend registry with custom package managers from config
    registry.package_managers.extend(packages::custom_managers(
        &effective_resolved.merged.packages.custom,
    ));

    // Resolve modules
    let module_names = if let Some(mod_name) = module_filter {
        vec![mod_name.to_string()]
    } else {
        effective_resolved.merged.modules.clone()
    };

    let resolved_modules = if !module_names.is_empty() {
        let platform = Platform::detect();
        let mgr_map = managers_map(&registry);
        let cache_base = modules::default_module_cache_dir()?;
        match modules::resolve_modules(
            &module_names,
            &config_dir,
            &cache_base,
            &platform,
            &mgr_map,
            printer,
        ) {
            Ok(mods) => mods,
            Err(e) if module_filter.is_some() => {
                tracing::debug!("module filter '{}' not found: {}", module_names[0], e);
                Vec::new()
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        Vec::new()
    };

    let module_only = module_filter.is_some();

    // Plan-only mode: no secret providers needed
    let (pkg_actions, file_actions, dry_run_fm) = if module_only {
        (Vec::new(), Vec::new(), None)
    } else {
        let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
            .package_managers
            .iter()
            .map(|m| m.as_ref())
            .collect();
        let pkg = packages::plan_packages(&effective_resolved.merged, &all_managers)?;

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
    );

    Ok(())
}

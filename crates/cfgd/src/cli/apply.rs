use super::*;

pub(super) fn cmd_apply(cli: &Cli, printer: &Printer, args: &ApplyArgs) -> anyhow::Result<()> {
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
    if dry_run {
        printer.header("Plan");
    } else {
        printer.header("Apply");
    }

    let config_dir = config_dir(cli);
    let state = open_state_store(cli.state_dir.as_deref())?;

    // When --module is set, try loading profile but fall back to empty if none configured
    let (cfg, resolved) = if let Some(mod_name) = module_filter {
        match load_config_and_profile(cli, printer) {
            Ok(pair) => pair,
            Err(e) => {
                tracing::debug!("profile load failed, using module-only mode: {}", e);
                let cfg =
                    config::load_config(&cli.config).unwrap_or_else(|_| config::minimal_config());
                let resolved = empty_resolved_profile(mod_name);
                printer.key_value("Config", &cli.config.display().to_string());
                printer.key_value("Profile", "(module-only)");
                (cfg, resolved)
            }
        }
    } else {
        load_config_and_profile(cli, printer)?
    };

    let mut registry = build_registry_with_config(Some(&cfg));

    // `ApplyPhase` (clap ValueEnum) is already validated at parse time.
    let phase_filter: Option<PhaseName> = args.phase.map(apply_phase_to_phase_name);

    // Compose with sources if configured
    let (source_env, source_commits) = if !cfg.spec.sources.is_empty() {
        let composition_result = compose_with_sources(cli, &cfg, &resolved, printer)?;
        let se = composition_result.source_env;
        let sc = composition_result.source_commits;
        ((Some(composition_result.resolved), se), sc)
    } else {
        (
            (None, std::collections::HashMap::new()),
            std::collections::HashMap::new(),
        )
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

    // If --module is set, skip profile-level packages/files
    let module_only = module_filter.is_some();

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
        let pkg = packages::plan_packages(&effective_resolved.merged, &all_managers)?;

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

    let reconciler = Reconciler::new(&registry, &state);
    let mut plan = reconciler.plan(
        &effective_resolved,
        file_actions,
        pkg_actions,
        resolved_modules.clone(),
        ReconcileContext::Apply,
    )?;

    // Apply --skip / --only filters
    filter_plan(&mut plan, skip, only);

    // Strip script phases when --skip-scripts is set
    if args.skip_scripts {
        strip_scripts_from_plan(&mut plan);
    }

    if dry_run {
        display_plan_preview(
            &plan,
            printer,
            &state,
            "apply",
            phase_filter.as_ref(),
            dry_run_fm.as_ref(),
        );
        return Ok(());
    }

    // --- Apply mode ---

    // Handle unmanaged file targets: if a target exists as a non-cfgd file, prompt to
    // adopt (proceed), backup (rename to .cfgd-backup), or skip.
    handle_unmanaged_file_targets(&mut plan, &config_dir, &state, printer, yes)?;

    // Check if filtered plan has actions
    let has_actions = if let Some(ref pf) = phase_filter {
        plan.phases
            .iter()
            .any(|p| &p.name == pf && !p.actions.is_empty())
    } else {
        !plan.is_empty()
    };

    if !has_actions {
        printer.success(MSG_NOTHING_TO_DO);
        return Ok(());
    }

    // Show what will change
    printer.newline();
    for phase_item in &plan.phases {
        if let Some(ref pf) = phase_filter
            && &phase_item.name != pf
        {
            continue;
        }
        let items = reconciler::format_plan_items(phase_item);
        if !items.is_empty() {
            printer.plan_phase(phase_item.name.display_name(), &items);
        }
    }

    for w in &plan.warnings {
        printer.warning(w);
    }

    // Confirm
    if !yes {
        printer.newline();
        // Closed-TTY / non-interactive defaults to "no" — apply is destructive
        // and silence is treated as decline, not as approval.
        let confirmed = printer
            .prompt_confirm("Apply these changes?")
            .unwrap_or(false);
        if !confirmed {
            printer.info("Aborted");
            return Ok(());
        }
    }

    printer.newline();

    // Acquire apply lock to prevent concurrent applies
    let lock_state_dir;
    let apply_lock_dir: &std::path::Path = if let Some(dir) = cli.state_dir.as_deref() {
        dir
    } else {
        lock_state_dir = cfgd_core::state::default_state_dir()
            .map_err(|e| anyhow::anyhow!("cannot determine state directory: {}", e))?;
        &lock_state_dir
    };
    let _apply_lock = cfgd_core::acquire_apply_lock(apply_lock_dir)?;

    // Apply
    let result = reconciler.apply(
        &plan,
        &effective_resolved,
        &config_dir,
        printer,
        phase_filter.as_ref(),
        &resolved_modules,
        ReconcileContext::Apply,
        args.skip_scripts,
    )?;

    printer.newline();
    print_apply_result(&result, printer);

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

    // Prune old backups — keep last 10 applies' worth.
    let _ = state.prune_old_backups(10);

    Ok(())
}

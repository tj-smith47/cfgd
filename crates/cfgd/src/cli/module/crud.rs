use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{
    Doc, OwnerLabel, Printer, Role, TitleLabel, collapse_to_subject_line, condense_script_label,
};

/// Parse a `--package` token for a MODULE document.
///
/// A module entry names a package and its preferred managers; it has no
/// per-sub-list slot, so a token whose schema path resolves to a slot that is
/// not itself a registered manager (`snap.classic`) is refused rather than
/// silently confirmed as a path the document cannot hold. `snap` installs both
/// of its lists and retries with `--classic`, so nothing is lost by saying so.
fn module_package_ref(token: &str, native: &str) -> anyhow::Result<PackageRef> {
    let pkg = super::parse_package_flag(token, &[], native)?;
    if let (Some(path), Some(slot), Some(manager)) = (&pkg.schema_path, &pkg.slot, &pkg.manager)
        && slot != manager
    {
        anyhow::bail!(
            "'{path}' is a profile-only package list; a module entry names a manager \
             — use {manager}:{}",
            pkg.name
        );
    }
    Ok(pkg)
}

pub fn cmd_module_create(
    cli: &Cli,
    printer: &Printer,
    args: &ModuleCreateArgs,
) -> anyhow::Result<()> {
    let name = &args.name;
    let description = args.description.as_deref();
    let depends = &args.depends;
    let pkg_names = &args.packages;
    let files = &args.files;
    let env_list = &args.env;
    let post_apply = &args.post_apply;
    let sets = &args.sets;
    validate_resource_name(name, "Module")?;
    printer.heading_title(&TitleLabel::new("Create Module", name));

    let config_dir = config_dir(cli);
    let module_dir = config_dir.join("modules").join(name);
    let module_yaml_path = module_dir.join("module.yaml");

    if module_yaml_path.exists() {
        return Err(crate::cli::cli_error(
            name,
            "already_exists",
            format!("Module '{}' already exists at {}", name, module_dir.posix()),
            serde_json::json!({ "path": cfgd_core::to_posix_string(&module_dir) }),
        ));
    }

    std::fs::create_dir_all(&module_dir)?;

    // Interactive mode if no content flags provided
    let is_interactive = description.is_none()
        && depends.is_empty()
        && pkg_names.is_empty()
        && files.is_empty()
        && env_list.is_empty()
        && args.aliases.is_empty()
        && post_apply.is_empty()
        && sets.is_empty();

    let (desc, dep_list, pkg_list, file_list, post_apply_list) = if is_interactive {
        let desc = printer.prompt_text("Description", "")?;
        let desc = if desc.is_empty() { None } else { Some(desc) };

        let deps_str = printer.prompt_text("Dependencies (comma-separated, or empty)", "")?;
        let deps: Vec<String> = if deps_str.is_empty() {
            Vec::new()
        } else {
            deps_str.split(',').map(|s| s.trim().to_string()).collect()
        };

        let mut pkgs = Vec::new();
        loop {
            let pkg = printer.prompt_text("Add package (name, or empty to stop)", "")?;
            if pkg.is_empty() {
                break;
            }
            pkgs.push(pkg);
        }

        let mut imported_files: Vec<String> = Vec::new();
        loop {
            let file =
                printer.prompt_text("Add file (path or source:target, or empty to stop)", "")?;
            if file.is_empty() {
                break;
            }
            imported_files.push(file);
        }

        let mut scripts = Vec::new();
        loop {
            let script =
                printer.prompt_text("Add post-apply script (command, or empty to stop)", "")?;
            if script.is_empty() {
                break;
            }
            scripts.push(script);
        }

        (desc, deps, pkgs, imported_files, scripts)
    } else {
        (
            description.map(String::from),
            depends.to_vec(),
            pkg_names.to_vec(),
            files.to_vec(),
            post_apply.to_vec(),
        )
    };

    // Create directories and copy files into module (detect basename collisions)
    let files_dir = module_dir.join("files");
    {
        let mut seen = std::collections::HashSet::new();
        for spec in &file_list {
            let (source, _) = parse_file_spec(spec)?;
            let base = source
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if !seen.insert(base.clone()) {
                return Err(crate::cli::cli_error(
                    name,
                    "duplicate_basename",
                    format!(
                        "Duplicate file basename '{}' — multiple files would overwrite each other in modules/{}/files/",
                        base, name
                    ),
                    serde_json::json!({ "basename": base }),
                ));
            }
        }
    }
    let copied = copy_files_to_dir(&file_list, &files_dir)?;
    let is_private = args.private;
    let file_entries: Vec<config::ModuleFileEntry> = copied
        .iter()
        .map(|(basename, target)| config::ModuleFileEntry {
            patch: None,
            source: format!("files/{}", basename),
            target: target.display().to_string(),
            strategy: None,
            private: is_private,
            encryption: None,
            permissions: None,
        })
        .collect();
    if is_private {
        for (basename, _) in &copied {
            add_to_gitignore(&config_dir, &format!("modules/{}/files/{}", name, basename))?;
        }
    }

    // Build package entries
    let native = Platform::current().native_manager().to_string();
    let package_entries: Vec<config::ModulePackageEntry> = pkg_list
        .iter()
        .map(|s| {
            let pkg = module_package_ref(s, &native)?;
            Ok(config::ModulePackageEntry {
                name: pkg.name,
                min_version: None,
                // The REGISTERED manager, never the schema path: `prefer` is a
                // persisted manager name and `brew.casks` is not one.
                prefer: pkg.manager.into_iter().collect(),
                deny: Vec::new(),
                aliases: std::collections::HashMap::new(),
                script: None,
                platforms: Vec::new(),
                ..Default::default()
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Build env
    let mut env_entries = Vec::new();
    for e in env_list {
        env_entries.push(cfgd_core::parse_env_var(e).map_err(|e| anyhow::anyhow!(e))?);
    }

    // Build aliases
    let mut alias_entries = Vec::new();
    for a in &args.aliases {
        alias_entries.push(cfgd_core::parse_alias(a).map_err(|e| anyhow::anyhow!(e))?);
    }

    // Build scripts — normalize shell escape artifacts (bash escapes ! to \! in double quotes)
    let scripts = if post_apply_list.is_empty() {
        None
    } else {
        Some(config::ScriptSpec {
            post_apply: post_apply_list
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.replace("\\!", "!")))
                .collect(),
            ..Default::default()
        })
    };

    // Build document
    let mut doc = config::ModuleDocument {
        api_version: cfgd_core::API_VERSION.to_string(),
        kind: "Module".to_string(),
        metadata: config::ModuleMetadata {
            name: name.to_string(),
            description: desc,
            // Scaffolded with a version so the module is taggable by the
            // generated release workflow without a follow-up edit.
            version: Some("0.1.0".to_string()),
        },
        spec: config::ModuleSpec {
            depends: dep_list,
            platforms: Vec::new(),
            packages: package_entries,
            files: file_entries,
            env: env_entries,
            aliases: alias_entries,
            scripts,
            system: std::collections::BTreeMap::new(),
        },
    };

    // Apply --set overrides
    if !sets.is_empty() {
        apply_module_sets(sets, &mut doc)?;
    }

    // Write
    scaffold_module_document(&doc, &module_yaml_path)?;

    let summary_sec = printer.section(format!(
        "Created module '{}' at {}",
        name,
        module_dir.posix()
    ));
    if !doc.spec.packages.is_empty() {
        summary_sec.kv(
            "Packages",
            doc.spec
                .packages
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if !doc.spec.files.is_empty() {
        summary_sec.kv("Files", doc.spec.files.len().to_string());
    }
    if !doc.spec.depends.is_empty() {
        summary_sec.kv("Dependencies", doc.spec.depends.join(", "));
    }
    drop(summary_sec);

    printer.hint("Add to a profile with: cfgd profile update <profile> --module <name>");
    printer.hint("Fine-tune with: cfgd module edit <name>");

    update_workflow_best_effort(cli, printer);

    // Apply if requested
    let mut applied = false;
    // Deferred so the structured payload below still reaches `-o json`
    // consumers before the process exits nonzero on a failed apply.
    let mut apply_status = cfgd_core::state::ApplyStatus::Success;
    if args.apply {
        let config_path = config_dir.join(cfgd_core::config::CONFIG_FILENAME);
        let mut cfg = config::load_config(&config_path)?;
        drain_config_deprecations(printer, &mut cfg);
        let mut registry = super::build_registry_with_config(Some(&cfg));
        registry.set_system_config_dir(&config_dir);
        let store = super::open_state_store(cli.state_dir.as_deref(), cli.scope())?;

        let platform = cfgd_core::platform::Platform::current();
        let mgr_map = registry.manager_map();
        let cache_base = module_cache_dir(cli)?;
        let mut resolved_modules = modules::resolve_modules(
            std::slice::from_ref(name),
            &config_dir,
            &cache_base,
            &[],
            platform,
            &mgr_map,
            printer,
        )?;
        let resolved = config::ResolvedProfile {
            layers: Vec::new(),
            merged: config::MergedProfile::default(),
        };

        let pkg_cx = cfgd_core::providers::PackageContext::new(printer, &store);
        let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store)
            .with_config_dir(&config_dir)
            .diffing_installed(&pkg_cx);
        // Survivor-gated pricing: only a package this plan will surface is
        // asked for the version its install action renders and persists.
        reconciler.fill_planned_versions(&mut resolved_modules, &mgr_map);
        let plan = reconciler.plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            resolved_modules.clone(),
            cfgd_core::reconciler::ReconcileContext::Apply,
        )?;

        // The module just created is the whole of this run: one owner, no
        // profile, and the same skeleton `cfgd apply` renders.
        let module_names = vec![name.to_string()];
        let ctx = cfgd_core::reconciler::RunContext {
            title: cfgd_core::reconciler::RunTitle::Apply,
            config_path: Some(config_path.as_path()),
            profile: None,
            sources: &[],
            modules: &module_names,
            trigger: None,
        };
        let run = cfgd_core::reconciler::ApplyRun::new(ctx, &plan);

        if plan.total_actions() == 0 {
            run.header(printer);
            // `module create` exposes no scoping flag, so the verdict takes the
            // filter-less arm of the one helper that owns both spellings.
            crate::cli::plan_ops::report_plan_verdict(printer, 0, None);
        } else {
            // Same requirement as `cfgd init --apply-module`: the apply records
            // module state from this slice, and regenerates the env files from
            // the PATH directories of a manager it bootstrapped mid-run.
            //
            // see helpers::run_state_dir — honor --state-dir so this lock
            // mutually-excludes against `cfgd apply` and the daemon.
            let abort = cfgd_core::AbortFlag::new();
            let mut exec = crate::cli::apply::ReconcilerExecutor::unscoped(
                &reconciler,
                &resolved,
                &config_dir,
                &resolved_modules,
                &abort,
                run_state_dir(cli.state_dir.as_deref(), cli.scope())?,
            );
            let confirm = if args.yes {
                cfgd_core::reconciler::Confirm::Skip
            } else {
                cfgd_core::reconciler::Confirm::Ask("Apply these changes?")
            };
            match run.execute(printer, confirm, &mut exec)? {
                cfgd_core::reconciler::RunDisposition::Applied { result, .. } => {
                    apply_status = result.status.clone();
                    // A module whose packages come from a manager this apply
                    // bootstrapped leaves the invoking shell one `source` away
                    // from reaching them.
                    crate::cli::plan_ops::print_caveats(&result, printer);
                    applied = true;
                }
                cfgd_core::reconciler::RunDisposition::Declined => {
                    printer
                        .status(Role::Info, "Skipped")
                        .detail("run 'cfgd apply' to apply later");
                    printer.emit(Doc::new().with_data(serde_json::json!({
                        "name": name,
                        "path": module_dir.display().to_string(),
                        "applied": false,
                    })));
                    return Ok(());
                }
                // Unreachable for a run carrying a plan with work and no
                // `preview_only`, and none of them ran an action.
                cfgd_core::reconciler::RunDisposition::NothingToDo
                | cfgd_core::reconciler::RunDisposition::Previewed
                | cfgd_core::reconciler::RunDisposition::BackupsApplied { .. } => {}
            }
        }
    }

    printer.emit(Doc::new().with_data(serde_json::json!({
        "name": name,
        "path": module_dir.display().to_string(),
        "applied": applied,
    })));

    // Same contract as `cfgd apply`: an apply that ran and lost actions must not
    // report success. Exiting directly (rather than returning an error) keeps
    // render_cli_error from double-printing a failure line after the per-action
    // report above.
    if matches!(
        apply_status,
        cfgd_core::state::ApplyStatus::Partial | cfgd_core::state::ApplyStatus::Failed
    ) {
        cfgd_core::exit::ExitCode::ApplyFailed.exit();
    }

    Ok(())
}

// --- Module Update (local) ---

pub fn cmd_module_update_local(
    cli: &Cli,
    printer: &Printer,
    args: &ModuleUpdateArgs,
) -> anyhow::Result<()> {
    let name = &args.name;
    let (add_packages, remove_packages) = cfgd_core::split_add_remove(&args.packages);
    let (add_files, remove_files) = cfgd_core::split_add_remove(&args.files);
    let (add_env, remove_env) = cfgd_core::split_add_remove(&args.env);
    let (add_aliases, remove_aliases) = cfgd_core::split_add_remove(&args.aliases);
    let (add_depends, remove_depends) = cfgd_core::split_add_remove(&args.depends);
    let (add_post_apply, remove_post_apply) = cfgd_core::split_add_remove(&args.post_apply);
    let description = args.description.as_deref();
    let sets = &args.sets;
    validate_resource_name(name, "Module")?;
    printer.heading_owner_prefixed("Update", &OwnerLabel::new("module", name));

    let config_dir = config_dir(cli);
    let (mut doc, module_yaml_path) = match load_module_document(&config_dir, name) {
        Ok(v) => v,
        Err(e) => {
            let error_kind = e.error_code();
            let message = e.to_string();
            return Err(crate::cli::cli_error_ctx(
                e.into(),
                name,
                error_kind,
                message,
                serde_json::json!({}),
            ));
        }
    };
    let module_dir = config_dir.join("modules").join(name);
    let files_dir = module_dir.join("files");
    let mut changes = 0u32;

    // Update description
    if let Some(desc) = description {
        doc.metadata.description = if desc.is_empty() {
            None
        } else {
            Some(desc.to_string())
        };
        changes += 1;
    }

    // Add dependencies
    for dep in &add_depends {
        if !doc.spec.depends.contains(dep) {
            doc.spec.depends.push(dep.clone());
            printer
                .status(Role::Ok, "Added dependency")
                .qualifier(dep.clone());
            changes += 1;
        }
    }

    // Remove dependencies
    for dep in &remove_depends {
        let before = doc.spec.depends.len();
        doc.spec.depends.retain(|d| d != dep);
        if doc.spec.depends.len() < before {
            printer
                .status(Role::Ok, "Removed dependency")
                .qualifier(dep.clone());
            changes += 1;
        } else {
            printer.status_simple(Role::Warn, format!("Dependency '{}' not found", dep));
        }
    }

    // Add and remove packages, both through the SAME parser: a prefix legal to
    // add is legal to remove, and the removal strips it rather than searching
    // for a name that carries it.
    let native = Platform::current().native_manager().to_string();
    for pkg_str in &add_packages {
        let pkg = module_package_ref(pkg_str, &native)?;
        if doc.spec.packages.iter().any(|p| p.name == pkg.name) {
            printer.status_simple(
                Role::Info,
                format!(
                    "{} '{}' already in module",
                    pkg.noun_capitalized(),
                    pkg.name
                ),
            );
            continue;
        }
        let qualifier = pkg.display(&native);
        let pkg_noun = pkg.noun();
        doc.spec.packages.push(config::ModulePackageEntry {
            name: pkg.name,
            min_version: None,
            // The REGISTERED manager, never the schema path: `prefer` is a
            // persisted manager name and `brew.casks` is not one.
            prefer: pkg.manager.into_iter().collect(),
            deny: Vec::new(),
            aliases: std::collections::HashMap::new(),
            script: None,
            platforms: Vec::new(),
            ..Default::default()
        });
        printer
            .status(Role::Ok, format!("Added {}", pkg_noun))
            .qualifier(qualifier);
        changes += 1;
    }

    for pkg_str in &remove_packages {
        let pkg = module_package_ref(pkg_str, &native)?;
        let before = doc.spec.packages.len();
        doc.spec.packages.retain(|p| p.name != pkg.name);
        if doc.spec.packages.len() < before {
            printer
                .status(Role::Ok, format!("Removed {}", pkg.noun()))
                .qualifier(pkg.display(&native));
            changes += 1;
        } else {
            printer.status_simple(
                Role::Warn,
                format!(
                    "{} '{}' not found in module",
                    pkg.noun_capitalized(),
                    pkg.name
                ),
            );
        }
    }

    // Add files (detect basename collisions within the batch, skip already-tracked)
    let mut files_to_copy: Vec<String> = Vec::new();
    {
        let mut added_basenames = std::collections::HashSet::new();
        for spec in &add_files {
            let (source, _) = parse_file_spec(spec)?;
            let basename = source
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Invalid file path: {}", source.posix()))?
                .to_string_lossy()
                .to_string();

            let source_key = format!("files/{}", basename);
            if doc.spec.files.iter().any(|f| f.source == source_key) {
                printer.status_simple(Role::Info, format!("File '{}' already in module", basename));
                continue;
            }
            if !added_basenames.insert(basename) {
                anyhow::bail!(
                    "Duplicate file basename '{}' — multiple files would overwrite each other",
                    source_key
                );
            }
            files_to_copy.push(spec.clone());
        }
    }
    let copied = copy_files_to_dir(&files_to_copy, &files_dir)?;
    for (basename, target) in &copied {
        if args.private {
            add_to_gitignore(&config_dir, &format!("modules/{}/files/{}", name, basename))?;
        }
        doc.spec.files.push(config::ModuleFileEntry {
            patch: None,
            source: format!("files/{}", basename),
            target: target.display().to_string(),
            strategy: None,
            private: args.private,
            encryption: None,
            permissions: None,
        });
        printer
            .status(Role::Ok, "Added file")
            .qualifier(target.posix().to_string());
        changes += 1;
    }

    // Remove files
    for target in &remove_files {
        let expanded = cfgd_core::expand_tilde(&PathBuf::from(target));
        let target_str = expanded.display().to_string();
        let before = doc.spec.files.len();
        let mut removed_source = None;
        doc.spec.files.retain(|f| {
            let f_target = cfgd_core::expand_tilde(&PathBuf::from(&f.target));
            if f_target.display().to_string() == target_str || f.target == *target {
                removed_source = Some(f.source.clone());
                false
            } else {
                true
            }
        });
        if doc.spec.files.len() < before {
            // Clean up the source file
            if let Some(ref source) = removed_source {
                let source_path = module_dir.join(source);
                if source_path.exists() {
                    if source_path.is_dir() {
                        std::fs::remove_dir_all(&source_path)?;
                    } else {
                        std::fs::remove_file(&source_path)?;
                    }
                }
            }
            printer
                .status(Role::Ok, "Removed file")
                .qualifier(target.clone());
            changes += 1;
        } else {
            printer.status_simple(Role::Warn, format!("File '{}' not found in module", target));
        }
    }

    // Add env vars
    for e in &add_env {
        let ev = cfgd_core::parse_env_var(e).map_err(|e| anyhow::anyhow!(e))?;
        cfgd_core::merge_env(&mut doc.spec.env, std::slice::from_ref(&ev));
        printer
            .status(Role::Ok, "Set env")
            .qualifier(format!("{}={}", ev.name, ev.value));
        changes += 1;
    }

    // Remove env vars
    for key in &remove_env {
        let before = doc.spec.env.len();
        doc.spec.env.retain(|ev| ev.name != *key);
        if doc.spec.env.len() < before {
            printer
                .status(Role::Ok, "Removed env")
                .qualifier(key.clone());
            changes += 1;
        } else {
            printer.status_simple(Role::Warn, format!("Env var '{}' not found", key));
        }
    }

    // Add aliases
    for a in &add_aliases {
        let alias = cfgd_core::parse_alias(a).map_err(|e| anyhow::anyhow!(e))?;
        cfgd_core::merge_aliases(&mut doc.spec.aliases, std::slice::from_ref(&alias));
        printer
            .status(Role::Ok, "Set alias")
            .qualifier(format!("{}={}", alias.name, alias.command));
        changes += 1;
    }

    // Remove aliases
    for name in &remove_aliases {
        let before = doc.spec.aliases.len();
        doc.spec.aliases.retain(|a| a.name != *name);
        if doc.spec.aliases.len() < before {
            printer
                .status(Role::Ok, "Removed alias")
                .qualifier(name.clone());
            changes += 1;
        } else {
            printer.status_simple(Role::Warn, format!("Alias '{}' not found", name));
        }
    }

    // Add post-apply scripts. `--add-post-apply-script` takes the same
    // free-form body a YAML `run:` field would, so condense before it lands
    // in a status subject below — a multi-line value must never go raw.
    for script in &add_post_apply {
        let scripts = doc
            .spec
            .scripts
            .get_or_insert_with(config::ScriptSpec::default);
        let entry = config::ScriptEntry::Simple(script.clone());
        if !scripts.post_apply.contains(&entry) {
            scripts.post_apply.push(entry);
            printer
                .status(Role::Ok, "Added post-apply script")
                .qualifier(condense_script_label(script));
            changes += 1;
        }
    }

    // Remove post-apply scripts. A module with no scripts block at all still
    // reports the script as not-found, matching the env/alias remove paths
    // rather than silently no-opping.
    for script in &remove_post_apply {
        let removed = doc
            .spec
            .scripts
            .as_mut()
            .map(|scripts| {
                let before = scripts.post_apply.len();
                scripts.post_apply.retain(|e| e.run_str() != script);
                scripts.post_apply.len() < before
            })
            .unwrap_or(false);
        let label_text = condense_script_label(script);
        if removed {
            printer
                .status(Role::Ok, "Removed post-apply script")
                .qualifier(label_text);
            changes += 1;
        } else {
            // Echo back the exact raw argument the user searched for — a
            // condensed/truncated view would hide a copy-paste-whitespace
            // mismatch that's exactly the thing worth debugging here.
            // `collapse_to_subject_line` flattens any embedded newlines
            // safely without truncating content.
            printer.status_simple(
                Role::Warn,
                format!("Script '{}' not found", collapse_to_subject_line(script)),
            );
        }
    }

    // Apply --set overrides
    if !sets.is_empty() {
        apply_module_sets(sets, &mut doc)?;
        changes += sets.len() as u32;
    }

    if changes == 0 {
        printer.emit(
            Doc::new()
                .status(Role::Info, "No changes specified")
                .with_data(serde_json::json!({
                    "name": name,
                    "changes": 0,
                })),
        );
        return Ok(());
    }

    save_module_document(&doc, &module_yaml_path)?;
    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!(
                    "Updated module '{}' ({})",
                    name,
                    cfgd_core::pluralize(changes as usize, "change")
                ),
            )
            .with_data(serde_json::json!({
                "name": name,
                "changes": changes,
            })),
    );

    Ok(())
}

// --- Module Edit ---

pub fn cmd_module_edit(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    validate_resource_name(name, "Module")?;
    let config_dir = config_dir(cli);
    let module_yaml = config_dir.join("modules").join(name).join("module.yaml");

    if !module_yaml.exists() {
        // Carry the typed ModuleError::NotFound so the exit-code downcast resolves
        // to ExitCode::NotFound (6), uniform with every other named-resource miss.
        return Err(crate::cli::cli_error_ctx(
            cfgd_core::errors::CfgdError::Module(cfgd_core::errors::ModuleError::NotFound {
                name: name.to_string(),
            })
            .into(),
            name,
            "not_found",
            format!("Module '{}' not found at {}", name, module_yaml.posix()),
            serde_json::json!({ "path": cfgd_core::to_posix_string(&module_yaml) }),
        ));
    }

    open_in_editor(&module_yaml, printer)?;

    // Validate after editing — loop until valid or user cancels
    let mut valid = false;
    loop {
        let contents = std::fs::read_to_string(&module_yaml)?;
        match config::parse_module(&contents) {
            Ok(_) => {
                valid = true;
                break;
            }
            Err(e) => {
                printer.status_simple(
                    Role::Fail,
                    format!(
                        "Module '{}' has errors: {}",
                        name,
                        cfgd_core::output::collapse_to_subject_line(&e),
                    ),
                );
                if !printer.prompt_confirm("Re-open in editor?")? {
                    break;
                }
                open_in_editor(&module_yaml, printer)?;
            }
        }
    }

    if valid {
        printer.emit(
            Doc::new()
                .status(Role::Ok, format!("Module '{}' is valid", name))
                .with_data(serde_json::json!({
                    "name": name,
                    "path": module_yaml.display().to_string(),
                    "valid": true,
                })),
        );
    } else {
        printer.emit(
            Doc::new()
                .status(Role::Warn, "Saved with validation errors")
                .with_data(serde_json::json!({
                    "name": name,
                    "path": module_yaml.display().to_string(),
                    "valid": false,
                })),
        );
    }

    Ok(())
}

// --- Module Delete ---

pub fn cmd_module_delete(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    yes: bool,
    purge: bool,
    ignore_not_found: bool,
) -> anyhow::Result<()> {
    validate_resource_name(name, "Module")?;
    printer.heading_title(&TitleLabel::new("Delete Module", name));

    let config_dir = config_dir(cli);
    let module_dir = config_dir.join("modules").join(name);

    if !module_dir.exists() {
        if ignore_not_found {
            return crate::cli::emit_not_found_ignored(printer, "module", name);
        }
        // Carry the typed ModuleError::NotFound so the exit-code downcast resolves
        // to ExitCode::NotFound (6), uniform with every other named-resource miss.
        return Err(crate::cli::cli_error_ctx(
            cfgd_core::errors::CfgdError::Module(cfgd_core::errors::ModuleError::NotFound {
                name: name.to_string(),
            })
            .into(),
            name,
            "not_found",
            format!("Module '{}' not found at {}", name, module_dir.posix()),
            serde_json::json!({ "path": cfgd_core::to_posix_string(&module_dir) }),
        ));
    }

    // Safety: refuse if any profile references this module
    let referencing = profiles_using_module(&profiles_dir(cli), name)?;
    if !referencing.is_empty() {
        return Err(crate::cli::cli_error(
            name,
            "in_use",
            format!(
                "Cannot delete module '{}' — referenced by {}: {}. Remove it from those profiles first.",
                name,
                cfgd_core::plural_noun(referencing.len(), "profile"),
                referencing.join(", ")
            ),
            serde_json::json!({ "profiles": &referencing }),
        ));
    }

    if !yes && !printer.prompt_confirm(&format!("Delete module '{}'?", name))? {
        printer.emit(
            Doc::new()
                .status(Role::Info, "Cancelled")
                .with_data(serde_json::json!({
                    "name": name,
                    "cancelled": true,
                })),
        );
        return Ok(());
    }

    let module_yaml = module_dir.join("module.yaml");
    let mut files_processed = 0usize;
    if module_yaml.exists()
        && let Ok(doc) = config::parse_module(&std::fs::read_to_string(&module_yaml)?)
    {
        if purge {
            // Purge mode: remove all files deployed by this module to target locations.
            // This replaces symlink restoration — there's nothing to restore if we're
            // removing everything.
            let purge_sec = printer.section("Purging Files");
            for file_entry in &doc.spec.files {
                let target = cfgd_core::expand_tilde(std::path::Path::new(&file_entry.target));
                if target.is_symlink() || target.exists() {
                    if target.is_dir() && !target.is_symlink() {
                        std::fs::remove_dir_all(&target)?;
                    } else {
                        std::fs::remove_file(&target)?;
                    }
                    purge_sec.status_simple(Role::Info, format!("Purged {}", target.posix()));
                    files_processed += 1;
                }
            }
            drop(purge_sec);
        } else {
            // Default: restore symlinked files before deleting the module directory.
            // When module create adopts files, it moves them into the module dir and
            // symlinks the original location back. On delete, we reverse that.
            let restore_sec = printer.section("Restoring Files");
            for file_entry in &doc.spec.files {
                let target = cfgd_core::expand_tilde(std::path::Path::new(&file_entry.target));
                let source = module_dir.join(&file_entry.source);

                if let Ok(link_dest) = std::fs::read_link(&target)
                    && link_dest.starts_with(&module_dir)
                    && source.exists()
                {
                    std::fs::remove_file(&target).ok();
                    if source.is_dir() {
                        cfgd_core::copy_dir_recursive(&source, &target)?;
                    } else {
                        std::fs::copy(&source, &target)?;
                    }
                    restore_sec.status_simple(Role::Info, format!("Restored {}", target.posix()));
                    files_processed += 1;
                }
            }
            drop(restore_sec);
        }
    }

    // Delete directory
    std::fs::remove_dir_all(&module_dir)?;

    // Clean module state from DB
    if let Ok(state) = open_state_store(cli.state_dir.as_deref(), cli.scope())
        && let Err(e) = state.remove_module_state(name)
    {
        printer
            .status(Role::Warn, "Failed to clean module state")
            .qualifier(cfgd_core::output::collapse_to_subject_line(&e));
    }

    // Clean from lockfile if present
    let mut lockfile = modules::load_lockfile(&config_dir)?;
    let had_lock = lockfile.modules.iter().any(|e| e.name == name);
    if had_lock {
        lockfile.modules.retain(|e| e.name != name);
        modules::save_lockfile(&config_dir, &lockfile)?;
        printer.status_simple(Role::Info, format!("Removed '{}' from modules.lock", name));
    }

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Deleted module '{}'", name))
            .with_data(serde_json::json!({
                "name": name,
                "cancelled": false,
                "filesProcessed": files_processed,
                "removedFromLockfile": had_lock,
                "purge": purge,
            })),
    );

    update_workflow_best_effort(cli, printer);

    Ok(())
}

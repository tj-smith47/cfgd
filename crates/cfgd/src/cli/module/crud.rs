use super::*;
use cfgd_core::output::{Doc, Printer, Role};

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
    printer.heading(format!("Create Module: {}", name));

    let config_dir = config_dir(cli);
    let module_dir = config_dir.join("modules").join(name);
    let module_yaml_path = module_dir.join("module.yaml");

    if module_yaml_path.exists() {
        printer.emit(cfgd_core::output::error_doc(
            name,
            "already_exists",
            format!(
                "Module '{}' already exists at {}",
                name,
                module_dir.display()
            ),
            serde_json::json!({ "path": module_dir.display().to_string() }),
        ));
        anyhow::bail!(
            "Module '{}' already exists at {}",
            name,
            module_dir.display()
        );
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
                printer.emit(cfgd_core::output::error_doc(
                    name,
                    "duplicate_basename",
                    format!(
                        "Duplicate file basename '{}' — multiple files would overwrite each other in modules/{}/files/",
                        base, name
                    ),
                    serde_json::json!({ "basename": base, "name": name }),
                ));
                anyhow::bail!(
                    "Duplicate file basename '{}' — multiple files would overwrite each other in modules/{}/files/",
                    base,
                    name
                );
            }
        }
    }
    let copied = copy_files_to_dir(&file_list, &files_dir)?;
    let is_private = args.private;
    let file_entries: Vec<config::ModuleFileEntry> = copied
        .iter()
        .map(|(basename, target)| config::ModuleFileEntry {
            source: format!("files/{}", basename),
            target: target.display().to_string(),
            strategy: None,
            private: is_private,
            encryption: None,
        })
        .collect();
    if is_private {
        for (basename, _) in &copied {
            add_to_gitignore(&config_dir, &format!("modules/{}/files/{}", name, basename))?;
        }
    }

    // Build package entries
    let known = super::known_manager_names();
    let known_refs: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
    let package_entries: Vec<config::ModulePackageEntry> = pkg_list
        .iter()
        .map(|s| {
            let (mgr, pkg) = super::parse_package_flag(s, &known_refs);
            config::ModulePackageEntry {
                name: pkg,
                min_version: None,
                prefer: mgr.into_iter().collect(),
                deny: Vec::new(),
                aliases: std::collections::HashMap::new(),
                script: None,
                platforms: Vec::new(),
            }
        })
        .collect();

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
        },
        spec: config::ModuleSpec {
            depends: dep_list,
            packages: package_entries,
            files: file_entries,
            env: env_entries,
            aliases: alias_entries,
            scripts,
            system: std::collections::HashMap::new(),
        },
    };

    // Apply --set overrides
    if !sets.is_empty() {
        apply_module_sets(sets, &mut doc)?;
    }

    // Write
    save_module_document(&doc, &module_yaml_path)?;

    let summary_sec = printer.section(format!(
        "Created module '{}' at {}",
        name,
        module_dir.display()
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

    maybe_update_workflow(cli, printer)?;

    // Apply if requested
    let mut applied = false;
    if args.apply {
        printer.heading("Applying Module");

        let config_path = config_dir.join("cfgd.yaml");
        let cfg = config::load_config(&config_path)?;
        let registry = super::build_registry_with_config(Some(&cfg));
        let store = super::open_state_store(cli.state_dir.as_deref())?;

        let platform = cfgd_core::platform::Platform::detect();
        let mgr_map = super::managers_map(&registry);
        let cache_base = modules::default_module_cache_dir()?;
        let resolved_modules = modules::resolve_modules(
            std::slice::from_ref(name),
            &config_dir,
            &cache_base,
            &platform,
            &mgr_map,
            printer,
        )?;

        let resolved = config::ResolvedProfile {
            layers: Vec::new(),
            merged: config::MergedProfile::default(),
        };

        let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
        let plan = reconciler.plan(
            &resolved,
            Vec::new(),
            Vec::new(),
            resolved_modules,
            cfgd_core::reconciler::ReconcileContext::Apply,
        )?;

        let total = plan.total_actions();
        if total == 0 {
            printer.status_simple(Role::Info, "Nothing to do");
        } else {
            if !args.yes {
                super::display_plan_table(&plan, printer, None);
                printer.status_simple(Role::Info, format!("{} action(s) planned", total));
                let confirmed = printer
                    .prompt_confirm("Apply these changes?")
                    .unwrap_or(false);
                if !confirmed {
                    printer.status_simple(Role::Info, "Skipped — run 'cfgd apply' to apply later");
                    printer.emit(Doc::new().with_data(serde_json::json!({
                        "name": name,
                        "path": module_dir.display().to_string(),
                        "applied": false,
                    })));
                    return Ok(());
                }
            }

            let state_dir = cfgd_core::state::default_state_dir()
                .map_err(|e| anyhow::anyhow!("cannot determine state directory: {}", e))?;
            let _apply_lock = cfgd_core::acquire_apply_lock(&state_dir)?;

            let result = reconciler.apply(
                &plan,
                &resolved,
                &config_dir,
                printer,
                None,
                &[],
                cfgd_core::reconciler::ReconcileContext::Apply,
                false,
            )?;
            super::print_apply_result(&result, printer, None);
            applied = true;
        }
    }

    printer.emit(Doc::new().with_data(serde_json::json!({
        "name": name,
        "path": module_dir.display().to_string(),
        "applied": applied,
    })));

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
    printer.heading(format!("Update Module: {}", name));

    let config_dir = config_dir(cli);
    let (mut doc, module_yaml_path) = match load_module_document(&config_dir, name) {
        Ok(v) => v,
        Err(e) => {
            printer.emit(cfgd_core::output::error_doc(
                name,
                "not_found",
                e.to_string(),
                serde_json::json!({}),
            ));
            return Err(e);
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
            printer.status_simple(Role::Ok, format!("Added dependency: {}", dep));
            changes += 1;
        }
    }

    // Remove dependencies
    for dep in &remove_depends {
        let before = doc.spec.depends.len();
        doc.spec.depends.retain(|d| d != dep);
        if doc.spec.depends.len() < before {
            printer.status_simple(Role::Ok, format!("Removed dependency: {}", dep));
            changes += 1;
        } else {
            printer.status_simple(Role::Warn, format!("Dependency '{}' not found", dep));
        }
    }

    // Add packages
    let known = super::known_manager_names();
    let known_refs: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
    for pkg_str in &add_packages {
        let (mgr, pkg) = super::parse_package_flag(pkg_str, &known_refs);
        if doc.spec.packages.iter().any(|p| p.name == pkg) {
            printer.status_simple(Role::Info, format!("Package '{}' already in module", pkg));
            continue;
        }
        doc.spec.packages.push(config::ModulePackageEntry {
            name: pkg.clone(),
            min_version: None,
            prefer: mgr.into_iter().collect(),
            deny: Vec::new(),
            aliases: std::collections::HashMap::new(),
            script: None,
            platforms: Vec::new(),
        });
        printer.status_simple(Role::Ok, format!("Added package: {}", pkg));
        changes += 1;
    }

    // Remove packages (strip manager prefix if present, e.g. "brew:ripgrep" → "ripgrep")
    let known = super::known_manager_names();
    let known_refs: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
    for pkg in &remove_packages {
        let (_, canonical) = super::parse_package_flag(pkg, &known_refs);
        let before = doc.spec.packages.len();
        doc.spec.packages.retain(|p| p.name != canonical);
        if doc.spec.packages.len() < before {
            printer.status_simple(Role::Ok, format!("Removed package: {}", canonical));
            changes += 1;
        } else {
            printer.status_simple(
                Role::Warn,
                format!("Package '{}' not found in module", canonical),
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
                .ok_or_else(|| anyhow::anyhow!("Invalid file path: {}", source.display()))?
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
            source: format!("files/{}", basename),
            target: target.display().to_string(),
            strategy: None,
            private: args.private,
            encryption: None,
        });
        printer.status_simple(Role::Ok, format!("Added file: {}", target.display()));
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
            printer.status_simple(Role::Ok, format!("Removed file: {}", target));
            changes += 1;
        } else {
            printer.status_simple(Role::Warn, format!("File '{}' not found in module", target));
        }
    }

    // Add env vars
    for e in &add_env {
        let ev = cfgd_core::parse_env_var(e).map_err(|e| anyhow::anyhow!(e))?;
        cfgd_core::merge_env(&mut doc.spec.env, std::slice::from_ref(&ev));
        printer.status_simple(Role::Ok, format!("Set env: {}={}", ev.name, ev.value));
        changes += 1;
    }

    // Remove env vars
    for key in &remove_env {
        let before = doc.spec.env.len();
        doc.spec.env.retain(|ev| ev.name != *key);
        if doc.spec.env.len() < before {
            printer.status_simple(Role::Ok, format!("Removed env: {}", key));
            changes += 1;
        } else {
            printer.status_simple(Role::Warn, format!("Env var '{}' not found", key));
        }
    }

    // Add aliases
    for a in &add_aliases {
        let alias = cfgd_core::parse_alias(a).map_err(|e| anyhow::anyhow!(e))?;
        cfgd_core::merge_aliases(&mut doc.spec.aliases, std::slice::from_ref(&alias));
        printer.status_simple(
            Role::Ok,
            format!("Set alias: {}={}", alias.name, alias.command),
        );
        changes += 1;
    }

    // Remove aliases
    for name in &remove_aliases {
        let before = doc.spec.aliases.len();
        doc.spec.aliases.retain(|a| a.name != *name);
        if doc.spec.aliases.len() < before {
            printer.status_simple(Role::Ok, format!("Removed alias: {}", name));
            changes += 1;
        } else {
            printer.status_simple(Role::Warn, format!("Alias '{}' not found", name));
        }
    }

    // Add post-apply scripts
    for script in &add_post_apply {
        let scripts = doc
            .spec
            .scripts
            .get_or_insert_with(config::ScriptSpec::default);
        let entry = config::ScriptEntry::Simple(script.clone());
        if !scripts.post_apply.contains(&entry) {
            scripts.post_apply.push(entry);
            printer.status_simple(Role::Ok, format!("Added post-apply script: {}", script));
            changes += 1;
        }
    }

    // Remove post-apply scripts
    for script in &remove_post_apply {
        if let Some(ref mut scripts) = doc.spec.scripts {
            let before = scripts.post_apply.len();
            scripts.post_apply.retain(|e| e.run_str() != script);
            if scripts.post_apply.len() < before {
                printer.status_simple(Role::Ok, format!("Removed post-apply script: {}", script));
                changes += 1;
            } else {
                printer.status_simple(Role::Warn, format!("Script '{}' not found", script));
            }
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
                format!("Updated module '{}' ({} change(s))", name, changes),
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
        printer.emit(cfgd_core::output::error_doc(
            name,
            "not_found",
            format!("Module '{}' not found at {}", name, module_yaml.display()),
            serde_json::json!({ "path": module_yaml.display().to_string() }),
        ));
        anyhow::bail!("Module '{}' not found at {}", name, module_yaml.display());
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
                printer.status_simple(Role::Fail, format!("Module '{}' has errors: {}", name, e));
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
) -> anyhow::Result<()> {
    validate_resource_name(name, "Module")?;
    printer.heading(format!("Delete Module: {}", name));

    let config_dir = config_dir(cli);
    let module_dir = config_dir.join("modules").join(name);

    if !module_dir.exists() {
        printer.emit(cfgd_core::output::error_doc(
            name,
            "not_found",
            format!("Module '{}' not found at {}", name, module_dir.display()),
            serde_json::json!({ "path": module_dir.display().to_string() }),
        ));
        anyhow::bail!("Module '{}' not found at {}", name, module_dir.display());
    }

    // Safety: refuse if any profile references this module
    let referencing = profiles_using_module(&profiles_dir(cli), name)?;
    if !referencing.is_empty() {
        printer.emit(cfgd_core::output::error_doc(
            name,
            "in_use",
            format!(
                "Cannot delete module '{}' — referenced by profile(s): {}. Remove it from those profiles first.",
                name,
                referencing.join(", ")
            ),
            serde_json::json!({ "profiles": &referencing }),
        ));
        anyhow::bail!(
            "Cannot delete module '{}' — referenced by profile(s): {}. Remove it from those profiles first.",
            name,
            referencing.join(", ")
        );
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
            let purge_sec = printer.section("Purging files");
            for file_entry in &doc.spec.files {
                let target = cfgd_core::expand_tilde(std::path::Path::new(&file_entry.target));
                if target.is_symlink() || target.exists() {
                    if target.is_dir() && !target.is_symlink() {
                        std::fs::remove_dir_all(&target)?;
                    } else {
                        std::fs::remove_file(&target)?;
                    }
                    purge_sec.status_simple(Role::Info, format!("Purged {}", target.display()));
                    files_processed += 1;
                }
            }
            drop(purge_sec);
        } else {
            // Default: restore symlinked files before deleting the module directory.
            // When module create adopts files, it moves them into the module dir and
            // symlinks the original location back. On delete, we reverse that.
            let restore_sec = printer.section("Restoring files");
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
                    restore_sec.status_simple(Role::Info, format!("Restored {}", target.display()));
                    files_processed += 1;
                }
            }
            drop(restore_sec);
        }
    }

    // Delete directory
    std::fs::remove_dir_all(&module_dir)?;

    // Clean module state from DB
    if let Ok(state) = open_state_store(cli.state_dir.as_deref())
        && let Err(e) = state.remove_module_state(name)
    {
        printer.status_simple(Role::Warn, format!("Failed to clean module state: {}", e));
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

    maybe_update_workflow(cli, printer)?;

    Ok(())
}

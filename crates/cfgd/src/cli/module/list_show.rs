use super::*;

pub(crate) fn cmd_module_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, printer)?;
    let lockfile = modules::load_lockfile(&config_dir)?;

    if all_modules.is_empty() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<ModuleListEntry>::new());
            return Ok(());
        }
        printer.header("Modules");
        printer.info("No modules found");
        printer.info(&format!("Add modules to {}/modules/", config_dir.display()));
        return Ok(());
    }

    // Load profile to determine which modules are active
    let active_modules: Vec<String> = if cli.config.exists() {
        let (_, resolved) = load_config_and_profile(cli, printer)?;
        if !printer.is_structured() {
            printer.newline();
        }
        resolved.merged.modules
    } else {
        Vec::new()
    };

    // Load module state from DB
    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_map = module_state_map(&state);

    let mut names: Vec<String> = all_modules.keys().cloned().collect();
    names.sort();

    let entries: Vec<ModuleListEntry> = names
        .iter()
        .map(|name| {
            let module = &all_modules[name];
            let in_profile = active_modules
                .iter()
                .any(|r| modules::resolve_profile_module_name(r) == name);
            let status = if let Some(state_rec) = state_map.get(name) {
                state_rec.status.clone()
            } else if in_profile {
                "pending".to_string()
            } else {
                "available".to_string()
            };
            let source_type = if lockfile.modules.iter().any(|e| e.name == *name) {
                "remote"
            } else {
                "local"
            };
            ModuleListEntry {
                name: name.clone(),
                active: in_profile,
                source: source_type.to_string(),
                status,
                packages: module.spec.packages.len(),
                files: module.spec.files.len(),
                depends: module.spec.depends.len(),
            }
        })
        .collect();

    if printer.write_structured(&entries) {
        return Ok(());
    }

    printer.header("Modules");

    if printer.is_wide() {
        let rows: Vec<Vec<String>> = entries
            .iter()
            .map(|e| {
                vec![
                    e.name.clone(),
                    if e.active { "yes" } else { "-" }.to_string(),
                    e.source.clone(),
                    e.status.clone(),
                    e.packages.to_string(),
                    e.files.to_string(),
                    e.depends.to_string(),
                ]
            })
            .collect();
        printer.table(
            &[
                "Module", "Active", "Source", "Status", "Packages", "Files", "Deps",
            ],
            &rows,
        );
    } else {
        let rows: Vec<Vec<String>> = entries
            .iter()
            .map(|e| {
                vec![
                    e.name.clone(),
                    if e.active { "yes" } else { "-" }.to_string(),
                    e.source.clone(),
                    e.status.clone(),
                    format!("{} pkgs, {} files, {} deps", e.packages, e.files, e.depends),
                ]
            })
            .collect();
        printer.table(&["Module", "Active", "Source", "Status", "Contents"], &rows);
    }

    Ok(())
}

pub(crate) fn cmd_module_show(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    show_values: bool,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, printer)?;

    let module = match all_modules.get(name) {
        Some(m) => m,
        None => {
            let available: Vec<&String> = all_modules.keys().collect();
            if !available.is_empty() {
                let mut sorted: Vec<&str> = available.iter().map(|s| s.as_str()).collect();
                sorted.sort();
                printer.info(&format!("Available modules: {}", sorted.join(", ")));
            }
            anyhow::bail!("Module '{}' not found", name);
        }
    };

    if printer.is_structured() {
        let lockfile = modules::load_lockfile(&config_dir)?;
        let source_type = if lockfile.modules.iter().any(|e| e.name == name) {
            "remote"
        } else {
            "local"
        };
        let state = open_state_store(cli.state_dir.as_deref())?;
        let state_rec = state.module_state_by_name(name)?;
        let output = ModuleShowOutput {
            name: name.to_string(),
            directory: module.dir.display().to_string(),
            source: source_type.to_string(),
            depends: module.spec.depends.clone(),
            state: state_rec,
            spec: module.spec.clone(),
        };
        printer.write_structured(&output);
        return Ok(());
    }

    printer.header(&format!("Module: {}", name));

    // Basic info
    if !module.spec.depends.is_empty() {
        printer.key_value("Dependencies", &module.spec.depends.join(", "));
    }
    printer.key_value("Directory", &module.dir.display().to_string());

    // Lockfile info for remote modules
    let lockfile = modules::load_lockfile(&config_dir)?;
    if let Some(lock_entry) = lockfile.modules.iter().find(|e| e.name == name) {
        printer.key_value("Source", "remote (locked)");
        printer.key_value("URL", &lock_entry.url);
        printer.key_value("Pinned ref", &lock_entry.pinned_ref);
        printer.key_value("Commit", &lock_entry.commit);
        printer.key_value("Integrity", &lock_entry.integrity);
    } else {
        printer.key_value("Source", "local");
    }

    // Module state from DB
    let state = open_state_store(cli.state_dir.as_deref())?;
    if let Some(state_rec) = state.module_state_by_name(name)? {
        printer.key_value("Status", &state_rec.status);
        printer.key_value("Last applied", &state_rec.installed_at);
        printer.key_value("Packages hash", &state_rec.packages_hash);
        printer.key_value("Files hash", &state_rec.files_hash);
    }

    // Packages
    if !module.spec.packages.is_empty() {
        printer.newline();
        printer.subheader("Packages");

        // Try to resolve packages to show which manager would be used
        let registry = build_registry();
        let mgr_map = managers_map(&registry);
        let platform = Platform::detect();

        for entry in &module.spec.packages {
            let prefer_str = if entry.prefer.is_empty() {
                String::new()
            } else {
                format!(" (prefer: {})", entry.prefer.join(", "))
            };
            let version_str = entry
                .min_version
                .as_ref()
                .map(|v| format!(", min: {}", v))
                .unwrap_or_default();
            let alias_str = if entry.aliases.is_empty() {
                String::new()
            } else {
                let aliases: Vec<String> = entry
                    .aliases
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect();
                format!(", aliases: {}", aliases.join(", "))
            };
            let platform_str = if entry.platforms.is_empty() {
                String::new()
            } else {
                format!(", platforms: {}", entry.platforms.join("/"))
            };

            // Try resolution
            match modules::resolve_package(entry, name, &platform, &mgr_map) {
                Ok(Some(resolved)) => {
                    let ver = resolved
                        .version
                        .as_ref()
                        .map(|v| format!(" ({})", v))
                        .unwrap_or_default();
                    printer.success(&format!(
                        "{} -> {} install {}{}",
                        entry.name, resolved.manager, resolved.resolved_name, ver
                    ));
                }
                Ok(None) => {
                    printer.info(&format!(
                        "{}{} — skipped (platform filter)",
                        entry.name, platform_str
                    ));
                }
                Err(e) => {
                    printer.warning(&format!(
                        "{}{}{}{}{} — unresolved: {}",
                        entry.name, prefer_str, version_str, alias_str, platform_str, e
                    ));
                }
            }
        }
    }

    // Files
    if !module.spec.files.is_empty() {
        printer.newline();
        printer.subheader("Files");
        for file in &module.spec.files {
            let git_indicator = if modules::is_git_source(&file.source) {
                " (git)"
            } else {
                ""
            };
            printer.key_value(&format!("{}{}", file.source, git_indicator), &file.target);
        }
    }

    // Env
    if !module.spec.env.is_empty() {
        printer.newline();
        printer.subheader("Env");
        for ev in &module.spec.env {
            if show_values {
                printer.key_value(&ev.name, &ev.value);
            } else {
                printer.key_value(&ev.name, &mask_value(&ev.value));
            }
        }
    }

    // Aliases
    if !module.spec.aliases.is_empty() {
        printer.newline();
        printer.subheader("Aliases");
        for alias in &module.spec.aliases {
            printer.key_value(&alias.name, &alias.command);
        }
    }

    // Scripts
    if let Some(ref scripts) = module.spec.scripts
        && !scripts.post_apply.is_empty()
    {
        printer.newline();
        printer.subheader("Post-apply Scripts");
        for script in &scripts.post_apply {
            printer.info(&format!("  {}", script));
        }
    }

    Ok(())
}

// --- Module CRUD helpers ---

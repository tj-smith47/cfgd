use std::path::{Path, PathBuf};

use super::*;

pub(super) fn cmd_module_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Modules");

    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base)?;
    let lockfile = modules::load_lockfile(&config_dir)?;

    if all_modules.is_empty() {
        printer.info("No modules found");
        printer.info(&format!("Add modules to {}/modules/", config_dir.display()));
        return Ok(());
    }

    // Load profile to determine which modules are active
    let active_modules: Vec<String> = if cli.config.exists() {
        let (_, resolved) = load_config_and_profile(cli, printer)?;
        printer.newline();
        resolved.merged.modules
    } else {
        Vec::new()
    };

    // Load module state from DB
    let state = open_state_store()?;
    let state_map = module_state_map(&state);

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut names: Vec<String> = all_modules.keys().cloned().collect();
    names.sort();

    for name in &names {
        let module = &all_modules[name];
        let in_profile = active_modules
            .iter()
            .any(|r| modules::resolve_profile_module_name(r) == name);
        let pkg_count = module.spec.packages.len();
        let file_count = module.spec.files.len();
        let dep_count = module.spec.depends.len();

        let status = if let Some(state_rec) = state_map.get(name) {
            state_rec.status.clone()
        } else if in_profile {
            "pending".to_string()
        } else {
            "available".to_string()
        };

        let profile_indicator = if in_profile { "yes" } else { "-" };
        let source_indicator = if lockfile.modules.iter().any(|e| e.name == *name) {
            "remote"
        } else {
            "local"
        };

        rows.push(vec![
            name.clone(),
            profile_indicator.to_string(),
            source_indicator.to_string(),
            status,
            format!(
                "{} pkgs, {} files, {} deps",
                pkg_count, file_count, dep_count
            ),
        ]);
    }

    printer.table(&["Module", "Active", "Source", "Status", "Contents"], &rows);

    Ok(())
}

pub(super) fn cmd_module_show(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    printer.header(&format!("Module: {}", name));

    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base)?;

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
    let state = open_state_store()?;
    if let Some(state_rec) = state.module_state_by_name(name)? {
        printer.key_value("Status", &state_rec.status);
        printer.key_value("Installed at", &state_rec.installed_at);
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

pub(super) fn load_module_document(
    config_dir: &Path,
    module_name: &str,
) -> anyhow::Result<(config::ModuleDocument, PathBuf)> {
    let module_dir = config_dir.join("modules").join(module_name);
    let module_yaml = module_dir.join("module.yaml");
    if !module_yaml.exists() {
        anyhow::bail!(
            "Module '{}' not found at {}",
            module_name,
            module_yaml.display()
        );
    }
    let contents = std::fs::read_to_string(&module_yaml)?;
    let doc = config::parse_module(&contents)?;
    Ok((doc, module_yaml))
}

fn save_module_document(doc: &config::ModuleDocument, path: &Path) -> anyhow::Result<()> {
    let yaml = serde_yaml::to_string(doc)?;
    std::fs::write(path, &yaml)?;
    Ok(())
}

pub(super) fn profiles_using_module(
    profiles_dir: &Path,
    module_name: &str,
) -> anyhow::Result<Vec<String>> {
    let mut result = Vec::new();
    if !profiles_dir.exists() {
        return Ok(result);
    }
    for entry in std::fs::read_dir(profiles_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "yaml" || e == "yml")
            && let Ok(doc) = config::load_profile(&path)
            && doc.spec.modules.contains(&module_name.to_string())
        {
            result.push(doc.metadata.name.clone());
        }
    }
    Ok(result)
}

/// Parse helm-style `--set` overrides and apply them to a ModuleDocument.
/// Supported paths:
///   package.<name>.min-version=<value>
///   package.<name>.prefer=<a>,<b>,<c>
///   package.<name>.alias.<manager>=<alias>
///   package.<name>.platforms=<a>,<b>
///   package.<name>.script=<value>
pub(super) fn apply_module_sets(
    sets: &[String],
    doc: &mut config::ModuleDocument,
) -> anyhow::Result<()> {
    for set_str in sets {
        let (path, value) = set_str.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("Invalid --set format '{}' — expected key=value", set_str)
        })?;

        let parts: Vec<&str> = path.split('.').collect();
        if parts.len() < 3 || parts[0] != "package" || parts[1].is_empty() || parts[2].is_empty() {
            anyhow::bail!(
                "Invalid --set path '{}' — expected package.<name>.<field>[.<subfield>]",
                path
            );
        }

        let pkg_name = parts[1];
        let field = parts[2];

        let pkg = doc
            .spec
            .packages
            .iter_mut()
            .find(|p| p.name == pkg_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Package '{}' not found in module — add it with --package first",
                    pkg_name
                )
            })?;

        match field {
            "min-version" => {
                pkg.min_version = Some(value.to_string());
            }
            "prefer" => {
                pkg.prefer = value.split(',').map(|s| s.trim().to_string()).collect();
            }
            "platforms" => {
                pkg.platforms = value.split(',').map(|s| s.trim().to_string()).collect();
            }
            "script" => {
                pkg.script = Some(value.to_string());
            }
            "alias" => {
                if parts.len() < 4 {
                    anyhow::bail!(
                        "Invalid alias path '{}' — expected package.<name>.alias.<manager>=<alias>",
                        path
                    );
                }
                let manager = parts[3];
                pkg.aliases.insert(manager.to_string(), value.to_string());
            }
            _ => {
                anyhow::bail!(
                    "Unknown package field '{}' — valid fields: min-version, prefer, platforms, script, alias",
                    field
                );
            }
        }
    }
    Ok(())
}

// --- Module Create ---

pub(super) fn cmd_module_create(
    cli: &Cli,
    printer: &Printer,
    args: &ModuleCreateArgs,
) -> anyhow::Result<()> {
    let name = &args.name;
    let description = args.description.as_deref();
    let depends = &args.depends;
    let pkg_names = &args.packages;
    let files = &args.files;
    let post_apply = &args.post_apply;
    let sets = &args.sets;
    validate_resource_name(name, "Module")?;
    printer.header(&format!("Create Module: {}", name));

    let config_dir = config_dir(cli);
    let module_dir = config_dir.join("modules").join(name);
    let module_yaml_path = module_dir.join("module.yaml");

    if module_yaml_path.exists() {
        anyhow::bail!(
            "Module '{}' already exists at {}",
            name,
            module_dir.display()
        );
    }

    // Interactive mode if no content flags provided
    let is_interactive = description.is_none()
        && depends.is_empty()
        && pkg_names.is_empty()
        && files.is_empty()
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
        })
        .collect();
    if is_private {
        for (basename, _) in &copied {
            add_to_gitignore(&config_dir, &format!("modules/{}/files/{}", name, basename))?;
        }
    }

    // Build package entries
    let package_entries: Vec<config::ModulePackageEntry> = pkg_list
        .iter()
        .map(|name| config::ModulePackageEntry {
            name: name.clone(),
            min_version: None,
            prefer: Vec::new(),
            aliases: std::collections::HashMap::new(),
            script: None,
            platforms: Vec::new(),
        })
        .collect();

    // Build scripts
    let scripts = if post_apply_list.is_empty() {
        None
    } else {
        Some(config::ModuleScriptSpec {
            post_apply: post_apply_list,
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
            scripts,
        },
    };

    // Apply --set overrides
    if !sets.is_empty() {
        apply_module_sets(sets, &mut doc)?;
    }

    // Write
    save_module_document(&doc, &module_yaml_path)?;

    printer.success(&format!(
        "Created module '{}' at {}",
        name,
        module_dir.display()
    ));
    if !doc.spec.packages.is_empty() {
        printer.key_value(
            "Packages",
            &doc.spec
                .packages
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if !doc.spec.files.is_empty() {
        printer.key_value("Files", &doc.spec.files.len().to_string());
    }
    if !doc.spec.depends.is_empty() {
        printer.key_value("Dependencies", &doc.spec.depends.join(", "));
    }
    printer.newline();
    printer.info("Add to a profile with: cfgd profile update <profile> --add-module <name>");
    printer.info("Fine-tune with: cfgd module edit <name>");

    maybe_update_workflow(cli, printer)?;

    Ok(())
}

// --- Module Update (local) ---

pub(super) fn cmd_module_update_local(
    cli: &Cli,
    printer: &Printer,
    args: &ModuleUpdateArgs,
) -> anyhow::Result<()> {
    let name = &args.name;
    let add_packages = &args.add_packages;
    let remove_packages = &args.remove_packages;
    let add_files = &args.add_files;
    let remove_files = &args.remove_files;
    let add_depends = &args.add_depends;
    let remove_depends = &args.remove_depends;
    let description = args.description.as_deref();
    let add_post_apply = &args.add_post_apply;
    let remove_post_apply = &args.remove_post_apply;
    let sets = &args.sets;
    validate_resource_name(name, "Module")?;
    printer.header(&format!("Update Module: {}", name));

    let config_dir = config_dir(cli);
    let (mut doc, module_yaml_path) = load_module_document(&config_dir, name)?;
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
    for dep in add_depends {
        if !doc.spec.depends.contains(dep) {
            doc.spec.depends.push(dep.clone());
            printer.success(&format!("Added dependency: {}", dep));
            changes += 1;
        }
    }

    // Remove dependencies
    for dep in remove_depends {
        let before = doc.spec.depends.len();
        doc.spec.depends.retain(|d| d != dep);
        if doc.spec.depends.len() < before {
            printer.success(&format!("Removed dependency: {}", dep));
            changes += 1;
        } else {
            printer.warning(&format!("Dependency '{}' not found", dep));
        }
    }

    // Add packages
    for pkg in add_packages {
        if doc.spec.packages.iter().any(|p| p.name == *pkg) {
            printer.info(&format!("Package '{}' already in module", pkg));
            continue;
        }
        doc.spec.packages.push(config::ModulePackageEntry {
            name: pkg.clone(),
            min_version: None,
            prefer: Vec::new(),
            aliases: std::collections::HashMap::new(),
            script: None,
            platforms: Vec::new(),
        });
        printer.success(&format!("Added package: {}", pkg));
        changes += 1;
    }

    // Remove packages
    for pkg in remove_packages {
        let before = doc.spec.packages.len();
        doc.spec.packages.retain(|p| p.name != *pkg);
        if doc.spec.packages.len() < before {
            printer.success(&format!("Removed package: {}", pkg));
            changes += 1;
        } else {
            printer.warning(&format!("Package '{}' not found in module", pkg));
        }
    }

    // Add files (detect basename collisions within the batch, skip already-tracked)
    let mut files_to_copy: Vec<String> = Vec::new();
    {
        let mut added_basenames = std::collections::HashSet::new();
        for spec in add_files {
            let (source, _) = parse_file_spec(spec)?;
            let basename = source
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Invalid file path: {}", source.display()))?
                .to_string_lossy()
                .to_string();

            let source_key = format!("files/{}", basename);
            if doc.spec.files.iter().any(|f| f.source == source_key) {
                printer.info(&format!("File '{}' already in module", basename));
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
        });
        printer.success(&format!("Added file: {}", target.display()));
        changes += 1;
    }

    // Remove files
    for target in remove_files {
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
            printer.success(&format!("Removed file: {}", target));
            changes += 1;
        } else {
            printer.warning(&format!("File '{}' not found in module", target));
        }
    }

    // Add post-apply scripts
    for script in add_post_apply {
        let scripts = doc
            .spec
            .scripts
            .get_or_insert_with(config::ModuleScriptSpec::default);
        if !scripts.post_apply.contains(script) {
            scripts.post_apply.push(script.clone());
            printer.success(&format!("Added post-apply script: {}", script));
            changes += 1;
        }
    }

    // Remove post-apply scripts
    for script in remove_post_apply {
        if let Some(ref mut scripts) = doc.spec.scripts {
            let before = scripts.post_apply.len();
            scripts.post_apply.retain(|s| s != script);
            if scripts.post_apply.len() < before {
                printer.success(&format!("Removed post-apply script: {}", script));
                changes += 1;
            } else {
                printer.warning(&format!("Script '{}' not found", script));
            }
        }
    }

    // Apply --set overrides
    if !sets.is_empty() {
        apply_module_sets(sets, &mut doc)?;
        changes += sets.len() as u32;
    }

    if changes == 0 {
        printer.info("No changes specified");
        return Ok(());
    }

    save_module_document(&doc, &module_yaml_path)?;
    printer.newline();
    printer.success(&format!(
        "Updated module '{}' ({} change(s))",
        name, changes
    ));

    Ok(())
}

// --- Module Edit ---

pub(super) fn cmd_module_edit(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    validate_resource_name(name, "Module")?;
    let config_dir = config_dir(cli);
    let module_yaml = config_dir.join("modules").join(name).join("module.yaml");

    if !module_yaml.exists() {
        anyhow::bail!("Module '{}' not found at {}", name, module_yaml.display());
    }

    open_in_editor(&module_yaml, printer)?;

    // Validate after editing — loop until valid or user cancels
    loop {
        let contents = std::fs::read_to_string(&module_yaml)?;
        match config::parse_module(&contents) {
            Ok(_) => {
                printer.success(&format!("Module '{}' is valid", name));
                break;
            }
            Err(e) => {
                printer.error(&format!("Module '{}' has errors: {}", name, e));
                if !printer.prompt_confirm("Re-open in editor?")? {
                    printer.warning("Saved with validation errors");
                    break;
                }
                open_in_editor(&module_yaml, printer)?;
            }
        }
    }

    Ok(())
}

// --- Module Delete ---

pub(super) fn cmd_module_delete(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    yes: bool,
) -> anyhow::Result<()> {
    validate_resource_name(name, "Module")?;
    printer.header(&format!("Delete Module: {}", name));

    let config_dir = config_dir(cli);
    let module_dir = config_dir.join("modules").join(name);

    if !module_dir.exists() {
        anyhow::bail!("Module '{}' not found at {}", name, module_dir.display());
    }

    // Safety: refuse if any profile references this module
    let referencing = profiles_using_module(&profiles_dir(cli), name)?;
    if !referencing.is_empty() {
        anyhow::bail!(
            "Cannot delete module '{}' — referenced by profile(s): {}. Remove it from those profiles first.",
            name,
            referencing.join(", ")
        );
    }

    if !yes && !printer.prompt_confirm(&format!("Delete module '{}'?", name))? {
        printer.info("Cancelled");
        return Ok(());
    }

    // Delete directory
    std::fs::remove_dir_all(&module_dir)?;

    // Clean module state from DB
    if let Ok(state) = open_state_store()
        && let Err(e) = state.remove_module_state(name)
    {
        printer.warning(&format!("Failed to clean module state: {}", e));
    }

    // Clean from lockfile if present
    let mut lockfile = modules::load_lockfile(&config_dir)?;
    let had_lock = lockfile.modules.iter().any(|e| e.name == name);
    if had_lock {
        lockfile.modules.retain(|e| e.name != name);
        modules::save_lockfile(&config_dir, &lockfile)?;
        printer.info(&format!("Removed '{}' from modules.lock", name));
    }

    printer.success(&format!("Deleted module '{}'", name));

    maybe_update_workflow(cli, printer)?;

    Ok(())
}

pub(super) fn cmd_module_add_from_registry(
    cli: &Cli,
    printer: &Printer,
    reference: &str,
    yes: bool,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    let reg_ref = modules::parse_registry_ref(reference).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid registry reference '{}' — expected registry/module[@tag]",
            reference
        )
    })?;

    // Load config to find the registry
    if !cli.config.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }
    let cfg = config::load_config(&cli.config)?;

    let registries = cfg
        .spec
        .modules
        .as_ref()
        .map(|m| &m.registries[..])
        .unwrap_or(&[]);
    let registry_entry = registries
        .iter()
        .find(|s| s.name == reg_ref.registry)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Registry '{}' not configured — run 'cfgd module registry add <url>' first",
                reg_ref.registry
            )
        })?;

    // Resolve the tag — use explicit tag or find latest
    let cache_base = modules::default_module_cache_dir()?;
    let tag = match reg_ref.tag {
        Some(t) => t,
        None => {
            printer.info(&format!(
                "No tag specified — looking up latest for '{}'",
                reg_ref.module
            ));
            // Fetch the registry repo so we can read tags
            modules::fetch_registry_modules(registry_entry, &cache_base)?;
            modules::latest_module_version(registry_entry, &reg_ref.module, &cache_base)?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No tags found for module '{}' in registry '{}'",
                        reg_ref.module,
                        reg_ref.registry
                    )
                })?
        }
    };

    // Build the full git URL with subdir and delegate to cmd_module_add_remote
    // Subdir follows prescribed registry layout: modules/<name>
    let subdir = format!("modules/{}", reg_ref.module);
    let full_url = format!(
        "{}//{}@{}/{}",
        registry_entry.url, subdir, reg_ref.module, tag
    );
    printer.info(&format!(
        "Resolved: {}/{} -> {}",
        reg_ref.registry, reg_ref.module, full_url
    ));

    cmd_module_add_remote(
        cli,
        printer,
        &full_url,
        Some(&reg_ref.registry),
        yes,
        allow_unsigned,
    )
}

pub(super) fn cmd_module_add_remote(
    cli: &Cli,
    printer: &Printer,
    url: &str,
    source_name: Option<&str>,
    yes: bool,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    printer.header("Add Remote Module");

    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;

    // Fetch the remote module (validates pinned ref)
    printer.info(&format!("Fetching {}", url));
    let fetched = modules::fetch_remote_module(url, &cache_base)?;
    let module_name = fetched.module.name.clone();
    let module = fetched.module;
    let commit = fetched.commit;
    let integrity = fetched.integrity;

    // Check if already in lockfile
    let mut lockfile = modules::load_lockfile(&config_dir)?;
    if lockfile.modules.iter().any(|e| e.name == module_name) {
        printer.info(&format!(
            "Module '{}' is already in the lockfile — use 'cfgd module update {}' to change versions",
            module_name, module_name
        ));
        return Ok(());
    }

    // Check if a local module with the same name exists
    let local_modules = modules::load_modules(&config_dir)?;
    if local_modules.contains_key(&module_name) {
        anyhow::bail!(
            "Local module '{}' already exists in {}/modules/ — local modules take precedence over remote",
            module_name,
            config_dir.display()
        );
    }

    // Display the module spec for review
    printer.newline();
    printer.subheader(&format!("Module: {}", module_name));

    if !module.spec.depends.is_empty() {
        printer.key_value("Dependencies", &module.spec.depends.join(", "));
    }

    if !module.spec.packages.is_empty() {
        printer.newline();
        printer.info(&format!("Packages ({}):", module.spec.packages.len()));
        for pkg in &module.spec.packages {
            let ver = pkg
                .min_version
                .as_ref()
                .map(|v| format!(" (min: {})", v))
                .unwrap_or_default();
            printer.info(&format!("  {} {}{}", "+", pkg.name, ver));
        }
    }

    if !module.spec.files.is_empty() {
        printer.newline();
        printer.info(&format!("Files ({}):", module.spec.files.len()));
        for file in &module.spec.files {
            printer.info(&format!("  {} -> {}", file.source, file.target));
        }
    }

    if let Some(ref scripts) = module.spec.scripts
        && !scripts.post_apply.is_empty()
    {
        printer.newline();
        printer.warning(&format!(
            "Post-apply scripts ({}) — these will execute on your machine:",
            scripts.post_apply.len()
        ));
        for script in &scripts.post_apply {
            printer.warning(&format!("  $ {}", script));
        }
    }

    printer.newline();
    printer.key_value("Commit", &commit);
    printer.key_value("Integrity", &integrity);

    // Check for GPG/SSH signature on the tag
    let git_src = modules::parse_git_source(url)?;
    enforce_signature_policy(
        cli,
        printer,
        git_src.tag.as_deref(),
        &module_name,
        allow_unsigned,
        &cache_base,
        &git_src.repo_url,
    )?;

    // Confirm
    printer.newline();
    if !yes && !printer.prompt_confirm("Add this remote module?")? {
        printer.info("Cancelled");
        return Ok(());
    }

    // Build clean lockfile URL: repo@tag (no //subdir — subdir is a separate field)
    let lock_url = if git_src.subdir.is_some() {
        let mut clean = git_src.repo_url.clone();
        if let Some(ref t) = git_src.tag {
            clean = format!("{}@{}", clean, t);
        } else if let Some(ref r) = git_src.git_ref {
            clean = format!("{}?ref={}", clean, r);
        }
        clean
    } else {
        url.to_string()
    };

    let pinned_ref = git_src
        .tag
        .or(git_src.git_ref)
        .unwrap_or_else(|| commit.clone());

    let lock_entry = cfgd_core::config::ModuleLockEntry {
        name: module_name.clone(),
        url: lock_url,
        pinned_ref,
        commit,
        integrity,
        subdir: git_src.subdir,
    };
    lockfile.modules.push(lock_entry);
    modules::save_lockfile(&config_dir, &lockfile)?;

    // Add to profile if config exists — use registry/module format for registry-backed modules
    let profile_module_ref = match source_name {
        Some(src) => format!("{}/{}", src, module_name),
        None => module_name.clone(),
    };
    if cli.config.exists() {
        let cfg = config::load_config(&cli.config)?;
        let profile_name = match cli.profile.as_deref() {
            Some(p) => p,
            None => cfg.active_profile()?,
        };
        let profile_path = profiles_dir(cli).join(format!("{}.yaml", profile_name));

        if profile_path.exists() {
            let contents = std::fs::read_to_string(&profile_path)?;
            let mut doc: config::ProfileDocument = serde_yaml::from_str(&contents)?;

            if !doc.spec.modules.contains(&profile_module_ref) {
                doc.spec.modules.push(profile_module_ref.clone());
                let yaml = serde_yaml::to_string(&doc)?;
                std::fs::write(&profile_path, &yaml)?;
                printer.success(&format!(
                    "Added module '{}' to profile '{}'",
                    profile_module_ref, profile_name
                ));
            }
        }
    }

    printer.success(&format!(
        "Locked remote module '{}' in modules.lock",
        module_name
    ));
    printer.info(MSG_RUN_APPLY);

    Ok(())
}

pub(super) fn cmd_module_upgrade(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    new_ref: Option<&str>,
    yes: bool,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    printer.header(&format!("Update Module: {}", name));

    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;

    // Find the lockfile entry
    let mut lockfile = modules::load_lockfile(&config_dir)?;
    let entry_idx = lockfile.modules.iter().position(|e| e.name == name);

    let entry_idx = match entry_idx {
        Some(idx) => idx,
        None => {
            let local_modules = modules::load_modules(&config_dir)?;
            if local_modules.contains_key(name) {
                anyhow::bail!(
                    "Module '{}' is a local module — edit it directly in {}/modules/{}/",
                    name,
                    config_dir.display(),
                    name
                );
            } else {
                anyhow::bail!("Module '{}' not found", name);
            }
        }
    };

    let old_entry = lockfile.modules[entry_idx].clone();

    // Load the current module for diffing
    let old_git_src = modules::parse_git_source(&old_entry.url)?;
    let old_pinned_src = modules::GitSource {
        repo_url: old_git_src.repo_url.clone(),
        tag: Some(old_entry.pinned_ref.clone()),
        git_ref: None,
        subdir: old_entry.subdir.clone(),
    };
    let old_local_path = modules::fetch_git_source(&old_pinned_src, &cache_base, name)?;
    let old_module = modules::load_module(&old_local_path)?;

    // Build the new URL with the updated ref
    let new_ref = match new_ref {
        Some(r) => r.to_string(),
        None => {
            // Fetch latest from remote and use HEAD
            let git_src = modules::GitSource {
                repo_url: old_git_src.repo_url.clone(),
                tag: None,
                git_ref: None,
                subdir: None,
            };
            modules::fetch_git_source(&git_src, &cache_base, name)?;
            let repo_dir = modules::git_cache_dir(&cache_base, &old_git_src.repo_url);
            let head = modules::get_head_commit_sha(&repo_dir)?;
            printer.info(&format!("Latest commit: {}", head));
            head
        }
    };

    // Fetch the new version
    let new_src = modules::GitSource {
        repo_url: old_git_src.repo_url.clone(),
        tag: Some(new_ref.clone()),
        git_ref: None,
        subdir: old_entry.subdir.clone(),
    };
    let new_local_path = modules::fetch_git_source(&new_src, &cache_base, name)?;
    let new_module = modules::load_module(&new_local_path)?;
    let repo_dir = modules::git_cache_dir(&cache_base, &old_git_src.repo_url);
    let new_commit = modules::get_head_commit_sha(&repo_dir)?;
    let new_integrity = modules::hash_module_contents(&new_local_path)?;

    if new_commit == old_entry.commit {
        printer.info("Module is already at this version");
        return Ok(());
    }

    // Show diff
    printer.subheader("Changes");
    let changes = modules::diff_module_specs(&old_module, &new_module);
    for change in &changes {
        printer.info(&format!("  {}", change));
    }

    printer.newline();
    printer.key_value("Old commit", &old_entry.commit);
    printer.key_value("New commit", &new_commit);
    printer.key_value("New integrity", &new_integrity);

    // Check for signature on new ref
    enforce_signature_policy(
        cli,
        printer,
        Some(&new_ref),
        name,
        allow_unsigned,
        &cache_base,
        &old_git_src.repo_url,
    )?;

    // Confirm
    printer.newline();
    if !yes && !printer.prompt_confirm("Update this module?")? {
        printer.info("Cancelled");
        return Ok(());
    }

    // Update lockfile entry
    lockfile.modules[entry_idx] = cfgd_core::config::ModuleLockEntry {
        name: name.to_string(),
        url: old_entry.url.clone(),
        pinned_ref: new_ref,
        commit: new_commit,
        integrity: new_integrity,
        subdir: old_entry.subdir,
    };
    modules::save_lockfile(&config_dir, &lockfile)?;

    printer.success(&format!("Updated module '{}' in modules.lock", name));
    printer.info(MSG_RUN_APPLY);

    Ok(())
}

pub(super) fn cmd_module_search(cli: &Cli, printer: &Printer, query: &str) -> anyhow::Result<()> {
    printer.header(&format!("Search Modules: {}", query));

    let cache_base = modules::default_module_cache_dir()?;

    if !cli.config.exists() {
        anyhow::bail!("No config found — add module registries to cfgd.yaml first");
    }

    let cfg = config::load_config(&cli.config)?;
    let registries = cfg
        .spec
        .modules
        .as_ref()
        .map(|m| &m.registries[..])
        .unwrap_or(&[]);
    if registries.is_empty() {
        printer.info("No module registries configured");
        printer.info("Add a registry: cfgd module registry add <git-url>");
        return Ok(());
    }

    let mut found_any = false;

    for source in registries {
        printer.info(&format!("Searching {} ({})", source.name, source.url));

        match modules::fetch_registry_modules(source, &cache_base) {
            Ok(registry_modules) => {
                let query_lower = query.to_lowercase();
                let matches: Vec<&modules::RegistryModule> = registry_modules
                    .iter()
                    .filter(|m| m.name.to_lowercase().contains(&query_lower))
                    .collect();

                if matches.is_empty() {
                    printer.info("  No matches");
                    continue;
                }

                found_any = true;
                let rows: Vec<Vec<String>> = matches
                    .iter()
                    .map(|m| {
                        let latest = m.tags.last().cloned().unwrap_or_else(|| "-".into());
                        vec![
                            format!("{}/{}", m.registry, m.name),
                            m.description.clone(),
                            latest,
                            format!("{}", m.tags.len()),
                        ]
                    })
                    .collect();

                printer.table(&["Module", "Description", "Latest", "Tags"], &rows);
            }
            Err(e) => {
                printer.warning(&format!("  Failed to fetch source: {}", e));
            }
        }
    }

    if !found_any {
        printer.newline();
        printer.info("No modules found matching your query");
    }

    Ok(())
}

/// Cryptographically verify a git tag signature using `git tag -v`.
/// Returns `Ok(true)` if verified, `Ok(false)` if verification fails (bad sig),
/// or `Err` if `git` is not available or keyring is not configured.
fn verify_tag_signature_cryptographic(repo_dir: &Path, tag_name: &str) -> anyhow::Result<bool> {
    let output = std::process::Command::new("git")
        .args(["tag", "-v", tag_name])
        .current_dir(repo_dir)
        .output()?;

    if output.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Distinguish "bad signature" from "no keyring / gpg not found"
        if stderr.contains("BAD signature")
            || stderr.contains("BADSIG")
            || stderr.contains("verification failed")
        {
            Ok(false) // Signature present but invalid
        } else {
            // gpg not installed, key not in keyring, etc.
            anyhow::bail!("{}", stderr.trim())
        }
    }
}

/// Check tag signature and enforce require-signatures policy.
/// Loads `require_signatures` from config, checks the tag signature,
/// and bails if policy is violated. Returns Ok(()) if allowed to proceed.
fn enforce_signature_policy(
    cli: &Cli,
    printer: &Printer,
    tag: Option<&str>,
    module_name: &str,
    allow_unsigned: bool,
    cache_base: &Path,
    repo_url: &str,
) -> anyhow::Result<()> {
    let require_signatures = cli
        .config
        .exists()
        .then(|| config::load_config(&cli.config).ok())
        .flatten()
        .and_then(|c| c.spec.modules)
        .and_then(|m| m.security)
        .is_some_and(|s| s.require_signatures);

    let Some(tag) = tag else {
        if require_signatures && !allow_unsigned {
            anyhow::bail!(
                "Module '{}' has no tag — your config has require-signatures enabled. Pass --allow-unsigned to override.",
                module_name
            );
        }
        return Ok(());
    };

    let repo_dir = modules::git_cache_dir(cache_base, repo_url);
    let sig_status = modules::check_tag_signature(&repo_dir, tag, module_name);

    match &sig_status {
        Ok(modules::TagSignatureStatus::SignaturePresent) => {
            match verify_tag_signature_cryptographic(&repo_dir, tag) {
                Ok(true) => printer.success(&format!("Tag '{}' signature verified", tag)),
                Ok(false) => {
                    printer.error(&format!(
                        "Tag '{}' has an INVALID signature — refusing to proceed",
                        tag
                    ));
                    anyhow::bail!(
                        "Signature verification failed for tag '{}' — the signature is present but invalid",
                        tag
                    );
                }
                Err(e) => {
                    printer.warning(&format!(
                        "Tag '{}' has a signature but verification skipped: {}",
                        tag, e
                    ));
                }
            }
        }
        Ok(modules::TagSignatureStatus::Unsigned)
        | Ok(modules::TagSignatureStatus::LightweightTag) => {
            let label = match &sig_status {
                Ok(modules::TagSignatureStatus::LightweightTag) => {
                    format!("Tag '{}' is lightweight (cannot carry a signature)", tag)
                }
                _ => format!("Tag '{}' is unsigned", tag),
            };
            if require_signatures && !allow_unsigned {
                printer.error(&label);
                anyhow::bail!(
                    "Module '{}' tag '{}' is unsigned — your config has require-signatures enabled. Pass --allow-unsigned to override.",
                    module_name,
                    tag
                );
            }
            printer.info(&label);
        }
        Ok(modules::TagSignatureStatus::TagNotFound) => {
            printer.warning(&format!("Tag '{}' not found in repo", tag));
        }
        Err(e) => printer.warning(&format!("Signature check: {}", e)),
    }

    Ok(())
}

pub(super) fn cmd_module_registry_add(
    cli: &Cli,
    printer: &Printer,
    url: &str,
    name: Option<&str>,
) -> anyhow::Result<()> {
    printer.header("Add Module Registry");

    let registry_name = match name {
        Some(n) => n.to_string(),
        None => modules::extract_registry_name(url).ok_or_else(|| {
            anyhow::anyhow!("Cannot extract registry name from URL — use --name to specify one")
        })?,
    };

    if !cli.config.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    // Read and modify the raw YAML to preserve formatting
    let raw = std::fs::read_to_string(&cli.config)?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&raw)?;

    let spec = doc
        .get_mut("spec")
        .ok_or_else(|| anyhow::anyhow!("config has no spec"))?;

    // Ensure spec.modules.registries exists
    if spec.get("modules").is_none() {
        spec["modules"] = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let modules = spec
        .get_mut("modules")
        .ok_or_else(|| anyhow::anyhow!("failed to create modules section"))?;

    let registries = modules
        .get_mut("registries")
        .and_then(|v| v.as_sequence_mut());

    let new_entry = serde_yaml::to_value(cfgd_core::config::ModuleRegistryEntry {
        name: registry_name.clone(),
        url: url.to_string(),
    })?;

    if let Some(registries) = registries {
        for s in registries.iter() {
            if s.get("name").and_then(|v| v.as_str()) == Some(&registry_name) {
                printer.info(&format!("Registry '{}' already configured", registry_name));
                return Ok(());
            }
        }
        registries.push(new_entry);
    } else {
        modules["registries"] = serde_yaml::Value::Sequence(vec![new_entry]);
    }

    let yaml = serde_yaml::to_string(&doc)?;
    std::fs::write(&cli.config, &yaml)?;

    printer.success(&format!(
        "Added module registry '{}' ({})",
        registry_name, url
    ));
    printer.info("Search for modules: cfgd module search <query>");

    Ok(())
}

pub(super) fn cmd_module_registry_remove(
    cli: &Cli,
    printer: &Printer,
    name: &str,
) -> anyhow::Result<()> {
    printer.header("Remove Module Registry");

    if !cli.config.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let raw = std::fs::read_to_string(&cli.config)?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&raw)?;

    let registries = doc
        .get_mut("spec")
        .and_then(|s| s.get_mut("modules"))
        .and_then(|m| m.get_mut("registries"))
        .and_then(|v| v.as_sequence_mut());

    if let Some(registries) = registries {
        let before = registries.len();
        registries.retain(|s| s.get("name").and_then(|v| v.as_str()) != Some(name));
        if registries.len() < before {
            let yaml = serde_yaml::to_string(&doc)?;
            std::fs::write(&cli.config, &yaml)?;
            printer.success(&format!("Removed module registry '{}'", name));

            // Warn about profile references that now point to a removed registry
            let config_dir = cli.config.parent().unwrap_or_else(|| Path::new("."));
            let profiles_dir = config_dir.join("profiles");
            let old_prefix = format!("{}/", name);
            if profiles_dir.is_dir() {
                for entry in std::fs::read_dir(&profiles_dir)?.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "yaml" || e == "yml")
                        && let Ok(contents) = std::fs::read_to_string(&path)
                        && contents.contains(&old_prefix)
                    {
                        printer.warning(&format!(
                            "Profile '{}' still references '{}/...' — those modules will fail to resolve",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            name
                        ));
                    }
                }
            }
        } else {
            printer.info(&format!("Registry '{}' not found", name));
        }
    } else {
        printer.info("No module registries configured");
    }

    Ok(())
}

pub(super) fn cmd_module_registry_rename(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    new_name: &str,
) -> anyhow::Result<()> {
    if !cli.config.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    let cfg = config::load_config(&cli.config)?;
    let registries = cfg
        .spec
        .modules
        .as_ref()
        .map(|m| &m.registries[..])
        .unwrap_or(&[]);

    if !registries.iter().any(|s| s.name == name) {
        anyhow::bail!("Registry '{}' not found", name);
    }
    if registries.iter().any(|s| s.name == new_name) {
        anyhow::bail!("A registry named '{}' already exists", new_name);
    }

    // Update registry name in cfgd.yaml
    let raw = std::fs::read_to_string(&cli.config)?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&raw)?;
    if let Some(registries) = doc
        .get_mut("spec")
        .and_then(|s| s.get_mut("modules"))
        .and_then(|m| m.get_mut("registries"))
        .and_then(|v| v.as_sequence_mut())
    {
        for entry in registries.iter_mut() {
            if entry.get("name").and_then(|v| v.as_str()) == Some(name) {
                entry["name"] = serde_yaml::Value::String(new_name.to_string());
                break;
            }
        }
    }
    let yaml = serde_yaml::to_string(&doc)?;
    std::fs::write(&cli.config, &yaml)?;

    // Cascade to profiles: old_name/module → new_name/module
    let profiles_dir = cli
        .config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("profiles");
    if profiles_dir.is_dir() {
        let old_prefix = format!("{}/", name);
        let new_prefix = format!("{}/", new_name);
        for entry in std::fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
                continue;
            }
            let contents = std::fs::read_to_string(&path)?;
            let mut profile: config::ProfileDocument = match serde_yaml::from_str(&contents) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let mut changed = false;
            for module_ref in &mut profile.spec.modules {
                if module_ref.starts_with(&old_prefix) {
                    *module_ref = format!("{}{}", new_prefix, &module_ref[old_prefix.len()..]);
                    changed = true;
                }
            }
            if changed {
                let yaml = serde_yaml::to_string(&profile)?;
                std::fs::write(&path, &yaml)?;
            }
        }
    }

    printer.success(&format!("Renamed registry '{}' to '{}'", name, new_name));
    Ok(())
}

pub(super) fn cmd_module_registry_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Module Registries");

    if !cli.config.exists() {
        printer.info("No config found");
        return Ok(());
    }

    let cfg = config::load_config(&cli.config)?;
    let registries = cfg
        .spec
        .modules
        .as_ref()
        .map(|m| &m.registries[..])
        .unwrap_or(&[]);
    if registries.is_empty() {
        printer.info("No module registries configured");
        printer.info("Add one: cfgd module registry add <git-url>");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = registries
        .iter()
        .map(|s| vec![s.name.clone(), s.url.clone()])
        .collect();

    printer.table(&["Name", "URL"], &rows);

    Ok(())
}

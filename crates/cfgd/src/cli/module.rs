use std::path::{Path, PathBuf};

use serde::Serialize;

use super::*;

#[derive(Serialize)]
struct ModuleListEntry {
    name: String,
    active: bool,
    source: String,
    status: String,
    packages: usize,
    files: usize,
    depends: usize,
}

#[derive(Serialize)]
struct ModuleShowOutput {
    name: String,
    directory: String,
    source: String,
    depends: Vec<String>,
    state: Option<cfgd_core::state::ModuleStateRecord>,
    spec: cfgd_core::config::ModuleSpec,
}

pub(super) fn cmd_module_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base)?;
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
    let state = open_state_store()?;
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

    Ok(())
}

pub(super) fn cmd_module_show(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    show_values: bool,
) -> anyhow::Result<()> {
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

    if printer.is_structured() {
        let lockfile = modules::load_lockfile(&config_dir)?;
        let source_type = if lockfile.modules.iter().any(|e| e.name == name) {
            "remote"
        } else {
            "local"
        };
        let state = open_state_store()?;
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
    let state = open_state_store()?;
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
///   package.<name>.minVersion=<value>
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
            "minVersion" => {
                pkg.min_version = Some(value.to_string());
            }
            "prefer" => {
                pkg.prefer = value.split(',').map(|s| s.trim().to_string()).collect();
            }
            "platforms" => {
                pkg.platforms = value.split(',').map(|s| s.trim().to_string()).collect();
            }
            "deny" => {
                pkg.deny = value.split(',').map(|s| s.trim().to_string()).collect();
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
                    "Unknown package field '{}' — valid fields: minVersion, prefer, deny, platforms, script, alias",
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
    let env_list = &args.env;
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
        Some(config::ModuleScriptSpec {
            post_apply: post_apply_list
                .iter()
                .map(|s| s.replace("\\!", "!"))
                .collect(),
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
    printer.info("Add to a profile with: cfgd profile update <profile> --module <name>");
    printer.info("Fine-tune with: cfgd module edit <name>");

    maybe_update_workflow(cli, printer)?;

    // Apply if requested
    if args.apply {
        printer.newline();
        printer.header("Applying Module");

        let config_path = config_dir.join("cfgd.yaml");
        let cfg = config::load_config(&config_path)?;
        let registry = super::build_registry_with_config(Some(&cfg));
        let store = super::open_state_store()?;

        let platform = cfgd_core::platform::Platform::detect();
        let mgr_map = super::managers_map(&registry);
        let cache_base = modules::default_module_cache_dir()?;
        let resolved_modules = modules::resolve_modules(
            std::slice::from_ref(name),
            &config_dir,
            &cache_base,
            &platform,
            &mgr_map,
        )?;

        let resolved = config::ResolvedProfile {
            layers: Vec::new(),
            merged: config::MergedProfile::default(),
        };

        let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
        let plan = reconciler.plan(&resolved, Vec::new(), Vec::new(), resolved_modules)?;

        let total = plan.total_actions();
        if total == 0 {
            printer.success("Nothing to do");
        } else {
            if !args.yes {
                for phase in &plan.phases {
                    let items = cfgd_core::reconciler::format_plan_items(phase);
                    printer.plan_phase(phase.name.display_name(), &items);
                }
                printer.info(&format!("{} action(s) planned", total));
                let confirmed = printer
                    .prompt_confirm("Apply these changes?")
                    .unwrap_or(false);
                if !confirmed {
                    printer.info("Skipped — run 'cfgd apply' to apply later");
                    return Ok(());
                }
            }

            let state_dir = cfgd_core::state::default_state_dir()
                .map_err(|e| anyhow::anyhow!("cannot determine state directory: {}", e))?;
            let _apply_lock = cfgd_core::acquire_apply_lock(&state_dir)?;

            let result = reconciler.apply(&plan, &resolved, &config_dir, printer, None, &[])?;
            super::print_apply_result(&result, printer);
        }
    }

    Ok(())
}

// --- Module Update (local) ---

pub(super) fn cmd_module_update_local(
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
    for dep in &add_depends {
        if !doc.spec.depends.contains(dep) {
            doc.spec.depends.push(dep.clone());
            printer.success(&format!("Added dependency: {}", dep));
            changes += 1;
        }
    }

    // Remove dependencies
    for dep in &remove_depends {
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
    let known = super::known_manager_names();
    let known_refs: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
    for pkg_str in &add_packages {
        let (mgr, pkg) = super::parse_package_flag(pkg_str, &known_refs);
        if doc.spec.packages.iter().any(|p| p.name == pkg) {
            printer.info(&format!("Package '{}' already in module", pkg));
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
        printer.success(&format!("Added package: {}", pkg));
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
            printer.success(&format!("Removed package: {}", canonical));
            changes += 1;
        } else {
            printer.warning(&format!("Package '{}' not found in module", canonical));
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
            printer.success(&format!("Removed file: {}", target));
            changes += 1;
        } else {
            printer.warning(&format!("File '{}' not found in module", target));
        }
    }

    // Add env vars
    for e in &add_env {
        let ev = cfgd_core::parse_env_var(e).map_err(|e| anyhow::anyhow!(e))?;
        cfgd_core::merge_env(&mut doc.spec.env, std::slice::from_ref(&ev));
        printer.success(&format!("Set env: {}={}", ev.name, ev.value));
        changes += 1;
    }

    // Remove env vars
    for key in &remove_env {
        let before = doc.spec.env.len();
        doc.spec.env.retain(|ev| ev.name != *key);
        if doc.spec.env.len() < before {
            printer.success(&format!("Removed env: {}", key));
            changes += 1;
        } else {
            printer.warning(&format!("Env var '{}' not found", key));
        }
    }

    // Add aliases
    for a in &add_aliases {
        let alias = cfgd_core::parse_alias(a).map_err(|e| anyhow::anyhow!(e))?;
        cfgd_core::merge_aliases(&mut doc.spec.aliases, std::slice::from_ref(&alias));
        printer.success(&format!("Set alias: {}={}", alias.name, alias.command));
        changes += 1;
    }

    // Remove aliases
    for name in &remove_aliases {
        let before = doc.spec.aliases.len();
        doc.spec.aliases.retain(|a| a.name != *name);
        if doc.spec.aliases.len() < before {
            printer.success(&format!("Removed alias: {}", name));
            changes += 1;
        } else {
            printer.warning(&format!("Alias '{}' not found", name));
        }
    }

    // Add post-apply scripts
    for script in &add_post_apply {
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
    for script in &remove_post_apply {
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
    purge: bool,
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

    let module_yaml = module_dir.join("module.yaml");
    if module_yaml.exists()
        && let Ok(doc) = config::parse_module(&std::fs::read_to_string(&module_yaml)?)
    {
        if purge {
            // Purge mode: remove all files deployed by this module to target locations.
            // This replaces symlink restoration — there's nothing to restore if we're
            // removing everything.
            for file_entry in &doc.spec.files {
                let target = cfgd_core::expand_tilde(std::path::Path::new(&file_entry.target));
                if target.is_symlink() || target.exists() {
                    if target.is_dir() && !target.is_symlink() {
                        std::fs::remove_dir_all(&target)?;
                    } else {
                        std::fs::remove_file(&target)?;
                    }
                    printer.info(&format!("Purged {}", target.display()));
                }
            }
        } else {
            // Default: restore symlinked files before deleting the module directory.
            // When module create adopts files, it moves them into the module dir and
            // symlinks the original location back. On delete, we reverse that.
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
                    printer.info(&format!("Restored {}", target.display()));
                }
            }
        }
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

pub(super) fn cmd_module_export(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    format: &super::ExportFormat,
    output_dir: Option<&str>,
) -> anyhow::Result<()> {
    match format {
        super::ExportFormat::Devcontainer => export_devcontainer(cli, printer, name, output_dir),
    }
}

fn export_devcontainer(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    output_dir: Option<&str>,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let cache_base = modules::default_module_cache_dir()?;
    let all_modules = modules::load_all_modules(&config_dir, &cache_base)?;

    let module = all_modules
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Module '{}' not found", name))?;

    let out = PathBuf::from(output_dir.unwrap_or("."));
    let feature_dir = out.join(name);
    std::fs::create_dir_all(&feature_dir)?;

    // Build install.sh
    let mut install_lines = Vec::new();
    install_lines.push("#!/bin/sh".to_string());
    install_lines.push("set -e".to_string());
    install_lines.push(String::new());
    install_lines.push(format!("echo \"Installing cfgd module: {}\"", name));
    install_lines.push(String::new());

    // Package install commands — use apt as DevContainer default
    let apt_packages: Vec<&str> = module
        .spec
        .packages
        .iter()
        .filter_map(|p| {
            // Use apt alias if available, otherwise use canonical name
            if let Some(apt_name) = p.aliases.get("apt") {
                Some(apt_name.as_str())
            } else if p.platforms.is_empty() || p.platforms.iter().any(|pl| pl == "linux") {
                Some(p.name.as_str())
            } else {
                None
            }
        })
        .collect();

    if !apt_packages.is_empty() {
        install_lines.push("apt-get update".to_string());
        install_lines.push(format!(
            "apt-get install -y --no-install-recommends {}",
            apt_packages.join(" ")
        ));
        install_lines.push("rm -rf /var/lib/apt/lists/*".to_string());
        install_lines.push(String::new());
    }

    // Script-based packages
    for pkg in &module.spec.packages {
        if let Some(ref script) = pkg.script {
            install_lines.push(format!("# Install {} via script", pkg.name));
            install_lines.push(script.clone());
            install_lines.push(String::new());
        }
    }

    // Environment variables
    for ev in &module.spec.env {
        install_lines.push(format!(
            "echo 'export {}=\"{}\"' >> /etc/profile.d/cfgd-{}.sh",
            ev.name,
            cfgd_core::shell_escape_value(&ev.value),
            name
        ));
    }

    // Post-apply scripts
    if let Some(ref scripts) = module.spec.scripts {
        for script in &scripts.post_apply {
            install_lines.push(String::new());
            install_lines.push(format!("# Post-apply: {}", script));
            install_lines.push(script.clone());
        }
    }

    let install_path = feature_dir.join("install.sh");
    let mut install_content = install_lines.join("\n");
    install_content.push('\n');
    cfgd_core::atomic_write_str(&install_path, &install_content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&install_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // Build devcontainer-feature.json
    let mut options = serde_json::Map::new();
    for ev in &module.spec.env {
        options.insert(
            ev.name.clone(),
            serde_json::json!({
                "type": "string",
                "default": ev.value,
                "description": format!("Environment variable: {}", ev.name)
            }),
        );
    }

    // Try to get description from module.yaml metadata
    let description = load_module_document(&config_dir, name)
        .ok()
        .and_then(|(doc, _)| doc.metadata.description)
        .unwrap_or_else(|| {
            format!(
                "cfgd module: {}",
                module
                    .spec
                    .packages
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });

    let feature = serde_json::json!({
        "id": name,
        "version": "1.0.0",
        "name": name,
        "description": description,
        "options": options,
        "installsAfter": module.spec.depends.iter()
            .map(|d| format!("ghcr.io/cfgd-org/features/{}", d))
            .collect::<Vec<_>>(),
    });

    let feature_json = serde_json::to_string_pretty(&feature)?;
    cfgd_core::atomic_write_str(
        &feature_dir.join("devcontainer-feature.json"),
        &feature_json,
    )?;

    printer.success(&format!(
        "Exported module '{}' as DevContainer Feature to {}",
        name,
        feature_dir.display()
    ));
    printer.info(&format!("  {}/install.sh", feature_dir.display()));
    printer.info(&format!(
        "  {}/devcontainer-feature.json",
        feature_dir.display()
    ));

    Ok(())
}

// --- OCI Push / Pull ---

pub(super) struct PushOptions<'a> {
    pub platform: Option<&'a str>,
    pub apply: bool,
    pub sign: bool,
    pub key: Option<&'a str>,
    pub attest: bool,
}

pub(super) fn cmd_module_push(
    printer: &Printer,
    dir: &str,
    artifact: &str,
    opts: PushOptions<'_>,
) -> anyhow::Result<()> {
    let PushOptions {
        platform,
        apply,
        sign,
        key,
        attest,
    } = opts;
    let dir_path = Path::new(dir);
    if !dir_path.join("module.yaml").exists() {
        anyhow::bail!(
            "Directory '{}' does not contain a module.yaml",
            dir_path.display()
        );
    }

    printer.header("Push Module");
    printer.key_value("Directory", dir);
    printer.key_value("Artifact", artifact);
    if let Some(p) = platform {
        printer.key_value("Platform", p);
    }

    let digest = cfgd_core::oci::push_module(dir_path, artifact, platform)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    printer.success(&format!("Pushed {artifact}"));
    printer.key_value("Digest", &digest);

    // Sign if requested
    if sign {
        cfgd_core::oci::sign_artifact(artifact, key)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("Signed artifact with cosign");
    }

    // Attach SLSA provenance attestation if requested
    if attest {
        let repo = detect_git_remote().unwrap_or_else(|| "unknown".to_string());
        let commit = detect_git_head().unwrap_or_else(|| "unknown".to_string());

        let provenance =
            cfgd_core::oci::generate_slsa_provenance(artifact, &digest, &repo, &commit);
        let tmp = tempfile::NamedTempFile::new()?;
        std::fs::write(tmp.path(), &provenance)?;
        cfgd_core::oci::attach_attestation(
            artifact,
            &tmp.path().display().to_string(),
            key,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("Attached SLSA provenance attestation");
    }

    if apply {
        let module_yaml = std::fs::read_to_string(dir_path.join("module.yaml"))?;
        let module_doc = cfgd_core::config::parse_module(&module_yaml)
            .map_err(|e| anyhow::anyhow!("Failed to parse module.yaml: {e}"))?;

        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(apply_module_crd(printer, &module_doc, artifact))?;
    }

    Ok(())
}

fn detect_git_remote() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn detect_git_head() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

async fn apply_module_crd(
    printer: &Printer,
    module_doc: &cfgd_core::config::ModuleDocument,
    artifact: &str,
) -> anyhow::Result<()> {
    use kube::Client;
    use kube::api::{Api, Patch, PatchParams};

    let client = Client::try_default()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to cluster: {e}"))?;

    let name = &module_doc.metadata.name;
    let module_json = serde_json::json!({
        "apiVersion": cfgd_core::API_VERSION,
        "kind": "Module",
        "metadata": {
            "name": name,
        },
        "spec": {
            "ociArtifact": artifact,
            "packages": module_doc.spec.packages.iter().map(|p| {
                serde_json::json!({ "name": p.name })
            }).collect::<Vec<_>>(),
            "files": module_doc.spec.files.iter().map(|f| {
                serde_json::json!({
                    "source": f.source,
                    "target": f.target,
                })
            }).collect::<Vec<_>>(),
            "depends": module_doc.spec.depends,
        }
    });

    let modules: Api<kube::core::DynamicObject> = Api::all_with(
        client,
        &kube::discovery::ApiResource {
            group: "cfgd.io".into(),
            version: "v1alpha1".into(),
            api_version: cfgd_core::API_VERSION.into(),
            kind: "Module".into(),
            plural: "modules".into(),
        },
    );

    modules
        .patch(
            name,
            &PatchParams::apply("cfgd"),
            &Patch::Apply(module_json),
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to apply Module CRD: {e}"))?;

    printer.success(&format!("Applied Module CRD '{name}' to cluster"));
    Ok(())
}

pub(super) fn cmd_module_pull(
    printer: &Printer,
    artifact_ref: &str,
    output: &str,
    require_signature: bool,
    verify_attestation: bool,
    key: Option<&str>,
) -> anyhow::Result<()> {
    let output_path = Path::new(output);

    printer.header("Pull Module");
    printer.key_value("Artifact", artifact_ref);
    printer.key_value("Output", output);

    // Verify signature if requested (uses cosign, not the old tag-check)
    if require_signature {
        cfgd_core::oci::verify_signature(artifact_ref, key)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("Signature verified");
    }

    // Verify attestation if requested
    if verify_attestation {
        cfgd_core::oci::verify_attestation(artifact_ref, "slsaprovenance", key)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("SLSA provenance attestation verified");
    }

    // Pull uses the existing require_signature=false since we've already verified above
    cfgd_core::oci::pull_module(artifact_ref, output_path, false)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    printer.success(&format!("Pulled {artifact_ref} to {output}"));

    // Check if module.yaml exists in extracted content
    if output_path.join("module.yaml").exists() {
        let contents = std::fs::read_to_string(output_path.join("module.yaml"))?;
        if let Ok(doc) = cfgd_core::config::parse_module(&contents) {
            printer.key_value("Module", &doc.metadata.name);
            if let Some(desc) = &doc.metadata.description {
                printer.key_value("Description", desc);
            }
            printer.key_value("Packages", &doc.spec.packages.len().to_string());
            printer.key_value("Files", &doc.spec.files.len().to_string());
        }
    }

    Ok(())
}

pub(super) fn cmd_module_build(
    printer: &Printer,
    dir: &str,
    target: Option<&str>,
    base_image: Option<&str>,
    artifact: Option<&str>,
    sign: bool,
    key: Option<&str>,
) -> anyhow::Result<()> {
    let dir_path = Path::new(dir);
    if !dir_path.join("module.yaml").exists() {
        anyhow::bail!(
            "Directory '{}' does not contain a module.yaml",
            dir_path.display()
        );
    }

    printer.header("Build Module");
    printer.key_value("Directory", dir);
    if let Some(t) = target {
        printer.key_value("Target", t);
    }
    if let Some(img) = base_image {
        printer.key_value("Base image", img);
    }

    let default_platform = format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH);
    let targets: Vec<&str> = target
        .map(|t| t.split(',').collect())
        .unwrap_or_else(|| vec![default_platform.as_str()]);

    if targets.len() == 1 {
        let output_dir = cfgd_core::oci::build_module(dir_path, Some(targets[0]), base_image)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success(&format!("Built to {}", output_dir.display()));

        if let Some(art) = artifact {
            let digest = cfgd_core::oci::push_module(&output_dir, art, Some(targets[0]))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            printer.success(&format!("Pushed {art}"));
            printer.key_value("Digest", &digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                printer.success("Signed artifact");
            }
        }
    } else {
        let mut builds: Vec<(std::path::PathBuf, String)> = Vec::new();
        for t in &targets {
            printer.info(&format!("Building for {t}..."));
            let output_dir = cfgd_core::oci::build_module(dir_path, Some(t), base_image)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            printer.success(&format!("Built {t} to {}", output_dir.display()));
            builds.push((output_dir, t.to_string()));
        }

        if let Some(art) = artifact {
            let build_refs: Vec<(&Path, &str)> = builds
                .iter()
                .map(|(dir, plat)| (dir.as_path(), plat.as_str()))
                .collect();
            let digest = cfgd_core::oci::push_module_multiplatform(&build_refs, art)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            printer.success(&format!("Pushed multi-platform index {art}"));
            printer.key_value("Digest", &digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                printer.success("Signed artifact");
            }
        }
    }

    Ok(())
}

pub(super) fn cmd_module_keys_generate(
    printer: &Printer,
    output_dir: Option<&str>,
) -> anyhow::Result<()> {
    if !cfgd_core::command_available("cosign") {
        anyhow::bail!(
            "cosign not found — install it from https://docs.sigstore.dev/cosign/installation/"
        );
    }

    printer.header("Generate Cosign Key Pair");

    let dir = output_dir.unwrap_or(".");
    std::fs::create_dir_all(dir)?;

    let status = std::process::Command::new("cosign")
        .args(["generate-key-pair"])
        .current_dir(dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run cosign: {e}"))?;

    if !status.success() {
        anyhow::bail!("cosign generate-key-pair failed");
    }

    let key_dir = Path::new(dir);
    if key_dir.join("cosign.key").exists() {
        printer.success(&format!("Private key: {}/cosign.key", dir));
        printer.success(&format!("Public key:  {}/cosign.pub", dir));
        printer.info("Sign with: cfgd module push --sign --key cosign.key ...");
        printer.info("Verify with: cosign verify --key cosign.pub <artifact>");
    }

    Ok(())
}

pub(super) fn cmd_module_keys_list(printer: &Printer) -> anyhow::Result<()> {
    printer.header("Signing Keys");

    let locations = [
        ("./cosign.key", "./cosign.pub"),
        ("~/.cfgd/cosign.key", "~/.cfgd/cosign.pub"),
    ];

    let mut found = false;
    for (private, public) in &locations {
        let priv_path = cfgd_core::expand_tilde(Path::new(private));
        let pub_path = cfgd_core::expand_tilde(Path::new(public));

        if pub_path.exists() {
            found = true;
            let has_private = if priv_path.exists() { "yes" } else { "no" };
            printer.key_value(
                &pub_path.display().to_string(),
                &format!("private key: {has_private}"),
            );
        }
    }

    if !found {
        printer.info("No signing keys found");
        printer.info("Generate with: cfgd module keys generate");
    }

    Ok(())
}

/// Mask a value for display: show `***` with last 3 chars visible.
/// Short values (3 chars or fewer) are fully masked.
fn mask_value(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 3 {
        "***".to_string()
    } else {
        let suffix: String = chars[chars.len() - 3..].iter().collect();
        format!("***{}", suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_module_doc(packages: Vec<config::ModulePackageEntry>) -> config::ModuleDocument {
        config::ModuleDocument {
            api_version: cfgd_core::API_VERSION.to_string(),
            kind: "Module".to_string(),
            metadata: config::ModuleMetadata {
                name: "test".to_string(),
                description: None,
            },
            spec: config::ModuleSpec {
                packages,
                ..Default::default()
            },
        }
    }

    fn make_pkg(name: &str) -> config::ModulePackageEntry {
        config::ModulePackageEntry {
            name: name.to_string(),
            ..Default::default()
        }
    }

    // --- apply_module_sets ---

    #[test]
    fn apply_set_min_version() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        apply_module_sets(&["package.curl.minVersion=7.0".into()], &mut doc).unwrap();
        assert_eq!(doc.spec.packages[0].min_version.as_deref(), Some("7.0"));
    }

    #[test]
    fn apply_set_prefer() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        apply_module_sets(&["package.curl.prefer=brew,cargo".into()], &mut doc).unwrap();
        assert_eq!(doc.spec.packages[0].prefer, vec!["brew", "cargo"]);
    }

    #[test]
    fn apply_set_platforms() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        apply_module_sets(&["package.curl.platforms=linux,macos".into()], &mut doc).unwrap();
        assert_eq!(doc.spec.packages[0].platforms, vec!["linux", "macos"]);
    }

    #[test]
    fn apply_set_deny() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        apply_module_sets(&["package.curl.deny=snap".into()], &mut doc).unwrap();
        assert_eq!(doc.spec.packages[0].deny, vec!["snap"]);
    }

    #[test]
    fn apply_set_script() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        apply_module_sets(&["package.curl.script=install.sh".into()], &mut doc).unwrap();
        assert_eq!(doc.spec.packages[0].script.as_deref(), Some("install.sh"));
    }

    #[test]
    fn apply_set_alias() {
        let mut doc = make_module_doc(vec![make_pkg("vim")]);
        apply_module_sets(&["package.vim.alias.brew=neovim".into()], &mut doc).unwrap();
        assert_eq!(doc.spec.packages[0].aliases.get("brew").unwrap(), "neovim");
    }

    #[test]
    fn apply_set_invalid_no_equals() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        assert!(apply_module_sets(&["noequals".into()], &mut doc).is_err());
    }

    #[test]
    fn apply_set_invalid_path_too_short() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        assert!(apply_module_sets(&["foo=bar".into()], &mut doc).is_err());
    }

    #[test]
    fn apply_set_unknown_field() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        assert!(apply_module_sets(&["package.curl.unknown=val".into()], &mut doc).is_err());
    }

    #[test]
    fn apply_set_package_not_found() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        assert!(apply_module_sets(&["package.vim.minVersion=9.0".into()], &mut doc).is_err());
    }

    // --- load_module_document ---

    #[test]
    fn load_module_document_valid() {
        let dir = tempfile::tempdir().unwrap();
        let module_dir = dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&module_dir).unwrap();
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n";
        std::fs::write(module_dir.join("module.yaml"), yaml).unwrap();

        let (doc, path) = load_module_document(dir.path(), "test-mod").unwrap();
        assert_eq!(doc.metadata.name, "test-mod");
        assert_eq!(doc.spec.packages.len(), 1);
        assert!(path.ends_with("module.yaml"));
    }

    #[test]
    fn load_module_document_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_module_document(dir.path(), "nope").is_err());
    }

    #[test]
    fn load_module_document_missing_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let module_dir = dir.path().join("modules").join("empty-mod");
        std::fs::create_dir_all(&module_dir).unwrap();
        assert!(load_module_document(dir.path(), "empty-mod").is_err());
    }

    // --- save_module_document ---

    #[test]
    fn save_and_reload_module() {
        let dir = tempfile::tempdir().unwrap();
        let module_dir = dir.path().join("modules").join("roundtrip");
        std::fs::create_dir_all(&module_dir).unwrap();
        let path = module_dir.join("module.yaml");

        let doc = make_module_doc(vec![make_pkg("ripgrep")]);
        save_module_document(&doc, &path).unwrap();

        let (loaded, _) = load_module_document(dir.path(), "roundtrip").unwrap();
        assert_eq!(loaded.spec.packages[0].name, "ripgrep");
    }

    // --- profiles_using_module ---

    #[test]
    fn profiles_using_module_no_dir() {
        let result = profiles_using_module(Path::new("/nonexistent-12345"), "test").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn profiles_using_module_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  inherits: []\n  modules:\n    - other-mod\n";
        std::fs::write(dir.path().join("work.yaml"), yaml).unwrap();

        let result = profiles_using_module(dir.path(), "test-mod").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn profiles_using_module_match_found() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  inherits: []\n  modules:\n    - test-mod\n";
        std::fs::write(dir.path().join("work.yaml"), yaml).unwrap();

        let result = profiles_using_module(dir.path(), "test-mod").unwrap();
        assert_eq!(result, vec!["work"]);
    }

    // --- mask_value ---

    #[test]
    fn mask_value_long_string() {
        assert_eq!(mask_value("my-secret-token"), "***ken");
    }

    #[test]
    fn mask_value_short_string() {
        assert_eq!(mask_value("abc"), "***");
        assert_eq!(mask_value("ab"), "***");
        assert_eq!(mask_value(""), "***");
    }

    #[test]
    fn mask_value_four_chars() {
        assert_eq!(mask_value("abcd"), "***bcd");
    }

    // --- export_devcontainer ---

    #[test]
    fn export_devcontainer_creates_files() {
        let config_dir = tempfile::tempdir().unwrap();
        let mod_dir = config_dir.path().join("modules").join("test-tool");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(
            mod_dir.join("module.yaml"),
            r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: test-tool
spec:
  packages:
    - name: curl
    - name: jq
  env:
    - name: EDITOR
      value: vim
"#,
        )
        .unwrap();

        let output_dir = tempfile::tempdir().unwrap();
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cli = super::Cli {
            command: super::Command::Status { module: None },
            config: config_dir.path().join("cfgd.yaml"),
            profile: None,
            verbose: false,
            quiet: true,
            no_color: false,
            output: "table".to_string(),
            jsonpath: None,
        };

        let result = super::export_devcontainer(
            &cli,
            &printer,
            "test-tool",
            Some(output_dir.path().to_str().unwrap()),
        );
        assert!(result.is_ok(), "export failed: {:?}", result);

        let feature_dir = output_dir.path().join("test-tool");
        assert!(feature_dir.join("install.sh").exists());
        assert!(feature_dir.join("devcontainer-feature.json").exists());

        let install = std::fs::read_to_string(feature_dir.join("install.sh")).unwrap();
        assert!(install.contains("apt-get install"));
        assert!(install.contains("curl"));
        assert!(install.contains("jq"));
        assert!(install.contains("EDITOR"));

        let feature_json =
            std::fs::read_to_string(feature_dir.join("devcontainer-feature.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&feature_json).unwrap();
        assert_eq!(parsed["id"], "test-tool");
        assert!(parsed["options"]["EDITOR"].is_object());
    }
}

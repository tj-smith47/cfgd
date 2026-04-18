use std::path::{Path, PathBuf};

use serde::Serialize;

use super::*;

const NO_REGISTRIES_MSG: &str = "No module registries configured";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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

pub(super) fn cmd_module_show(
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
    cfgd_core::atomic_write_str(path, &yaml)?;
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
            printer.success("Nothing to do");
        } else {
            if !args.yes {
                super::display_plan_table(&plan, printer, None);
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
            encryption: None,
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
            .get_or_insert_with(config::ScriptSpec::default);
        let entry = config::ScriptEntry::Simple(script.clone());
        if !scripts.post_apply.contains(&entry) {
            scripts.post_apply.push(entry);
            printer.success(&format!("Added post-apply script: {}", script));
            changes += 1;
        }
    }

    // Remove post-apply scripts
    for script in &remove_post_apply {
        if let Some(ref mut scripts) = doc.spec.scripts {
            let before = scripts.post_apply.len();
            scripts.post_apply.retain(|e| e.run_str() != script);
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
    if let Ok(state) = open_state_store(cli.state_dir.as_deref())
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
            modules::fetch_registry_modules(registry_entry, &cache_base, printer)?;
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
    let fetched = modules::fetch_remote_module(url, &cache_base, printer)?;
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
                cfgd_core::atomic_write_str(&profile_path, &yaml)?;
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
    let old_local_path = modules::fetch_git_source(&old_pinned_src, &cache_base, name, printer)?;
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
            modules::fetch_git_source(&git_src, &cache_base, name, printer)?;
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
    let new_local_path = modules::fetch_git_source(&new_src, &cache_base, name, printer)?;
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
        if printer.is_structured() {
            printer.write_structured(&Vec::<super::ModuleSearchResult>::new());
            return Ok(());
        }
        printer.header(&format!("Search Modules: {}", query));
        printer.info(NO_REGISTRIES_MSG);
        printer.info("Add a registry: cfgd module registry add <git-url>");
        return Ok(());
    }

    let mut all_results: Vec<super::ModuleSearchResult> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for source in registries {
        match modules::fetch_registry_modules(source, &cache_base, printer) {
            Ok(registry_modules) => {
                let query_lower = query.to_lowercase();
                let matches: Vec<&modules::RegistryModule> = registry_modules
                    .iter()
                    .filter(|m| m.name.to_lowercase().contains(&query_lower))
                    .collect();

                for m in &matches {
                    let latest = m.tags.last().cloned();
                    let desc = if m.description.is_empty() {
                        None
                    } else {
                        Some(m.description.clone())
                    };
                    all_results.push(super::ModuleSearchResult {
                        name: format!("{}/{}", m.registry, m.name),
                        registry: m.registry.clone(),
                        description: desc,
                        version: latest,
                    });
                }
            }
            Err(e) => {
                errors.push(format!("{}: {}", source.name, e));
            }
        }
    }

    if printer.write_structured(&all_results) {
        return Ok(());
    }

    printer.header(&format!("Search Modules: {}", query));

    for source in registries {
        printer.info(&format!("Searching {} ({})", source.name, source.url));
    }

    for err in &errors {
        printer.warning(&format!("  Failed to fetch source: {}", err));
    }

    if all_results.is_empty() {
        printer.newline();
        printer.info("No modules found matching your query");
    } else if printer.is_wide() {
        let rows: Vec<Vec<String>> = all_results
            .iter()
            .map(|m| {
                vec![
                    m.name.clone(),
                    m.registry.clone(),
                    m.description.clone().unwrap_or_default(),
                    m.version.clone().unwrap_or_else(|| "-".into()),
                ]
            })
            .collect();
        printer.table(&["Module", "Registry", "Description", "Latest"], &rows);
    } else {
        let rows: Vec<Vec<String>> = all_results
            .iter()
            .map(|m| {
                vec![
                    m.name.clone(),
                    m.description.clone().unwrap_or_default(),
                    m.version.clone().unwrap_or_else(|| "-".into()),
                ]
            })
            .collect();
        printer.table(&["Module", "Description", "Latest"], &rows);
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
    cfgd_core::atomic_write_str(&cli.config, &yaml)?;

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
            cfgd_core::atomic_write_str(&cli.config, &yaml)?;
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
        printer.info(NO_REGISTRIES_MSG);
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
    cfgd_core::atomic_write_str(&cli.config, &yaml)?;

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
                cfgd_core::atomic_write_str(&path, &yaml)?;
            }
        }
    }

    printer.success(&format!("Renamed registry '{}' to '{}'", name, new_name));
    Ok(())
}

pub(super) fn cmd_module_registry_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    if !cli.config.exists() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<super::RegistryListEntry>::new());
            return Ok(());
        }
        printer.header("Module Registries");
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
        if printer.is_structured() {
            printer.write_structured(&Vec::<super::RegistryListEntry>::new());
            return Ok(());
        }
        printer.header("Module Registries");
        printer.info(NO_REGISTRIES_MSG);
        printer.info("Add one: cfgd module registry add <git-url>");
        return Ok(());
    }

    let entries: Vec<super::RegistryListEntry> = registries
        .iter()
        .map(|s| super::RegistryListEntry {
            name: s.name.clone(),
            url: s.url.clone(),
        })
        .collect();

    if printer.write_structured(&entries) {
        return Ok(());
    }

    printer.header("Module Registries");

    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| vec![e.name.clone(), e.url.clone()])
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
    let all_modules = modules::load_all_modules(&config_dir, &cache_base, printer)?;

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
            install_lines.push(script.run_str().to_string());
        }
    }

    let install_path = feature_dir.join("install.sh");
    let mut install_content = install_lines.join("\n");
    install_content.push('\n');
    cfgd_core::atomic_write_str(&install_path, &install_content)?;
    cfgd_core::set_file_permissions(&install_path, 0o755)?;

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

    let digest = cfgd_core::oci::push_module(dir_path, artifact, platform, Some(printer))
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    printer.success(&format!("Pushed {artifact}"));
    printer.key_value("Digest", &digest);

    // Sign if requested
    if sign {
        cfgd_core::oci::sign_artifact(artifact, key).map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("Signed artifact with cosign");
    }

    // Attach SLSA provenance attestation if requested
    if attest {
        let repo = detect_git_remote().unwrap_or_else(|| "unknown".to_string());
        let commit = detect_git_head().unwrap_or_else(|| "unknown".to_string());

        let provenance =
            cfgd_core::oci::generate_slsa_provenance(artifact, &digest, &repo, &commit)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        let tmp = tempfile::NamedTempFile::new()?;
        std::fs::write(tmp.path(), &provenance)?;
        cfgd_core::oci::attach_attestation(artifact, &tmp.path().display().to_string(), key)
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

fn git_output(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    if output.status.success() {
        Some(cfgd_core::stdout_lossy_trimmed(&output))
    } else {
        None
    }
}

fn detect_git_remote() -> Option<String> {
    git_output(&["remote", "get-url", "origin"])
}

fn detect_git_head() -> Option<String> {
    git_output(&["rev-parse", "HEAD"])
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
    verify_opts: cfgd_core::oci::VerifyOptions<'_>,
) -> anyhow::Result<()> {
    let output_path = Path::new(output);

    printer.header("Pull Module");
    printer.key_value("Artifact", artifact_ref);
    printer.key_value("Output", output);

    // Verify signature if requested (uses cosign, not the old tag-check)
    if require_signature {
        cfgd_core::oci::verify_signature(artifact_ref, &verify_opts)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("Signature verified");
    }

    // Verify attestation if requested
    if verify_attestation {
        cfgd_core::oci::verify_attestation(artifact_ref, "slsaprovenance", &verify_opts)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("SLSA provenance attestation verified");
    }

    // Pull uses the existing require_signature=false since we've already verified above
    cfgd_core::oci::pull_module(artifact_ref, output_path, false, Some(printer))
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

    let default_platform = cfgd_core::oci::current_platform();
    let targets: Vec<&str> = target
        .map(|t| t.split(',').collect())
        .unwrap_or_else(|| vec![default_platform.as_str()]);

    if targets.len() == 1 {
        let output_dir = cfgd_core::oci::build_module(dir_path, Some(targets[0]), base_image)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success(&format!("Built to {}", output_dir.display()));

        if let Some(art) = artifact {
            let digest =
                cfgd_core::oci::push_module(&output_dir, art, Some(targets[0]), Some(printer))
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            printer.success(&format!("Pushed {art}"));
            printer.key_value("Digest", &digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key).map_err(|e| anyhow::anyhow!("{e}"))?;
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
            let digest = cfgd_core::oci::push_module_multiplatform(&build_refs, art, Some(printer))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            printer.success(&format!("Pushed multi-platform index {art}"));
            printer.key_value("Digest", &digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key).map_err(|e| anyhow::anyhow!("{e}"))?;
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
    cfgd_core::require_tool(
        "cosign",
        Some("install it from https://docs.sigstore.dev/cosign/installation/"),
    )
    .map_err(|m| anyhow::anyhow!(m))?;

    printer.header("Generate Cosign Key Pair");

    let dir = output_dir.unwrap_or(".");
    std::fs::create_dir_all(dir)?;

    // If COSIGN_PASSWORD is set, cosign reads it from the env and doesn't prompt.
    // Use null stdin in that case so it never blocks waiting for terminal input.
    let stdin_cfg = if std::env::var("COSIGN_PASSWORD").is_ok() {
        std::process::Stdio::null()
    } else {
        std::process::Stdio::inherit()
    };
    let status = std::process::Command::new("cosign")
        .args(["generate-key-pair"])
        .current_dir(dir)
        .stdin(stdin_cfg)
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
    let locations = [
        ("./cosign.key", "./cosign.pub"),
        ("~/.cfgd/cosign.key", "~/.cfgd/cosign.pub"),
    ];

    let mut entries: Vec<super::KeyListEntry> = Vec::new();
    for (private, public) in &locations {
        let priv_path = cfgd_core::expand_tilde(Path::new(private));
        let pub_path = cfgd_core::expand_tilde(Path::new(public));

        if pub_path.exists() {
            let fingerprint = if priv_path.exists() {
                Some("private key: yes".to_string())
            } else {
                Some("private key: no".to_string())
            };
            let created = std::fs::metadata(&pub_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    let secs = t
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    cfgd_core::unix_secs_to_iso8601(secs)
                });
            entries.push(super::KeyListEntry {
                name: pub_path.display().to_string(),
                fingerprint,
                created,
            });
        }
    }

    if printer.write_structured(&entries) {
        return Ok(());
    }

    printer.header("Signing Keys");

    if entries.is_empty() {
        printer.info("No signing keys found");
        printer.info("Generate with: cfgd module keys generate");
    } else {
        for entry in &entries {
            printer.key_value(&entry.name, entry.fingerprint.as_deref().unwrap_or(""));
        }
    }

    Ok(())
}

pub(super) fn cmd_module_keys_rotate(
    printer: &Printer,
    dir: Option<&str>,
    artifacts: &[String],
) -> anyhow::Result<()> {
    cfgd_core::require_tool(
        "cosign",
        Some("install it from https://docs.sigstore.dev/cosign/installation/"),
    )
    .map_err(|m| anyhow::anyhow!(m))?;

    let key_dir = dir.unwrap_or(".");
    let old_key = Path::new(key_dir).join("cosign.key");
    let old_pub = Path::new(key_dir).join("cosign.pub");

    if !old_key.exists() {
        anyhow::bail!(
            "No existing cosign.key found in {} — generate one first with: cfgd module keys generate",
            key_dir
        );
    }

    printer.header("Rotate Cosign Key Pair");

    // Back up old keys
    let backup_suffix = cfgd_core::utc_now_iso8601().replace([':', '-', 'T', 'Z'], "");
    let backup_key = Path::new(key_dir).join(format!("cosign.key.{backup_suffix}"));
    let backup_pub = Path::new(key_dir).join(format!("cosign.pub.{backup_suffix}"));

    std::fs::rename(&old_key, &backup_key)?;
    printer.info(&format!(
        "Backed up old private key to {}",
        backup_key.display()
    ));
    if old_pub.exists() {
        std::fs::rename(&old_pub, &backup_pub)?;
        printer.info(&format!(
            "Backed up old public key to {}",
            backup_pub.display()
        ));
    }

    // Generate new key pair
    let status = std::process::Command::new("cosign")
        .args(["generate-key-pair"])
        .current_dir(key_dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run cosign: {e}"))?;

    if !status.success() {
        // Restore old keys on failure
        if backup_key.exists() {
            let _ = std::fs::rename(&backup_key, &old_key);
        }
        if backup_pub.exists() {
            let _ = std::fs::rename(&backup_pub, &old_pub);
        }
        anyhow::bail!("cosign generate-key-pair failed — old keys restored");
    }

    printer.success("Generated new key pair");

    // Re-sign artifacts with the new key
    let new_key_path = Path::new(key_dir).join("cosign.key");
    for artifact in artifacts {
        printer.info(&format!("Re-signing {artifact}..."));
        cfgd_core::oci::sign_artifact(artifact, Some(&new_key_path.display().to_string()))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success(&format!("Re-signed {artifact}"));
    }

    if artifacts.is_empty() {
        printer.info("No artifacts specified — re-sign manually with: cfgd module push --sign --key cosign.key ...");
    }

    printer.success("Key rotation complete");
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
        let err = apply_module_sets(&["noequals".into()], &mut doc).unwrap_err();
        assert!(
            err.to_string().contains("expected key=value"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apply_set_invalid_path_too_short() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        let err = apply_module_sets(&["foo=bar".into()], &mut doc).unwrap_err();
        assert!(
            err.to_string().contains("expected package."),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apply_set_unknown_field() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        let err = apply_module_sets(&["package.curl.unknown=val".into()], &mut doc).unwrap_err();
        assert!(
            err.to_string().contains("Unknown package field"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apply_set_package_not_found() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        let err = apply_module_sets(&["package.vim.minVersion=9.0".into()], &mut doc).unwrap_err();
        assert!(
            err.to_string().contains("not found in module"),
            "unexpected error: {err}"
        );
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
        let err = load_module_document(dir.path(), "nope").unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_module_document_missing_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let module_dir = dir.path().join("modules").join("empty-mod");
        std::fs::create_dir_all(&module_dir).unwrap();
        let err = load_module_document(dir.path(), "empty-mod").unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "unexpected error: {err}"
        );
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
            command: super::Command::Status {
                module: None,
                exit_code: false,
            },
            config: config_dir.path().join("cfgd.yaml"),
            profile: None,
            verbose: false,
            quiet: true,
            no_color: false,
            output: super::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            jsonpath: None,
            state_dir: None,
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

    // ─── helpers for harness-style tests ────────────────────────

    fn test_cli(dir: &std::path::Path) -> super::Cli {
        super::Cli {
            command: super::Command::Status {
                module: None,
                exit_code: false,
            },
            config: dir.join("cfgd.yaml"),
            profile: None,
            verbose: false,
            quiet: true,
            no_color: true,
            output: super::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            jsonpath: None,
            state_dir: None,
        }
    }

    fn test_cli_json(dir: &std::path::Path) -> super::Cli {
        super::Cli {
            output: super::OutputFormatArg(cfgd_core::output::OutputFormat::Json),
            ..test_cli(dir)
        }
    }

    fn setup_config_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules: []\n",
        ).unwrap();
        std::fs::write(
            dir.path().join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        ).unwrap();
        std::fs::create_dir_all(dir.path().join("modules")).unwrap();
        dir
    }

    fn make_module(dir: &std::path::Path, name: &str, yaml: &str) {
        let mod_dir = dir.join("modules").join(name);
        std::fs::create_dir_all(mod_dir.join("files")).unwrap();
        std::fs::write(mod_dir.join("module.yaml"), yaml).unwrap();
    }

    const RICH_MODULE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: devtools
  description: Dev tools module
spec:
  depends:
    - base
  packages:
    - name: ripgrep
    - name: fd-find
  files:
    - source: files/config.toml
      target: ~/.config/app/config.toml
  env:
    - name: EDITOR
      value: nvim
  aliases:
    - name: ll
      command: ls -la
  scripts:
    postApply:
      - echo done
"#;

    // ─── cmd_module_list ────────────────────────────────────────

    #[test]
    fn cmd_module_list_empty() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("No modules found"),
            "should report no modules, got: {output}"
        );
    }

    #[test]
    fn cmd_module_list_shows_modules() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "alpha",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: alpha\nspec:\n  packages:\n    - name: curl\n",
        );
        make_module(
            dir.path(),
            "beta",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: beta\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        assert!(output.contains("alpha"), "should list alpha, got: {output}");
        assert!(output.contains("beta"), "should list beta, got: {output}");
    }

    #[test]
    fn cmd_module_list_json_empty() {
        let dir = setup_config_dir();
        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    #[test]
    fn cmd_module_list_json_with_modules() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "alpha",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: alpha\nspec:\n  packages:\n    - name: curl\n    - name: vim\n",
        );

        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        // JSON may have preamble text from load_config_and_profile — find first '['
        let start = output.find('[').expect("should have JSON array in output");
        let json: serde_json::Value = serde_json::from_str(output[start..].trim()).unwrap();
        assert!(json.is_array());
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "alpha");
        assert_eq!(arr[0]["packages"], 2);
        assert_eq!(arr[0]["source"], "local");
    }

    // ─── cmd_module_show ────────────────────────────────────────

    #[test]
    fn cmd_module_show_not_found() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_show(&cli, &printer, "ghost", false).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "should report not found, got: {err}"
        );
    }

    #[test]
    fn cmd_module_show_displays_details() {
        let dir = setup_config_dir();
        make_module(dir.path(), "devtools", RICH_MODULE_YAML);
        // Create the source file so the file entry is valid
        std::fs::write(
            dir.path().join("modules/devtools/files/config.toml"),
            "# config",
        )
        .unwrap();

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_show(&cli, &printer, "devtools", false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Module: devtools"),
            "should show module header, got: {output}"
        );
        assert!(
            output.contains("base"),
            "should show dependencies, got: {output}"
        );
        assert!(
            output.contains("local"),
            "should show source as local, got: {output}"
        );
        assert!(
            output.contains("Packages"),
            "should have packages section, got: {output}"
        );
    }

    #[test]
    fn cmd_module_show_with_available_hint() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "existing",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: existing\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_show(&cli, &printer, "missing", false).unwrap_err();
        let output = buf.lock().unwrap();
        assert!(
            output.contains("existing"),
            "should hint available modules, got: {output}"
        );
        assert!(
            err.to_string().contains("not found"),
            "should report not found, got: {err}"
        );
    }

    #[test]
    fn cmd_module_show_env_masking() {
        let dir = setup_config_dir();
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: secrets-mod\nspec:\n  env:\n    - name: API_KEY\n      value: super-secret-token\n";
        make_module(dir.path(), "secrets-mod", yaml);

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        // Without --show-values, env values should be masked
        cmd_module_show(&cli, &printer, "secrets-mod", false).unwrap();
        let output = buf.lock().unwrap();
        assert!(
            output.contains("***"),
            "env values should be masked, got: {output}"
        );
        assert!(
            !output.contains("super-secret-token"),
            "actual value should not appear when masked, got: {output}"
        );
    }

    #[test]
    fn cmd_module_show_env_unmasked() {
        let dir = setup_config_dir();
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-mod\nspec:\n  env:\n    - name: GREETING\n      value: hello-world\n";
        make_module(dir.path(), "env-mod", yaml);

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_show(&cli, &printer, "env-mod", true).unwrap();
        let output = buf.lock().unwrap();
        assert!(
            output.contains("hello-world"),
            "actual value should appear with show_values=true, got: {output}"
        );
    }

    #[test]
    fn cmd_module_show_json_schema() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "jmod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: jmod\nspec:\n  packages:\n    - name: bat\n",
        );

        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_show(&cli, &printer, "jmod", false).unwrap();

        let output = buf.lock().unwrap();
        let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert!(json.get("name").is_some(), "JSON should have name field");
        assert_eq!(json["name"], "jmod");
        assert!(
            json.get("directory").is_some(),
            "JSON should have directory field"
        );
        assert!(
            json.get("source").is_some(),
            "JSON should have source field"
        );
        assert_eq!(json["source"], "local");
        assert!(json.get("spec").is_some(), "JSON should have spec field");
    }

    // ─── local test factory helpers ──────────────────────────────

    fn make_module_update_args(name: &str) -> super::ModuleUpdateArgs {
        super::ModuleUpdateArgs {
            name: name.to_string(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            depends: vec![],
            post_apply: vec![],
            private: false,
            description: None,
            sets: vec![],
        }
    }

    fn make_module_create_args(name: &str) -> super::ModuleCreateArgs {
        super::ModuleCreateArgs {
            name: name.to_string(),
            description: None,
            depends: vec![],
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            private: false,
            post_apply: vec![],
            sets: vec![],
            apply: false,
            yes: false,
        }
    }

    // ─── cmd_module_update_local — env, aliases, deps, scripts ─

    #[test]
    fn cmd_module_update_add_env() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            env: vec!["EDITOR=nvim".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.env.len(), 1);
        assert_eq!(doc.spec.env[0].name, "EDITOR");
        assert_eq!(doc.spec.env[0].value, "nvim");
    }

    #[test]
    fn cmd_module_update_remove_env() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n    - name: PAGER\n      value: less\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            env: vec!["-EDITOR".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.env.len(), 1);
        assert_eq!(doc.spec.env[0].name, "PAGER");
    }

    #[test]
    fn cmd_module_update_add_alias() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            aliases: vec!["ll=ls -la".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.aliases.len(), 1);
        assert_eq!(doc.spec.aliases[0].name, "ll");
        assert_eq!(doc.spec.aliases[0].command, "ls -la");
    }

    #[test]
    fn cmd_module_update_remove_alias() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  aliases:\n    - name: ll\n      command: ls -la\n    - name: gs\n      command: git status\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            aliases: vec!["-ll".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.aliases.len(), 1);
        assert_eq!(doc.spec.aliases[0].name, "gs");
    }

    #[test]
    fn cmd_module_update_add_depends() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            depends: vec!["base".to_string(), "core".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.depends, vec!["base", "core"]);
    }

    #[test]
    fn cmd_module_update_remove_depends() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  depends:\n    - base\n    - core\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            depends: vec!["-base".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.depends, vec!["core"]);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Removed dependency: base"),
            "should confirm removal, got: {output}"
        );
    }

    #[test]
    fn cmd_module_update_add_post_apply_script() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            post_apply: vec!["echo hello".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        let scripts = doc.spec.scripts.unwrap();
        assert_eq!(scripts.post_apply.len(), 1);
        assert_eq!(scripts.post_apply[0].run_str(), "echo hello");
    }

    #[test]
    fn cmd_module_update_remove_post_apply_script() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  scripts:\n    postApply:\n      - echo hello\n      - echo bye\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            post_apply: vec!["-echo hello".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        let scripts = doc.spec.scripts.unwrap();
        assert_eq!(scripts.post_apply.len(), 1);
        assert_eq!(scripts.post_apply[0].run_str(), "echo bye");
    }

    #[test]
    fn cmd_module_update_description() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            description: Some("Updated description".to_string()),
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(
            doc.metadata.description,
            Some("Updated description".to_string())
        );
    }

    #[test]
    fn cmd_module_update_no_changes_reports() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = make_module_update_args("mod1");
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("No changes specified"),
            "should report no changes, got: {output}"
        );
    }

    #[test]
    fn cmd_module_update_nonexistent_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("modules")).unwrap();

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = make_module_update_args("ghost");
        let err = cmd_module_update_local(&cli, &printer, &args).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "should report not found, got: {err}"
        );
    }

    // ─── cmd_module_create — non-interactive flags ─────────────

    #[test]
    fn cmd_module_create_with_env_and_aliases() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleCreateArgs {
            description: Some("Test module".to_string()),
            env: vec!["EDITOR=nvim".to_string()],
            aliases: vec!["ll=ls -la".to_string()],
            ..make_module_create_args("env-mod")
        };
        cmd_module_create(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "env-mod").unwrap();
        assert_eq!(doc.spec.env.len(), 1);
        assert_eq!(doc.spec.env[0].name, "EDITOR");
        assert_eq!(doc.spec.aliases.len(), 1);
        assert_eq!(doc.spec.aliases[0].name, "ll");

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Created module 'env-mod'"),
            "should confirm creation, got: {output}"
        );
    }

    #[test]
    fn cmd_module_create_with_depends_and_scripts() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleCreateArgs {
            depends: vec!["base".to_string()],
            post_apply: vec!["echo setup".to_string()],
            ..make_module_create_args("dep-mod")
        };
        cmd_module_create(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "dep-mod").unwrap();
        assert_eq!(doc.spec.depends, vec!["base"]);
        assert!(doc.spec.scripts.is_some());
        let scripts = doc.spec.scripts.unwrap();
        assert_eq!(scripts.post_apply.len(), 1);
    }

    #[test]
    fn cmd_module_create_invalid_name_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = make_module_create_args(".bad-name");
        let err = cmd_module_create(&cli, &printer, &args).unwrap_err();
        assert!(
            err.to_string().contains("cannot start with"),
            "should reject invalid name, got: {err}"
        );
    }

    // ─── cmd_module_delete — edge cases ────────────────────────

    #[test]
    fn cmd_module_delete_nonexistent_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_delete(&cli, &printer, "ghost", true, false).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "should report not found, got: {err}"
        );
    }

    #[test]
    fn cmd_module_delete_invalid_name_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_delete(&cli, &printer, "-bad", true, false).unwrap_err();
        assert!(
            err.to_string().contains("cannot start with"),
            "should reject invalid name, got: {err}"
        );
    }

    // ─── cmd_module_registry_add ────────────────────────────────

    #[test]
    fn cmd_module_registry_add_creates_entry() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_add(
            &cli,
            &printer,
            "https://github.com/team/modules.git",
            Some("team"),
        )
        .unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Added module registry 'team'"),
            "should confirm add, got: {output}"
        );

        // Verify config was updated
        let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
        assert!(
            contents.contains("team"),
            "config should contain registry name, got: {contents}"
        );
    }

    #[test]
    fn cmd_module_registry_add_duplicate_is_noop() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_add(
            &cli,
            &printer,
            "https://github.com/team/modules.git",
            Some("team"),
        )
        .unwrap();

        // Second add should be a no-op
        let (printer2, buf2) = cfgd_core::output::Printer::for_test();
        cmd_module_registry_add(
            &cli,
            &printer2,
            "https://github.com/team/other.git",
            Some("team"),
        )
        .unwrap();

        let output = buf2.lock().unwrap();
        assert!(
            output.contains("already configured"),
            "should report already configured, got: {output}"
        );
    }

    #[test]
    fn cmd_module_registry_add_no_config_fails() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err =
            cmd_module_registry_add(&cli, &printer, "https://example.com/reg.git", Some("test"))
                .unwrap_err();
        assert!(
            err.to_string().contains("cfgd.yaml"),
            "should fail without config, got: {err}"
        );
    }

    // ─── cmd_module_registry_remove ─────────────────────────────

    #[test]
    fn cmd_module_registry_remove_existing() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        // Add first
        cmd_module_registry_add(
            &cli,
            &printer,
            "https://example.com/reg.git",
            Some("myrepo"),
        )
        .unwrap();

        // Remove
        let (printer2, buf2) = cfgd_core::output::Printer::for_test();
        cmd_module_registry_remove(&cli, &printer2, "myrepo").unwrap();

        let output = buf2.lock().unwrap();
        assert!(
            output.contains("Removed module registry 'myrepo'"),
            "should confirm removal, got: {output}"
        );
    }

    #[test]
    fn cmd_module_registry_remove_not_found() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_remove(&cli, &printer, "nonexistent").unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("No module registries") || output.contains("not found"),
            "should report not found or no registries, got: {output}"
        );
    }

    // ─── cmd_module_registry_rename ─────────────────────────────

    #[test]
    fn cmd_module_registry_rename_success() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_add(
            &cli,
            &printer,
            "https://example.com/reg.git",
            Some("old-name"),
        )
        .unwrap();

        let (printer2, buf2) = cfgd_core::output::Printer::for_test();
        cmd_module_registry_rename(&cli, &printer2, "old-name", "new-name").unwrap();

        let output = buf2.lock().unwrap();
        assert!(
            output.contains("Renamed registry 'old-name' to 'new-name'"),
            "should confirm rename, got: {output}"
        );

        // Verify config
        let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
        assert!(contents.contains("new-name"));
        assert!(!contents.contains("old-name"));
    }

    #[test]
    fn cmd_module_registry_rename_not_found_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_registry_rename(&cli, &printer, "ghost", "new").unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "should report not found, got: {err}"
        );
    }

    #[test]
    fn cmd_module_registry_rename_target_exists_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_add(&cli, &printer, "https://example.com/a.git", Some("alpha"))
            .unwrap();
        cmd_module_registry_add(&cli, &printer, "https://example.com/b.git", Some("beta")).unwrap();

        let (printer2, _buf2) = cfgd_core::output::Printer::for_test();
        let err = cmd_module_registry_rename(&cli, &printer2, "alpha", "beta").unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "should report already exists, got: {err}"
        );
    }

    // ─── cmd_module_registry_list ───────────────────────────────

    #[test]
    fn cmd_module_registry_list_empty() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("No module registries"),
            "should report no registries, got: {output}"
        );
    }

    #[test]
    fn cmd_module_registry_list_with_entries() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_add(&cli, &printer, "https://example.com/a.git", Some("alpha"))
            .unwrap();
        cmd_module_registry_add(&cli, &printer, "https://example.com/b.git", Some("beta")).unwrap();

        let (printer2, buf2) = cfgd_core::output::Printer::for_test();
        cmd_module_registry_list(&cli, &printer2).unwrap();

        let output = buf2.lock().unwrap();
        assert!(output.contains("alpha"), "should list alpha, got: {output}");
        assert!(output.contains("beta"), "should list beta, got: {output}");
    }

    #[test]
    fn cmd_module_registry_list_json() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_add(&cli, &printer, "https://example.com/r.git", Some("team")).unwrap();

        let cli_json = test_cli_json(dir.path());
        let (printer2, buf2) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
        cmd_module_registry_list(&cli_json, &printer2).unwrap();

        let output = buf2.lock().unwrap();
        let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert!(json.is_array());
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "team");
        assert!(arr[0]["url"].as_str().unwrap().contains("example.com"));
    }

    #[test]
    fn cmd_module_registry_list_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("No config found"),
            "should report no config, got: {output}"
        );
    }

    // ─── cmd_module_keys_list ───────────────────────────────────

    #[test]
    fn cmd_module_keys_list_no_keys() {
        let (printer, buf) = cfgd_core::output::Printer::for_test();
        cmd_module_keys_list(&printer).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("No signing keys found"),
            "should report no keys, got: {output}"
        );
    }

    // ─── cmd_module_search — no registries ──────────────────────

    #[test]
    fn cmd_module_search_no_config_fails() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_search(&cli, &printer, "test").unwrap_err();
        assert!(
            err.to_string().contains("config"),
            "should fail without config, got: {err}"
        );
    }

    #[test]
    fn cmd_module_search_no_registries() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_search(&cli, &printer, "test").unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("No module registries"),
            "should report no registries, got: {output}"
        );
    }

    #[test]
    fn cmd_module_search_no_registries_json() {
        let dir = setup_config_dir();
        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_search(&cli, &printer, "test").unwrap();

        let output = buf.lock().unwrap();
        let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    // ─── cmd_module_keys_generate — no cosign ───────────────────

    #[test]
    fn cmd_module_keys_generate_no_cosign_fails() {
        // In test environment, cosign is very unlikely to be available
        if cfgd_core::command_available("cosign") {
            return; // skip if cosign is actually installed
        }
        let (printer, _buf) = cfgd_core::output::Printer::for_test();
        let err = cmd_module_keys_generate(&printer, None).unwrap_err();
        assert!(
            err.to_string().contains("cosign not found"),
            "should report cosign missing, got: {err}"
        );
    }

    // ─── cmd_module_push / pull — precondition errors ───────────

    #[test]
    fn cmd_module_push_no_module_yaml_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let opts = PushOptions {
            platform: None,
            apply: false,
            sign: false,
            key: None,
            attest: false,
        };
        let err = cmd_module_push(
            &printer,
            dir.path().to_str().unwrap(),
            "oci.example.com/test:v1",
            opts,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("does not contain a module.yaml"),
            "should report missing module.yaml, got: {err}"
        );
    }

    #[test]
    fn cmd_module_build_no_module_yaml_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            None,
            None,
            None,
            false,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("does not contain a module.yaml"),
            "should report missing module.yaml, got: {err}"
        );
    }

    // ─── cmd_module_export — not-found ──────────────────────────

    #[test]
    fn cmd_module_export_not_found() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_export(
            &cli,
            &printer,
            "ghost",
            &super::ExportFormat::Devcontainer,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "should report not found, got: {err}"
        );
    }

    // ─── profiles_using_module — edge cases ─────────────────────

    #[test]
    fn profiles_using_module_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = profiles_using_module(dir.path(), "test").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn profiles_using_module_nonexistent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = profiles_using_module(&dir.path().join("nonexistent"), "test").unwrap();
        assert!(result.is_empty());
    }

    // ─── cmd_module_registry_rename cascades to profiles ────────

    #[test]
    fn cmd_module_registry_rename_cascades_to_profiles() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        cmd_module_registry_add(&cli, &printer, "https://example.com/r.git", Some("old")).unwrap();

        // Add a profile that references old/somemod
        let profile_path = dir.path().join("profiles").join("default.yaml");
        let mut pdoc = config::load_profile(&profile_path).unwrap();
        pdoc.spec.modules.push("old/somemod".to_string());
        let yaml = serde_yaml::to_string(&pdoc).unwrap();
        std::fs::write(&profile_path, &yaml).unwrap();

        cmd_module_registry_rename(&cli, &printer, "old", "fresh").unwrap();

        let pdoc = config::load_profile(&profile_path).unwrap();
        assert!(
            pdoc.spec.modules.contains(&"fresh/somemod".to_string()),
            "profile should have updated reference, got: {:?}",
            pdoc.spec.modules
        );
        assert!(
            !pdoc.spec.modules.contains(&"old/somemod".to_string()),
            "old reference should be gone"
        );
    }

    // ─── cmd_module_show — JSON with lockfile entry (remote source) ─

    #[test]
    fn cmd_module_show_json_with_lockfile_entry() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "remote-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: remote-mod\nspec:\n  packages:\n    - name: tmux\n",
        );

        // Write a lockfile entry for this module
        let lockfile = cfgd_core::config::ModuleLockfile {
            modules: vec![cfgd_core::config::ModuleLockEntry {
                name: "remote-mod".to_string(),
                url: "https://github.com/team/modules.git@v1.0".to_string(),
                pinned_ref: "v1.0".to_string(),
                commit: "abc123def456".to_string(),
                integrity: "sha256:deadbeef".to_string(),
                subdir: Some("modules/remote-mod".to_string()),
            }],
        };
        modules::save_lockfile(dir.path(), &lockfile).unwrap();

        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_show(&cli, &printer, "remote-mod", false).unwrap();

        let output = buf.lock().unwrap();
        let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(json["name"], "remote-mod");
        assert_eq!(json["source"], "remote", "lockfile module should be remote");
        assert!(
            json["spec"]["packages"].as_array().unwrap().len() == 1,
            "should have 1 package"
        );
    }

    // ─── cmd_module_show — table with lockfile entry (remote source) ─

    #[test]
    fn cmd_module_show_table_with_lockfile_entry() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "locked-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: locked-mod\nspec:\n  packages:\n    - name: git\n",
        );

        let lockfile = cfgd_core::config::ModuleLockfile {
            modules: vec![cfgd_core::config::ModuleLockEntry {
                name: "locked-mod".to_string(),
                url: "https://github.com/team/modules.git@v2.0".to_string(),
                pinned_ref: "v2.0".to_string(),
                commit: "aabbccdd".to_string(),
                integrity: "sha256:cafebabe".to_string(),
                subdir: None,
            }],
        };
        modules::save_lockfile(dir.path(), &lockfile).unwrap();

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_show(&cli, &printer, "locked-mod", false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("remote (locked)"),
            "should show 'remote (locked)' source, got: {output}"
        );
        assert!(
            output.contains("v2.0"),
            "should show pinned ref, got: {output}"
        );
        assert!(
            output.contains("aabbccdd"),
            "should show commit, got: {output}"
        );
        assert!(
            output.contains("sha256:cafebabe"),
            "should show integrity, got: {output}"
        );
    }

    // ─── cmd_module_show — aliases section ─────────────────────────

    #[test]
    fn cmd_module_show_aliases() {
        let dir = setup_config_dir();
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: alias-mod\nspec:\n  aliases:\n    - name: gs\n      command: git status\n    - name: gp\n      command: git push\n";
        make_module(dir.path(), "alias-mod", yaml);

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_show(&cli, &printer, "alias-mod", false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Aliases"),
            "should have Aliases section, got: {output}"
        );
        assert!(
            output.contains("git status"),
            "should show alias command, got: {output}"
        );
        assert!(
            output.contains("git push"),
            "should show alias command, got: {output}"
        );
    }

    // ─── cmd_module_show — scripts section ─────────────────────────

    #[test]
    fn cmd_module_show_scripts() {
        let dir = setup_config_dir();
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: script-mod\nspec:\n  scripts:\n    postApply:\n      - echo setup\n      - make install\n";
        make_module(dir.path(), "script-mod", yaml);

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_show(&cli, &printer, "script-mod", false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Post-apply Scripts"),
            "should have post-apply scripts section, got: {output}"
        );
        assert!(
            output.contains("echo setup"),
            "should show script, got: {output}"
        );
        assert!(
            output.contains("make install"),
            "should show script, got: {output}"
        );
    }

    // ─── cmd_module_show — files with git source indicator ─────────

    #[test]
    fn cmd_module_show_files_with_git_source() {
        let dir = setup_config_dir();
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: git-file-mod\nspec:\n  files:\n    - source: https://github.com/user/repo.git//config.toml\n      target: ~/.config/app/config.toml\n    - source: files/local.conf\n      target: ~/.local.conf\n";
        make_module(dir.path(), "git-file-mod", yaml);

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_show(&cli, &printer, "git-file-mod", false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Files"),
            "should have Files section, got: {output}"
        );
        assert!(
            output.contains("(git)"),
            "git sources should have (git) indicator, got: {output}"
        );
    }

    // ─── cmd_module_create — with packages and --set overrides ─────

    #[test]
    fn cmd_module_create_with_packages_and_sets() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleCreateArgs {
            packages: vec!["ripgrep".to_string(), "fd-find".to_string()],
            sets: vec![
                "package.ripgrep.minVersion=13.0".to_string(),
                "package.fd-find.platforms=linux,macos".to_string(),
            ],
            ..make_module_create_args("pkg-set-mod")
        };
        cmd_module_create(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "pkg-set-mod").unwrap();
        assert_eq!(doc.spec.packages.len(), 2);
        assert_eq!(
            doc.spec.packages[0].min_version.as_deref(),
            Some("13.0"),
            "ripgrep should have minVersion set"
        );
        assert_eq!(
            doc.spec.packages[1].platforms,
            vec!["linux", "macos"],
            "fd-find should have platforms set"
        );

        let output = buf.lock().unwrap();
        assert!(
            output.contains("ripgrep"),
            "should list ripgrep in output, got: {output}"
        );
        assert!(
            output.contains("fd-find"),
            "should list fd-find in output, got: {output}"
        );
    }

    // ─── cmd_module_create — duplicate name fails ──────────────────

    #[test]
    fn cmd_module_create_duplicate_name_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleCreateArgs {
            description: Some("test module".to_string()),
            ..make_module_create_args("dup-mod")
        };
        cmd_module_create(&cli, &printer, &args).unwrap();

        // Second create with same name should fail
        let (printer2, _buf2) = cfgd_core::output::Printer::for_test();
        let err = cmd_module_create(&cli, &printer2, &args).unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "should report already exists, got: {err}"
        );
    }

    // ─── cmd_module_create — post-apply scripts with shell escape ──

    #[test]
    fn cmd_module_create_post_apply_scripts_escape() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleCreateArgs {
            post_apply: vec![r"echo hello \! world".to_string()],
            ..make_module_create_args("script-esc-mod")
        };
        cmd_module_create(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "script-esc-mod").unwrap();
        let scripts = doc.spec.scripts.unwrap();
        // Shell escape: \! should be normalized to !
        assert_eq!(
            scripts.post_apply[0].run_str(),
            "echo hello ! world",
            "backslash-exclamation should be unescaped"
        );
    }

    // ─── cmd_module_create — with manager-prefixed packages ────────

    #[test]
    fn cmd_module_create_with_prefixed_packages() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleCreateArgs {
            packages: vec!["brew:ripgrep".to_string(), "cargo:fd-find".to_string()],
            ..make_module_create_args("prefix-mod")
        };
        cmd_module_create(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "prefix-mod").unwrap();
        assert_eq!(doc.spec.packages[0].name, "ripgrep");
        assert_eq!(doc.spec.packages[0].prefer, vec!["brew"]);
        assert_eq!(doc.spec.packages[1].name, "fd-find");
        assert_eq!(doc.spec.packages[1].prefer, vec!["cargo"]);
    }

    // ─── cmd_module_create — with file import ──────────────────────

    #[test]
    fn cmd_module_create_with_file_import() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        // Create a source file to import
        let source_file = dir.path().join("my-config.toml");
        std::fs::write(&source_file, "[settings]\nfoo = true\n").unwrap();

        let target = "~/.config/myapp/config.toml";
        let file_spec = format!("{}:{}", source_file.display(), target);

        let args = super::ModuleCreateArgs {
            files: vec![file_spec],
            ..make_module_create_args("file-mod")
        };
        cmd_module_create(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "file-mod").unwrap();
        assert_eq!(doc.spec.files.len(), 1);
        assert_eq!(doc.spec.files[0].source, "files/my-config.toml");
        assert!(doc.spec.files[0].target.contains("config.toml"));

        // Verify the file was actually copied into the module
        let copied_file = dir.path().join("modules/file-mod/files/my-config.toml");
        assert!(
            copied_file.exists(),
            "file should be copied into module files/ dir"
        );
        let contents = std::fs::read_to_string(&copied_file).unwrap();
        assert!(contents.contains("foo = true"));

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Files"),
            "should report file count, got: {output}"
        );
    }

    // ─── cmd_module_create — duplicate file basenames fail ─────────

    #[test]
    fn cmd_module_create_duplicate_file_basenames_fail() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        // Create two files with the same basename in different dirs
        let dir_a = dir.path().join("a");
        let dir_b = dir.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        std::fs::write(dir_a.join("config.toml"), "a").unwrap();
        std::fs::write(dir_b.join("config.toml"), "b").unwrap();

        let args = super::ModuleCreateArgs {
            files: vec![
                format!("{}:~/.a/config.toml", dir_a.join("config.toml").display()),
                format!("{}:~/.b/config.toml", dir_b.join("config.toml").display()),
            ],
            ..make_module_create_args("dup-file-mod")
        };
        let err = cmd_module_create(&cli, &printer, &args).unwrap_err();
        assert!(
            err.to_string().contains("Duplicate file basename"),
            "should report duplicate basenames, got: {err}"
        );
    }

    // ─── cmd_module_create — private files add to gitignore ────────

    #[test]
    fn cmd_module_create_private_files_gitignore() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let source_file = dir.path().join("secret.key");
        std::fs::write(&source_file, "private-data").unwrap();

        let args = super::ModuleCreateArgs {
            files: vec![format!("{}:~/.ssh/secret.key", source_file.display())],
            private: true,
            ..make_module_create_args("priv-mod")
        };
        cmd_module_create(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "priv-mod").unwrap();
        assert!(doc.spec.files[0].private, "file should be marked private");

        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(
            gitignore.contains("modules/priv-mod/files/secret.key"),
            "gitignore should contain private file path, got: {gitignore}"
        );
    }

    // ─── cmd_module_update_local — --set overrides ─────────────────

    #[test]
    fn cmd_module_update_with_set_overrides() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: curl\n    - name: wget\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            sets: vec![
                "package.curl.minVersion=8.0".to_string(),
                "package.wget.deny=snap".to_string(),
            ],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.packages[0].min_version.as_deref(), Some("8.0"));
        assert_eq!(doc.spec.packages[1].deny, vec!["snap"]);
    }

    // ─── cmd_module_update_local — add/remove packages ─────────────

    #[test]
    fn cmd_module_update_add_and_remove_packages() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: curl\n    - name: wget\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            packages: vec!["ripgrep".to_string(), "-wget".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        let names: Vec<&str> = doc.spec.packages.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"curl"), "curl should remain");
        assert!(names.contains(&"ripgrep"), "ripgrep should be added");
        assert!(!names.contains(&"wget"), "wget should be removed");

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Added package: ripgrep"),
            "should confirm addition, got: {output}"
        );
        assert!(
            output.contains("Removed package: wget"),
            "should confirm removal, got: {output}"
        );
    }

    // ─── cmd_module_update_local — add duplicate package is noop ────

    #[test]
    fn cmd_module_update_add_duplicate_package_noop() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: curl\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            packages: vec!["curl".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("already in module"),
            "should note duplicate, got: {output}"
        );
    }

    // ─── cmd_module_update_local — remove nonexistent package warns ─

    #[test]
    fn cmd_module_update_remove_nonexistent_package_warns() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: curl\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            packages: vec!["-nonexistent".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("not found in module"),
            "should warn about nonexistent package, got: {output}"
        );
    }

    // ─── cmd_module_update_local — remove nonexistent env warns ─────

    #[test]
    fn cmd_module_update_remove_nonexistent_env_warns() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            env: vec!["-NONEXISTENT".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("not found"),
            "should warn about nonexistent env var, got: {output}"
        );
    }

    // ─── cmd_module_update_local — remove nonexistent alias warns ───

    #[test]
    fn cmd_module_update_remove_nonexistent_alias_warns() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            aliases: vec!["-noalias".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("not found"),
            "should warn about nonexistent alias, got: {output}"
        );
    }

    // ─── cmd_module_update_local — remove nonexistent depends warns ─

    #[test]
    fn cmd_module_update_remove_nonexistent_depends_warns() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            depends: vec!["-nonexistent".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("not found"),
            "should warn about nonexistent dep, got: {output}"
        );
    }

    // ─── cmd_module_update_local — remove nonexistent script warns ──

    #[test]
    fn cmd_module_update_remove_nonexistent_script_warns() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  scripts:\n    postApply:\n      - echo hello\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            post_apply: vec!["-nonexistent script".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("not found"),
            "should warn about nonexistent script, got: {output}"
        );
    }

    // ─── cmd_module_update_local — description empty clears ─────────

    #[test]
    fn cmd_module_update_empty_description_clears() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\n  description: Old desc\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            description: Some(String::new()),
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(
            doc.metadata.description, None,
            "empty description should clear it"
        );
    }

    // ─── cmd_module_update_local — add duplicate depends is noop ────

    #[test]
    fn cmd_module_update_add_duplicate_depends_noop() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  depends:\n    - base\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            depends: vec!["base".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.depends.len(), 1, "should not duplicate depends");
    }

    // ─── cmd_module_update_local — add duplicate post-apply is noop ─

    #[test]
    fn cmd_module_update_add_duplicate_post_apply_noop() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  scripts:\n    postApply:\n      - echo hello\n",
        );

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            post_apply: vec!["echo hello".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        let scripts = doc.spec.scripts.unwrap();
        assert_eq!(
            scripts.post_apply.len(),
            1,
            "should not duplicate post-apply script"
        );
    }

    // ─── cmd_module_update_local — add files ───────────────────────

    #[test]
    fn cmd_module_update_add_files() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        // Create a source file
        let source_file = dir.path().join("new-config.toml");
        std::fs::write(&source_file, "setting = true").unwrap();

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            files: vec![format!(
                "{}:~/.config/app/new-config.toml",
                source_file.display()
            )],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.files.len(), 1, "should have one file");
        assert_eq!(doc.spec.files[0].source, "files/new-config.toml");

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Added file"),
            "should confirm file addition, got: {output}"
        );
    }

    // ─── cmd_module_update_local — remove files ────────────────────

    #[test]
    fn cmd_module_update_remove_files() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  files:\n    - source: files/config.toml\n      target: ~/.config/app/config.toml\n",
        );
        // Create the source file so it can be cleaned up
        std::fs::write(dir.path().join("modules/mod1/files/config.toml"), "content").unwrap();

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            files: vec!["-~/.config/app/config.toml".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert!(
            doc.spec.files.is_empty(),
            "files should be empty after removal"
        );

        // Verify the source file was cleaned up
        assert!(
            !dir.path().join("modules/mod1/files/config.toml").exists(),
            "source file should be deleted"
        );

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Removed file"),
            "should confirm file removal, got: {output}"
        );
    }

    // ─── cmd_module_update_local — remove nonexistent file warns ────

    #[test]
    fn cmd_module_update_remove_nonexistent_file_warns() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            files: vec!["-~/.nonexistent/file".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("not found in module"),
            "should warn about nonexistent file, got: {output}"
        );
    }

    // ─── cmd_module_update_local — add file with private flag ───────

    #[test]
    fn cmd_module_update_add_file_private() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages: []\n",
        );

        let source_file = dir.path().join("secret.key");
        std::fs::write(&source_file, "secret-data").unwrap();

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            files: vec![format!("{}:~/.ssh/secret.key", source_file.display())],
            private: true,
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert!(doc.spec.files[0].private, "file should be marked private");

        let gitignore_path = dir.path().join(".gitignore");
        assert!(gitignore_path.exists(), "gitignore should be created");
        let gitignore = std::fs::read_to_string(&gitignore_path).unwrap();
        assert!(
            gitignore.contains("modules/mod1/files/secret.key"),
            "gitignore should contain private file path"
        );
    }

    // ─── cmd_module_delete — with yes flag succeeds ────────────────

    #[test]
    fn cmd_module_delete_with_yes_succeeds() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "to-delete",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: to-delete\nspec:\n  packages: []\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_delete(&cli, &printer, "to-delete", true, false).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Deleted module 'to-delete'"),
            "should confirm deletion, got: {output}"
        );
        assert!(
            !dir.path().join("modules/to-delete").exists(),
            "module directory should be removed"
        );
    }

    // ─── cmd_module_delete — refused when profiles reference module ─

    #[test]
    fn cmd_module_delete_refused_when_profile_references() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "referenced",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: referenced\nspec:\n  packages: []\n",
        );

        // Add the module to the profile
        let profile_path = dir.path().join("profiles/default.yaml");
        std::fs::write(
            &profile_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - referenced\n",
        ).unwrap();

        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_delete(&cli, &printer, "referenced", true, false).unwrap_err();
        assert!(
            err.to_string().contains("referenced by profile"),
            "should refuse deletion when profiles reference module, got: {err}"
        );
    }

    // ─── cmd_module_delete — with purge removes target files ────────

    #[test]
    fn cmd_module_delete_with_purge() {
        let dir = setup_config_dir();
        let target_dir = tempfile::tempdir().unwrap();
        let target_file = target_dir.path().join("deployed.conf");
        std::fs::write(&target_file, "deployed content").unwrap();

        let yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: purge-mod\nspec:\n  files:\n    - source: files/config\n      target: {}\n",
            target_file.display()
        );
        make_module(dir.path(), "purge-mod", &yaml);

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_delete(&cli, &printer, "purge-mod", true, true).unwrap();

        assert!(!target_file.exists(), "target file should be purged");
        let output = buf.lock().unwrap();
        assert!(
            output.contains("Purged"),
            "should report purge, got: {output}"
        );
    }

    // ─── cmd_module_delete — without purge restores symlinks ────────

    #[test]
    fn cmd_module_delete_restores_symlinked_files() {
        let dir = setup_config_dir();
        let target_dir = tempfile::tempdir().unwrap();
        let target_file = target_dir.path().join("restored.conf");
        let module_source = dir.path().join("modules/restore-mod/files/config");
        std::fs::create_dir_all(module_source.parent().unwrap()).unwrap();
        std::fs::write(&module_source, "original content").unwrap();

        // Create symlink from target to module source
        #[cfg(unix)]
        std::os::unix::fs::symlink(&module_source, &target_file).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&module_source, &target_file).unwrap();

        let yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: restore-mod\nspec:\n  files:\n    - source: files/config\n      target: {}\n",
            target_file.display()
        );
        // Write module.yaml (module dir already created above)
        std::fs::write(dir.path().join("modules/restore-mod/module.yaml"), &yaml).unwrap();

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_delete(&cli, &printer, "restore-mod", true, false).unwrap();

        assert!(
            target_file.exists(),
            "target file should still exist after restore"
        );
        assert!(
            !target_file.is_symlink(),
            "target should be a regular file, not a symlink"
        );
        let contents = std::fs::read_to_string(&target_file).unwrap();
        assert_eq!(contents, "original content", "content should be restored");

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Restored"),
            "should report restoration, got: {output}"
        );
    }

    // ─── cmd_module_delete — cleans lockfile entry ──────────────────

    #[test]
    fn cmd_module_delete_cleans_lockfile() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "lock-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: lock-mod\nspec:\n  packages: []\n",
        );

        // Add a lockfile entry
        let lockfile = cfgd_core::config::ModuleLockfile {
            modules: vec![cfgd_core::config::ModuleLockEntry {
                name: "lock-mod".to_string(),
                url: "https://example.com/mod.git@v1".to_string(),
                pinned_ref: "v1".to_string(),
                commit: "abc".to_string(),
                integrity: "sha256:123".to_string(),
                subdir: None,
            }],
        };
        modules::save_lockfile(dir.path(), &lockfile).unwrap();

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_delete(&cli, &printer, "lock-mod", true, false).unwrap();

        let lockfile = modules::load_lockfile(dir.path()).unwrap();
        assert!(
            lockfile.modules.is_empty(),
            "lockfile should be cleaned after module delete"
        );
        let output = buf.lock().unwrap();
        assert!(
            output.contains("modules.lock"),
            "should report lockfile cleanup, got: {output}"
        );
    }

    // ─── cmd_module_export — devcontainer with complex spec ─────────

    #[test]
    fn cmd_module_export_devcontainer_complex() {
        let dir = setup_config_dir();
        let yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: complex-tool
  description: A complex module
spec:
  depends:
    - base
  packages:
    - name: curl
    - name: custom-tool
      script: curl -sS https://install.sh | bash
      platforms:
        - macos
    - name: linux-pkg
      aliases:
        apt: linux-specific-pkg
  env:
    - name: TOOL_HOME
      value: /opt/tool
    - name: DEBUG
      value: "false"
  aliases:
    - name: ct
      command: custom-tool --verbose
  scripts:
    postApply:
      - echo setup complete
"#;
        make_module(dir.path(), "complex-tool", yaml);

        let output_dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let result = super::export_devcontainer(
            &cli,
            &printer,
            "complex-tool",
            Some(output_dir.path().to_str().unwrap()),
        );
        assert!(result.is_ok(), "export failed: {:?}", result);

        let feature_dir = output_dir.path().join("complex-tool");
        let install = std::fs::read_to_string(feature_dir.join("install.sh")).unwrap();

        // Verify apt packages include alias-mapped names
        assert!(
            install.contains("linux-specific-pkg"),
            "should use apt alias for linux-pkg, got:\n{install}"
        );
        // Verify script-based packages
        assert!(
            install.contains("curl -sS https://install.sh | bash"),
            "should include script-based install, got:\n{install}"
        );
        // Verify env vars
        assert!(
            install.contains("TOOL_HOME"),
            "should include env vars, got:\n{install}"
        );
        assert!(
            install.contains("DEBUG"),
            "should include env vars, got:\n{install}"
        );
        // Verify post-apply scripts
        assert!(
            install.contains("echo setup complete"),
            "should include post-apply scripts, got:\n{install}"
        );

        let feature_json =
            std::fs::read_to_string(feature_dir.join("devcontainer-feature.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&feature_json).unwrap();
        assert_eq!(parsed["id"], "complex-tool");
        assert_eq!(parsed["description"], "A complex module");
        // Options should have both env vars
        assert!(parsed["options"]["TOOL_HOME"].is_object());
        assert!(parsed["options"]["DEBUG"].is_object());
        // installsAfter should reference dependencies
        let installs_after = parsed["installsAfter"].as_array().unwrap();
        assert_eq!(installs_after.len(), 1);
        assert!(installs_after[0].as_str().unwrap().contains("base"));
    }

    // ─── cmd_module_list — JSON with active modules ─────────────────

    #[test]
    fn cmd_module_list_json_active_modules() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "active-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: active-mod\nspec:\n  packages:\n    - name: git\n",
        );
        make_module(
            dir.path(),
            "inactive-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: inactive-mod\nspec:\n  packages: []\n",
        );

        // Set profile to include active-mod
        let profile_path = dir.path().join("profiles/default.yaml");
        std::fs::write(
            &profile_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - active-mod\n",
        ).unwrap();

        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        let start = output.find('[').expect("should have JSON array");
        let json: serde_json::Value = serde_json::from_str(output[start..].trim()).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);

        let active = arr.iter().find(|e| e["name"] == "active-mod").unwrap();
        assert_eq!(active["active"], true);
        assert_eq!(active["status"], "pending");

        let inactive = arr.iter().find(|e| e["name"] == "inactive-mod").unwrap();
        assert_eq!(inactive["active"], false);
        assert_eq!(inactive["status"], "available");
    }

    // ─── cmd_module_list — table with active modules ────────────────

    #[test]
    fn cmd_module_list_table_active_modules() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "my-mod",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: my-mod\nspec:\n  depends:\n    - base\n  packages:\n    - name: curl\n    - name: wget\n  files:\n    - source: files/x\n      target: ~/.x\n",
        );

        let profile_path = dir.path().join("profiles/default.yaml");
        std::fs::write(
            &profile_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - my-mod\n",
        ).unwrap();

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        cmd_module_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("my-mod"),
            "should list module name, got: {output}"
        );
        assert!(
            output.contains("yes"),
            "should show active=yes, got: {output}"
        );
        assert!(
            output.contains("pending"),
            "should show pending status, got: {output}"
        );
    }

    // ─── cmd_module_list — with lockfile entry shows remote source ──

    #[test]
    fn cmd_module_list_with_lockfile_shows_remote() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "remote-listed",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: remote-listed\nspec:\n  packages: []\n",
        );

        let lockfile = cfgd_core::config::ModuleLockfile {
            modules: vec![cfgd_core::config::ModuleLockEntry {
                name: "remote-listed".to_string(),
                url: "https://example.com/mod.git@v1".to_string(),
                pinned_ref: "v1".to_string(),
                commit: "abc".to_string(),
                integrity: "sha256:123".to_string(),
                subdir: None,
            }],
        };
        modules::save_lockfile(dir.path(), &lockfile).unwrap();

        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        let start = output.find('[').expect("should have JSON array");
        let json: serde_json::Value = serde_json::from_str(output[start..].trim()).unwrap();
        let arr = json.as_array().unwrap();
        let entry = &arr[0];
        assert_eq!(entry["source"], "remote");
    }

    // ─── cmd_module_registry_remove — no config fails ──────────────

    #[test]
    fn cmd_module_registry_remove_no_config_fails() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_registry_remove(&cli, &printer, "test").unwrap_err();
        assert!(
            err.to_string().contains("cfgd.yaml"),
            "should fail without config, got: {err}"
        );
    }

    // ─── cmd_module_registry_remove — warns about profile references ─

    #[test]
    fn cmd_module_registry_remove_warns_profile_refs() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        // Add a registry
        cmd_module_registry_add(&cli, &printer, "https://example.com/reg.git", Some("team"))
            .unwrap();

        // Add a profile that references team/somemod
        let profile_path = dir.path().join("profiles/default.yaml");
        std::fs::write(
            &profile_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - team/somemod\n",
        ).unwrap();

        let (printer2, buf2) = cfgd_core::output::Printer::for_test();
        cmd_module_registry_remove(&cli, &printer2, "team").unwrap();

        let output = buf2.lock().unwrap();
        assert!(
            output.contains("Removed module registry 'team'"),
            "should confirm removal, got: {output}"
        );
        assert!(
            output.contains("still references"),
            "should warn about profile references, got: {output}"
        );
    }

    // ─── cmd_module_registry_list — JSON empty registries ───────────

    #[test]
    fn cmd_module_registry_list_json_empty() {
        let dir = setup_config_dir();
        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_registry_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    // ─── cmd_module_registry_list — JSON no config ──────────────────

    #[test]
    fn cmd_module_registry_list_json_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_registry_list(&cli, &printer).unwrap();

        let output = buf.lock().unwrap();
        let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    // ─── apply_module_sets — multiple sets at once ──────────────────

    #[test]
    fn apply_set_multiple_at_once() {
        let mut doc = make_module_doc(vec![make_pkg("curl"), make_pkg("vim")]);
        apply_module_sets(
            &[
                "package.curl.minVersion=8.0".into(),
                "package.curl.prefer=brew".into(),
                "package.vim.script=install-vim.sh".into(),
                "package.vim.alias.apt=vim-gtk3".into(),
            ],
            &mut doc,
        )
        .unwrap();

        assert_eq!(doc.spec.packages[0].min_version.as_deref(), Some("8.0"));
        assert_eq!(doc.spec.packages[0].prefer, vec!["brew"]);
        assert_eq!(
            doc.spec.packages[1].script.as_deref(),
            Some("install-vim.sh")
        );
        assert_eq!(doc.spec.packages[1].aliases.get("apt").unwrap(), "vim-gtk3");
    }

    // ─── apply_module_sets — alias path too short ───────────────────

    #[test]
    fn apply_set_alias_path_too_short() {
        let mut doc = make_module_doc(vec![make_pkg("vim")]);
        let err = apply_module_sets(&["package.vim.alias=nope".into()], &mut doc).unwrap_err();
        assert!(
            err.to_string()
                .contains("expected package.<name>.alias.<manager>"),
            "should require manager for alias, got: {err}"
        );
    }

    // ─── apply_module_sets — empty package name ─────────────────────

    #[test]
    fn apply_set_empty_package_name() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        let err = apply_module_sets(&["package..minVersion=1.0".into()], &mut doc).unwrap_err();
        assert!(
            err.to_string().contains("expected package."),
            "should reject empty package name, got: {err}"
        );
    }

    // ─── apply_module_sets — empty field name ───────────────────────

    #[test]
    fn apply_set_empty_field_name() {
        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        let err = apply_module_sets(&["package.curl.=value".into()], &mut doc).unwrap_err();
        assert!(
            err.to_string().contains("expected package."),
            "should reject empty field name, got: {err}"
        );
    }

    // ─── mask_value — unicode characters ────────────────────────────

    #[test]
    fn mask_value_unicode() {
        // Unicode chars can be multi-byte but mask_value uses .chars()
        assert_eq!(mask_value(""), "***");
        assert_eq!(mask_value("ab"), "***");
        assert_eq!(mask_value("abcde"), "***cde");
    }

    // ─── profiles_using_module — multiple profiles match ────────────

    #[test]
    fn profiles_using_module_multiple_matches() {
        let dir = tempfile::tempdir().unwrap();
        let yaml1 = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  inherits: []\n  modules:\n    - shared-mod\n";
        let yaml2 = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: home\nspec:\n  inherits: []\n  modules:\n    - shared-mod\n    - other-mod\n";
        let yaml3 = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: minimal\nspec:\n  inherits: []\n  modules:\n    - other-mod\n";
        std::fs::write(dir.path().join("work.yaml"), yaml1).unwrap();
        std::fs::write(dir.path().join("home.yaml"), yaml2).unwrap();
        std::fs::write(dir.path().join("minimal.yaml"), yaml3).unwrap();

        let result = profiles_using_module(dir.path(), "shared-mod").unwrap();
        assert_eq!(result.len(), 2, "should match 2 profiles");
        assert!(result.contains(&"work".to_string()));
        assert!(result.contains(&"home".to_string()));
    }

    // ─── profiles_using_module — ignores non-yaml files ─────────────

    #[test]
    fn profiles_using_module_ignores_non_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("notes.txt"),
            "not a yaml file with shared-mod",
        )
        .unwrap();

        let result = profiles_using_module(dir.path(), "shared-mod").unwrap();
        assert!(result.is_empty(), "should ignore non-yaml files");
    }

    // ─── profiles_using_module — handles invalid yaml gracefully ────

    #[test]
    fn profiles_using_module_invalid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.yaml"), "{{{{not valid yaml").unwrap();

        // Should not error — just skip invalid files
        let result = profiles_using_module(dir.path(), "shared-mod").unwrap();
        assert!(result.is_empty());
    }

    // ─── cmd_module_show — JSON with depends field ──────────────────

    #[test]
    fn cmd_module_show_json_depends() {
        let dir = setup_config_dir();
        make_module(
            dir.path(),
            "dep-show",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: dep-show\nspec:\n  depends:\n    - base\n    - core\n  packages: []\n",
        );

        let cli = test_cli_json(dir.path());
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

        cmd_module_show(&cli, &printer, "dep-show", false).unwrap();

        let output = buf.lock().unwrap();
        let json: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        let depends = json["depends"].as_array().unwrap();
        assert_eq!(depends.len(), 2);
        assert_eq!(depends[0], "base");
        assert_eq!(depends[1], "core");
    }

    // ─── cmd_module_update_local — manager-prefixed package removal ─

    #[test]
    fn cmd_module_update_remove_prefixed_package() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  packages:\n    - name: ripgrep\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        // Remove with manager prefix: -brew:ripgrep should strip prefix and remove "ripgrep"
        let args = super::ModuleUpdateArgs {
            packages: vec!["-brew:ripgrep".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert!(
            doc.spec.packages.is_empty(),
            "ripgrep should be removed even with brew: prefix"
        );

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Removed package: ripgrep"),
            "should confirm removal, got: {output}"
        );
    }

    // ─── cmd_module_update_local — add file already tracked is noop ─

    #[test]
    fn cmd_module_update_add_already_tracked_file_noop() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  files:\n    - source: files/config.toml\n      target: ~/.config/app/config.toml\n",
        );

        // Create a source file with the same basename
        let source_file = dir.path().join("config.toml");
        std::fs::write(&source_file, "content").unwrap();

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            files: vec![format!(
                "{}:~/.somewhere/config.toml",
                source_file.display()
            )],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let output = buf.lock().unwrap();
        assert!(
            output.contains("already in module"),
            "should note file already tracked, got: {output}"
        );

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.spec.files.len(), 1, "should not add duplicate file");
    }

    // ─── cmd_module_create — with description shows in output ───────

    #[test]
    fn cmd_module_create_description_and_depends_output() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleCreateArgs {
            description: Some("My awesome module".to_string()),
            depends: vec!["base".to_string(), "core".to_string()],
            packages: vec!["curl".to_string()],
            ..make_module_create_args("desc-mod")
        };
        cmd_module_create(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "desc-mod").unwrap();
        assert_eq!(
            doc.metadata.description,
            Some("My awesome module".to_string())
        );
        assert_eq!(doc.spec.depends, vec!["base", "core"]);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Dependencies"),
            "should show dependencies in output, got: {output}"
        );
        assert!(
            output.contains("base, core"),
            "should list dependencies, got: {output}"
        );
    }

    // ─── cmd_module_registry_rename — no config fails ───────────────

    #[test]
    fn cmd_module_registry_rename_no_config_fails() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let err = cmd_module_registry_rename(&cli, &printer, "old", "new").unwrap_err();
        assert!(
            err.to_string().contains("cfgd.yaml"),
            "should fail without config, got: {err}"
        );
    }

    // ─── ModuleListEntry — serde serialization ──────────────────────

    #[test]
    fn module_list_entry_json_fields() {
        let entry = ModuleListEntry {
            name: "test-mod".to_string(),
            active: true,
            source: "local".to_string(),
            status: "applied".to_string(),
            packages: 3,
            files: 2,
            depends: 1,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["name"], "test-mod");
        assert_eq!(json["active"], true);
        assert_eq!(json["source"], "local");
        assert_eq!(json["status"], "applied");
        assert_eq!(json["packages"], 3);
        assert_eq!(json["files"], 2);
        assert_eq!(json["depends"], 1);
    }

    // ─── ModuleShowOutput — serde serialization ─────────────────────

    #[test]
    fn module_show_output_json_fields() {
        let output = ModuleShowOutput {
            name: "test-mod".to_string(),
            directory: "/home/user/.config/cfgd/modules/test-mod".to_string(),
            source: "remote".to_string(),
            depends: vec!["base".to_string()],
            state: None,
            spec: config::ModuleSpec::default(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "test-mod");
        assert_eq!(json["source"], "remote");
        assert_eq!(json["depends"][0], "base");
        assert!(json["state"].is_null());
        assert!(json["spec"].is_object());
    }

    // ─── cmd_module_update_local — invalid name fails ───────────────

    #[test]
    fn cmd_module_update_invalid_name_fails() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        let args = make_module_update_args(".bad-name");
        let err = cmd_module_update_local(&cli, &printer, &args).unwrap_err();
        assert!(
            err.to_string().contains("cannot start with"),
            "should reject invalid module name, got: {err}"
        );
    }

    // ─── cmd_module_update_local — combined operations ──────────────

    #[test]
    fn cmd_module_update_combined_operations() {
        let dir = tempfile::tempdir().unwrap();
        make_module(
            dir.path(),
            "mod1",
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: mod1\nspec:\n  depends:\n    - base\n  packages:\n    - name: curl\n  env:\n    - name: EDITOR\n      value: vim\n  aliases:\n    - name: gs\n      command: git status\n",
        );

        let cli = test_cli(dir.path());
        let (printer, buf) = cfgd_core::output::Printer::for_test();

        let args = super::ModuleUpdateArgs {
            description: Some("Updated".to_string()),
            depends: vec!["core".to_string(), "-base".to_string()],
            packages: vec!["ripgrep".to_string(), "-curl".to_string()],
            env: vec!["PAGER=less".to_string(), "-EDITOR".to_string()],
            aliases: vec!["gp=git push".to_string(), "-gs".to_string()],
            post_apply: vec!["echo done".to_string()],
            ..make_module_update_args("mod1")
        };
        cmd_module_update_local(&cli, &printer, &args).unwrap();

        let (doc, _) = load_module_document(dir.path(), "mod1").unwrap();
        assert_eq!(doc.metadata.description, Some("Updated".to_string()));
        assert_eq!(doc.spec.depends, vec!["core"]);
        assert_eq!(doc.spec.packages.len(), 1);
        assert_eq!(doc.spec.packages[0].name, "ripgrep");
        assert_eq!(doc.spec.env.len(), 1);
        assert_eq!(doc.spec.env[0].name, "PAGER");
        assert_eq!(doc.spec.aliases.len(), 1);
        assert_eq!(doc.spec.aliases[0].name, "gp");
        assert!(doc.spec.scripts.is_some());

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Updated module 'mod1'"),
            "should confirm update, got: {output}"
        );
    }

    // ─── cmd_module_keys_rotate — no cosign fails ───────────────────

    #[test]
    fn cmd_module_keys_rotate_no_cosign_fails() {
        if cfgd_core::command_available("cosign") {
            return;
        }
        let (printer, _buf) = cfgd_core::output::Printer::for_test();
        let err = cmd_module_keys_rotate(&printer, None, &[]).unwrap_err();
        assert!(
            err.to_string().contains("cosign not found"),
            "should report cosign missing, got: {err}"
        );
    }

    // ─── cmd_module_keys_rotate — no existing key fails ─────────────

    #[test]
    fn cmd_module_keys_rotate_no_existing_key_fails() {
        if !cfgd_core::command_available("cosign") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (printer, _buf) = cfgd_core::output::Printer::for_test();
        let err =
            cmd_module_keys_rotate(&printer, Some(dir.path().to_str().unwrap()), &[]).unwrap_err();
        assert!(
            err.to_string().contains("No existing cosign.key"),
            "should report no existing key, got: {err}"
        );
    }

    // ─── save_module_document — writes valid yaml ───────────────────

    #[test]
    fn save_module_document_writes_valid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module.yaml");

        let mut doc = make_module_doc(vec![make_pkg("curl")]);
        doc.metadata.description = Some("Test description".to_string());
        doc.spec.depends = vec!["base".to_string()];
        doc.spec.env = vec![cfgd_core::config::EnvVar {
            name: "FOO".to_string(),
            value: "bar".to_string(),
        }];

        save_module_document(&doc, &path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: config::ModuleDocument = serde_yaml::from_str(&contents).unwrap();
        assert_eq!(parsed.metadata.name, "test");
        assert_eq!(
            parsed.metadata.description,
            Some("Test description".to_string())
        );
        assert_eq!(parsed.spec.depends, vec!["base"]);
        assert_eq!(parsed.spec.packages[0].name, "curl");
        assert_eq!(parsed.spec.env[0].name, "FOO");
    }

    // ─── cmd_module_export — module with no packages ────────────────

    #[test]
    fn cmd_module_export_devcontainer_no_packages() {
        let dir = setup_config_dir();
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-only\nspec:\n  env:\n    - name: MY_VAR\n      value: hello\n";
        make_module(dir.path(), "env-only", yaml);

        let output_dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let (printer, _buf) = cfgd_core::output::Printer::for_test();

        super::export_devcontainer(
            &cli,
            &printer,
            "env-only",
            Some(output_dir.path().to_str().unwrap()),
        )
        .unwrap();

        let install =
            std::fs::read_to_string(output_dir.path().join("env-only/install.sh")).unwrap();
        // Should not have apt-get install if no packages
        assert!(
            !install.contains("apt-get install"),
            "should not have apt install with no packages, got:\n{install}"
        );
        // Should still have env var setup
        assert!(
            install.contains("MY_VAR"),
            "should include env vars, got:\n{install}"
        );
    }
}

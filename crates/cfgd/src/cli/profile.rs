use std::path::{Path, PathBuf};

use super::*;

pub(super) fn cmd_profile_show(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Resolved Profile");

    let (_cfg, resolved) = load_config_and_profile(cli, printer)?;

    printer.newline();
    printer.subheader("Layers");
    for layer in &resolved.layers {
        printer.key_value(
            &layer.profile_name,
            &format!("source={} priority={}", layer.source, layer.priority),
        );
    }

    printer.newline();
    printer.subheader("Variables");
    if resolved.merged.variables.is_empty() {
        printer.info("(none)");
    } else {
        let mut vars: Vec<_> = resolved.merged.variables.iter().collect();
        vars.sort_by_key(|(k, _)| (*k).clone());
        for (key, value) in vars {
            let val_str = match value {
                serde_yaml::Value::String(s) => s.clone(),
                other => format!("{:?}", other),
            };
            printer.key_value(key, &val_str);
        }
    }

    printer.newline();
    printer.subheader("Packages");
    let pkgs = &resolved.merged.packages;
    let mut has_packages = false;
    if let Some(ref brew) = pkgs.brew {
        if !brew.taps.is_empty() {
            printer.key_value("brew taps", &brew.taps.join(", "));
            has_packages = true;
        }
        if !brew.formulae.is_empty() {
            printer.key_value("brew formulae", &brew.formulae.join(", "));
            has_packages = true;
        }
        if !brew.casks.is_empty() {
            printer.key_value("brew casks", &brew.casks.join(", "));
            has_packages = true;
        }
    }
    if let Some(ref apt) = pkgs.apt
        && !apt.packages.is_empty()
    {
        printer.key_value("apt", &apt.packages.join(", "));
        has_packages = true;
    }
    if let Some(ref cargo) = pkgs.cargo
        && !cargo.packages.is_empty()
    {
        printer.key_value("cargo", &cargo.packages.join(", "));
        has_packages = true;
    }
    if let Some(ref npm) = pkgs.npm
        && !npm.global.is_empty()
    {
        printer.key_value("npm", &npm.global.join(", "));
        has_packages = true;
    }
    if !pkgs.pipx.is_empty() {
        printer.key_value("pipx", &pkgs.pipx.join(", "));
        has_packages = true;
    }
    if !pkgs.dnf.is_empty() {
        printer.key_value("dnf", &pkgs.dnf.join(", "));
        has_packages = true;
    }
    if !has_packages {
        printer.info("(none)");
    }

    printer.newline();
    printer.subheader("Files");
    if resolved.merged.files.managed.is_empty() {
        printer.info("(none)");
    } else {
        for file in &resolved.merged.files.managed {
            printer.key_value(&file.source, &file.target.display().to_string());
        }
    }

    if !resolved.merged.system.is_empty() {
        printer.newline();
        printer.subheader("System");
        for key in resolved.merged.system.keys() {
            printer.key_value(key, "(configured)");
        }
    }

    if !resolved.merged.secrets.is_empty() {
        printer.newline();
        printer.subheader("Secrets");
        for secret in &resolved.merged.secrets {
            printer.key_value(&secret.source, &secret.target.display().to_string());
        }
    }

    Ok(())
}

pub(super) fn cmd_profile_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Available Profiles");

    let profiles_dir = profiles_dir(cli);

    if !profiles_dir.exists() {
        printer.warning(&format!(
            "Profiles directory not found: {}",
            profiles_dir.display()
        ));
        return Ok(());
    }

    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(&profiles_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            profiles.push(stem.to_string());
        }
    }

    profiles.sort();

    let active = cli.profile.clone().unwrap_or_else(|| {
        config::load_config(&cli.config)
            .map(|c| c.spec.profile.unwrap_or_default())
            .unwrap_or_default()
    });

    for name in &profiles {
        if *name == active {
            printer.success(&format!("{} (active)", name));
        } else {
            printer.info(name);
        }
    }

    if profiles.is_empty() {
        printer.info("No profiles found");
    }

    Ok(())
}

pub(super) fn cmd_profile_switch(name: &str, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Switch Profile");

    let config_path = PathBuf::from("cfgd.yaml");
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    // Verify the target profile exists
    let profiles_dir = PathBuf::from("profiles");
    let profile_path = profiles_dir.join(format!("{}.yaml", name));
    if !profile_path.exists() {
        // List available profiles for the error message
        let mut hint = String::new();
        if profiles_dir.exists() {
            let mut available = Vec::new();
            for entry in std::fs::read_dir(&profiles_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    available.push(stem.to_string());
                }
            }
            if !available.is_empty() {
                available.sort();
                hint = format!("\nAvailable profiles: {}", available.join(", "));
            }
        }
        anyhow::bail!(
            "Profile '{}' not found at {}{}",
            name,
            profile_path.display(),
            hint
        );
    }

    // Read current config, update profile field, write back
    let contents = std::fs::read_to_string(&config_path)?;
    let mut cfg: config::CfgdConfig = config::parse_config(&contents, &config_path)?;
    let old_profile = cfg.spec.profile.clone().unwrap_or_default();
    cfg.spec.profile = Some(name.to_string());

    let yaml = serde_yaml::to_string(&cfg)?;
    std::fs::write(&config_path, &yaml)?;

    printer.success(&format!("Switched profile: {} → {}", old_profile, name));
    printer.info(MSG_RUN_APPLY);

    Ok(())
}

// --- Profile CRUD ---

pub(super) fn profiles_inheriting(profiles_dir: &Path, name: &str) -> anyhow::Result<Vec<String>> {
    let mut result = Vec::new();
    if !profiles_dir.exists() {
        return Ok(result);
    }
    for entry in std::fs::read_dir(profiles_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "yaml" || e == "yml")
            && let Ok(doc) = config::load_profile(&path)
            && doc.spec.inherits.contains(&name.to_string())
        {
            result.push(doc.metadata.name.clone());
        }
    }
    Ok(result)
}

/// Parse a "manager:package" string into (manager, package).
pub(super) fn parse_manager_package(s: &str) -> anyhow::Result<(String, String)> {
    let (mgr, pkg) = s.split_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid package format '{}' — expected manager:package (e.g. brew:curl)",
            s
        )
    })?;
    if mgr.is_empty() || pkg.is_empty() {
        anyhow::bail!(
            "Invalid package format '{}' — manager and package name cannot be empty",
            s
        );
    }
    Ok((mgr.to_string(), pkg.to_string()))
}

pub(super) fn parse_secret_spec(s: &str) -> anyhow::Result<config::SecretSpec> {
    // Split on last colon so provider URLs like op://vault/item:~/target work correctly
    let (source, target) = s.rsplit_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid secret format '{}' — expected source:target (e.g. secrets/api-key.enc:~/.config/app/key)",
            s
        )
    })?;
    if source.is_empty() || target.is_empty() {
        anyhow::bail!(
            "Invalid secret format '{}' — source and target cannot be empty",
            s
        );
    }
    Ok(config::SecretSpec {
        source: source.to_string(),
        target: PathBuf::from(target),
        template: None,
        backend: None,
    })
}

fn update_script_list(
    scripts_opt: &mut Option<config::ScriptSpec>,
    add: &[PathBuf],
    remove: &[PathBuf],
    label: &str,
    field: fn(&mut config::ScriptSpec) -> &mut Vec<PathBuf>,
    printer: &Printer,
) -> u32 {
    let mut changes = 0u32;
    for script in add {
        let scripts = scripts_opt.get_or_insert_with(Default::default);
        let list = field(scripts);
        if list.contains(script) {
            printer.warning(&format!(
                "{} script '{}' already exists",
                label,
                script.display()
            ));
            continue;
        }
        list.push(script.clone());
        printer.success(&format!("Added {}: {}", label, script.display()));
        changes += 1;
    }
    for script in remove {
        if let Some(scripts) = scripts_opt.as_mut() {
            let list = field(scripts);
            let before = list.len();
            list.retain(|s| s != script);
            if list.len() < before {
                printer.success(&format!("Removed {}: {}", label, script.display()));
                changes += 1;
            } else {
                printer.warning(&format!(
                    "{} script '{}' not found",
                    label,
                    script.display()
                ));
            }
        } else {
            printer.warning(&format!(
                "{} script '{}' not found",
                label,
                script.display()
            ));
        }
    }
    changes
}

pub(super) fn cmd_profile_create(
    cli: &Cli,
    printer: &Printer,
    args: &ProfileCreateArgs,
) -> anyhow::Result<()> {
    let name = &args.name;
    let inherits = &args.inherits;
    let module_list = &args.modules;
    let pkg_list = &args.packages;
    let var_list = &args.variables;
    let sys_list = &args.system;
    let files = &args.files;
    let secret_list = &args.secrets;
    let pre_reconcile = &args.pre_reconcile;
    let post_reconcile = &args.post_reconcile;
    validate_resource_name(name, "Profile")?;
    printer.header(&format!("Create Profile: {}", name));

    let config_dir = config_dir(cli);
    let pdir = config_dir.join("profiles");
    std::fs::create_dir_all(&pdir)?;

    let profile_path = pdir.join(format!("{}.yaml", name));
    if profile_path.exists() {
        anyhow::bail!(
            "Profile '{}' already exists at {}",
            name,
            profile_path.display()
        );
    }

    // Verify inherited profiles exist
    for parent in inherits {
        let parent_path = pdir.join(format!("{}.yaml", parent));
        if !parent_path.exists() {
            anyhow::bail!("Parent profile '{}' not found", parent);
        }
    }

    // Interactive mode if no flags
    let is_interactive = inherits.is_empty()
        && module_list.is_empty()
        && pkg_list.is_empty()
        && var_list.is_empty()
        && sys_list.is_empty()
        && files.is_empty()
        && secret_list.is_empty()
        && pre_reconcile.is_empty()
        && post_reconcile.is_empty();

    let (inh, mods, pkgs_parsed, vars, sys) = if is_interactive {
        let inh_str = printer.prompt_text("Inherit from (comma-separated, or empty)", "")?;
        let inh: Vec<String> = if inh_str.is_empty() {
            Vec::new()
        } else {
            inh_str.split(',').map(|s| s.trim().to_string()).collect()
        };
        for parent in &inh {
            let parent_path = pdir.join(format!("{}.yaml", parent));
            if !parent_path.exists() {
                anyhow::bail!("Parent profile '{}' not found", parent);
            }
        }

        let mods_str = printer.prompt_text("Modules (comma-separated, or empty)", "")?;
        let mods: Vec<String> = if mods_str.is_empty() {
            Vec::new()
        } else {
            mods_str.split(',').map(|s| s.trim().to_string()).collect()
        };

        (inh, mods, Vec::new(), Vec::new(), Vec::new())
    } else {
        let pkgs = pkg_list
            .iter()
            .map(|s| parse_manager_package(s))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let vars = var_list.to_vec();
        let sys = sys_list.to_vec();
        (inherits.to_vec(), module_list.to_vec(), pkgs, vars, sys)
    };

    // Warn about modules that don't exist locally (could be remote)
    let modules_dir = config_dir.join("modules");
    for m in &mods {
        if !modules_dir.join(m).join("module.yaml").exists() {
            printer.warning(&format!(
                "Module '{}' not found locally — make sure it exists or is a remote module",
                m
            ));
        }
    }

    // Build packages spec
    let mut packages_spec = config::PackagesSpec::default();
    for (mgr, pkg) in &pkgs_parsed {
        packages::add_package(mgr, pkg, &mut packages_spec)?;
    }
    let has_packages = !pkgs_parsed.is_empty();

    // Build variables
    let mut variables = std::collections::HashMap::new();
    for v in &vars {
        let (key, value) = v
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid variable '{}' — expected key=value", v))?;
        variables.insert(
            key.to_string(),
            serde_yaml::Value::String(value.to_string()),
        );
    }

    // Build system settings
    let mut system = std::collections::HashMap::new();
    for s in &sys {
        let (key, value) = s.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("Invalid system setting '{}' — expected key=value", s)
        })?;
        system.insert(
            key.to_string(),
            serde_yaml::Value::String(value.to_string()),
        );
    }

    // Copy files
    let files_dir = config_dir.join("profiles").join(name).join("files");
    let copied = copy_files_to_dir(files, &files_dir)?;
    let is_private = args.private;
    let file_entries: Vec<config::ManagedFileSpec> = copied
        .iter()
        .map(|(basename, deploy_target)| config::ManagedFileSpec {
            source: format!("profiles/{}/files/{}", name, basename),
            target: deploy_target.clone(),
            strategy: None,
            private: is_private,
            origin: None,
        })
        .collect();
    if is_private {
        for (basename, _) in &copied {
            add_to_gitignore(
                &config_dir,
                &format!("profiles/{}/files/{}", name, basename),
            )?;
        }
    }

    // Build secrets
    let secrets = secret_list
        .iter()
        .map(|s| parse_secret_spec(s))
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Build scripts
    let scripts = if pre_reconcile.is_empty() && post_reconcile.is_empty() {
        None
    } else {
        Some(config::ScriptSpec {
            pre_reconcile: pre_reconcile.to_vec(),
            post_reconcile: post_reconcile.to_vec(),
        })
    };

    let doc = config::ProfileDocument {
        api_version: cfgd_core::API_VERSION.to_string(),
        kind: "Profile".to_string(),
        metadata: config::ProfileMetadata {
            name: name.to_string(),
        },
        spec: config::ProfileSpec {
            inherits: inh,
            modules: mods,
            variables,
            packages: if has_packages {
                Some(packages_spec)
            } else {
                None
            },
            files: if file_entries.is_empty() {
                None
            } else {
                Some(config::FilesSpec {
                    managed: file_entries,
                    permissions: std::collections::HashMap::new(),
                })
            },
            system,
            secrets,
            scripts,
        },
    };

    let yaml = serde_yaml::to_string(&doc)?;
    std::fs::write(&profile_path, &yaml)?;

    printer.success(&format!(
        "Created profile '{}' at {}",
        name,
        profile_path.display()
    ));
    if !doc.spec.inherits.is_empty() {
        printer.key_value("Inherits", &doc.spec.inherits.join(", "));
    }
    if !doc.spec.modules.is_empty() {
        printer.key_value("Modules", &doc.spec.modules.join(", "));
    }
    printer.newline();
    printer.info(&format!("Activate with: cfgd profile switch {}", name));

    maybe_update_workflow(cli, printer)?;

    Ok(())
}

pub(super) fn cmd_profile_update(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    args: &ProfileUpdateArgs,
) -> anyhow::Result<()> {
    let add_inherits = &args.add_inherits;
    let remove_inherits = &args.remove_inherits;
    let add_modules = &args.add_modules;
    let remove_modules = &args.remove_modules;
    let add_packages = &args.add_packages;
    let remove_packages = &args.remove_packages;
    let add_files = &args.add_files;
    let remove_files = &args.remove_files;
    let add_variables = &args.add_variables;
    let remove_variables = &args.remove_variables;
    let add_system = &args.add_system;
    let remove_system = &args.remove_system;
    let add_secrets = &args.add_secrets;
    let remove_secrets = &args.remove_secrets;
    let add_pre_reconcile = &args.add_pre_reconcile;
    let remove_pre_reconcile = &args.remove_pre_reconcile;
    let add_post_reconcile = &args.add_post_reconcile;
    let remove_post_reconcile = &args.remove_post_reconcile;
    validate_resource_name(name, "Profile")?;
    printer.header(&format!("Update Profile: {}", name));

    let config_dir = config_dir(cli);
    let profile_path = config_dir.join("profiles").join(format!("{}.yaml", name));
    if !profile_path.exists() {
        anyhow::bail!("Profile '{}' not found", name);
    }

    // Migrate old file layout if needed
    migrate_profile_file_layout(&config_dir, name, printer)?;

    let mut doc = config::load_profile(&profile_path)?;
    let mut changes = 0u32;

    // Add inherits
    let profiles_dir = config_dir.join("profiles");
    for parent in add_inherits {
        if doc.spec.inherits.contains(parent) {
            printer.warning(&format!("Profile already inherits '{}'", parent));
            continue;
        }
        let parent_path = profiles_dir.join(format!("{}.yaml", parent));
        if !parent_path.exists() {
            anyhow::bail!("Parent profile '{}' not found", parent);
        }
        doc.spec.inherits.push(parent.clone());
        printer.success(&format!("Added inherits: {}", parent));
        changes += 1;
    }

    // Remove inherits
    for parent in remove_inherits {
        let before = doc.spec.inherits.len();
        doc.spec.inherits.retain(|x| x != parent);
        if doc.spec.inherits.len() < before {
            printer.success(&format!("Removed inherits: {}", parent));
            changes += 1;
        } else {
            printer.warning(&format!("Inherits '{}' not found in profile", parent));
        }
    }

    // Add modules — detect remote references and handle accordingly
    let modules_dir = config_dir.join("modules");
    for m in add_modules {
        if doc.spec.modules.contains(m) {
            continue;
        }
        if modules::is_git_source(m) {
            // Remote git URL — fetch, lock, and add to profile
            // Save profile first with current changes, then delegate to remote add
            let yaml = serde_yaml::to_string(&doc)?;
            std::fs::write(&profile_path, &yaml)?;
            module::cmd_module_add_remote(cli, printer, m, None, false, false)?;
            // Reload profile (remote add may have modified it)
            doc = config::load_profile(&profile_path)?;
            changes += 1;
        } else if modules::is_registry_ref(m) {
            let yaml = serde_yaml::to_string(&doc)?;
            std::fs::write(&profile_path, &yaml)?;
            module::cmd_module_add_from_registry(cli, printer, m, false, false)?;
            doc = config::load_profile(&profile_path)?;
            changes += 1;
        } else {
            if !modules_dir.join(m).join("module.yaml").exists() {
                printer.warning(&format!(
                    "Module '{}' not found locally — make sure it exists or is a remote module",
                    m
                ));
            }
            doc.spec.modules.push(m.clone());
            printer.success(&format!("Added module: {}", m));
            changes += 1;
        }
    }

    // Remove modules
    for m in remove_modules {
        let before = doc.spec.modules.len();
        doc.spec.modules.retain(|x| x != m);
        if doc.spec.modules.len() < before {
            // Collect module file targets before cleanup for backup restore prompts
            let module_file_targets = collect_module_file_targets(m, &config_dir);

            // Clean up lockfile entry and cache if this was a remote module
            let mut lockfile = modules::load_lockfile(&config_dir)?;
            let removed_entry = lockfile.modules.iter().find(|e| e.name == *m).cloned();
            lockfile.modules.retain(|e| e.name != *m);
            if let Some(entry) = removed_entry {
                modules::save_lockfile(&config_dir, &lockfile)?;
                printer.info(&format!("Removed '{}' from modules.lock", m));
                if let Ok(git_src) = modules::parse_git_source(&entry.url) {
                    let cache_base = modules::default_module_cache_dir().unwrap_or_default();
                    let cache_dir = modules::git_cache_dir(&cache_base, &git_src.repo_url);
                    if cache_dir.exists() {
                        if let Err(e) = std::fs::remove_dir_all(&cache_dir) {
                            printer.warning(&format!("Failed to clean cache: {}", e));
                        } else {
                            printer.info("Cleaned cached checkout");
                        }
                    }
                }
            }
            // Clean module state from DB
            if let Ok(state) = open_state_store()
                && let Err(e) = state.remove_module_state(m)
            {
                printer.warning(&format!("Failed to clean module state: {}", e));
            }
            printer.success(&format!("Removed module: {}", m));
            changes += 1;

            // Check for .cfgd-backup files at module file targets and offer to restore
            prompt_restore_backups(&module_file_targets, printer)?;
        } else {
            printer.warning(&format!("Module '{}' not found in profile", m));
        }
    }

    // Add packages
    for pkg_str in add_packages {
        let (mgr, pkg) = parse_manager_package(pkg_str)?;
        let pkgs = doc.spec.packages.get_or_insert_with(Default::default);
        packages::add_package(&mgr, &pkg, pkgs)?;
        printer.success(&format!("Added package: {} ({})", pkg, mgr));
        changes += 1;
    }

    // Remove packages
    for pkg_str in remove_packages {
        let (mgr, pkg) = parse_manager_package(pkg_str)?;
        let pkgs = doc.spec.packages.get_or_insert_with(Default::default);
        if packages::remove_package(&mgr, &pkg, pkgs)? {
            printer.success(&format!("Removed package: {} ({})", pkg, mgr));
            changes += 1;
        } else {
            printer.warning(&format!("Package '{}' not found in {}", pkg, mgr));
        }
    }

    // Add files
    {
        let files_dir = config_dir.join("profiles").join(name).join("files");
        let copied = copy_files_to_dir(add_files, &files_dir)?;
        for (basename, deploy_target) in copied {
            let files = doc
                .spec
                .files
                .get_or_insert_with(config::FilesSpec::default);
            let source = format!("profiles/{}/files/{}", name, basename);
            if !files.managed.iter().any(|f| f.source == source) {
                if args.private {
                    add_to_gitignore(&config_dir, &source)?;
                }
                files.managed.push(config::ManagedFileSpec {
                    source,
                    target: deploy_target,
                    strategy: None,
                    private: args.private,
                    origin: None,
                });
                printer.success(&format!("Added file: {}", basename));
                changes += 1;
            }
        }
    }

    // Remove files
    for target in remove_files {
        let expanded = cfgd_core::expand_tilde(&PathBuf::from(target));
        if let Some(ref mut files) = doc.spec.files {
            let before = files.managed.len();
            let mut removed_source = None;
            files.managed.retain(|f| {
                let f_target = cfgd_core::expand_tilde(&f.target);
                if f_target == expanded {
                    removed_source = Some(f.source.clone());
                    false
                } else {
                    true
                }
            });
            if files.managed.len() < before {
                if let Some(ref source) = removed_source {
                    let source_path = config_dir.join(source);
                    if source_path.exists() {
                        std::fs::remove_file(&source_path)?;
                    }
                }
                printer.success(&format!("Removed file: {}", target));
                changes += 1;
            } else {
                printer.warning(&format!("File '{}' not found in profile", target));
            }
        }
    }

    // Add variables
    for v in add_variables {
        let (key, value) = v
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid variable '{}' — expected key=value", v))?;
        doc.spec.variables.insert(
            key.to_string(),
            serde_yaml::Value::String(value.to_string()),
        );
        printer.success(&format!("Set variable: {}={}", key, value));
        changes += 1;
    }

    // Remove variables
    for key in remove_variables {
        if doc.spec.variables.remove(key).is_some() {
            printer.success(&format!("Removed variable: {}", key));
            changes += 1;
        } else {
            printer.warning(&format!("Variable '{}' not found", key));
        }
    }

    // Add system settings
    for s in add_system {
        let (key, value) = s.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("Invalid system setting '{}' — expected key=value", s)
        })?;
        doc.spec.system.insert(
            key.to_string(),
            serde_yaml::Value::String(value.to_string()),
        );
        printer.success(&format!("Set system: {}={}", key, value));
        changes += 1;
    }

    // Remove system settings
    for key in remove_system {
        if doc.spec.system.remove(key).is_some() {
            printer.success(&format!("Removed system setting: {}", key));
            changes += 1;
        } else {
            printer.warning(&format!("System setting '{}' not found", key));
        }
    }

    // Add secrets
    for secret_str in add_secrets {
        let secret = parse_secret_spec(secret_str)?;
        let target = cfgd_core::expand_tilde(&secret.target);
        if doc
            .spec
            .secrets
            .iter()
            .any(|s| cfgd_core::expand_tilde(&s.target) == target)
        {
            printer.warning(&format!(
                "Secret targeting '{}' already exists",
                target.display()
            ));
            continue;
        }
        printer.success(&format!(
            "Added secret: {} → {}",
            secret.source,
            target.display()
        ));
        doc.spec.secrets.push(secret);
        changes += 1;
    }

    // Remove secrets
    for target_str in remove_secrets {
        let target = cfgd_core::expand_tilde(&PathBuf::from(target_str));
        let before = doc.spec.secrets.len();
        doc.spec
            .secrets
            .retain(|s| cfgd_core::expand_tilde(&s.target) != target);
        if doc.spec.secrets.len() < before {
            printer.success(&format!("Removed secret: {}", target_str));
            changes += 1;
        } else {
            printer.warning(&format!("Secret targeting '{}' not found", target_str));
        }
    }

    // Add/remove reconcile scripts
    changes += update_script_list(
        &mut doc.spec.scripts,
        add_pre_reconcile,
        remove_pre_reconcile,
        "pre-reconcile",
        |s| &mut s.pre_reconcile,
        printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        add_post_reconcile,
        remove_post_reconcile,
        "post-reconcile",
        |s| &mut s.post_reconcile,
        printer,
    );

    if changes == 0 {
        printer.info("No changes specified");
        return Ok(());
    }

    let yaml = serde_yaml::to_string(&doc)?;
    std::fs::write(&profile_path, &yaml)?;
    printer.newline();
    printer.success(&format!(
        "Updated profile '{}' ({} change(s))",
        name, changes
    ));

    Ok(())
}

pub(super) fn cmd_profile_edit(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    validate_resource_name(name, "Profile")?;
    let profile_path = profiles_dir(cli).join(format!("{}.yaml", name));
    if !profile_path.exists() {
        anyhow::bail!("Profile '{}' not found", name);
    }

    open_in_editor(&profile_path, printer)?;

    // Validate — loop until valid or user cancels
    loop {
        let contents = std::fs::read_to_string(&profile_path)?;
        match serde_yaml::from_str::<config::ProfileDocument>(&contents) {
            Ok(_) => {
                printer.success(&format!("Profile '{}' is valid", name));
                break;
            }
            Err(e) => {
                printer.error(&format!("Profile '{}' has errors: {}", name, e));
                if !printer.prompt_confirm("Re-open in editor?")? {
                    printer.warning("Saved with validation errors");
                    break;
                }
                open_in_editor(&profile_path, printer)?;
            }
        }
    }

    Ok(())
}

pub(super) fn cmd_profile_delete(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    yes: bool,
) -> anyhow::Result<()> {
    validate_resource_name(name, "Profile")?;
    printer.header(&format!("Delete Profile: {}", name));

    let config_dir = config_dir(cli);
    let pdir = profiles_dir(cli);
    let profile_path = pdir.join(format!("{}.yaml", name));

    if !profile_path.exists() {
        anyhow::bail!("Profile '{}' not found", name);
    }

    // Safety: refuse if active profile
    if cli.config.exists()
        && let Ok(cfg) = config::load_config(&cli.config)
        && cfg.spec.profile.as_deref() == Some(name)
    {
        anyhow::bail!(
            "Cannot delete '{}' — it is the active profile. Switch first with: cfgd profile switch <other>",
            name
        );
    }

    // Safety: refuse if inherited by other profiles
    let inheritors = profiles_inheriting(&pdir, name)?;
    if !inheritors.is_empty() {
        anyhow::bail!(
            "Cannot delete '{}' — inherited by: {}",
            name,
            inheritors.join(", ")
        );
    }

    if !yes && !printer.prompt_confirm(&format!("Delete profile '{}'?", name))? {
        printer.info("Cancelled");
        return Ok(());
    }

    std::fs::remove_file(&profile_path)?;

    // Clean up files directory if it exists (new layout)
    let files_dir = config_dir.join("profiles").join(name).join("files");
    if files_dir.exists() {
        std::fs::remove_dir_all(&files_dir)?;
    }
    // Also clean up old layout if present
    let old_files_dir = config_dir.join("files").join(name);
    if old_files_dir.exists() {
        std::fs::remove_dir_all(&old_files_dir)?;
    }

    printer.success(&format!("Deleted profile '{}'", name));

    maybe_update_workflow(cli, printer)?;

    Ok(())
}

/// Migrate profile files from old `files/<name>/` layout to `profiles/<name>/files/`.
/// Returns true if migration was performed.
pub(super) fn migrate_profile_file_layout(
    config_dir: &Path,
    profile_name: &str,
    printer: &Printer,
) -> anyhow::Result<bool> {
    let old_dir = config_dir.join("files").join(profile_name);
    let new_dir = config_dir.join("profiles").join(profile_name).join("files");

    if !old_dir.exists() || old_dir.read_dir()?.next().is_none() {
        return Ok(false);
    }

    // Move files from old to new location
    std::fs::create_dir_all(&new_dir)?;
    for entry in std::fs::read_dir(&old_dir)? {
        let entry = entry?;
        let dest = new_dir.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            cfgd_core::copy_dir_recursive(&entry.path(), &dest)?;
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::rename(entry.path(), &dest)?;
        }
    }
    std::fs::remove_dir_all(&old_dir)?;

    // Update source paths in profile YAML
    let profile_path = config_dir
        .join("profiles")
        .join(format!("{}.yaml", profile_name));
    if profile_path.exists() {
        let mut doc = config::load_profile(&profile_path)?;
        let old_prefix = format!("files/{}/", profile_name);
        let new_prefix = format!("profiles/{}/files/", profile_name);
        let mut updated = false;
        if let Some(ref mut files) = doc.spec.files {
            for managed in &mut files.managed {
                if managed.source.starts_with(&old_prefix) {
                    managed.source = managed.source.replacen(&old_prefix, &new_prefix, 1);
                    updated = true;
                }
            }
        }
        if updated {
            let yaml = serde_yaml::to_string(&doc)?;
            std::fs::write(&profile_path, &yaml)?;
        }
    }

    printer.info(&format!(
        "Migrated profile '{}' files to profiles/{}/files/",
        profile_name, profile_name
    ));
    Ok(true)
}

/// Scan for all profiles with old-style `files/<name>/` directories and migrate them.
pub(super) fn migrate_all_profile_file_layouts(
    config_dir: &Path,
    printer: &Printer,
) -> anyhow::Result<()> {
    let old_files_dir = config_dir.join("files");
    if !old_files_dir.exists() {
        return Ok(());
    }
    let entries: Vec<_> = std::fs::read_dir(&old_files_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Only migrate if a matching profile YAML exists
        let profile_path = config_dir.join("profiles").join(format!("{}.yaml", name));
        if profile_path.exists() {
            migrate_profile_file_layout(config_dir, &name, printer)?;
        }
    }
    Ok(())
}

/// Collect expanded file target paths from a module's definition.
/// Returns an empty vec if the module can't be loaded (already cleaned up, etc.).
fn collect_module_file_targets(module_name: &str, config_dir: &Path) -> Vec<PathBuf> {
    // Try local module first
    let module_dir = config_dir.join("modules").join(module_name);
    if let Ok(loaded) = modules::load_module(&module_dir) {
        return loaded
            .spec
            .files
            .iter()
            .map(|f| cfgd_core::expand_tilde(&PathBuf::from(&f.target)))
            .collect();
    }

    // Try cached remote module
    if let Ok(cache_base) = modules::default_module_cache_dir() {
        let lockfile = modules::load_lockfile(config_dir).unwrap_or_default();
        if let Some(entry) = lockfile.modules.iter().find(|e| e.name == module_name)
            && let Ok(git_src) = modules::parse_git_source(&entry.url)
        {
            let cache_dir = modules::git_cache_dir(&cache_base, &git_src.repo_url);
            if let Ok(loaded) = modules::load_module(&cache_dir) {
                return loaded
                    .spec
                    .files
                    .iter()
                    .map(|f| cfgd_core::expand_tilde(&PathBuf::from(&f.target)))
                    .collect();
            }
        }
    }

    Vec::new()
}

/// After removing a module, check if any of its file targets have `.cfgd-backup` files
/// and prompt the user to restore them.
fn prompt_restore_backups(targets: &[PathBuf], printer: &Printer) -> anyhow::Result<()> {
    for target in targets {
        let backup_path = PathBuf::from(format!("{}.cfgd-backup", target.display()));
        if backup_path.exists() {
            let confirmed = printer
                .prompt_confirm(&format!(
                    "Restore backup {} to {}?",
                    backup_path.display(),
                    target.display()
                ))
                .unwrap_or(false);
            if confirmed {
                // Remove the cfgd-managed file/symlink if it exists
                if target.symlink_metadata().is_ok() {
                    std::fs::remove_file(target)?;
                }
                std::fs::rename(&backup_path, target)?;
                printer.success(&format!("Restored {}", target.display()));
            }
        }
    }
    Ok(())
}

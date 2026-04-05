use super::*;

pub(super) fn cmd_profile_show(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
) -> anyhow::Result<()> {
    let (_cfg, resolved) = match name {
        Some(n) => {
            let cfg = config::load_config(&cli.config)?;
            printer.key_value("Config", &cli.config.display().to_string());
            printer.key_value("Profile", n);
            let resolved = config::resolve_profile(n, &profiles_dir(cli))?;
            (cfg, resolved)
        }
        None => load_config_and_profile(cli, printer)?,
    };

    if printer.write_structured(&resolved) {
        return Ok(());
    }

    printer.header("Resolved Profile");
    printer.newline();
    printer.subheader("Layers");
    for layer in &resolved.layers {
        printer.key_value(
            &layer.profile_name,
            &format!("source={} priority={}", layer.source, layer.priority),
        );
    }

    printer.newline();
    printer.subheader("Env");
    if resolved.merged.env.is_empty() {
        printer.info("(none)");
    } else {
        let mut env: Vec<_> = resolved.merged.env.iter().collect();
        env.sort_by(|a, b| a.name.cmp(&b.name));
        for ev in env {
            printer.key_value(&ev.name, &ev.value);
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
    for (name, list) in pkgs.non_empty_simple_lists() {
        printer.key_value(name, &list.join(", "));
        has_packages = true;
    }
    if let Some(ref snap) = pkgs.snap
        && !snap.packages.is_empty()
    {
        printer.key_value("snap", &snap.packages.join(", "));
        has_packages = true;
    }
    if let Some(ref flatpak) = pkgs.flatpak
        && !flatpak.packages.is_empty()
    {
        printer.key_value("flatpak", &flatpak.packages.join(", "));
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
            let value = match (&secret.target, &secret.envs) {
                (Some(t), Some(envs)) => {
                    format!("{} (envs: {})", t.display(), envs.join(", "))
                }
                (Some(t), None) => t.display().to_string(),
                (None, Some(envs)) => format!("envs: {}", envs.join(", ")),
                (None, None) => "(invalid)".to_string(),
            };
            printer.key_value(&secret.source, &value);
        }
    }

    Ok(())
}

pub(super) fn cmd_profile_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let profiles_dir = profiles_dir(cli);

    if !profiles_dir.exists() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<super::ProfileListEntry>::new());
            return Ok(());
        }
        printer.header("Available Profiles");
        printer.warning(&format!(
            "Profiles directory not found: {}",
            profiles_dir.display()
        ));
        return Ok(());
    }

    let profiles = super::list_yaml_stems(&profiles_dir)?;

    let active = cli.profile.clone().unwrap_or_else(|| {
        config::load_config(&cli.config)
            .map(|c| c.spec.profile.unwrap_or_default())
            .unwrap_or_default()
    });

    let entries: Vec<super::ProfileListEntry> = profiles
        .iter()
        .map(|name| {
            let profile_path = profiles_dir.join(format!("{}.yaml", name));
            let (inherits, module_count) = if let Ok(doc) = config::load_profile(&profile_path) {
                let inh = if doc.spec.inherits.is_empty() {
                    None
                } else {
                    Some(doc.spec.inherits.join(", "))
                };
                (inh, doc.spec.modules.len())
            } else {
                // Try .yml extension
                let yml_path = profiles_dir.join(format!("{}.yml", name));
                if let Ok(doc) = config::load_profile(&yml_path) {
                    let inh = if doc.spec.inherits.is_empty() {
                        None
                    } else {
                        Some(doc.spec.inherits.join(", "))
                    };
                    (inh, doc.spec.modules.len())
                } else {
                    (None, 0)
                }
            };
            super::ProfileListEntry {
                name: name.clone(),
                active: *name == active,
                inherits,
                module_count,
            }
        })
        .collect();

    if printer.write_structured(&entries) {
        return Ok(());
    }

    printer.header("Available Profiles");

    if printer.is_wide() {
        let rows: Vec<Vec<String>> = entries
            .iter()
            .map(|e| {
                vec![
                    e.name.clone(),
                    if e.active { "yes" } else { "-" }.to_string(),
                    e.inherits.clone().unwrap_or_else(|| "-".into()),
                    e.module_count.to_string(),
                ]
            })
            .collect();
        printer.table(&["Profile", "Active", "Inherits", "Modules"], &rows);
    } else {
        for entry in &entries {
            if entry.active {
                printer.success(&format!("{} (active)", entry.name));
            } else {
                printer.info(&entry.name);
            }
        }
    }

    if entries.is_empty() {
        printer.info("No profiles found");
    }

    Ok(())
}

pub(super) fn cmd_profile_switch(cli: &Cli, name: &str, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Switch Profile");

    let config_dir = super::config_dir(cli);
    let config_path = config_dir.join("cfgd.yaml");
    if !config_path.exists() {
        anyhow::bail!("{}", MSG_NO_CONFIG);
    }

    // Verify the target profile exists
    let profiles_dir = config_dir.join("profiles");
    let profile_path = profiles_dir.join(format!("{}.yaml", name));
    if !profile_path.exists() {
        // List available profiles for the error message
        let mut hint = String::new();
        let available = super::list_yaml_stems(&profiles_dir).unwrap_or_default();
        if !available.is_empty() {
            hint = format!("\nAvailable profiles: {}", available.join(", "));
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
    cfgd_core::atomic_write_str(&config_path, &yaml)?;

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
        target: Some(PathBuf::from(target)),
        template: None,
        backend: None,
        envs: None,
    })
}

fn update_script_list(
    scripts_opt: &mut Option<config::ScriptSpec>,
    add: &[String],
    remove: &[String],
    label: &str,
    field: fn(&mut config::ScriptSpec) -> &mut Vec<config::ScriptEntry>,
    printer: &Printer,
) -> u32 {
    let mut changes = 0u32;
    for script in add {
        let scripts = scripts_opt.get_or_insert_with(Default::default);
        let list = field(scripts);
        let entry = config::ScriptEntry::Simple(script.clone());
        if list.contains(&entry) {
            printer.warning(&format!("{} script '{}' already exists", label, script));
            continue;
        }
        list.push(entry);
        printer.success(&format!("Added {}: {}", label, script));
        changes += 1;
    }
    for script in remove {
        if let Some(scripts) = scripts_opt.as_mut() {
            let list = field(scripts);
            let before = list.len();
            list.retain(|e| e.run_str() != script.as_str());
            if list.len() < before {
                printer.success(&format!("Removed {}: {}", label, script));
                changes += 1;
            } else {
                printer.warning(&format!("{} script '{}' not found", label, script));
            }
        } else {
            printer.warning(&format!("{} script '{}' not found", label, script));
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
    let var_list = &args.env;
    let alias_list = &args.aliases;
    let sys_list = &args.system;
    let files = &args.files;
    let secret_list = &args.secrets;
    let pre_apply = &args.pre_apply;
    let post_apply = &args.post_apply;
    let pre_reconcile = &args.pre_reconcile;
    let post_reconcile = &args.post_reconcile;
    let on_change = &args.on_change;
    let on_drift = &args.on_drift;
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
        && alias_list.is_empty()
        && sys_list.is_empty()
        && files.is_empty()
        && secret_list.is_empty()
        && pre_apply.is_empty()
        && post_apply.is_empty()
        && pre_reconcile.is_empty()
        && post_reconcile.is_empty()
        && on_change.is_empty()
        && on_drift.is_empty();

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
        let known = super::known_manager_names();
        let known_refs: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
        let default_mgr = Platform::detect().native_manager().to_string();
        let pkgs = pkg_list
            .iter()
            .map(|s| {
                let (mgr, pkg) = super::parse_package_flag(s, &known_refs);
                (mgr.unwrap_or_else(|| default_mgr.clone()), pkg)
            })
            .collect::<Vec<_>>();
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

    // Build env
    let mut env_vars = Vec::new();
    for v in &vars {
        env_vars.push(cfgd_core::parse_env_var(v).map_err(|e| anyhow::anyhow!(e))?);
    }

    // Build aliases
    let mut shell_aliases = Vec::new();
    for a in alias_list {
        shell_aliases.push(cfgd_core::parse_alias(a).map_err(|e| anyhow::anyhow!(e))?);
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
            encryption: None,
            permissions: None,
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
    let has_scripts = !pre_apply.is_empty()
        || !post_apply.is_empty()
        || !pre_reconcile.is_empty()
        || !post_reconcile.is_empty()
        || !on_change.is_empty()
        || !on_drift.is_empty();
    let scripts = if !has_scripts {
        None
    } else {
        Some(config::ScriptSpec {
            pre_apply: pre_apply
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            post_apply: post_apply
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            pre_reconcile: pre_reconcile
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            post_reconcile: post_reconcile
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            on_change: on_change
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
            on_drift: on_drift
                .iter()
                .map(|s| config::ScriptEntry::Simple(s.clone()))
                .collect(),
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
            env: env_vars,
            aliases: shell_aliases,
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
    cfgd_core::atomic_write_str(&profile_path, &yaml)?;

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
    let (add_inherits, remove_inherits) = cfgd_core::split_add_remove(&args.inherits);
    let (add_modules, remove_modules) = cfgd_core::split_add_remove(&args.modules);
    let (add_packages, remove_packages) = cfgd_core::split_add_remove(&args.packages);
    let (add_files, remove_files) = cfgd_core::split_add_remove(&args.files);
    let (add_env, remove_env) = cfgd_core::split_add_remove(&args.env);
    let (add_aliases, remove_aliases) = cfgd_core::split_add_remove(&args.aliases);
    let (add_system, remove_system) = cfgd_core::split_add_remove(&args.system);
    let (add_secrets, remove_secrets) = cfgd_core::split_add_remove(&args.secrets);
    let (add_pre_apply, remove_pre_apply) = cfgd_core::split_add_remove(&args.pre_apply);
    let (add_post_apply, remove_post_apply) = cfgd_core::split_add_remove(&args.post_apply);
    let (add_pre_reconcile, remove_pre_reconcile) =
        cfgd_core::split_add_remove(&args.pre_reconcile);
    let (add_post_reconcile, remove_post_reconcile) =
        cfgd_core::split_add_remove(&args.post_reconcile);
    let (add_on_change, remove_on_change) = cfgd_core::split_add_remove(&args.on_change);
    let (add_on_drift, remove_on_drift) = cfgd_core::split_add_remove(&args.on_drift);
    validate_resource_name(name, "Profile")?;
    printer.header(&format!("Update Profile: {}", name));

    let config_dir = config_dir(cli);
    let profile_path = config_dir.join("profiles").join(format!("{}.yaml", name));
    if !profile_path.exists() {
        anyhow::bail!("Profile '{}' not found", name);
    }

    let mut doc = config::load_profile(&profile_path)?;
    let mut changes = 0u32;

    // Add inherits
    let profiles_dir = config_dir.join("profiles");
    for parent in &add_inherits {
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
    for parent in &remove_inherits {
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
    for m in &add_modules {
        if doc.spec.modules.contains(m) {
            continue;
        }
        if modules::is_git_source(m) {
            // Remote git URL — fetch, lock, and add to profile
            // Save profile first with current changes, then delegate to remote add
            let yaml = serde_yaml::to_string(&doc)?;
            cfgd_core::atomic_write_str(&profile_path, &yaml)?;
            module::cmd_module_add_remote(cli, printer, m, None, false, false)?;
            // Reload profile (remote add may have modified it)
            doc = config::load_profile(&profile_path)?;
            changes += 1;
        } else if modules::is_registry_ref(m) {
            let yaml = serde_yaml::to_string(&doc)?;
            cfgd_core::atomic_write_str(&profile_path, &yaml)?;
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
    for m in &remove_modules {
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
            // Clean module state and deployed files from DB
            if let Ok(state) = open_state_store(cli.state_dir.as_deref()) {
                // Query file manifest for deployed files
                if let Ok(manifest) = state.module_deployed_files(m) {
                    if !manifest.is_empty() {
                        printer.info(&format!(
                            "Module '{}' deployed {} file(s):",
                            m,
                            manifest.len()
                        ));
                        for f in &manifest {
                            printer.info(&format!("  {}", f.file_path));
                        }

                        let should_clean = printer
                            .prompt_confirm(
                                "Remove deployed files? Backups will be restored where available.",
                            )
                            .unwrap_or(false);

                        if should_clean {
                            for f in &manifest {
                                let path = std::path::Path::new(&f.file_path);
                                // Try to restore from backup store
                                if let Ok(Some(backup)) = state.latest_backup_for_path(&f.file_path)
                                {
                                    if backup.was_symlink {
                                        let _ = std::fs::remove_file(path);
                                        if let Some(ref link_target) = backup.symlink_target {
                                            let _ = cfgd_core::create_symlink(
                                                std::path::Path::new(link_target),
                                                path,
                                            );
                                        }
                                        printer
                                            .info(&format!("  Restored symlink: {}", f.file_path));
                                    } else if !backup.oversized && !backup.content.is_empty() {
                                        let _ = cfgd_core::atomic_write(path, &backup.content);
                                        printer.info(&format!("  Restored: {}", f.file_path));
                                    }
                                } else if path.exists() || path.symlink_metadata().is_ok() {
                                    let _ = std::fs::remove_file(path);
                                    printer.info(&format!("  Removed: {}", f.file_path));
                                }
                            }
                        }
                    }
                    let _ = state.delete_module_files(m);
                }

                if let Err(e) = state.remove_module_state(m) {
                    printer.warning(&format!("Failed to clean module state: {}", e));
                }
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
    let known = super::known_manager_names();
    let known_refs: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
    let default_mgr = Platform::detect().native_manager().to_string();
    for pkg_str in &add_packages {
        let (mgr_opt, pkg) = super::parse_package_flag(pkg_str, &known_refs);
        let mgr = mgr_opt.unwrap_or_else(|| default_mgr.clone());
        let pkgs = doc.spec.packages.get_or_insert_with(Default::default);
        packages::add_package(&mgr, &pkg, pkgs)?;
        printer.success(&format!("Added package: {} ({})", pkg, mgr));
        changes += 1;
    }

    // Remove packages
    for pkg_str in &remove_packages {
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
        let copied = copy_files_to_dir(&add_files, &files_dir)?;
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
                    encryption: None,
                    permissions: None,
                });
                printer.success(&format!("Added file: {}", basename));
                changes += 1;
            }
        }
    }

    // Remove files
    for target in &remove_files {
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

    // Add env vars
    for v in &add_env {
        let ev = cfgd_core::parse_env_var(v).map_err(|e| anyhow::anyhow!(e))?;
        cfgd_core::merge_env(&mut doc.spec.env, std::slice::from_ref(&ev));
        printer.success(&format!("Set env: {}={}", ev.name, ev.value));
        changes += 1;
    }

    // Remove env vars
    for key in &remove_env {
        let before = doc.spec.env.len();
        doc.spec.env.retain(|e| e.name != *key);
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

    // Add system settings
    for s in &add_system {
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
    for key in &remove_system {
        if doc.spec.system.remove(key.as_str()).is_some() {
            printer.success(&format!("Removed system setting: {}", key));
            changes += 1;
        } else {
            printer.warning(&format!("System setting '{}' not found", key));
        }
    }

    // Add secrets
    for secret_str in &add_secrets {
        let secret = parse_secret_spec(secret_str)?;
        // parse_secret_spec always produces a target
        let target = match secret.target.as_ref() {
            Some(t) => cfgd_core::expand_tilde(t),
            None => anyhow::bail!("secret parsed without target"),
        };
        if doc
            .spec
            .secrets
            .iter()
            .any(|s| s.target.as_ref().map(|t| cfgd_core::expand_tilde(t)) == Some(target.clone()))
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
    for target_str in &remove_secrets {
        let target = cfgd_core::expand_tilde(&PathBuf::from(target_str));
        let before = doc.spec.secrets.len();
        doc.spec.secrets.retain(|s| {
            s.target.as_ref().map(|t| cfgd_core::expand_tilde(t)) != Some(target.clone())
        });
        if doc.spec.secrets.len() < before {
            printer.success(&format!("Removed secret: {}", target_str));
            changes += 1;
        } else {
            printer.warning(&format!("Secret targeting '{}' not found", target_str));
        }
    }

    // Add/remove script hooks
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_pre_apply,
        &remove_pre_apply,
        "preApply",
        |s| &mut s.pre_apply,
        printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_post_apply,
        &remove_post_apply,
        "postApply",
        |s| &mut s.post_apply,
        printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_pre_reconcile,
        &remove_pre_reconcile,
        "preReconcile",
        |s| &mut s.pre_reconcile,
        printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_post_reconcile,
        &remove_post_reconcile,
        "postReconcile",
        |s| &mut s.post_reconcile,
        printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_on_change,
        &remove_on_change,
        "onChange",
        |s| &mut s.on_change,
        printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_on_drift,
        &remove_on_drift,
        "onDrift",
        |s| &mut s.on_drift,
        printer,
    );

    if changes == 0 {
        printer.info("No changes specified");
        return Ok(());
    }

    let yaml = serde_yaml::to_string(&doc)?;
    cfgd_core::atomic_write_str(&profile_path, &yaml)?;
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

    printer.success(&format!("Deleted profile '{}'", name));

    maybe_update_workflow(cli, printer)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_manager_package ---

    #[test]
    fn parse_manager_package_valid_brew() {
        let (mgr, pkg) = parse_manager_package("brew:curl").unwrap();
        assert_eq!(mgr, "brew");
        assert_eq!(pkg, "curl");
    }

    #[test]
    fn parse_manager_package_valid_cargo() {
        let (mgr, pkg) = parse_manager_package("cargo:bat").unwrap();
        assert_eq!(mgr, "cargo");
        assert_eq!(pkg, "bat");
    }

    #[test]
    fn parse_manager_package_missing_colon() {
        assert!(parse_manager_package("brewcurl").is_err());
    }

    #[test]
    fn parse_manager_package_empty_manager() {
        assert!(parse_manager_package(":curl").is_err());
    }

    #[test]
    fn parse_manager_package_empty_package() {
        assert!(parse_manager_package("brew:").is_err());
    }

    // --- parse_secret_spec ---

    #[test]
    fn parse_secret_spec_valid_simple() {
        let spec = parse_secret_spec("secrets/api-key.enc:~/.config/app/key").unwrap();
        assert_eq!(spec.source, "secrets/api-key.enc");
        assert_eq!(spec.target, Some(PathBuf::from("~/.config/app/key")));
        assert!(spec.template.is_none());
        assert!(spec.backend.is_none());
        assert!(spec.envs.is_none());
    }

    #[test]
    fn parse_secret_spec_op_url() {
        // rsplit_once should split on the LAST colon, so op://vault/item stays together
        let spec = parse_secret_spec("op://vault/item:~/target").unwrap();
        assert_eq!(spec.source, "op://vault/item");
        assert_eq!(spec.target, Some(PathBuf::from("~/target")));
    }

    #[test]
    fn parse_secret_spec_missing_colon() {
        assert!(parse_secret_spec("noseparator").is_err());
    }

    #[test]
    fn parse_secret_spec_empty_source() {
        assert!(parse_secret_spec(":target").is_err());
    }

    #[test]
    fn parse_secret_spec_empty_target() {
        assert!(parse_secret_spec("source:").is_err());
    }

    // --- update_script_list ---

    fn make_printer() -> Printer {
        Printer::new(cfgd_core::output::Verbosity::Quiet)
    }

    #[test]
    fn update_script_list_add_to_empty() {
        let printer = make_printer();
        let mut scripts: Option<config::ScriptSpec> = None;
        let add = vec!["setup.sh".to_string()];
        let changes = update_script_list(
            &mut scripts,
            &add,
            &[],
            "preApply",
            |s| &mut s.pre_apply,
            &printer,
        );
        assert_eq!(changes, 1);
        assert!(scripts.is_some());
        assert_eq!(
            scripts.unwrap().pre_apply,
            vec![config::ScriptEntry::Simple("setup.sh".to_string())]
        );
    }

    #[test]
    fn update_script_list_add_to_existing() {
        let printer = make_printer();
        let mut scripts = Some(config::ScriptSpec {
            pre_apply: vec![config::ScriptEntry::Simple("a.sh".to_string())],
            ..Default::default()
        });
        let add = vec!["b.sh".to_string()];
        let changes = update_script_list(
            &mut scripts,
            &add,
            &[],
            "preApply",
            |s| &mut s.pre_apply,
            &printer,
        );
        assert_eq!(changes, 1);
        assert_eq!(
            scripts.unwrap().pre_apply,
            vec![
                config::ScriptEntry::Simple("a.sh".to_string()),
                config::ScriptEntry::Simple("b.sh".to_string())
            ]
        );
    }

    #[test]
    fn update_script_list_add_duplicate() {
        let printer = make_printer();
        let mut scripts = Some(config::ScriptSpec {
            pre_apply: vec![config::ScriptEntry::Simple("a.sh".to_string())],
            ..Default::default()
        });
        let add = vec!["a.sh".to_string()];
        let changes = update_script_list(
            &mut scripts,
            &add,
            &[],
            "preApply",
            |s| &mut s.pre_apply,
            &printer,
        );
        assert_eq!(changes, 0);
        assert_eq!(scripts.unwrap().pre_apply.len(), 1);
    }

    #[test]
    fn update_script_list_remove_existing() {
        let printer = make_printer();
        let mut scripts = Some(config::ScriptSpec {
            pre_apply: vec![
                config::ScriptEntry::Simple("a.sh".to_string()),
                config::ScriptEntry::Simple("b.sh".to_string()),
            ],
            ..Default::default()
        });
        let remove = vec!["a.sh".to_string()];
        let changes = update_script_list(
            &mut scripts,
            &[],
            &remove,
            "preApply",
            |s| &mut s.pre_apply,
            &printer,
        );
        assert_eq!(changes, 1);
        assert_eq!(
            scripts.unwrap().pre_apply,
            vec![config::ScriptEntry::Simple("b.sh".to_string())]
        );
    }

    #[test]
    fn update_script_list_remove_nonexistent() {
        let printer = make_printer();
        let mut scripts = Some(config::ScriptSpec {
            pre_apply: vec![config::ScriptEntry::Simple("a.sh".to_string())],
            ..Default::default()
        });
        let remove = vec!["nope.sh".to_string()];
        let changes = update_script_list(
            &mut scripts,
            &[],
            &remove,
            "preApply",
            |s| &mut s.pre_apply,
            &printer,
        );
        assert_eq!(changes, 0);
    }

    #[test]
    fn update_script_list_remove_from_none() {
        let printer = make_printer();
        let mut scripts: Option<config::ScriptSpec> = None;
        let remove = vec!["nope.sh".to_string()];
        let changes = update_script_list(
            &mut scripts,
            &[],
            &remove,
            "preApply",
            |s| &mut s.pre_apply,
            &printer,
        );
        assert_eq!(changes, 0);
        assert!(scripts.is_none());
    }

    // --- profiles_inheriting ---

    #[test]
    fn profiles_inheriting_no_dir() {
        let result = profiles_inheriting(Path::new("/nonexistent-dir-12345"), "base").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn profiles_inheriting_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - other\n  modules: []\n".to_string();
        std::fs::write(dir.path().join("child.yaml"), &profile).unwrap();

        let result = profiles_inheriting(dir.path(), "base").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn profiles_inheriting_match_found() {
        let dir = tempfile::tempdir().unwrap();
        let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - base\n  modules: []\n".to_string();
        std::fs::write(dir.path().join("child.yaml"), &profile).unwrap();

        let result = profiles_inheriting(dir.path(), "base").unwrap();
        assert_eq!(result, vec!["child"]);
    }

    // --- collect_module_file_targets ---

    #[test]
    fn collect_module_file_targets_nonexistent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result = collect_module_file_targets("nope", dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn collect_module_file_targets_local_module() {
        let dir = tempfile::tempdir().unwrap();
        let module_dir = dir.path().join("modules").join("test-mod");
        std::fs::create_dir_all(&module_dir).unwrap();
        let module_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages: []\n  files:\n    - source: foo.conf\n      target: /tmp/foo.conf\n";
        std::fs::write(module_dir.join("module.yaml"), module_yaml).unwrap();

        let result = collect_module_file_targets("test-mod", dir.path());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PathBuf::from("/tmp/foo.conf"));
    }

    // =========================================================================
    // Command-level tests
    // =========================================================================

    const TEST_CONFIG_YAML: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n";
    const DEFAULT_PROFILE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  env:
    - name: EDITOR
      value: vim
  packages:
    cargo:
      - bat
"#;
    const WORK_PROFILE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - default
  env:
    - name: EDITOR
      value: code
  packages:
    cargo:
      - exa
"#;

    fn setup_config_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
        std::fs::write(profiles_dir.join("default.yaml"), DEFAULT_PROFILE_YAML).unwrap();
        std::fs::write(profiles_dir.join("work.yaml"), WORK_PROFILE_YAML).unwrap();
        dir
    }

    fn test_cli(dir: &Path) -> super::super::Cli {
        super::super::Cli {
            config: dir.join("cfgd.yaml"),
            profile: None,
            no_color: true,
            verbose: false,
            quiet: true,
            output: super::super::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            jsonpath: None,
            state_dir: None,
            command: super::super::Command::Status { module: None },
        }
    }

    fn make_profile_create_args(name: &str) -> super::super::ProfileCreateArgs {
        super::super::ProfileCreateArgs {
            name: name.to_string(),
            inherits: vec![],
            modules: vec![],
            packages: vec![],
            env: vec![],
            aliases: vec![],
            system: vec![],
            files: vec![],
            private: false,
            secrets: vec![],
            pre_apply: vec![],
            post_apply: vec![],
            pre_reconcile: vec![],
            post_reconcile: vec![],
            on_change: vec![],
            on_drift: vec![],
        }
    }

    fn make_profile_update_args() -> super::super::ProfileUpdateArgs {
        super::super::ProfileUpdateArgs {
            name: None,
            inherits: vec![],
            modules: vec![],
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            system: vec![],
            secrets: vec![],
            pre_apply: vec![],
            post_apply: vec![],
            pre_reconcile: vec![],
            post_reconcile: vec![],
            on_change: vec![],
            on_drift: vec![],
            private: false,
        }
    }

    // --- cmd_profile_show ---

    #[test]
    fn profile_show_named_profile() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_show(&cli, &printer, Some("default"));
        assert!(
            result.is_ok(),
            "cmd_profile_show should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn profile_show_active_profile() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // None means "show the active profile" — reads from cfgd.yaml
        let result = cmd_profile_show(&cli, &printer, None);
        assert!(
            result.is_ok(),
            "cmd_profile_show(None) should use active profile: {:?}",
            result.err()
        );
    }

    #[test]
    fn profile_show_inherited_profile_resolves_layers() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // work inherits from default, should resolve both layers
        let result = cmd_profile_show(&cli, &printer, Some("work"));
        assert!(
            result.is_ok(),
            "showing inherited profile should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn profile_show_nonexistent_profile_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_show(&cli, &printer, Some("nonexistent"));
        assert!(result.is_err(), "showing nonexistent profile should fail");
    }

    #[test]
    fn profile_show_no_config_fails() {
        let dir = tempfile::tempdir().unwrap();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // No cfgd.yaml — showing active profile should fail
        let result = cmd_profile_show(&cli, &printer, None);
        assert!(
            result.is_err(),
            "cmd_profile_show with no config should fail"
        );
    }

    // --- cmd_profile_list ---

    #[test]
    fn profile_list_shows_profiles() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_list(&cli, &printer);
        assert!(
            result.is_ok(),
            "cmd_profile_list should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn profile_list_no_profiles_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
        // Don't create profiles dir
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_list(&cli, &printer);
        assert!(
            result.is_ok(),
            "cmd_profile_list with no profiles dir should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn profile_list_empty_profiles_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();

        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_list(&cli, &printer);
        assert!(
            result.is_ok(),
            "cmd_profile_list with empty profiles dir should succeed: {:?}",
            result.err()
        );
    }

    // --- cmd_profile_switch ---

    #[test]
    fn profile_switch_updates_config() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_switch(&cli, "work", &printer);
        assert!(
            result.is_ok(),
            "cmd_profile_switch should succeed: {:?}",
            result.err()
        );

        // Verify the config file was updated
        let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
        assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
    }

    #[test]
    fn profile_switch_nonexistent_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_switch(&cli, "nonexistent", &printer);
        assert!(
            result.is_err(),
            "switching to nonexistent profile should fail"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "error should mention 'not found': {}",
            err
        );
    }

    #[test]
    fn profile_switch_no_config_fails() {
        let dir = tempfile::tempdir().unwrap();
        // No cfgd.yaml
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_switch(&cli, "default", &printer);
        assert!(result.is_err(), "switch with no config should fail");
    }

    #[test]
    fn profile_switch_preserves_other_config() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // Start at default, switch to work
        cmd_profile_switch(&cli, "work", &printer).unwrap();
        let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
        assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
        // Config name should still be preserved
        assert_eq!(cfg.metadata.name, "test");
    }

    #[test]
    fn profile_switch_error_lists_available_profiles() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_switch(&cli, "nope", &printer);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Available profiles"),
            "error should list available profiles: {}",
            err
        );
    }

    // --- cmd_profile_create ---

    #[test]
    fn profile_create_minimal() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args("devops");
        // Need at least one flag to avoid interactive mode
        args.modules = vec![];
        args.env = vec!["FOO=bar".to_string()];

        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(
            result.is_ok(),
            "cmd_profile_create should succeed: {:?}",
            result.err()
        );

        let profile_path = dir.path().join("profiles").join("devops.yaml");
        assert!(profile_path.exists(), "profile YAML should be created");

        let doc = config::load_profile(&profile_path).unwrap();
        assert_eq!(doc.metadata.name, "devops");
        assert_eq!(doc.spec.env.len(), 1);
        assert_eq!(doc.spec.env[0].name, "FOO");
        assert_eq!(doc.spec.env[0].value, "bar");
    }

    #[test]
    fn profile_create_with_inherits() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args("child");
        args.inherits = vec!["default".to_string()];

        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(
            result.is_ok(),
            "cmd_profile_create with inherits should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("child.yaml")).unwrap();
        assert_eq!(doc.spec.inherits, vec!["default"]);
    }

    #[test]
    fn profile_create_with_modules() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args("modular");
        args.modules = vec!["shell".to_string()];

        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(
            result.is_ok(),
            "cmd_profile_create with modules should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("modular.yaml")).unwrap();
        assert_eq!(doc.spec.modules, vec!["shell"]);
    }

    #[test]
    fn profile_create_duplicate_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args("default");
        args.env = vec!["X=1".to_string()]; // avoid interactive
        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(result.is_err(), "creating duplicate profile should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("already exists"),
            "error should mention 'already exists': {}",
            err
        );
    }

    #[test]
    fn profile_create_missing_parent_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args("orphan");
        args.inherits = vec!["nonexistent-parent".to_string()];

        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(
            result.is_err(),
            "creating profile with missing parent should fail"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "error should mention parent not found: {}",
            err
        );
    }

    #[test]
    fn profile_create_with_aliases() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args("alias-test");
        args.aliases = vec!["ll=ls -la".to_string()];

        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(
            result.is_ok(),
            "cmd_profile_create with aliases should succeed: {:?}",
            result.err()
        );

        let doc =
            config::load_profile(&dir.path().join("profiles").join("alias-test.yaml")).unwrap();
        assert_eq!(doc.spec.aliases.len(), 1);
        assert_eq!(doc.spec.aliases[0].name, "ll");
        assert_eq!(doc.spec.aliases[0].command, "ls -la");
    }

    #[test]
    fn profile_create_with_system_settings() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args("sys-test");
        args.system = vec!["sysctl=net.core.somaxconn".to_string()];

        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(
            result.is_ok(),
            "cmd_profile_create with system should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("sys-test.yaml")).unwrap();
        assert!(doc.spec.system.contains_key("sysctl"));
    }

    #[test]
    fn profile_create_with_secrets() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args("secret-test");
        args.secrets = vec!["secrets/key.enc:/tmp/key".to_string()];

        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(
            result.is_ok(),
            "cmd_profile_create with secrets should succeed: {:?}",
            result.err()
        );

        let doc =
            config::load_profile(&dir.path().join("profiles").join("secret-test.yaml")).unwrap();
        assert_eq!(doc.spec.secrets.len(), 1);
        assert_eq!(doc.spec.secrets[0].source, "secrets/key.enc");
        assert_eq!(doc.spec.secrets[0].target, Some(PathBuf::from("/tmp/key")));
    }

    #[test]
    fn profile_create_with_scripts() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args("script-test");
        args.pre_apply = vec!["check.sh".to_string()];
        args.post_apply = vec!["notify.sh".to_string()];
        args.on_drift = vec!["alert.sh".to_string()];

        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(
            result.is_ok(),
            "cmd_profile_create with scripts should succeed: {:?}",
            result.err()
        );

        let doc =
            config::load_profile(&dir.path().join("profiles").join("script-test.yaml")).unwrap();
        let scripts = doc.spec.scripts.unwrap();
        assert_eq!(scripts.pre_apply.len(), 1);
        assert_eq!(scripts.post_apply.len(), 1);
        assert_eq!(scripts.on_drift.len(), 1);
        assert_eq!(scripts.pre_apply[0].run_str(), "check.sh");
        assert_eq!(scripts.post_apply[0].run_str(), "notify.sh");
        assert_eq!(scripts.on_drift[0].run_str(), "alert.sh");
    }

    #[test]
    fn profile_create_invalid_name_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_create_args(".hidden");
        args.env = vec!["X=1".to_string()]; // avoid interactive
        let result = cmd_profile_create(&cli, &printer, &args);
        assert!(
            result.is_err(),
            "profile with leading dot should fail validation"
        );
    }

    // --- cmd_profile_update ---

    #[test]
    fn profile_update_add_env() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.env = vec!["NEW_VAR=hello".to_string()];

        let result = cmd_profile_update(&cli, &printer, "default", &args);
        assert!(
            result.is_ok(),
            "cmd_profile_update should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(
            doc.spec
                .env
                .iter()
                .any(|e| e.name == "NEW_VAR" && e.value == "hello")
        );
    }

    #[test]
    fn profile_update_remove_env() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.env = vec!["-EDITOR".to_string()];

        let result = cmd_profile_update(&cli, &printer, "default", &args);
        assert!(
            result.is_ok(),
            "removing env should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(
            !doc.spec.env.iter().any(|e| e.name == "EDITOR"),
            "EDITOR should be removed"
        );
    }

    #[test]
    fn profile_update_add_alias() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.aliases = vec!["gs=git status".to_string()];

        let result = cmd_profile_update(&cli, &printer, "default", &args);
        assert!(
            result.is_ok(),
            "adding alias should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(
            doc.spec
                .aliases
                .iter()
                .any(|a| a.name == "gs" && a.command == "git status")
        );
    }

    #[test]
    fn profile_update_remove_alias() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // First add an alias
        let mut args = make_profile_update_args();
        args.aliases = vec!["gs=git status".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        // Then remove it
        let mut args2 = make_profile_update_args();
        args2.aliases = vec!["-gs".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(
            !doc.spec.aliases.iter().any(|a| a.name == "gs"),
            "alias gs should be removed"
        );
    }

    #[test]
    fn profile_update_add_inherits() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // Create a base profile to inherit from
        let mut create_args = make_profile_create_args("base");
        create_args.env = vec!["X=1".to_string()]; // avoid interactive
        cmd_profile_create(&cli, &printer, &create_args).unwrap();

        // Update work to also inherit base
        let mut args = make_profile_update_args();
        args.inherits = vec!["base".to_string()];

        let result = cmd_profile_update(&cli, &printer, "work", &args);
        assert!(
            result.is_ok(),
            "adding inherits should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("work.yaml")).unwrap();
        assert!(doc.spec.inherits.contains(&"base".to_string()));
        assert!(doc.spec.inherits.contains(&"default".to_string()));
    }

    #[test]
    fn profile_update_remove_inherits() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.inherits = vec!["-default".to_string()];

        let result = cmd_profile_update(&cli, &printer, "work", &args);
        assert!(
            result.is_ok(),
            "removing inherits should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("work.yaml")).unwrap();
        assert!(!doc.spec.inherits.contains(&"default".to_string()));
    }

    #[test]
    fn profile_update_add_module() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.modules = vec!["shell".to_string()];

        let result = cmd_profile_update(&cli, &printer, "default", &args);
        assert!(
            result.is_ok(),
            "adding module should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(doc.spec.modules.contains(&"shell".to_string()));
    }

    #[test]
    fn profile_update_remove_module() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // First add a module
        let mut args = make_profile_update_args();
        args.modules = vec!["shell".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        // Then remove it
        let mut args2 = make_profile_update_args();
        args2.modules = vec!["-shell".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(
            !doc.spec.modules.contains(&"shell".to_string()),
            "shell module should be removed"
        );
    }

    #[test]
    fn profile_update_add_system_setting() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.system = vec!["sysctl=net.core.somaxconn".to_string()];

        let result = cmd_profile_update(&cli, &printer, "default", &args);
        assert!(
            result.is_ok(),
            "adding system should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(doc.spec.system.contains_key("sysctl"));
    }

    #[test]
    fn profile_update_remove_system_setting() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // First add a system setting
        let mut args = make_profile_update_args();
        args.system = vec!["sysctl=net.core.somaxconn".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        // Then remove it
        let mut args2 = make_profile_update_args();
        args2.system = vec!["-sysctl".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(
            !doc.spec.system.contains_key("sysctl"),
            "sysctl should be removed"
        );
    }

    #[test]
    fn profile_update_add_secret() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.secrets = vec!["secrets/key.enc:/tmp/out".to_string()];

        let result = cmd_profile_update(&cli, &printer, "default", &args);
        assert!(
            result.is_ok(),
            "adding secret should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert_eq!(doc.spec.secrets.len(), 1);
        assert_eq!(doc.spec.secrets[0].source, "secrets/key.enc");
    }

    #[test]
    fn profile_update_remove_secret() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // First add a secret
        let mut args = make_profile_update_args();
        args.secrets = vec!["secrets/key.enc:/tmp/out".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        // Then remove it
        let mut args2 = make_profile_update_args();
        args2.secrets = vec!["-/tmp/out".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        assert!(doc.spec.secrets.is_empty(), "secret should be removed");
    }

    #[test]
    fn profile_update_add_scripts() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.pre_apply = vec!["lint.sh".to_string()];
        args.post_apply = vec!["notify.sh".to_string()];
        args.on_change = vec!["reload.sh".to_string()];

        let result = cmd_profile_update(&cli, &printer, "default", &args);
        assert!(
            result.is_ok(),
            "adding scripts should succeed: {:?}",
            result.err()
        );

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        let scripts = doc.spec.scripts.unwrap();
        assert_eq!(scripts.pre_apply.len(), 1);
        assert_eq!(scripts.post_apply.len(), 1);
        assert_eq!(scripts.on_change.len(), 1);
    }

    #[test]
    fn profile_update_remove_scripts() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // Add scripts first
        let mut args = make_profile_update_args();
        args.pre_apply = vec!["lint.sh".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args).unwrap();

        // Remove them
        let mut args2 = make_profile_update_args();
        args2.pre_apply = vec!["-lint.sh".to_string()];
        cmd_profile_update(&cli, &printer, "default", &args2).unwrap();

        let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
        if let Some(scripts) = doc.spec.scripts {
            assert!(
                scripts.pre_apply.is_empty(),
                "pre_apply should be empty after removal"
            );
        }
    }

    #[test]
    fn profile_update_nonexistent_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let args = make_profile_update_args();
        let result = cmd_profile_update(&cli, &printer, "nonexistent", &args);
        assert!(result.is_err(), "updating nonexistent profile should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "error should mention not found: {}",
            err
        );
    }

    #[test]
    fn profile_update_no_changes_succeeds() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let args = make_profile_update_args();
        let result = cmd_profile_update(&cli, &printer, "default", &args);
        assert!(
            result.is_ok(),
            "update with no changes should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn profile_update_invalid_name_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let args = make_profile_update_args();
        let result = cmd_profile_update(&cli, &printer, ".bad-name", &args);
        assert!(
            result.is_err(),
            "invalid profile name should fail validation"
        );
    }

    #[test]
    fn profile_update_add_inherits_missing_parent_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let mut args = make_profile_update_args();
        args.inherits = vec!["ghost-parent".to_string()];

        let result = cmd_profile_update(&cli, &printer, "default", &args);
        assert!(
            result.is_err(),
            "inheriting from missing parent should fail"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "error should mention not found: {}",
            err
        );
    }

    // --- cmd_profile_delete ---

    #[test]
    fn profile_delete_with_yes_flag() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // Delete 'work' (not active, not inherited by others)
        let result = cmd_profile_delete(&cli, &printer, "work", true);
        assert!(
            result.is_ok(),
            "cmd_profile_delete should succeed: {:?}",
            result.err()
        );

        let profile_path = dir.path().join("profiles").join("work.yaml");
        assert!(!profile_path.exists(), "profile file should be deleted");
    }

    #[test]
    fn profile_delete_nonexistent_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_delete(&cli, &printer, "nonexistent", true);
        assert!(result.is_err(), "deleting nonexistent profile should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "error should mention not found: {}",
            err
        );
    }

    #[test]
    fn profile_delete_active_profile_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // 'default' is the active profile in cfgd.yaml
        let result = cmd_profile_delete(&cli, &printer, "default", true);
        assert!(result.is_err(), "deleting active profile should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("active profile"),
            "error should mention active profile: {}",
            err
        );
    }

    #[test]
    fn profile_delete_inherited_profile_fails() {
        let dir = setup_config_dir();
        // Switch active to 'work' so 'default' is not active but is inherited by 'work'
        let cli = test_cli(dir.path());
        let printer = make_printer();
        cmd_profile_switch(&cli, "work", &printer).unwrap();

        let result = cmd_profile_delete(&cli, &printer, "default", true);
        assert!(result.is_err(), "deleting inherited profile should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("inherited by"),
            "error should mention inherited: {}",
            err
        );
    }

    #[test]
    fn profile_delete_cleans_files_dir() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        // Create a profile with a files subdirectory
        let files_dir = dir.path().join("profiles").join("ephemeral").join("files");
        std::fs::create_dir_all(&files_dir).unwrap();
        std::fs::write(files_dir.join("test.txt"), "data").unwrap();

        // Create the profile YAML
        let profile_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: ephemeral\nspec:\n  modules: []\n";
        std::fs::write(
            dir.path().join("profiles").join("ephemeral.yaml"),
            profile_yaml,
        )
        .unwrap();

        cmd_profile_delete(&cli, &printer, "ephemeral", true).unwrap();

        assert!(!files_dir.exists(), "files directory should be cleaned up");
    }

    #[test]
    fn profile_delete_invalid_name_fails() {
        let dir = setup_config_dir();
        let cli = test_cli(dir.path());
        let printer = make_printer();

        let result = cmd_profile_delete(&cli, &printer, "-bad", true);
        assert!(
            result.is_err(),
            "invalid profile name should fail validation"
        );
    }
}

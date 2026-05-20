use super::*;
use cfgd_core::output::{Doc, Printer as PrinterV2, Role};

pub fn cmd_profile_update(
    cli: &Cli,
    v2_printer: &PrinterV2,
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
    v2_printer.heading(format!("Update Profile: {}", name));

    let config_dir = config_dir(cli);
    let profile_path = config_dir.join("profiles").join(format!("{}.yaml", name));
    if !profile_path.exists() {
        v2_printer.emit(cfgd_core::output::error_doc(
            name,
            "not_found",
            format!("Profile '{}' not found", name),
            serde_json::Value::Null,
        ));
        anyhow::bail!("Profile '{}' not found", name);
    }

    let mut doc = config::load_profile(&profile_path)?;
    let mut changes = 0u32;

    // Add inherits
    let profiles_dir = config_dir.join("profiles");
    for parent in &add_inherits {
        if doc.spec.inherits.contains(parent) {
            v2_printer.status_simple(Role::Warn, format!("Profile already inherits '{}'", parent));
            continue;
        }
        let parent_path = profiles_dir.join(format!("{}.yaml", parent));
        if !parent_path.exists() {
            v2_printer.emit(cfgd_core::output::error_doc(
                name,
                "parent_not_found",
                format!("Parent profile '{}' not found", parent),
                serde_json::json!({ "parent": parent }),
            ));
            anyhow::bail!("Parent profile '{}' not found", parent);
        }
        doc.spec.inherits.push(parent.clone());
        v2_printer.status_simple(Role::Ok, format!("Added inherits: {}", parent));
        changes += 1;
    }

    // Remove inherits
    for parent in &remove_inherits {
        let before = doc.spec.inherits.len();
        doc.spec.inherits.retain(|x| x != parent);
        if doc.spec.inherits.len() < before {
            v2_printer.status_simple(Role::Ok, format!("Removed inherits: {}", parent));
            changes += 1;
        } else {
            v2_printer.status_simple(
                Role::Warn,
                format!("Inherits '{}' not found in profile", parent),
            );
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
            module::cmd_module_add_remote(cli, v2_printer, m, None, false, false)?;
            // Reload profile (remote add may have modified it)
            doc = config::load_profile(&profile_path)?;
            changes += 1;
        } else if modules::is_registry_ref(m) {
            let yaml = serde_yaml::to_string(&doc)?;
            cfgd_core::atomic_write_str(&profile_path, &yaml)?;
            module::cmd_module_add_from_registry(cli, v2_printer, m, false, false)?;
            doc = config::load_profile(&profile_path)?;
            changes += 1;
        } else {
            if !modules_dir.join(m).join("module.yaml").exists() {
                v2_printer.status_simple(
                    Role::Warn,
                    format!(
                        "Module '{}' not found locally — make sure it exists or is a remote module",
                        m
                    ),
                );
            }
            doc.spec.modules.push(m.clone());
            v2_printer.status_simple(Role::Ok, format!("Added module: {}", m));
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
                v2_printer.status_simple(Role::Info, format!("Removed '{}' from modules.lock", m));
                if let Ok(git_src) = modules::parse_git_source(&entry.url) {
                    let cache_base = modules::default_module_cache_dir().unwrap_or_default();
                    let cache_dir = modules::git_cache_dir(&cache_base, &git_src.repo_url);
                    if cache_dir.exists() {
                        if let Err(e) = std::fs::remove_dir_all(&cache_dir) {
                            v2_printer
                                .status_simple(Role::Warn, format!("Failed to clean cache: {}", e));
                        } else {
                            v2_printer.status_simple(Role::Info, "Cleaned cached checkout");
                        }
                    }
                }
            }
            // Clean module state and deployed files from DB
            if let Ok(state) = open_state_store(cli.state_dir.as_deref()) {
                // Query file manifest for deployed files
                if let Ok(manifest) = state.module_deployed_files(m) {
                    if !manifest.is_empty() {
                        {
                            let deployed_sec = v2_printer.section(format!(
                                "Module '{}' deployed {} file(s)",
                                m,
                                manifest.len()
                            ));
                            for f in &manifest {
                                deployed_sec.bullet(f.file_path.clone());
                            }
                        }

                        let should_clean = v2_printer
                            .prompt_confirm(
                                "Remove deployed files? Backups will be restored where available.",
                            )
                            .unwrap_or(false);

                        if should_clean {
                            let cleanup_sec = v2_printer.section("Rollback");
                            for f in &manifest {
                                let path = std::path::Path::new(&f.file_path);
                                // Try to restore from backup store
                                if let Ok(Some(backup)) = state.latest_backup_for_path(&f.file_path)
                                {
                                    if backup.was_symlink {
                                        if let Err(e) = std::fs::remove_file(path) {
                                            cleanup_sec.status_simple(
                                                Role::Warn,
                                                format!(
                                                    "rollback: failed to remove {}: {}",
                                                    f.file_path, e
                                                ),
                                            );
                                        }
                                        if let Some(ref link_target) = backup.symlink_target
                                            && let Err(e) = cfgd_core::create_symlink(
                                                std::path::Path::new(link_target),
                                                path,
                                            )
                                        {
                                            cleanup_sec.status_simple(
                                                Role::Warn,
                                                format!(
                                                    "rollback: failed to restore symlink {}: {}",
                                                    f.file_path, e
                                                ),
                                            );
                                        }
                                        cleanup_sec.status_simple(
                                            Role::Ok,
                                            format!("Restored symlink: {}", f.file_path),
                                        );
                                    } else if !backup.oversized && !backup.content.is_empty() {
                                        if let Err(e) =
                                            cfgd_core::atomic_write(path, &backup.content)
                                        {
                                            cleanup_sec.status_simple(
                                                Role::Warn,
                                                format!(
                                                    "rollback: failed to restore {}: {}",
                                                    f.file_path, e
                                                ),
                                            );
                                        } else {
                                            cleanup_sec.status_simple(
                                                Role::Ok,
                                                format!("Restored: {}", f.file_path),
                                            );
                                        }
                                    }
                                } else if path.exists() || path.symlink_metadata().is_ok() {
                                    if let Err(e) = std::fs::remove_file(path) {
                                        cleanup_sec.status_simple(
                                            Role::Warn,
                                            format!(
                                                "rollback: failed to remove {}: {}",
                                                f.file_path, e
                                            ),
                                        );
                                    } else {
                                        cleanup_sec.status_simple(
                                            Role::Ok,
                                            format!("Removed: {}", f.file_path),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    if let Err(e) = state.delete_module_files(m) {
                        v2_printer.status_simple(
                            Role::Warn,
                            format!("rollback: failed to clean module files for {}: {}", m, e),
                        );
                    }
                }

                if let Err(e) = state.remove_module_state(m) {
                    v2_printer
                        .status_simple(Role::Warn, format!("Failed to clean module state: {}", e));
                }
            }
            v2_printer.status_simple(Role::Ok, format!("Removed module: {}", m));
            changes += 1;

            // Check for .cfgd-backup files at module file targets and offer to restore
            prompt_restore_backups(&module_file_targets, v2_printer)?;
        } else {
            v2_printer.status_simple(Role::Warn, format!("Module '{}' not found in profile", m));
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
        v2_printer.status_simple(Role::Ok, format!("Added package: {} ({})", pkg, mgr));
        changes += 1;
    }

    // Remove packages
    for pkg_str in &remove_packages {
        let (mgr, pkg) = parse_manager_package(pkg_str)?;
        let pkgs = doc.spec.packages.get_or_insert_with(Default::default);
        if packages::remove_package(&mgr, &pkg, pkgs)? {
            v2_printer.status_simple(Role::Ok, format!("Removed package: {} ({})", pkg, mgr));
            changes += 1;
        } else {
            v2_printer.status_simple(
                Role::Warn,
                format!("Package '{}' not found in {}", pkg, mgr),
            );
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
                v2_printer.status_simple(Role::Ok, format!("Added file: {}", basename));
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
                v2_printer.status_simple(Role::Ok, format!("Removed file: {}", target));
                changes += 1;
            } else {
                v2_printer.status_simple(
                    Role::Warn,
                    format!("File '{}' not found in profile", target),
                );
            }
        }
    }

    // Add env vars
    for v in &add_env {
        let ev = cfgd_core::parse_env_var(v).map_err(|e| anyhow::anyhow!(e))?;
        cfgd_core::merge_env(&mut doc.spec.env, std::slice::from_ref(&ev));
        v2_printer.status_simple(Role::Ok, format!("Set env: {}={}", ev.name, ev.value));
        changes += 1;
    }

    // Remove env vars
    for key in &remove_env {
        let before = doc.spec.env.len();
        doc.spec.env.retain(|e| e.name != *key);
        if doc.spec.env.len() < before {
            v2_printer.status_simple(Role::Ok, format!("Removed env: {}", key));
            changes += 1;
        } else {
            v2_printer.status_simple(Role::Warn, format!("Env var '{}' not found", key));
        }
    }

    // Add aliases
    for a in &add_aliases {
        let alias = cfgd_core::parse_alias(a).map_err(|e| anyhow::anyhow!(e))?;
        cfgd_core::merge_aliases(&mut doc.spec.aliases, std::slice::from_ref(&alias));
        v2_printer.status_simple(
            Role::Ok,
            format!("Set alias: {}={}", alias.name, alias.command),
        );
        changes += 1;
    }

    // Remove aliases
    for alias_name in &remove_aliases {
        let before = doc.spec.aliases.len();
        doc.spec.aliases.retain(|a| a.name != *alias_name);
        if doc.spec.aliases.len() < before {
            v2_printer.status_simple(Role::Ok, format!("Removed alias: {}", alias_name));
            changes += 1;
        } else {
            v2_printer.status_simple(Role::Warn, format!("Alias '{}' not found", alias_name));
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
        v2_printer.status_simple(Role::Ok, format!("Set system: {}={}", key, value));
        changes += 1;
    }

    // Remove system settings
    for key in &remove_system {
        if doc.spec.system.remove(key.as_str()).is_some() {
            v2_printer.status_simple(Role::Ok, format!("Removed system setting: {}", key));
            changes += 1;
        } else {
            v2_printer.status_simple(Role::Warn, format!("System setting '{}' not found", key));
        }
    }

    // Add secrets
    for secret_str in &add_secrets {
        let secret = parse_secret_spec(secret_str)?;
        // parse_secret_spec always produces a target; this branch guards an
        // internal invariant rather than a user-facing error, so it stays as
        // a bare bail without a structured Doc.
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
            v2_printer.status_simple(
                Role::Warn,
                format!("Secret targeting '{}' already exists", target.display()),
            );
            continue;
        }
        v2_printer.status_simple(
            Role::Ok,
            format!("Added secret: {} → {}", secret.source, target.display()),
        );
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
            v2_printer.status_simple(Role::Ok, format!("Removed secret: {}", target_str));
            changes += 1;
        } else {
            v2_printer.status_simple(
                Role::Warn,
                format!("Secret targeting '{}' not found", target_str),
            );
        }
    }

    // Add/remove script hooks
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_pre_apply,
        &remove_pre_apply,
        "preApply",
        |s| &mut s.pre_apply,
        v2_printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_post_apply,
        &remove_post_apply,
        "postApply",
        |s| &mut s.post_apply,
        v2_printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_pre_reconcile,
        &remove_pre_reconcile,
        "preReconcile",
        |s| &mut s.pre_reconcile,
        v2_printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_post_reconcile,
        &remove_post_reconcile,
        "postReconcile",
        |s| &mut s.post_reconcile,
        v2_printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_on_change,
        &remove_on_change,
        "onChange",
        |s| &mut s.on_change,
        v2_printer,
    );
    changes += update_script_list(
        &mut doc.spec.scripts,
        &add_on_drift,
        &remove_on_drift,
        "onDrift",
        |s| &mut s.on_drift,
        v2_printer,
    );

    if changes == 0 {
        v2_printer.emit(
            Doc::new()
                .status(Role::Info, "No changes specified")
                .with_data(serde_json::json!({
                    "name": name,
                    "changes": 0,
                })),
        );
        return Ok(());
    }

    let yaml = serde_yaml::to_string(&doc)?;
    cfgd_core::atomic_write_str(&profile_path, &yaml)?;
    v2_printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Updated profile '{}' ({} change(s))", name, changes),
            )
            .with_data(serde_json::json!({
                "name": name,
                "changes": changes,
            })),
    );

    Ok(())
}

use super::*;

pub(super) fn cmd_doctor(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    // Gather data for both structured and human output
    let (config_check, loaded_cfg) = if cli.config.exists() {
        match config::load_config(&cli.config) {
            Ok(cfg) => (
                DoctorConfigCheck {
                    valid: true,
                    path: cli.config.display().to_string(),
                    name: Some(cfg.metadata.name.clone()),
                    profile: cfg.spec.profile.clone(),
                    error: None,
                },
                Some(cfg),
            ),
            Err(e) => (
                DoctorConfigCheck {
                    valid: false,
                    path: cli.config.display().to_string(),
                    name: None,
                    profile: None,
                    error: Some(format!("{}", e)),
                },
                None,
            ),
        }
    } else {
        (
            DoctorConfigCheck {
                valid: false,
                path: cli.config.display().to_string(),
                name: None,
                profile: None,
                error: Some("not found".into()),
            },
            None,
        )
    };

    let git_available = cfgd_core::command_available("git");

    let config_dir = config_dir(cli);
    let age_key_override = loaded_cfg
        .as_ref()
        .and_then(|c| c.spec.secrets.as_ref())
        .and_then(|s| s.sops.as_ref())
        .and_then(|s| s.age_key.as_ref());

    let health = secrets::check_secrets_health(&config_dir, age_key_override.map(|p| p.as_path()));

    // Build structured doctor output for --output json/yaml
    // Resolve profile to get declared managers (including custom) and build registry
    let resolved_packages = if let Some(ref cfg) = loaded_cfg {
        let profiles_dir = profiles_dir(cli);
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref());
        if let Some(pn) = profile_name
            && let Ok(mut resolved) = config::resolve_profile(pn, &profiles_dir)
        {
            let _ = packages::resolve_manifest_packages(&mut resolved.merged.packages, &config_dir);
            Some(resolved.merged.packages)
        } else {
            None
        }
    } else {
        None
    };

    let registry = if let Some(ref pkgs) = resolved_packages {
        build_registry_with_profile(pkgs)
    } else {
        build_registry()
    };
    let all_managers = &registry.package_managers;

    // Determine which managers are declared in config
    let declared_managers: Vec<String> = if let Some(ref pkgs) = resolved_packages {
        let mut declared = Vec::new();
        if let Some(ref brew) = pkgs.brew
            && (!brew.formulae.is_empty() || !brew.taps.is_empty() || !brew.casks.is_empty())
        {
            declared.push("brew".to_string());
        }
        if let Some(ref apt) = pkgs.apt
            && !apt.packages.is_empty()
        {
            declared.push("apt".to_string());
        }
        if let Some(ref cargo) = pkgs.cargo
            && !cargo.packages.is_empty()
        {
            declared.push("cargo".to_string());
        }
        if let Some(ref npm) = pkgs.npm
            && !npm.global.is_empty()
        {
            declared.push("npm".to_string());
        }
        for (name, _) in pkgs.non_empty_simple_lists() {
            declared.push(name.to_string());
        }
        if let Some(ref snap) = pkgs.snap
            && !snap.packages.is_empty()
        {
            declared.push("snap".to_string());
        }
        if let Some(ref flatpak) = pkgs.flatpak
            && !flatpak.packages.is_empty()
        {
            declared.push("flatpak".to_string());
        }
        for custom in &pkgs.custom {
            if !custom.packages.is_empty() {
                declared.push(custom.name.clone());
            }
        }
        declared
    } else {
        Vec::new()
    };

    // Build manager check data (deduplicate brew-tap/brew-cask under brew)
    let mut manager_checks: Vec<DoctorManagerCheck> = Vec::new();
    {
        let mut seen = std::collections::HashSet::new();
        for mgr in all_managers.iter() {
            let name = mgr.name();
            if name == "brew-tap" || name == "brew-cask" {
                continue;
            }
            if !seen.insert(name.to_string()) {
                continue;
            }
            manager_checks.push(DoctorManagerCheck {
                name: name.to_string(),
                available: mgr.is_available(),
                declared: declared_managers.iter().any(|d| d == name),
                can_bootstrap: mgr.can_bootstrap(),
            });
        }
    }

    // Modules health
    let module_list: Vec<String> = if let Some(ref cfg) = loaded_cfg {
        let profiles_dir = profiles_dir(cli);
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref());
        profile_name
            .and_then(|pn| config::resolve_profile(pn, &profiles_dir).ok())
            .map(|r| r.merged.modules)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let cache_base = modules::default_module_cache_dir().unwrap_or_default();
    let all_modules =
        modules::load_all_modules(&config_dir, &cache_base, printer).unwrap_or_default();
    let module_checks: Vec<DoctorModuleCheck> = module_list
        .iter()
        .map(|mod_name| {
            if all_modules.contains_key(mod_name) {
                DoctorModuleCheck {
                    name: mod_name.clone(),
                    valid: true,
                    error: None,
                }
            } else {
                DoctorModuleCheck {
                    name: mod_name.clone(),
                    valid: false,
                    error: Some("module not found".into()),
                }
            }
        })
        .collect();

    // System configurators
    let configurator_checks: Vec<DoctorConfiguratorCheck> = registry
        .available_system_configurators()
        .iter()
        .map(|c| DoctorConfiguratorCheck {
            name: c.name().to_string(),
            available: true,
        })
        .collect();

    // Structured output
    if printer.write_structured(&DoctorOutput {
        config: config_check.clone(),
        git: git_available,
        secrets: DoctorSecretsCheck {
            sops_available: health.sops_available,
            sops_version: health.sops_version.clone(),
            age_key_exists: health.age_key_exists,
            age_key_path: health
                .age_key_path
                .as_ref()
                .map(|p| p.display().to_string()),
            sops_config_exists: health.sops_config_exists,
            providers: health
                .providers
                .iter()
                .map(|(n, a)| DoctorProviderCheck {
                    name: n.clone(),
                    available: *a,
                })
                .collect(),
        },
        package_managers: manager_checks,
        modules: module_checks,
        system_configurators: configurator_checks,
    }) {
        return Ok(());
    }

    // Human display
    printer.header("Doctor");

    let mut all_ok = config_check.valid && git_available;

    if config_check.valid {
        printer.success(&format!("Config file: {} (valid)", config_check.path));
        if let Some(name) = loaded_cfg.as_ref().map(|c| &c.metadata.name) {
            printer.key_value("Name", name);
        }
        printer.key_value(
            "Profile",
            loaded_cfg
                .as_ref()
                .and_then(|c| c.spec.profile.as_deref())
                .unwrap_or("(none)"),
        );
    } else if let Some(ref err) = config_check.error {
        if err == "not found" {
            printer.warning(&format!(
                "Config file not found: {} — run 'cfgd init' to create one",
                config_check.path
            ));
        } else {
            printer.error(&format!("Config file: {} — {}", config_check.path, err));
        }
    }

    if git_available {
        printer.success("git: found");
    } else {
        printer.error("git: not found — install git to use cfgd");
    }

    // Secrets
    printer.newline();
    printer.subheader("Secrets");

    if health.sops_available {
        let version_str = health.sops_version.as_deref().unwrap_or("unknown version");
        printer.success(&format!("sops: found ({})", version_str));
    } else {
        printer.warning(
            "sops: not found — required for secrets (https://github.com/getsops/sops#install)",
        );
    }

    if health.age_key_exists {
        if let Some(ref path) = health.age_key_path {
            printer.success(&format!("age key: {}", path.display()));
        }
    } else if let Some(ref path) = health.age_key_path {
        printer.warning(&format!(
            "age key: not found at {} — run 'cfgd init' to generate",
            path.display()
        ));
    }

    if health.sops_config_exists {
        if let Some(ref path) = health.sops_config_path {
            printer.success(&format!(".sops.yaml: {}", path.display()));
        }
    } else {
        printer.warning(".sops.yaml: not found — will be generated on 'cfgd init'");
    }

    for (name, available) in &health.providers {
        if *available {
            printer.success(&format!("provider {}: available", name));
        } else {
            printer.info(&format!("provider {}: not installed (optional)", name));
        }
    }

    // Package managers
    printer.newline();
    printer.subheader("Package Managers");

    let mut shown_managers = std::collections::HashSet::new();
    for mgr in all_managers.iter() {
        let name = mgr.name();
        if name == "brew-tap" || name == "brew-cask" {
            continue;
        }
        if !shown_managers.insert(name.to_string()) {
            continue;
        }
        let is_declared = declared_managers.iter().any(|d| d == name);
        let available = mgr.is_available();

        if is_declared {
            if available {
                printer.success(&format!("{}: available (declared in config)", name));
            } else if mgr.can_bootstrap() {
                let method = packages::bootstrap_method(mgr.as_ref());
                printer.warning(&format!(
                    "{}: not found — can auto-bootstrap via {}",
                    name, method
                ));
            } else {
                printer.error(&format!(
                    "{}: not found — declared in config but not available",
                    name
                ));
                all_ok = false;
            }
        } else if available {
            printer.info(&format!("{}: available (not used in config)", name));
        }
    }

    if !module_list.is_empty() {
        printer.newline();
        printer.subheader("Modules");

        let registry_for_modules = build_registry();
        let mgr_map = managers_map(&registry_for_modules);
        let platform = Platform::detect();

        for mod_name in &module_list {
            if let Some(module) = all_modules.get(mod_name) {
                printer.info(&format!("{}:", mod_name));
                for entry in &module.spec.packages {
                    match modules::resolve_package(entry, mod_name, &platform, &mgr_map) {
                        Ok(Some(resolved)) => {
                            let installed = mgr_map
                                .get(&resolved.manager)
                                .and_then(|m| m.installed_packages().ok())
                                .map(|pkgs| pkgs.contains(&resolved.resolved_name))
                                .unwrap_or(false);
                            if installed {
                                let ver = resolved.version.as_deref().unwrap_or("?");
                                printer.success(&format!(
                                    "  {} {} ({}, {})",
                                    entry.name, ver, resolved.manager, resolved.resolved_name
                                ));
                            } else {
                                printer.error(&format!(
                                    "  {} — not installed ({} {})",
                                    entry.name, resolved.manager, resolved.resolved_name
                                ));
                                all_ok = false;
                            }
                        }
                        Ok(None) => {
                            printer.info(&format!("  {} — skipped (platform)", entry.name));
                        }
                        Err(e) => {
                            printer.error(&format!("  {} — {}", entry.name, e));
                            all_ok = false;
                        }
                    }
                }
            } else {
                printer.error(&format!(
                    "{}: module not found in {}/modules/",
                    mod_name,
                    config_dir.display()
                ));
                all_ok = false;
            }
        }
    }

    // Check state store
    printer.newline();
    printer.subheader("System");

    match StateStore::open_default() {
        Ok(_) => printer.success("State store: accessible"),
        Err(e) => {
            printer.warning(&format!("State store: {}", e));
        }
    }

    // Check profiles directory
    let profiles_dir = profiles_dir(cli);
    if profiles_dir.exists() {
        let count = std::fs::read_dir(&profiles_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("yaml"))
                    .count()
            })
            .unwrap_or(0);
        printer.success(&format!(
            "Profiles directory: {} ({} profiles)",
            profiles_dir.display(),
            count
        ));
    } else {
        printer.warning(&format!(
            "Profiles directory not found: {}",
            profiles_dir.display()
        ));
    }

    // Check config sources
    let doctor_config_path = &cli.config;
    if doctor_config_path.exists()
        && let Ok(cfg) = config::load_config(doctor_config_path)
        && !cfg.spec.sources.is_empty()
    {
        printer.newline();
        printer.subheader("Config Sources");
        let cache_dir = source_cache_dir(cli).ok();
        for source in &cfg.spec.sources {
            let cached = cache_dir.as_ref().and_then(|cd| {
                if cd.join(&source.name).exists() {
                    Some(format!("cached at {}", cd.join(&source.name).display()))
                } else {
                    None
                }
            });
            match cached {
                Some(info) => printer.success(&format!("{}: {}", source.name, info)),
                None => printer.warning(&format!(
                    "{}: not cached (run 'cfgd source update')",
                    source.name
                )),
            }
        }
    }

    printer.newline();
    if all_ok {
        printer.success("All checks passed");
    } else {
        printer.error("Some checks failed — see above");
    }

    Ok(())
}

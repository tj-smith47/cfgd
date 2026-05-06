use super::*;

// --- Source CRUD ---

pub(super) fn cmd_source_edit(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let source_path = config_dir.join("cfgd-source.yaml");
    if !source_path.exists() {
        anyhow::bail!(
            "No cfgd-source.yaml found in {} — run 'cfgd source create' to scaffold one",
            config_dir.display()
        );
    }

    open_in_editor(&source_path, printer)?;

    // Validate after editing — loop until valid or user cancels
    loop {
        let contents = std::fs::read_to_string(&source_path)?;
        match config::parse_config_source(&contents) {
            Ok(_) => {
                printer.success("Source manifest is valid");
                break;
            }
            Err(e) => {
                printer.error(&format!("Invalid source manifest: {}", e));
                if !printer.prompt_confirm("Re-open in editor to fix?")? {
                    printer.warning("Saved with validation errors");
                    break;
                }
                open_in_editor(&source_path, printer)?;
            }
        }
    }

    Ok(())
}

pub(super) fn cmd_source_create(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    description: Option<&str>,
    version: Option<&str>,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let source_path = config_dir.join("cfgd-source.yaml");
    if source_path.exists() {
        anyhow::bail!(
            "cfgd-source.yaml already exists at {} — use 'cfgd source edit' to modify it",
            source_path.display()
        );
    }

    // Interactive mode if no flags provided
    let is_interactive = name.is_none() && description.is_none() && version.is_none();

    // Determine name: flag > interactive prompt > directory name
    let source_name = match name {
        Some(n) => n.to_string(),
        None => {
            let dir_name = config_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("my-config");
            if is_interactive {
                printer.prompt_text("Source name", dir_name)?
            } else {
                dir_name.to_string()
            }
        }
    };

    let source_description = match description {
        Some(d) => d.to_string(),
        None => {
            if is_interactive {
                printer.prompt_text("Description", "Team configuration source")?
            } else {
                "Team configuration source".to_string()
            }
        }
    };

    let source_version = match version {
        Some(v) => v.to_string(),
        None => "0.1.0".to_string(),
    };

    let profile_names = scan_profile_names(&config_dir.join("profiles"))?;
    let module_names = scan_module_names(&config_dir.join("modules"))?;

    // Build profiles YAML block
    let profiles_yaml = if profile_names.is_empty() {
        "    profiles: []".to_string()
    } else {
        let mut lines = vec!["    profiles:".to_string()];
        for p in &profile_names {
            lines.push(format!("      - {}", p));
        }
        lines.join("\n")
    };

    // Build modules YAML block
    let modules_yaml = if module_names.is_empty() {
        "    modules: []".to_string()
    } else {
        let mut lines = vec!["    modules:".to_string()];
        for m in &module_names {
            lines.push(format!("      - {}", m));
        }
        lines.join("\n")
    };

    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\n\
         kind: ConfigSource\n\
         metadata:\n\
         \x20 name: {}\n\
         \x20 version: \"{}\"\n\
         \x20 description: \"{}\"\n\
         spec:\n\
         \x20 provides:\n\
         {}\n\
         {}\n\
         \x20 policy:\n\
         \x20   required:\n\
         \x20     packages: {{}}\n\
         \x20     modules: []\n\
         \x20   recommended:\n\
         \x20     packages: {{}}\n\
         \x20     modules: []\n\
         \x20   optional:\n\
         \x20     packages: {{}}\n\
         \x20     modules: []\n\
         \x20   constraints:\n\
         \x20     noScripts: true\n\
         \x20     noSecretsRead: true\n",
        source_name, source_version, source_description, profiles_yaml, modules_yaml,
    );

    cfgd_core::atomic_write_str(&source_path, &yaml)?;
    printer.success(&format!(
        "Created cfgd-source.yaml at {}",
        source_path.display()
    ));
    if !profile_names.is_empty() {
        printer.info(&format!(
            "Included {} profile(s): {}",
            profile_names.len(),
            profile_names.join(", ")
        ));
    }
    if !module_names.is_empty() {
        printer.info(&format!(
            "Included {} module(s): {}",
            module_names.len(),
            module_names.join(", ")
        ));
    }
    printer.info("Edit the file to configure policy tiers and platform-profiles");

    Ok(())
}

// --- Source management commands (Phase 9) ---

pub(super) fn source_cache_dir(cli: &Cli) -> anyhow::Result<std::path::PathBuf> {
    if let Some(ref state_dir) = cli.state_dir {
        Ok(state_dir.join("sources"))
    } else {
        SourceManager::default_cache_dir().map_err(|e| anyhow::anyhow!(e))
    }
}

pub(super) fn cmd_source_add(
    cli: &Cli,
    printer: &Printer,
    args: &SourceAddArgs,
) -> anyhow::Result<()> {
    let url = &args.url;
    let name = args.name.as_deref();
    let branch = args.branch.as_deref();
    let profile = args.profile.as_deref();
    let accept_recommended = args.accept_recommended;
    let priority = args.priority;
    let opt_in = &args.opt_in;
    let sync_interval = args.sync_interval.as_deref();
    let auto_apply = args.auto_apply;
    let pin_version = args.version_pin.as_deref();
    printer.header("Add Config Source");

    // Infer name from URL if not provided
    let source_name = name
        .map(|s| s.to_string())
        .unwrap_or_else(|| infer_source_name(url));

    // Check if source already exists in config
    let config_path = cli.config.clone();
    if config_path.exists() {
        let cfg = config::load_config(&config_path)?;
        if cfg.spec.sources.iter().any(|s| s.name == source_name) {
            anyhow::bail!(
                "Source '{}' already exists. Use 'cfgd source update' to refresh.",
                source_name
            );
        }
    }

    // Clone and parse the source
    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    if config_path.exists()
        && let Ok(existing_cfg) = config::load_config(&config_path)
    {
        mgr.set_allow_unsigned(
            existing_cfg
                .spec
                .security
                .as_ref()
                .is_some_and(|s| s.allow_unsigned),
        );
    }
    let spec = SourceManager::build_source_spec(&source_name, url, profile);
    mgr.load_source(&spec, printer)?;

    let cached = mgr
        .get(&source_name)
        .ok_or_else(|| anyhow::anyhow!("Failed to load source '{}'", source_name))?;

    // Display source manifest info
    let manifest = &cached.manifest;
    printer.subheader("Source Manifest");
    printer.key_value("Name", &manifest.metadata.name);
    if let Some(ref version) = manifest.metadata.version {
        printer.key_value("Version", version);
    }
    if let Some(ref desc) = manifest.metadata.description {
        printer.key_value("Description", desc);
    }

    let provided_profiles = cfgd_core::config::source_profile_names(&manifest.spec.provides);
    if !provided_profiles.is_empty() {
        printer.key_value("Profiles", &provided_profiles.join(", "));
    }

    // Show policy summary
    let policy = &manifest.spec.policy;
    let required_count = count_policy_items(&policy.required);
    let recommended_count = count_policy_items(&policy.recommended);
    let locked_count = count_policy_items(&policy.locked);

    printer.newline();
    printer.subheader("Policy");
    if locked_count > 0 {
        printer.warning(&format!(
            "{} locked item(s) (cannot override)",
            locked_count
        ));
    }
    if required_count > 0 {
        printer.info(&format!(
            "{} required item(s) (team requirement)",
            required_count
        ));
    }
    if recommended_count > 0 {
        printer.info(&format!("{} recommended item(s)", recommended_count));
    }

    // Show constraints
    let constraints = &manifest.spec.policy.constraints;
    if constraints.no_scripts {
        printer.info("Scripts: blocked");
    }
    if constraints.no_secrets_read {
        printer.info("Secret access: blocked");
    }
    if !constraints.allowed_target_paths.is_empty() {
        printer.info(&format!(
            "Allowed paths: {}",
            constraints.allowed_target_paths.join(", ")
        ));
    }

    // Profile selection: explicit flag > platform auto-detect > single profile > interactive
    let auto_detected_profile =
        if profile.is_none() && !manifest.spec.provides.platform_profiles.is_empty() {
            let platform = cfgd_core::config::detect_platform();
            cfgd_core::config::match_platform_profile(
                &platform,
                &manifest.spec.provides.platform_profiles,
            )
            .inspect(|matched| {
                printer.success(&format!(
                    "Auto-selected profile '{}' for platform {}",
                    matched,
                    platform.distro.as_deref().unwrap_or(&platform.os)
                ));
            })
        } else {
            None
        };

    let selected_profile = if profile.is_some() {
        profile.map(|s| s.to_string())
    } else if auto_detected_profile.is_some() {
        auto_detected_profile
    } else if provided_profiles.len() == 1 {
        Some(provided_profiles[0].clone())
    } else if !provided_profiles.is_empty() {
        printer.newline();
        let selection =
            printer.prompt_select("Select a profile to subscribe to:", &provided_profiles)?;
        Some(selection.clone())
    } else {
        None
    };

    // Interactive priority prompt (when --priority not specified on command line)
    let resolved_priority = if let Some(p) = priority {
        p
    } else if args.yes {
        500
    } else {
        printer.newline();
        let input = printer.prompt_text("Set priority", "500")?;
        input
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("invalid priority: '{}' (must be a number)", input))?
    };

    // Conflict preview: check for conflicts with current config before subscribing
    if config_path.exists()
        && let Ok(cfg) = config::load_config(&config_path)
    {
        let pdir = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("profiles");
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref());

        if let Some(pn) = profile_name
            && let Ok(local_resolved) = config::resolve_profile(pn, &pdir)
        {
            // Build a CompositionInput for the prospective source
            let mut preview_layers = Vec::new();
            if let Some(ref pn) = selected_profile
                && let Ok(src_profiles_dir) = mgr.source_profiles_dir(&source_name)
                && src_profiles_dir.exists()
                && let Ok(r) = config::resolve_profile(pn, &src_profiles_dir)
            {
                preview_layers = r.layers;
            }

            let preview_sub = config::SubscriptionSpec {
                profile: selected_profile.clone(),
                priority: resolved_priority,
                accept_recommended,
                opt_in: opt_in.to_vec(),
                ..Default::default()
            };

            let preview_input = CompositionInput {
                source_name: source_name.clone(),
                priority: resolved_priority,
                policy: manifest.spec.policy.clone(),
                constraints: manifest.spec.policy.constraints.clone(),
                layers: preview_layers,
                subscription: SubscriptionConfig {
                    accept_recommended: preview_sub.accept_recommended,
                    opt_in: preview_sub.opt_in.clone(),
                    overrides: preview_sub.overrides.clone(),
                    reject: preview_sub.reject.clone(),
                },
            };

            match composition::compose(&local_resolved, &[preview_input]) {
                Ok(result) => {
                    if result.conflicts.is_empty() {
                        printer.newline();
                        printer.success("No conflicts with current config");
                    } else {
                        printer.newline();
                        printer.subheader("Conflicts with Current Config");
                        for conflict in &result.conflicts {
                            let label = conflict.resolution_type.label();
                            printer.warning(&format!(
                                "  {} {} <- {} ({})",
                                label,
                                conflict.resource_id,
                                conflict.winning_source,
                                conflict.details
                            ));
                        }
                    }
                }
                Err(e) => {
                    printer.warning(&format!("Failed to preview conflicts: {}", e));
                }
            }
        }
    }

    // Confirm subscription
    if !args.yes {
        printer.newline();
        if !printer.prompt_confirm("Subscribe to this source?")? {
            printer.info("Cancelled");
            return Ok(());
        }
    }

    // Build the source spec with user choices
    let mut source_spec =
        SourceManager::build_source_spec(&source_name, url, selected_profile.as_deref());
    if let Some(b) = branch {
        source_spec.origin.branch = b.to_string();
    }
    source_spec.subscription.accept_recommended = accept_recommended;
    source_spec.subscription.priority = resolved_priority;
    if !opt_in.is_empty() {
        source_spec.subscription.opt_in = opt_in.to_vec();
    }
    if let Some(interval) = sync_interval {
        source_spec.sync.interval = interval.to_string();
    }
    if auto_apply {
        source_spec.sync.auto_apply = true;
    }
    if let Some(pin) = pin_version {
        source_spec.sync.pin_version = Some(pin.to_string());
    }

    // Update cfgd.yaml
    add_source_to_config(&config_path, &source_spec)?;

    // Update state store
    let state = open_state_store(cli.state_dir.as_deref())?;
    state.upsert_config_source(
        &source_name,
        url,
        &spec.origin.branch,
        cached.last_commit.as_deref(),
        manifest.metadata.version.as_deref(),
        None,
    )?;

    printer.success(&format!("Subscribed to source '{}'", source_name));
    if let Some(ref profile) = selected_profile {
        printer.key_value("Profile", profile);
    }
    printer.info(MSG_RUN_APPLY);

    Ok(())
}

pub(super) fn cmd_source_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    if !config_path.exists() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<SourceListEntry>::new());
            return Ok(());
        }
        printer.header("Config Sources");
        printer.info("No config file found");
        return Ok(());
    }

    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        if printer.is_structured() {
            printer.write_structured(&Vec::<SourceListEntry>::new());
            return Ok(());
        }
        printer.header("Config Sources");
        printer.info("No sources configured");
        return Ok(());
    }

    let state = open_state_store(cli.state_dir.as_deref())?;

    let entries: Vec<SourceListEntry> = cfg
        .spec
        .sources
        .iter()
        .map(|source| {
            let state_info = state.config_source_by_name(&source.name).ok().flatten();
            SourceListEntry {
                name: source.name.clone(),
                url: source.origin.url.clone(),
                priority: source.subscription.priority,
                version: state_info.as_ref().and_then(|s| s.source_version.clone()),
                status: state_info
                    .as_ref()
                    .map(|s| s.status.clone())
                    .unwrap_or_else(|| "unknown".into()),
                last_fetched: state_info.as_ref().and_then(|s| s.last_fetched.clone()),
            }
        })
        .collect();

    if printer.write_structured(&entries) {
        return Ok(());
    }

    printer.header("Config Sources");

    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            vec![
                e.name.clone(),
                e.url.clone(),
                e.priority.to_string(),
                e.version.clone().unwrap_or_else(|| "-".into()),
                e.status.clone(),
                e.last_fetched.clone().unwrap_or_else(|| "never".into()),
            ]
        })
        .collect();
    printer.table(
        &[
            "Name",
            "URL",
            "Priority",
            "Version",
            "Status",
            "Last Fetched",
        ],
        &rows,
    );

    Ok(())
}

pub(super) fn cmd_source_show(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    let source_spec = cfg
        .spec
        .sources
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("Source '{}' not found", name))?;

    if printer.is_structured() {
        let state = open_state_store(cli.state_dir.as_deref())?;
        let state_info = state.config_source_by_name(name)?;
        let resources = state.managed_resources_by_source(name)?;
        let output = SourceShowOutput {
            name: name.to_string(),
            url: source_spec.origin.url.clone(),
            branch: source_spec.origin.branch.clone(),
            priority: source_spec.subscription.priority,
            accept_recommended: source_spec.subscription.accept_recommended,
            profile: source_spec.subscription.profile.clone(),
            sync_interval: source_spec.sync.interval.clone(),
            auto_apply: source_spec.sync.auto_apply,
            version_pin: source_spec.sync.pin_version.clone(),
            state: state_info.map(|s| SourceStateInfo {
                status: s.status,
                last_fetched: s.last_fetched,
                last_commit: s.last_commit,
                version: s.source_version,
            }),
            managed_resources: resources
                .iter()
                .map(|r| SourceResourceEntry {
                    resource_type: r.resource_type.clone(),
                    resource_id: r.resource_id.clone(),
                })
                .collect(),
        };
        printer.write_structured(&output);
        return Ok(());
    }

    printer.header(&format!("Source: {}", name));
    printer.key_value("URL", &source_spec.origin.url);
    printer.key_value("Branch", &source_spec.origin.branch);
    printer.key_value("Priority", &source_spec.subscription.priority.to_string());
    printer.key_value(
        "Accept Recommended",
        &source_spec.subscription.accept_recommended.to_string(),
    );
    if let Some(ref profile) = source_spec.subscription.profile {
        printer.key_value("Profile", profile);
    }
    printer.key_value("Sync Interval", &source_spec.sync.interval);
    printer.key_value("Auto Apply", &source_spec.sync.auto_apply.to_string());
    if let Some(ref pin) = source_spec.sync.pin_version {
        printer.key_value("Version Pin", pin);
    }

    // Show state info
    let state = open_state_store(cli.state_dir.as_deref())?;
    if let Some(state_info) = state.config_source_by_name(name)? {
        printer.newline();
        printer.subheader("State");
        printer.key_value("Status", &state_info.status);
        if let Some(ref fetched) = state_info.last_fetched {
            printer.key_value("Last Fetched", fetched);
        }
        if let Some(ref commit) = state_info.last_commit {
            printer.key_value("Last Commit", &commit[..commit.len().min(12)]);
        }
        if let Some(ref version) = state_info.source_version {
            printer.key_value("Version", version);
        }
    }

    // Show managed resources from this source
    let resources = state.managed_resources_by_source(name)?;
    if !resources.is_empty() {
        printer.newline();
        printer.subheader("Managed Resources");
        let rows: Vec<Vec<String>> = resources
            .iter()
            .map(|r| vec![r.resource_type.clone(), r.resource_id.clone()])
            .collect();
        printer.table(&["Type", "Resource"], &rows);
    }

    // Load and show manifest from cache
    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
    // Populate the manager from the cached source on disk
    if let Err(e) = mgr.load_source(source_spec, printer) {
        printer.warning(&format!("Failed to load source manifest: {}", e));
    }
    if let Some(cached) = mgr.get(name) {
        printer.newline();
        printer.subheader("Manifest");
        printer.key_value("Name", &cached.manifest.metadata.name);
        if let Some(ref desc) = cached.manifest.metadata.description {
            printer.key_value("Description", desc);
        }

        let policy = &cached.manifest.spec.policy;
        let locked_count = count_policy_items(&policy.locked);
        let required_count = count_policy_items(&policy.required);
        let recommended_count = count_policy_items(&policy.recommended);

        if locked_count + required_count + recommended_count > 0 {
            printer.newline();
            printer.subheader("Policy Summary");

            if locked_count > 0 {
                printer.key_value("Locked", &locked_count.to_string());
                display_policy_items(printer, &policy.locked, "  ");
            }
            if required_count > 0 {
                printer.key_value("Required", &required_count.to_string());
                display_policy_items(printer, &policy.required, "  ");
            }
            if recommended_count > 0 {
                printer.key_value("Recommended", &recommended_count.to_string());
                display_policy_items(printer, &policy.recommended, "  ");
            }
        }
    }

    Ok(())
}

pub(super) fn cmd_source_remove(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    keep_all: bool,
    remove_all: bool,
) -> anyhow::Result<()> {
    if keep_all && remove_all {
        anyhow::bail!("cannot use --keep-all and --remove-all together");
    }

    printer.header(&format!("Remove Source: {}", name));

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if !cfg.spec.sources.iter().any(|s| s.name == name) {
        anyhow::bail!("Source '{}' not found in config", name);
    }

    let state = open_state_store(cli.state_dir.as_deref())?;
    let resources = state.managed_resources_by_source(name)?;

    if !resources.is_empty() && !keep_all && !remove_all {
        // Interactive: ask for each resource or batch
        printer.info(&format!(
            "This source manages {} resource(s):",
            resources.len()
        ));
        let rows: Vec<Vec<String>> = resources
            .iter()
            .map(|r| vec![r.resource_type.clone(), r.resource_id.clone()])
            .collect();
        printer.table(&["Type", "Resource"], &rows);
        printer.newline();

        let options = vec![
            "Keep all (resources become locally managed)".to_string(),
            "Remove all".to_string(),
        ];
        let choice = printer.prompt_select("What to do with these resources?", &options)?;

        if choice.starts_with("Keep") {
            // Re-assign resources to local
            for r in &resources {
                state.upsert_managed_resource(
                    &r.resource_type,
                    &r.resource_id,
                    "local",
                    r.last_hash.as_deref(),
                    r.last_applied,
                )?;
            }
            printer.info("Resources transferred to local management");
        }
        // If "Remove all", they'll be cleaned up when state is updated
    } else if keep_all {
        for r in &resources {
            state.upsert_managed_resource(
                &r.resource_type,
                &r.resource_id,
                "local",
                r.last_hash.as_deref(),
                r.last_applied,
            )?;
        }
    }

    // Remove from config
    remove_source_from_config(&config_path, name)?;

    // Remove from state
    state.remove_config_source(name)?;
    state.remove_source_config_hash(name)?;

    // Remove cached data
    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    let _ = mgr.remove_source(name);

    printer.success(&format!("Source '{}' removed", name));
    Ok(())
}

pub(super) fn cmd_source_update(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
) -> anyhow::Result<()> {
    printer.header("Update Sources");

    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        printer.info("No sources configured");
        return Ok(());
    }

    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
    let state = open_state_store(cli.state_dir.as_deref())?;

    let sources_to_update: Vec<&config::SourceSpec> = if let Some(name) = name {
        cfg.spec.sources.iter().filter(|s| s.name == name).collect()
    } else {
        cfg.spec.sources.iter().collect()
    };

    if sources_to_update.is_empty()
        && let Some(name) = name
    {
        anyhow::bail!("Source '{}' not found", name);
    }

    for source in &sources_to_update {
        // Capture old manifest before fetching (for permission change detection)
        let source_dir = cache_dir.join(&source.name);
        let old_manifest = if source_dir.exists() {
            mgr.parse_manifest(&source.name, &source_dir).ok()
        } else {
            None
        };

        match mgr.load_source(source, printer) {
            Ok(()) => {
                if let Some(cached) = mgr.get(&source.name) {
                    // Detect permission-expanding changes between old and new manifests
                    if let Some(ref old) = old_manifest {
                        let old_input = build_permission_input(&source.name, &old.spec.policy);
                        let new_input =
                            build_permission_input(&source.name, &cached.manifest.spec.policy);
                        let perm_changes =
                            composition::detect_permission_changes(&[old_input], &[new_input]);
                        if !perm_changes.is_empty() {
                            printer.newline();
                            printer.warning(&format!(
                                "Source '{}' update changes permissions:",
                                source.name
                            ));
                            for change in &perm_changes {
                                printer.warning(&format!("  - {}", change.description));
                            }
                            match printer.prompt_confirm("Accept permission changes?") {
                                Ok(true) => {}
                                Ok(false) => {
                                    printer.info(&format!(
                                        "Skipped source '{}' (permission changes rejected)",
                                        source.name
                                    ));
                                    continue;
                                }
                                Err(_) => {
                                    printer.info(&format!(
                                        "Skipped source '{}' (prompt cancelled)",
                                        source.name
                                    ));
                                    continue;
                                }
                            }
                        }
                    }

                    state.upsert_config_source(
                        &source.name,
                        &source.origin.url,
                        &source.origin.branch,
                        cached.last_commit.as_deref(),
                        cached.manifest.metadata.version.as_deref(),
                        source.sync.pin_version.as_deref(),
                    )?;
                    printer.success(&format!("Updated source '{}'", source.name));
                }
            }
            Err(e) => {
                printer.error(&format!("Failed to update source '{}': {}", source.name, e));
                state.update_config_source_status(&source.name, "error")?;
            }
        }
    }

    Ok(())
}

/// Build a minimal [`CompositionInput`] from a source policy for permission change detection.
/// Only the `source_name`, `policy`, and `constraints` fields are used by
/// [`composition::detect_permission_changes`]; the rest are defaulted.
pub(super) fn build_permission_input(
    name: &str,
    policy: &config::ConfigSourcePolicy,
) -> CompositionInput {
    CompositionInput {
        source_name: name.to_string(),
        priority: 0,
        policy: policy.clone(),
        constraints: policy.constraints.clone(),
        layers: Vec::new(),
        subscription: SubscriptionConfig::default(),
    }
}

pub(super) fn cmd_source_override(
    cli: &Cli,
    printer: &Printer,
    source_name: &str,
    action: SourceOverrideAction,
    path: &str,
    value: Option<&str>,
) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    // Verify source exists in config
    if !cfg.spec.sources.iter().any(|s| s.name == source_name) {
        anyhow::bail!("Source '{}' not found", source_name);
    }

    match action {
        SourceOverrideAction::Reject => {
            printer.info(&format!(
                "Rejecting '{}' from source '{}'",
                path, source_name
            ));
            update_source_rejection(&config_path, source_name, path)?;
            printer.success(&format!("Rejected '{}' from '{}'", path, source_name));
        }
        SourceOverrideAction::Set => {
            let val = value.ok_or_else(|| anyhow::anyhow!("'set' action requires a value"))?;
            printer.info(&format!(
                "Overriding '{}' = '{}' for source '{}'",
                path, val, source_name
            ));
            update_source_override(&config_path, source_name, path, val)?;
            printer.success(&format!(
                "Override set: {} = {} for '{}'",
                path, val, source_name
            ));
        }
    }

    Ok(())
}

pub(super) fn cmd_source_replace(
    cli: &Cli,
    printer: &Printer,
    old_name: &str,
    new_url: &str,
) -> anyhow::Result<()> {
    printer.header(&format!("Replace Source: {}", old_name));

    // Capture old source's profile and priority before removing
    let config_path = cli.config.clone();
    let old_cfg = config::load_config(&config_path)?;
    let old_source = old_cfg.spec.sources.iter().find(|s| s.name == old_name);
    let old_profile = old_source.and_then(|s| s.subscription.profile.clone());
    let old_priority = old_source.map(|s| s.subscription.priority).unwrap_or(500);

    // Remove old source (keeping resources)
    cmd_source_remove(cli, printer, old_name, true, false)?;

    // Add new source with same name, carrying over profile and priority
    cmd_source_add(
        cli,
        printer,
        &SourceAddArgs {
            url: new_url.to_string(),
            name: Some(old_name.to_string()),
            branch: None,
            profile: old_profile,
            accept_recommended: false,
            priority: Some(old_priority),
            opt_in: vec![],
            sync_interval: None,
            auto_apply: false,
            version_pin: None,
            yes: true,
        },
    )?;

    printer.success(&format!("Source '{}' replaced with {}", old_name, new_url));
    Ok(())
}

pub(super) fn cmd_source_priority(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    value: Option<u32>,
) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    let source = cfg
        .spec
        .sources
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("source '{}' not found", name))?;

    match value {
        Some(new_priority) => {
            // Update priority in cfgd.yaml
            with_source_config(&config_path, name, |source_entry| {
                let subscription = source_entry.get_mut("subscription").ok_or_else(|| {
                    anyhow::anyhow!("source '{}' has no subscription block", name)
                })?;

                if let Some(mapping) = subscription.as_mapping_mut() {
                    mapping.insert(
                        serde_yaml::Value::String("priority".into()),
                        serde_yaml::Value::Number(serde_yaml::Number::from(new_priority)),
                    );
                }
                Ok(())
            })?;

            printer.success(&format!(
                "Source '{}' priority updated: {} -> {}",
                name, source.subscription.priority, new_priority
            ));
        }
        None => {
            printer.key_value("Source", name);
            printer.key_value("Priority", &source.subscription.priority.to_string());
            printer.info("Local config priority is 1000");
        }
    }

    Ok(())
}

// --- Source config helpers ---

pub(super) fn infer_source_name(url: &str) -> String {
    // Extract name from URL: git@github.com:acme/dev-config.git -> acme-dev-config
    let cleaned = url
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .or_else(|| url.rsplit(':').next())
        .unwrap_or(url);

    // If the path component includes org/repo, use org-repo
    if let Some(rest) = url.strip_prefix("git@")
        && let Some(path) = rest.split(':').nth(1)
    {
        return path.trim_end_matches(".git").replace('/', "-");
    }

    cleaned.to_string()
}

pub(super) fn count_policy_items(items: &config::PolicyItems) -> usize {
    let mut count = 0;
    if let Some(ref pkgs) = items.packages {
        if let Some(ref brew) = pkgs.brew {
            count += brew.formulae.len() + brew.casks.len() + brew.taps.len();
        }
        if let Some(ref apt) = pkgs.apt {
            count += apt.packages.len();
        }
        if let Some(ref cargo) = pkgs.cargo {
            count += cargo.packages.len();
        }
        count += pkgs.pipx.len() + pkgs.dnf.len();
        if let Some(ref npm) = pkgs.npm {
            count += npm.global.len();
        }
    }
    count += items.files.len();
    count += items.env.len();
    count += items.system.len();
    count
}

pub(super) fn display_policy_items(printer: &Printer, items: &config::PolicyItems, indent: &str) {
    if let Some(ref pkgs) = items.packages {
        if let Some(ref brew) = pkgs.brew {
            for f in &brew.formulae {
                printer.info(&format!("{indent}brew formula: {f}"));
            }
            for c in &brew.casks {
                printer.info(&format!("{indent}brew cask: {c}"));
            }
        }
        if let Some(ref apt) = pkgs.apt {
            for p in &apt.packages {
                printer.info(&format!("{indent}apt: {p}"));
            }
        }
        if let Some(ref cargo) = pkgs.cargo {
            for p in &cargo.packages {
                printer.info(&format!("{indent}cargo: {p}"));
            }
        }
        for p in &pkgs.pipx {
            printer.info(&format!("{indent}pipx: {p}"));
        }
        for p in &pkgs.dnf {
            printer.info(&format!("{indent}dnf: {p}"));
        }
        if let Some(ref npm) = pkgs.npm {
            for p in &npm.global {
                printer.info(&format!("{indent}npm: {p}"));
            }
        }
    }
    for f in &items.files {
        printer.info(&format!("{indent}file: {}", f.target.display()));
    }
    for ev in &items.env {
        printer.info(&format!("{indent}env: {}", ev.name));
    }
    for k in items.system.keys() {
        printer.info(&format!("{indent}system: {k}"));
    }
}

pub(super) fn display_pending_decisions(
    printer: &Printer,
    decisions: &[cfgd_core::state::PendingDecision],
) {
    let mut by_source: std::collections::BTreeMap<&str, Vec<&cfgd_core::state::PendingDecision>> =
        std::collections::BTreeMap::new();
    for d in decisions {
        by_source.entry(&d.source).or_default().push(d);
    }
    for (source_name, items) in &by_source {
        printer.info(&format!(
            "{}: {} pending item{}",
            source_name,
            items.len(),
            if items.len() == 1 { "" } else { "s" }
        ));
        for item in items {
            printer.info(&format!(
                "  {} {} — {} ({})",
                item.tier, item.resource, item.summary, item.action
            ));
        }
    }
}

pub(super) fn add_source_to_config(
    config_path: &Path,
    source: &config::SourceSpec,
) -> anyhow::Result<()> {
    if !config_path.exists() {
        anyhow::bail!("Config file not found: {}", config_path.display());
    }

    mutate_config_yaml(config_path, true, |raw| {
        let spec = raw
            .get_mut("spec")
            .ok_or_else(|| anyhow::anyhow!("config missing 'spec'"))?;
        let sources = spec
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("spec is not a mapping"))?
            .entry(serde_yaml::Value::String("sources".into()))
            .or_insert(serde_yaml::Value::Sequence(vec![]));
        let seq = sources
            .as_sequence_mut()
            .ok_or_else(|| anyhow::anyhow!("sources is not a sequence"))?;
        let source_value = serde_yaml::to_value(source)?;
        seq.push(source_value);
        Ok(())
    })
}

pub(super) fn remove_source_from_config(config_path: &Path, name: &str) -> anyhow::Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    mutate_config_yaml(config_path, true, |raw| {
        if let Some(spec) = raw.get_mut("spec")
            && let Some(sources) = spec.get_mut("sources")
            && let Some(seq) = sources.as_sequence_mut()
        {
            seq.retain(|item| {
                item.get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| n != name)
                    .unwrap_or(true)
            });
        }
        Ok(())
    })
}

pub(super) fn update_source_rejection(
    config_path: &Path,
    source_name: &str,
    path: &str,
) -> anyhow::Result<()> {
    with_source_config(config_path, source_name, |source| {
        let subscription = source
            .as_mapping_mut()
            .and_then(|m| {
                m.entry(serde_yaml::Value::String("subscription".into()))
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                m.get_mut(serde_yaml::Value::String("subscription".into()))
            })
            .ok_or_else(|| anyhow::anyhow!("cannot access subscription"))?;

        let sub_map = subscription
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("subscription is not a mapping"))?;
        let reject = sub_map
            .entry(serde_yaml::Value::String("reject".into()))
            .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        // Replace null with empty mapping (serde serializes default Value::Null)
        if reject.is_null() {
            *reject = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }

        set_nested_yaml_value(reject, path, &serde_yaml::Value::Null)?;
        Ok(())
    })
}

pub(super) fn update_source_override(
    config_path: &Path,
    source_name: &str,
    path: &str,
    value: &str,
) -> anyhow::Result<()> {
    with_source_config(config_path, source_name, |source| {
        let subscription = source
            .as_mapping_mut()
            .and_then(|m| {
                m.entry(serde_yaml::Value::String("subscription".into()))
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                m.get_mut(serde_yaml::Value::String("subscription".into()))
            })
            .ok_or_else(|| anyhow::anyhow!("cannot access subscription"))?;

        let sub_map = subscription
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("subscription is not a mapping"))?;
        let overrides = sub_map
            .entry(serde_yaml::Value::String("overrides".into()))
            .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        // Replace null with empty mapping (serde serializes default Value::Null)
        if overrides.is_null() {
            *overrides = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }

        set_nested_yaml_value(
            overrides,
            path,
            &serde_yaml::Value::String(value.to_string()),
        )?;
        Ok(())
    })
}

pub(super) fn find_source_in_config<'a>(
    raw: &'a mut serde_yaml::Value,
    source_name: &str,
) -> Option<&'a mut serde_yaml::Value> {
    raw.get_mut("spec")?
        .get_mut("sources")?
        .as_sequence_mut()?
        .iter_mut()
        .find(|item| {
            item.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n == source_name)
                .unwrap_or(false)
        })
}

/// Generalized read-parse-mutate-write loop for `cfgd.yaml`.
///
/// Loads the YAML at `config_path`, hands the mutable root `serde_yaml::Value`
/// to `f`, then serializes and atomically writes the result. When `validate`
/// is `true`, the serialized output is round-tripped through
/// `config::parse_config` before write — callers that could produce schema-invalid
/// documents (`set`, `unset`) pass `true`; mechanical add/remove-by-key
/// operations pass `false` so the write path is free of the typed-parse cost.
///
/// Use this instead of open-coding the `read_to_string → from_str → mutate →
/// to_string → atomic_write_str` pattern, which diverged in validation
/// behavior (set/unset validated; add/remove did not) before this helper.
pub(super) fn mutate_config_yaml<F>(config_path: &Path, validate: bool, f: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut serde_yaml::Value) -> anyhow::Result<()>,
{
    let contents = std::fs::read_to_string(config_path)?;
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents)?;
    f(&mut raw)?;
    let output = serde_yaml::to_string(&raw)?;
    if validate {
        config::parse_config(&output, config_path)
            .map_err(|e| anyhow::anyhow!("config would become invalid: {}", e))?;
    }
    cfgd_core::atomic_write_str(config_path, &output)?;
    Ok(())
}

/// Load config YAML, find a named source, apply a mutation, and write back.
/// The closure receives the mutable source entry; the helper handles I/O.
pub(super) fn with_source_config<F>(
    config_path: &Path,
    source_name: &str,
    f: F,
) -> anyhow::Result<()>
where
    F: FnOnce(&mut serde_yaml::Value) -> anyhow::Result<()>,
{
    mutate_config_yaml(config_path, false, |raw| {
        let source = find_source_in_config(raw, source_name)
            .ok_or_else(|| anyhow::anyhow!("source '{}' not found in config file", source_name))?;
        f(source)
    })
}

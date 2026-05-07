use super::*;

pub(crate) fn cmd_source_add(
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

    let selected_profile: Option<String> = match resolve_non_interactive_profile(
        profile,
        auto_detected_profile.as_deref(),
        &provided_profiles,
    ) {
        Some(p) => Some(p),
        None if provided_profiles.is_empty() => None,
        None => {
            printer.newline();
            let selection =
                printer.prompt_select("Select a profile to subscribe to:", &provided_profiles)?;
            Some(selection.clone())
        }
    };

    // Interactive priority prompt (when --priority not specified on command line)
    let resolved_priority = if let Some(p) = priority {
        p
    } else if args.yes {
        DEFAULT_NONINTERACTIVE_PRIORITY
    } else {
        printer.newline();
        let input = printer.prompt_text("Set priority", "500")?;
        parse_priority_input(&input)?
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

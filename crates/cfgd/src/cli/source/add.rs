use super::*;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_source_add(cli: &Cli, printer: &Printer, args: &SourceAddArgs) -> anyhow::Result<()> {
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
    printer.heading("Add Config Source");

    // Infer name from URL if not provided
    let source_name = name
        .map(|s| s.to_string())
        .unwrap_or_else(|| infer_source_name(url));

    // Check if source already exists in config
    let config_path = cli.config.clone();
    if config_path.exists() {
        let cfg = config::load_config(&config_path)?;
        if cfg.spec.sources.iter().any(|s| s.name == source_name) {
            printer.emit(cfgd_core::output::error_doc(
                &source_name,
                "already_exists",
                format!(
                    "Source '{}' already exists. Use 'cfgd source update' to refresh.",
                    source_name
                ),
                serde_json::Value::Null,
            ));
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
    // Surface lib-side load failure as a load_failed Doc so structured
    // consumers see the same {"error": "load_failed", ...} shape as the
    // "Ok-but-no-cache-entry" fallback below.
    if let Err(e) = mgr.load_source(&spec, printer) {
        printer.emit(cfgd_core::output::error_doc(
            &source_name,
            "load_failed",
            format!("Failed to load source '{}': {}", source_name, e),
            serde_json::json!({ "url": url }),
        ));
        anyhow::bail!("Failed to load source '{}': {}", source_name, e);
    }

    let cached = match mgr.get(&source_name) {
        Some(c) => c,
        None => {
            printer.emit(cfgd_core::output::error_doc(
                &source_name,
                "load_failed",
                format!("Failed to load source '{}'", source_name),
                serde_json::json!({ "url": url }),
            ));
            anyhow::bail!("Failed to load source '{}'", source_name);
        }
    };

    // Display source manifest info
    let manifest = &cached.manifest;
    let provided_profiles = display_source_manifest(printer, manifest);

    // Profile selection: explicit flag > platform auto-detect > single profile > interactive
    let auto_detected_profile =
        if profile.is_none() && !manifest.spec.provides.platform_profiles.is_empty() {
            let platform = cfgd_core::config::detect_platform();
            cfgd_core::config::match_platform_profile(
                &platform,
                &manifest.spec.provides.platform_profiles,
            )
            .inspect(|matched| {
                printer.status_simple(
                    Role::Ok,
                    format!(
                        "Auto-selected profile '{}' for platform {}",
                        matched,
                        platform.distro.as_deref().unwrap_or(&platform.os)
                    ),
                );
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
            let mut preview_layers = Vec::new();
            if let Some(ref pn) = selected_profile
                && let Ok(src_profiles_dir) = mgr.source_profiles_dir(&source_name)
                && src_profiles_dir.exists()
                && let Ok(r) = config::resolve_profile(pn, &src_profiles_dir)
            {
                preview_layers = r.layers;
            }

            let preview_input = build_subscription_preview_input(
                &source_name,
                resolved_priority,
                &manifest.spec.policy,
                accept_recommended,
                opt_in,
                preview_layers,
            );

            match composition::compose(&local_resolved, &[preview_input]) {
                Ok(result) => {
                    let lines = format_conflict_preview_lines(&result.conflicts);
                    if lines.is_empty() {
                        // Role::Ok marks the conflict-check step as having passed cleanly
                        // (consistent with other clean-state preview steps).
                        printer.status_simple(Role::Ok, "No conflicts with current config");
                    } else {
                        let conflicts_sec = printer.section("Conflicts with Current Config");
                        for line in &lines {
                            conflicts_sec.status_simple(Role::Warn, line.trim_start().to_string());
                        }
                    }
                }
                Err(e) => {
                    printer
                        .status_simple(Role::Warn, format!("Failed to preview conflicts: {}", e));
                }
            }
        }
    }

    // Confirm subscription
    if !args.yes && !printer.prompt_confirm("Subscribe to this source?")? {
        printer.emit(
            Doc::new()
                .status(Role::Info, "Cancelled")
                .with_data(serde_json::json!({
                    "name": source_name,
                    "url": url,
                    "cancelled": true,
                })),
        );
        return Ok(());
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

    let mut doc = Doc::new().status(Role::Ok, format!("Subscribed to source '{}'", source_name));
    if let Some(ref p) = selected_profile {
        doc = doc.kv("Profile", p);
    }
    doc = doc.hint(MSG_RUN_APPLY).with_data(serde_json::json!({
        "name": source_name,
        "url": url,
        "branch": source_spec.origin.branch,
        "commit": cached.last_commit.clone().unwrap_or_default(),
        "profile": selected_profile,
        "priority": resolved_priority,
    }));
    printer.emit(doc);

    Ok(())
}

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
    let pin_version = args.pin_version.as_deref();
    printer.heading("Add Config Source");

    // Infer name from URL if not provided
    let source_name = name
        .map(|s| s.to_string())
        .unwrap_or_else(|| infer_source_name(url));

    // A pin selects its own git ref (tag or commit), so an explicit branch is
    // meaningless and contradictory — reject the combination before any clone.
    if args.branch.is_some() && args.pin_version.is_some() {
        return Err(crate::cli::cli_error(
            &source_name,
            "branch_pin_conflict",
            "--branch and --pin-version are mutually exclusive; a pin selects its own ref",
            serde_json::json!({}),
        ));
    }

    // Argument-injection guard: a `-`-leading pin would be parsed as a git flag
    // by the downstream `git fetch`/`checkout`. Reject early with a clear error.
    if pin_version.is_some_and(|p| p.trim_start().starts_with('-')) {
        return Err(crate::cli::cli_error(
            &source_name,
            "invalid_pin_version",
            "--pin-version must not start with '-' (a leading dash is reserved for git flags)",
            serde_json::json!({}),
        ));
    }

    // Check if source already exists in config
    let config_path = cli.config.clone();
    if config_path.exists() {
        let cfg = config::load_config(&config_path)?;
        if cfg.spec.sources.iter().any(|s| s.name == source_name) {
            return Err(crate::cli::cli_error(
                &source_name,
                "already_exists",
                format!(
                    "Source '{}' already exists. Use 'cfgd source update' to refresh.",
                    source_name
                ),
                serde_json::json!({}),
            ));
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
    let mut spec = SourceManager::build_source_spec(&source_name, url, profile);
    if let Some(b) = branch {
        spec.origin.branch = b.to_string();
    }
    // Apply the pin at clone time so resolution selects the pinned ref now,
    // not just when it is later persisted to config.
    if let Some(pin) = pin_version {
        spec.sync.pin_version = Some(pin.to_string());
    }
    // Surface lib-side load failure with the same {"error": "load_failed", ...}
    // structured shape as the "Ok-but-no-cache-entry" fallback below, so both
    // load-failure paths look identical to structured consumers.
    if let Err(e) = mgr.load_source(&spec, printer) {
        return Err(crate::cli::cli_error(
            &source_name,
            "load_failed",
            format!("Failed to load source '{}': {}", source_name, e),
            serde_json::json!({ "url": url }),
        ));
    }

    let cached = match mgr.get(&source_name) {
        Some(c) => c,
        None => {
            return Err(crate::cli::cli_error(
                &source_name,
                "load_failed",
                format!("Failed to load source '{}'", source_name),
                serde_json::json!({ "url": url }),
            ));
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
        cfgd_core::config::validate_source_priority(p).map_err(|m| anyhow::anyhow!(m))?
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

            match composition::compose(
                &local_resolved,
                &[preview_input],
                composition::ConstraintMode::Enforce,
            ) {
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
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "Failed to preview conflicts: {}",
                            cfgd_core::output::collapse_to_subject_line(&e),
                        ),
                    );
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

    // Record the resolved commit SHA in the sources lockfile so composition
    // is bit-reproducible across machines.
    if let Some(ref commit) = cached.last_commit {
        let lock_entry = cfgd_core::config::SourceLockEntry {
            name: source_name.clone(),
            url: url.clone(),
            pin_version: pin_version.map(|s| s.to_string()),
            resolved_ref: cached.resolved_ref.clone(),
            resolved_commit: commit.clone(),
            locked_at: cfgd_core::utc_now_iso8601(),
        };
        let cfg_dir = config_dir(cli);
        if let Err(e) = cfgd_core::update_source_lock_entry(&cfg_dir, lock_entry) {
            printer.status_simple(
                cfgd_core::output::Role::Warn,
                super::sources_lock_update_warning(&e),
            );
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::OutputFormat;

    fn base_args(url: &str) -> SourceAddArgs {
        SourceAddArgs {
            url: url.to_string(),
            name: None,
            branch: None,
            profile: None,
            accept_recommended: false,
            priority: None,
            opt_in: Vec::new(),
            sync_interval: None,
            auto_apply: false,
            pin_version: None,
            yes: true,
        }
    }

    fn cli_for(config: PathBuf) -> Cli {
        Cli {
            config,
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            output: crate::cli::OutputFormatArg(OutputFormat::Table),
            list_envelope: false,
            jsonpath: None,
            state_dir: None,
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    fn meta_of(err: &anyhow::Error) -> &crate::cli::CliErrorMeta {
        err.downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("source add handler must return CliErrorMeta")
    }

    #[test]
    fn add_rejects_branch_and_pin_version_together() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(dir.path().join("cfgd.yaml"));
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("https://example.com/acme/dev.git");
        args.branch = Some("main".into());
        args.pin_version = Some("v1.0.0".into());

        let err = cmd_source_add(&cli, &printer, &args)
            .expect_err("branch + pin must be rejected before any clone");
        drop(printer);

        let meta = meta_of(&err);
        assert_eq!(
            meta.error_kind, "branch_pin_conflict",
            "expected branch_pin_conflict, got: {meta:?}"
        );
        assert_eq!(
            meta.name, "dev",
            "error name must be the source name inferred from the URL's final path segment"
        );
    }

    #[test]
    fn add_rejects_pin_version_with_leading_dash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(dir.path().join("cfgd.yaml"));
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("https://example.com/acme/dev.git");
        args.pin_version = Some("--upload-pack=evil".into());

        let err = cmd_source_add(&cli, &printer, &args)
            .expect_err("dash-leading pin is an argument-injection risk and must be rejected");
        drop(printer);

        let meta = meta_of(&err);
        assert_eq!(
            meta.error_kind, "invalid_pin_version",
            "expected invalid_pin_version, got: {meta:?}"
        );
    }

    #[test]
    fn add_rejects_pin_version_with_leading_whitespace_dash() {
        // The guard trims leading whitespace before the dash check, so a
        // " -flag" pin must also be rejected.
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(dir.path().join("cfgd.yaml"));
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("https://example.com/acme/dev.git");
        args.pin_version = Some("  -x".into());

        let err = cmd_source_add(&cli, &printer, &args).expect_err("whitespace+dash pin rejected");
        drop(printer);

        assert_eq!(meta_of(&err).error_kind, "invalid_pin_version");
    }

    #[test]
    fn add_rejects_duplicate_source_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("cfgd.yaml");
        // Seed a config that already subscribes to a source named "dev"
        // (the name inferred from the URL below's final path segment).
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: dev\n      origin:\n        type: Git\n        url: https://example.com/acme/dev.git\n        branch: main\n",
        )
        .expect("write seed config");
        let cli = cli_for(config_path);
        let (printer, _cap) = Printer::for_test_doc();

        let args = base_args("https://example.com/acme/dev.git");

        let err = cmd_source_add(&cli, &printer, &args)
            .expect_err("re-adding an existing source name must error before clone");
        drop(printer);

        let meta = meta_of(&err);
        assert_eq!(
            meta.error_kind, "already_exists",
            "expected already_exists, got: {meta:?}"
        );
        assert_eq!(meta.name, "dev");
        assert!(
            meta.message.contains("cfgd source update"),
            "already_exists message must point to 'source update', got: {}",
            meta.message
        );
    }

    #[test]
    fn add_explicit_name_overrides_inferred_for_duplicate_check() {
        // With --name set, the duplicate check keys off the explicit name, not
        // the URL-inferred one.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: custom\n      origin:\n        type: Git\n        url: https://example.com/acme/dev.git\n        branch: main\n",
        )
        .expect("write seed config");
        let cli = cli_for(config_path);
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("https://example.com/acme/dev.git");
        args.name = Some("custom".into());

        let err =
            cmd_source_add(&cli, &printer, &args).expect_err("duplicate explicit name must error");
        drop(printer);

        let meta = meta_of(&err);
        assert_eq!(meta.error_kind, "already_exists");
        assert_eq!(
            meta.name, "custom",
            "duplicate check must use the explicit --name"
        );
    }
}

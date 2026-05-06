use super::*;

pub(crate) fn cmd_source_show(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
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

use super::*;

pub(crate) fn cmd_source_remove(
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

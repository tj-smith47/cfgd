use super::*;

pub(crate) fn cmd_source_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
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

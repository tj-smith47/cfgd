use super::*;
use cfgd_core::output::{Doc, Printer as PrinterV2, Role, renderer::Table as TableV2};

/// Build the `cfgd source list` Doc from a populated entries vector + `--wide`
/// flag. Pure; the caller assembles the entries from disk.
pub fn build_source_list_doc(entries: &[SourceListEntry], wide: bool) -> Doc {
    let mut doc = Doc::new().heading("Config Sources");

    if entries.is_empty() {
        doc = doc.status(Role::Info, "No sources configured");
        return doc.with_data(entries);
    }

    if wide {
        let mut t = TableV2::new([
            "Name",
            "URL",
            "Priority",
            "Version",
            "Status",
            "Last Fetched",
        ]);
        for e in entries {
            t = t.row([
                e.name.clone(),
                e.url.clone(),
                e.priority.to_string(),
                e.version.clone().unwrap_or_else(|| "-".into()),
                e.status.clone(),
                e.last_fetched.clone().unwrap_or_else(|| "never".into()),
            ]);
        }
        doc = doc.table(t);
    } else {
        let mut t = TableV2::new(["Name", "URL", "Priority", "Status"]);
        for e in entries {
            t = t.row([
                e.name.clone(),
                e.url.clone(),
                e.priority.to_string(),
                e.status.clone(),
            ]);
        }
        doc = doc.table(t);
    }

    doc.with_data(entries)
}

/// Doc emitted when no config file is present yet.
pub fn build_source_list_no_config_doc() -> Doc {
    let empty: Vec<SourceListEntry> = Vec::new();
    Doc::new()
        .heading("Config Sources")
        .status(Role::Info, "No config file found")
        .with_data(&empty)
}

pub fn cmd_source_list(cli: &Cli, v2_printer: &PrinterV2) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    if !config_path.exists() {
        if v2_printer.is_structured() {
            v2_printer.emit(Doc::new().with_data(Vec::<SourceListEntry>::new()));
            return Ok(());
        }
        v2_printer.emit(build_source_list_no_config_doc());
        return Ok(());
    }

    let cfg = config::load_config(&config_path)?;

    if cfg.spec.sources.is_empty() {
        let entries: Vec<SourceListEntry> = Vec::new();
        v2_printer.emit(build_source_list_doc(&entries, v2_printer.is_wide()));
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

    v2_printer.emit(build_source_list_doc(&entries, v2_printer.is_wide()));
    Ok(())
}

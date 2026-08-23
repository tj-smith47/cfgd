use super::*;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};
use cfgd_core::state::source_status_display;

/// Build the `cfgd source list` Doc from a populated entries vector + `--wide`
/// flag. Pure; the caller assembles the entries from disk.
pub fn build_source_list_doc(entries: &[SourceListEntry], wide: bool) -> Doc {
    let mut doc = Doc::new().heading("Config Sources");

    if entries.is_empty() {
        doc = doc.status(Role::Info, "No sources configured");
        return doc.with_data(entries);
    }

    // `Source`, not `URL`: the column names where the source comes FROM, which
    // is what a reader scans for, and the value is not always a URL a browser
    // would take. The `-o json` field stays `url`.
    if wide {
        let mut t = Table::new([
            "Name",
            "Source",
            "Priority",
            "Version",
            "Status",
            "Last Fetched",
        ]);
        for e in entries {
            let (status, role) = source_status_display(&e.status);
            t = t.row_styled([
                (e.name.clone(), None),
                (e.url.clone(), None),
                (e.priority.to_string(), None),
                (e.version.clone().unwrap_or_else(|| "-".into()), None),
                (status.to_string(), Some(role)),
                (
                    e.last_fetched.clone().unwrap_or_else(|| "never".into()),
                    None,
                ),
            ]);
        }
        doc = doc.table(t);
    } else {
        let mut t = Table::new(["Name", "Source", "Priority", "Status"]);
        for e in entries {
            let (status, role) = source_status_display(&e.status);
            t = t.row_styled([
                (e.name.clone(), None),
                (e.url.clone(), None),
                (e.priority.to_string(), None),
                (status.to_string(), Some(role)),
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

pub fn cmd_source_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    if !config_path.exists() {
        if printer.is_structured() {
            printer.emit(Doc::new().with_data(Vec::<SourceListEntry>::new()));
            return Ok(());
        }
        printer.emit(build_source_list_no_config_doc());
        return Ok(());
    }

    let mut cfg = config::load_config(&config_path)?;
    drain_config_deprecations(printer, &mut cfg);

    if cfg.spec.sources.is_empty() {
        let entries: Vec<SourceListEntry> = Vec::new();
        printer.emit(build_source_list_doc(&entries, printer.is_wide()));
        return Ok(());
    }

    let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;

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

    printer.emit(build_source_list_doc(&entries, printer.is_wide()));
    Ok(())
}

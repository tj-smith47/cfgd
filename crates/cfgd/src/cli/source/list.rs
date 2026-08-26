use super::*;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};
use cfgd_core::state::source_status_display;
use cfgd_core::yes_no;

/// The section every surface calls a config source's collection — one noun,
/// singular in `source:<name>` and plural here. `Config Sources` spelled the
/// `cfgd.yaml` key at the reader instead of the thing the rows are.
pub const SOURCES_SECTION: &str = "Sources";

/// Build the `cfgd source list` Doc from a populated entries vector + `--wide`
/// flag. Pure; the caller assembles the entries from disk and passes `now`, so
/// a render pins in a test rather than reading a clock inside the builder.
pub fn build_source_list_doc(entries: &[SourceListEntry], wide: bool, now: &str) -> Doc {
    let mut doc = Doc::new().heading(SOURCES_SECTION);

    if entries.is_empty() {
        doc = doc.status(Role::Info, "No sources configured");
        return doc.with_data(entries);
    }

    // `Source`, not `URL`: the column names where the source comes FROM, which
    // is what a reader scans for, and the value is not always a URL a browser
    // would take. The `-o json` field stays `url`.
    //
    // `Last Sync` and `Signed` are on the DEFAULT table, not `--wide`: they are
    // the two facts that change between one `cfgd source list` and the next,
    // and a listing whose every column is a restatement of cfgd.yaml tells a
    // reader nothing they could not read there.
    let mut columns = vec!["Name", "Source", "Priority"];
    if wide {
        columns.push("Version");
    }
    columns.extend(["Status", "Last Sync", "Signed"]);

    let mut t = Table::new(columns);
    for e in entries {
        let (status, role) = source_status_display(&e.status);
        let mut row = vec![
            (e.name.clone(), None),
            (e.url.clone(), None),
            (e.priority.to_string(), None),
        ];
        if wide {
            row.push((e.version.clone().unwrap_or_else(|| "-".into()), None));
        }
        row.extend([
            (status.to_string(), Some(role)),
            (last_sync_display(e.last_fetched.as_deref(), now), None),
            (yes_no(e.signed).to_string(), None),
        ]);
        t = t.row_styled(row);
    }
    doc = doc.table(t);

    doc.with_data(entries)
}

/// The ONE human rendering of a config source's last fetch, shared by every
/// surface that shows one (`source list`, `source show`, `status`) — the
/// workspace's [`cfgd_core::humanize_age_cell`] under the name this domain
/// calls it, so `Last Sync` and `backup list`'s `Last Run` cannot disagree
/// about what a listed instant reads as.
pub fn last_sync_display(last_fetched: Option<&str>, now: &str) -> String {
    cfgd_core::humanize_age_cell(last_fetched, now)
}

/// Doc emitted when no config file is present yet.
pub fn build_source_list_no_config_doc() -> Doc {
    let empty: Vec<SourceListEntry> = Vec::new();
    Doc::new()
        .heading(SOURCES_SECTION)
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

    let now = cfgd_core::utc_now_iso8601();

    if cfg.spec.sources.is_empty() {
        let entries: Vec<SourceListEntry> = Vec::new();
        printer.emit(build_source_list_doc(&entries, printer.is_wide(), &now));
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
                signed: state_info.as_ref().and_then(|s| s.last_commit_signed),
            }
        })
        .collect();

    printer.emit(build_source_list_doc(&entries, printer.is_wide(), &now));
    Ok(())
}

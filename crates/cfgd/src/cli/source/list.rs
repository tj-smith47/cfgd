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

    doc = doc.table(sources_table(entries, wide, now));

    doc.with_data(entries)
}

/// The `Sources` table, whichever surface renders it.
///
/// `cfgd source list` and `cfgd daemon status` printed two tables under one
/// section name with disjoint columns — `Signed` missing exactly where a daemon
/// operator wants it, `Drift` missing from the listing, and neither naming the
/// revision the daemon had just pulled. One builder, one column set, so the two
/// surfaces cannot answer the same question differently.
///
/// `Source`, not `URL`: the column names where the source comes FROM, which is
/// what a reader scans for, and the value is not always a URL a browser would
/// take. The `-o json` field stays `url`.
///
/// `Last Sync`, `Signed` and `Requires Signed` are on the DEFAULT table, not
/// `--wide`: they are the facts that change between one listing and the next,
/// and a listing whose every column restates cfgd.yaml tells a reader nothing
/// they could not read there. `Signed` reports what the last fetch FOUND and
/// `Requires Signed` what the subscription DEMANDS — a demanding source and a
/// non-demanding one with signed HEADs rendered identically without both.
pub fn sources_table(entries: &[SourceListEntry], wide: bool, now: &str) -> Table {
    let mut columns = vec!["Name", "Source", "Priority"];
    if wide {
        columns.push("Version");
    }
    columns.extend([
        "Status",
        "Drift",
        "Commit",
        "Last Sync",
        "Signed",
        "Requires Signed",
    ]);

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
            (
                e.drift_count.map_or_else(|| "-".into(), |n| n.to_string()),
                None,
            ),
            (
                e.last_commit.as_deref().map_or_else(
                    || "-".to_string(),
                    |c| cfgd_core::short_commit(c).to_string(),
                ),
                None,
            ),
            (last_sync_display(e.last_fetched.as_deref(), now), None),
            (yes_no(e.signed).to_string(), None),
            (yes_no(Some(e.require_signed_commits)).to_string(), None),
        ]);
        t = t.row_styled(row);
    }
    t
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
    let entries = configured_source_entries(&cfg, &state);

    printer.emit(build_source_list_doc(&entries, printer.is_wide(), &now));
    Ok(())
}

/// Every `spec.sources[]` entry paired with what the state store recorded for
/// it — the row set both `cfgd source list` and `cfgd daemon status` render.
///
/// `drift_count` is left `None` here: this read never scans, and the daemon,
/// which holds live per-source drift, fills it on its own rows.
pub fn configured_source_entries(
    cfg: &cfgd_core::config::CfgdConfig,
    state: &cfgd_core::state::StateStore,
) -> Vec<SourceListEntry> {
    cfg.spec
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
                require_signed_commits: source.subscription.require_signed_commits,
                last_commit: state_info.as_ref().and_then(|s| s.last_commit.clone()),
                drift_count: None,
            }
        })
        .collect()
}

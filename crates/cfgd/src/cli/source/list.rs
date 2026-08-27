use super::*;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};
use cfgd_core::state::source_status_display;
use cfgd_core::{ABSENT, yes_no};

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
///
/// A column no row in THIS render can fill is dropped, not padded
/// (`Table::without_unfillable_columns`): `Drift` is per-source drift, which
/// nothing can attribute today (see
/// `cfgd_core::daemon::SourceStatus::drift_count`), so every cell was `-` —
/// seven characters per row answering nothing on the listing that is the
/// family's rest point. The same rule covers `Version` (`--wide`, absent until
/// a manifest names one) and `Commit` / `Signed` before the first fetch. The
/// `-o json` payload keeps every field; a `null` there is a fact.
pub fn sources_table(entries: &[SourceListEntry], wide: bool, now: &str) -> Table {
    let cell = |value: Option<String>| (value.unwrap_or_else(|| ABSENT.to_string()), None);
    let mut columns: Vec<(&str, Vec<Cell>)> = vec![
        (
            "Name",
            entries.iter().map(|e| (e.name.clone(), None)).collect(),
        ),
        (
            "Source",
            entries.iter().map(|e| cell(e.url.clone())).collect(),
        ),
        (
            "Priority",
            entries
                .iter()
                .map(|e| cell(e.priority.map(|p| p.to_string())))
                .collect(),
        ),
    ];
    if wide {
        columns.push((
            "Version",
            entries.iter().map(|e| cell(e.version.clone())).collect(),
        ));
    }
    columns.extend([
        (
            "Status",
            entries
                .iter()
                .map(|e| {
                    let (status, role) = source_status_display(&e.status);
                    (status.to_string(), Some(role))
                })
                .collect(),
        ),
        (
            "Drift",
            entries
                .iter()
                .map(|e| cell(e.drift_count.map(|n| n.to_string())))
                .collect(),
        ),
        (
            "Commit",
            entries
                .iter()
                .map(|e| {
                    cell(
                        e.last_commit
                            .as_deref()
                            .map(|c| cfgd_core::short_commit(c).to_string()),
                    )
                })
                .collect(),
        ),
        (
            "Last Sync",
            entries
                .iter()
                .map(|e| (last_sync_display(e.last_fetched.as_deref(), now), None))
                .collect(),
        ),
        (
            "Signed",
            entries
                .iter()
                .map(|e| (yes_no(e.signed).to_string(), None))
                .collect(),
        ),
        (
            "Requires Signed",
            entries
                .iter()
                .map(|e| (yes_no(e.require_signed_commits).to_string(), None))
                .collect(),
        ),
    ]);
    let mut t = Table::new(columns.iter().map(|(name, _)| *name));
    for i in 0..entries.len() {
        t = t.row_styled(columns.iter().map(|(_, cells)| cells[i].clone()));
    }
    t.without_unfillable_columns()
}

/// One rendered `Sources` cell: its text and the role that re-styles it.
type Cell = (String, Option<Role>);

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
/// `drift_count` is left `None` here: this read never scans, and no surface
/// can attribute drift to one source (see
/// `cfgd_core::daemon::SourceStatus::drift_count`) — the machine-wide total is
/// stated once, in the header of whichever report holds it.
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
                url: Some(source.origin.url.clone()),
                priority: Some(source.subscription.priority),
                version: state_info.as_ref().and_then(|s| s.source_version.clone()),
                status: state_info
                    .as_ref()
                    .map(|s| s.status.clone())
                    .unwrap_or_else(|| "unknown".into()),
                last_fetched: state_info.as_ref().and_then(|s| s.last_fetched.clone()),
                signed: state_info.as_ref().and_then(|s| s.last_commit_signed),
                require_signed_commits: Some(source.subscription.require_signed_commits),
                last_commit: state_info.as_ref().and_then(|s| s.last_commit.clone()),
                drift_count: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::Verbosity;

    fn entry(name: &str) -> SourceListEntry {
        SourceListEntry {
            name: name.to_string(),
            url: Some(format!("https://example.test/{name}.git")),
            priority: Some(500),
            version: None,
            status: "active".to_string(),
            last_fetched: None,
            signed: None,
            require_signed_commits: Some(false),
            last_commit: None,
            drift_count: None,
        }
    }

    /// The rendered table as `(headers, cells per row)`, read back off the
    /// aligned text the way a reader does: a column spans from its header's
    /// first character to the next header's.
    fn rendered(table: Table) -> (Vec<String>, Vec<Vec<String>>) {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        printer.emit(Doc::new().table(table));
        drop(printer);
        let text = cfgd_core::test_helpers::captured_text(&buf);
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header = lines.next().expect("a header line");
        let mut starts = Vec::new();
        let mut in_word = false;
        let mut gap = 0usize;
        for (i, ch) in header.char_indices() {
            if ch == ' ' {
                gap += 1;
                in_word = in_word && gap < 2;
            } else {
                if !in_word {
                    starts.push(i);
                }
                in_word = true;
                gap = 0;
            }
        }
        let slice = |line: &str, i: usize| -> String {
            let end = starts
                .get(i + 1)
                .copied()
                .unwrap_or(line.len())
                .min(line.len());
            line.get(starts[i]..end).unwrap_or("").trim().to_string()
        };
        let headers: Vec<String> = (0..starts.len()).map(|i| slice(header, i)).collect();
        let rows: Vec<Vec<String>> = lines
            .filter(|l| !l.starts_with('─'))
            .map(|l| (0..starts.len()).map(|i| slice(l, i)).collect())
            .collect();
        (headers, rows)
    }

    /// A column no row in this render can fill is not on the table. `source
    /// list` had a `Drift` column every one of whose cells was `-`, because
    /// only the daemon holds per-source drift; the same rule drops `Version`,
    /// `Commit` and `Signed` before anything has recorded them, and keeps each
    /// the moment one row has a value.
    #[test]
    fn a_column_no_row_can_fill_is_dropped_from_the_listing() {
        let now = "2026-08-26T12:00:00Z";
        let (headers, rows) = rendered(sources_table(&[entry("a"), entry("b")], true, now));
        assert_eq!(
            headers,
            vec![
                "Name",
                "Source",
                "Priority",
                "Status",
                "Last Sync",
                "Requires Signed"
            ],
            "every all-absent column is gone, and only those"
        );
        for (i, header) in headers.iter().enumerate() {
            assert!(
                rows.iter().any(|r| r[i] != ABSENT),
                "column {header} has nothing to say in this render"
            );
        }

        let mut filled = entry("a");
        filled.drift_count = Some(0);
        filled.version = Some("1.2.0".to_string());
        filled.last_commit = Some("4b8857cd0f1e2222".to_string());
        filled.signed = Some(true);
        let (headers, rows) = rendered(sources_table(&[filled, entry("b")], true, now));
        assert_eq!(
            headers,
            vec![
                "Name",
                "Source",
                "Priority",
                "Version",
                "Status",
                "Drift",
                "Commit",
                "Last Sync",
                "Signed",
                "Requires Signed",
            ],
            "one row with a value keeps the column for every row"
        );
        assert_eq!(rows[1][5], ABSENT, "the row with no value still reads `-`");
    }
}

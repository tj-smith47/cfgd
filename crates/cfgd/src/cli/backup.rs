//! `cfgd backup` — run, inspect, or restore declarative backups
//! (`spec.backups[]`).

use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::backup::{BackupUnit, SnapshotInfo};
use cfgd_core::format_bytes;
use cfgd_core::output::{Doc, Printer, Role, TitleLabel, renderer::Table};
use cfgd_core::state::BackupRunRecord;

fn backup_not_found_error(name: &str, valid: Vec<String>) -> anyhow::Error {
    let hint = if valid.is_empty() {
        "no backups are declared in the active profile".to_string()
    } else {
        format!("valid backups: {}", valid.join(", "))
    };
    // `_with_hints` (not `cli_error_ctx`) so the valid-names hint renders in
    // human mode too, not just the `extras` JSON payload — see
    // `cli/profile/switch.rs::cmd_profile_switch` for the sibling pattern.
    cli_error_ctx_with_hints(
        cfgd_core::errors::CfgdError::Backup(cfgd_core::errors::BackupError::UnknownName {
            name: name.to_string(),
            valid,
        })
        .into(),
        name,
        "not_found",
        format!("Backup '{name}' not found"),
        serde_json::json!({ "hint": hint }),
        vec![hint],
    )
}

/// Resolve one declared backup by name, or fail with the shared name-listing
/// error. Every surface that takes a backup name resolves it through here so an
/// unknown name reads the same from `list`, `run`, and `restore`.
fn find_backup_spec<'a>(
    backups: &'a [config::BackupSpec],
    name: &str,
) -> anyhow::Result<&'a config::BackupSpec> {
    backups.iter().find(|b| b.name == name).ok_or_else(|| {
        backup_not_found_error(name, backups.iter().map(|b| b.name.clone()).collect())
    })
}

/// The three values every unit-constructing surface needs: where config lives,
/// the run-history store, and the state dir a `BackupUnit` anchors to.
///
/// The store is the RUN's, borrowed rather than opened here: every caller has
/// already built a context to resolve its config through, and a second open of
/// the same DB in the same command is exactly what that context exists to stop.
fn unit_context<'a>(
    ctx: &'a RunContext<'_>,
) -> anyhow::Result<(PathBuf, &'a cfgd_core::state::StateStore, PathBuf)> {
    let cli = ctx.cli();
    let config_dir = config_dir(cli);
    let state = ctx.state()?;
    let state_dir = cfgd_core::resolve_state_dir(cli.state_dir.as_deref(), cli.scope())?;
    Ok((config_dir, state, state_dir))
}

/// Build the `cfgd backup list` Doc from a populated entries vector. Pure; the
/// caller assembles the entries from config + the state store and passes `now`,
/// so a render pins in a test rather than reading a clock inside the builder.
///
/// `Status` and `Last Run` are two cells, the way `source list` splits them: one
/// cell holding a status word AND a timestamp can be tinted by neither, and the
/// instant it carried answered "when exactly" — the question the `-o json`
/// payload's `lastRunAt` is for — on the one column a reader scans to learn how
/// stale the unit is.
pub fn build_backup_list_doc(entries: &[BackupListEntry], now: &str) -> Doc {
    let mut doc = Doc::new().heading("Backups");

    if entries.is_empty() {
        doc = doc.status(Role::Info, "No backups configured");
        return doc.with_data(entries);
    }

    // `Snapshots` sits beside `Retention` because the two are one fact read
    // twice: how many this unit holds, and how many it is allowed to keep.
    let mut t = Table::new([
        "Name",
        "Source",
        "Schedule",
        "Retention",
        "Snapshots",
        "Status",
        "Last Run",
        "Next Run",
    ]);
    for e in entries {
        // TitleCased here and nowhere else: `last_run_status` stays the stored
        // token every `-o json` reader matches on.
        let (status, role) = match &e.last_run_status {
            Some(stored) => {
                let (word, role) = cfgd_core::state::backup_run_status_display(stored);
                // A run that wrote its snapshot and then failed a hook is
                // neither of the stored tokens: the data is there, something
                // still needs attention.
                match e.last_run_clean {
                    Some(false) => (format!("{word} (dirty)"), Some(Role::Warn)),
                    _ => (word.to_string(), Some(role)),
                }
            }
            None => ("-".to_string(), None),
        };
        t = t.row_styled(vec![
            (e.name.clone(), None),
            (e.source.clone(), None),
            (e.schedule.clone().unwrap_or_else(|| "-".into()), None),
            (e.retention.to_string(), None),
            (snapshots_cell(e.snapshots, e.safety_snapshots), None),
            (status, role),
            (
                cfgd_core::humanize_age_cell(e.last_run_at.as_deref(), now),
                None,
            ),
            (
                cfgd_core::humanize_until_cell(e.next_run_at.as_deref(), now),
                None,
            ),
        ]);
    }
    doc = doc.table(t);
    doc.with_data(entries)
}

/// The Snapshots cell: the total, with the safety-snapshot share called out
/// when there is one (`2 (1 safety)`).
///
/// A safety snapshot occupies the destination and counts against retention like
/// any other, so the cell leads with the total a reader compares to `Retention`.
/// The parenthetical is what stops the total reading as "this unit backed up
/// twice" after a single run and a restore. `-` when the count is unknown,
/// unchanged: an unreadable store is not a count of zero.
fn snapshots_cell(total: Option<usize>, safety: Option<usize>) -> String {
    match (total, safety) {
        (Some(n), Some(s)) if s > 0 => format!("{n} ({s} safety)"),
        (Some(n), _) => n.to_string(),
        (None, _) => "-".to_string(),
    }
}

/// Build the `cfgd backup list <name> --snapshots` Doc. Pure; the caller
/// assembles the entries from the unit's recorded runs.
///
/// Columns and payload keys are the snapshot analogue of
/// [`build_backup_list_doc`]: `Created` carries the age, `Kind` the display
/// word, and `Size` goes through the CLI's one byte renderer so it reads the
/// same as `cfgd upgrade`'s asset size. The payload keeps the ISO 8601 stamp and
/// the wire token.
///
/// `Created` earns its column only as an age: a snapshot's NAME is its stamp
/// (`BACKUP_TIMESTAMP_FORMAT`), so the instant restated the row's own first cell
/// one column later.
pub fn build_backup_snapshot_list_doc(
    name: &str,
    entries: &[BackupSnapshotEntry],
    now: &str,
) -> Doc {
    let mut doc = Doc::new().heading_title("Snapshots", name);

    if entries.is_empty() {
        doc = doc.status(Role::Info, format!("Backup '{name}' has no snapshots"));
        return doc.with_data(entries);
    }

    let mut t = Table::new(["Snapshot", "Kind", "Created", "Size"]);
    for e in entries {
        t = t.row([
            e.name.clone(),
            e.kind.display_str().to_string(),
            cfgd_core::humanize_age_cell(Some(&e.created), now),
            format_bytes(e.size_bytes),
        ]);
    }
    doc = doc.table(t);
    doc.with_data(entries)
}

pub fn cmd_backup_list(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    snapshots: bool,
) -> anyhow::Result<()> {
    // Clap's `requires = "name"` already rejects a bare `--snapshots`; this
    // covers the in-process callers (tests, MCP dispatch) that bypass it.
    if snapshots && name.is_none() {
        return Err(cli_error_with_hints(
            "--snapshots",
            "missing_argument",
            "--snapshots needs a backup name",
            serde_json::json!({ "flag": "--snapshots" }),
            vec!["cfgd backup list <name> --snapshots".to_string()],
        ));
    }

    let config_path = cli.config.clone();
    if !config_path.exists() {
        // A named lookup cannot be answered without a config, so it stays a
        // not-found error rather than degrading to "no backups" — the caller
        // asked about one unit, not for an inventory.
        if let Some(name) = name {
            return Err(backup_not_found_error(name, Vec::new()));
        }
        let empty: Vec<BackupListEntry> = Vec::new();
        if printer.is_structured() {
            printer.emit(Doc::new().with_data(&empty));
            return Ok(());
        }
        printer.emit(
            Doc::new()
                .heading("Backups")
                .status(Role::Info, "No config file found")
                .with_data(&empty),
        );
        return Ok(());
    }

    let ctx = RunContext::new(cli, printer);
    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    // Cache-only composition (no network refresh) and Report constraint mode:
    // listing backups is a read surface, the same class as
    // `status`/`diff`/`compliance`. `backup run` is not — it composes in
    // Enforce because it runs hooks and writes snapshots.
    let composition = compose_with_sources(
        &ctx,
        cfg,
        local_resolved,
        printer,
        false,
        composition::ConstraintMode::Report,
    )?;
    let backups = composition.resolved.merged.backups;

    // A named lookup resolves before the inventory is assembled so an unknown
    // name is the same typed, name-listing error `backup run` returns rather
    // than an empty table the caller has to interpret.
    let selected: Vec<&config::BackupSpec> = match name {
        Some(n) => {
            let spec = find_backup_spec(&backups, n)?;
            if snapshots {
                return list_unit_snapshots(&ctx, spec, profile_name);
            }
            vec![spec]
        }
        None => backups.iter().collect(),
    };

    if selected.is_empty() {
        printer.emit(build_backup_list_doc(&[], &cfgd_core::utc_now_iso8601()));
        return Ok(());
    }

    // A state-store failure costs the run HISTORY, not the inventory: the
    // declared units come from config, which loaded fine. Degrading to
    // "never" with a warning keeps the config half of the command useful when
    // `state.db` is unreadable, matching how `resolve_backup_tasks` treats the
    // same failure.
    let state = match ctx.state() {
        Ok(state) => Some(state),
        Err(e) => {
            printer
                .status(Role::Warn, "Backup history unavailable")
                .detail(cfgd_core::output::collapse_to_subject_line(&e));
            None
        }
    };
    // Counting a unit's snapshots means resolving its destination, which needs
    // the state dir as well as the store — so an unreadable state degrades the
    // count exactly as it degrades the history columns, rather than half-way.
    let unit_dirs = state.and_then(|_| {
        cfgd_core::resolve_state_dir(cli.state_dir.as_deref(), cli.scope())
            .ok()
            .map(|state_dir| (config_dir(cli), state_dir))
    });
    let entries: Vec<BackupListEntry> = selected
        .iter()
        .map(|spec| {
            let last = state.and_then(|state| state.latest_backup_run(&spec.name).ok().flatten());
            let counts =
                unit_dirs
                    .as_ref()
                    .zip(state)
                    .and_then(|((config_dir, state_dir), state)| {
                        let unit = BackupUnit::new(spec, config_dir, profile_name, state_dir);
                        cfgd_core::backup::list_snapshots(&unit, state)
                            .ok()
                            .map(|s| (s.len(), s.iter().filter(|i| i.kind.is_safety()).count()))
                    });
            BackupListEntry {
                name: spec.name.clone(),
                source: spec.source.posix().to_string(),
                schedule: spec.schedule.clone(),
                retention: spec.retention,
                last_run_status: last.as_ref().map(|r| r.status.as_str().to_string()),
                last_run_at: last.as_ref().map(|r| r.finished_at.clone()),
                last_run_clean: last.as_ref().map(BackupRunRecord::is_clean),
                // Seeded from the same `finished_at` the daemon anchors an
                // interval schedule on, so the listed time is the one the
                // timer will actually use rather than a second opinion.
                next_run_at: spec.schedule.as_deref().and_then(|schedule| {
                    cfgd_core::backup::next_run_at(
                        schedule,
                        last.as_ref().map(|r| r.finished_at.as_str()),
                    )
                }),
                snapshots: counts.map(|(total, _)| total),
                safety_snapshots: counts.map(|(_, safety)| safety),
            }
        })
        .collect();

    printer.emit(build_backup_list_doc(
        &entries,
        &cfgd_core::utc_now_iso8601(),
    ));
    Ok(())
}

/// Emit the `--snapshots` view for one already-resolved unit.
///
/// Unlike the inventory, this cannot degrade when the state store is
/// unreadable: the run records ARE the snapshot list — there is no config half
/// left to render — so a store failure is the command's failure.
fn list_unit_snapshots(
    ctx: &RunContext<'_>,
    spec: &config::BackupSpec,
    profile_name: &str,
) -> anyhow::Result<()> {
    let (config_dir, state, state_dir) = unit_context(ctx)?;
    let unit = BackupUnit::new(spec, &config_dir, profile_name, &state_dir);

    let entries: Vec<BackupSnapshotEntry> = cfgd_core::backup::list_snapshots(&unit, state)?
        .iter()
        .map(BackupSnapshotEntry::from)
        .collect();

    ctx.printer().emit(build_backup_snapshot_list_doc(
        &spec.name,
        &entries,
        &cfgd_core::utc_now_iso8601(),
    ));
    Ok(())
}

/// Everything `cfgd backup restore` was asked for, so the flag set travels as
/// one value instead of four positional booleans and options.
pub struct RestoreArgs<'a> {
    pub name: &'a str,
    pub at: Option<&'a str>,
    pub to: Option<&'a Path>,
    pub yes: bool,
}

/// Turn a snapshot-selection failure into the CLI's structured error shape,
/// with the alternatives rendered in human mode as well as in the payload —
/// the same treatment [`backup_not_found_error`] gives an unknown backup name.
fn snapshot_selection_error(name: &str, e: cfgd_core::errors::BackupError) -> anyhow::Error {
    let (kind, hint) = match &e {
        cfgd_core::errors::BackupError::NoSnapshots { .. } => (
            "no_snapshots",
            format!("take one with `cfgd backup run {name}`"),
        ),
        cfgd_core::errors::BackupError::AmbiguousSnapshot { matches, .. } => (
            "ambiguous_snapshot",
            format!("matching snapshots: {}", matches.join(", ")),
        ),
        cfgd_core::errors::BackupError::SnapshotNotFound { available, .. } => (
            "snapshot_not_found",
            if available.is_empty() {
                format!("take one with `cfgd backup run {name}`")
            } else {
                format!("available snapshots: {}", available.join(", "))
            },
        ),
        _ => ("restore_failed", format!("see `cfgd backup list {name}`")),
    };
    let message = cfgd_core::output::collapse_to_subject_line(&e);
    cli_error_ctx_with_hints(
        cfgd_core::errors::CfgdError::Backup(e).into(),
        name,
        kind,
        message,
        serde_json::json!({ "hint": hint }),
        vec![hint],
    )
}

pub fn cmd_backup_restore(
    cli: &Cli,
    printer: &Printer,
    args: &RestoreArgs<'_>,
) -> anyhow::Result<()> {
    // Same split `cmd_backup_run` uses: the payload Doc has already been
    // emitted by the time the exit code is decided, so exiting here keeps a
    // failed restore from being rendered as a SECOND top-level document that
    // no single-document `-o json` reader could parse.
    match run_backup_restore(cli, printer, args)? {
        Some(outcome) if !outcome.is_clean() => cfgd_core::exit::ExitCode::Error.exit(),
        _ => Ok(()),
    }
}

/// Core of `backup restore`. `Ok(None)` means the operator declined at the
/// confirmation prompt — nothing ran, and that is a success, not a failure.
///
/// Kept out of [`cmd_backup_restore`] so the body stays in-process testable
/// (`process::exit` would abort the test binary).
pub fn run_backup_restore(
    cli: &Cli,
    printer: &Printer,
    args: &RestoreArgs<'_>,
) -> anyhow::Result<Option<cfgd_core::backup::RestoreOutcome>> {
    printer.heading_title(&TitleLabel::new("Restore", args.name));

    let ctx = RunContext::new(cli, printer);
    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    // Enforce, like `backup run`: a restore executes the unit's hooks and
    // overwrites live data, so a source constraint violation must abort rather
    // than be recorded and stepped over.
    let composition = compose_with_sources(
        &ctx,
        cfg,
        local_resolved,
        printer,
        false,
        composition::ConstraintMode::Enforce,
    )?;
    let backups = composition.resolved.merged.backups;

    let spec = find_backup_spec(&backups, args.name)?;

    let (config_dir, state, state_dir) = unit_context(&ctx)?;
    let unit = BackupUnit::new(spec, &config_dir, profile_name, &state_dir);

    let snapshots = cfgd_core::backup::list_snapshots(&unit, state)?;
    let selected: &SnapshotInfo =
        cfgd_core::backup::select_snapshot(args.name, &snapshots, args.at)
            .map_err(|e| snapshot_selection_error(args.name, e))?;

    let target = cfgd_core::backup::restore_target(&unit, args.to);

    if !args.yes && !confirm_restore(printer, args.name, selected, &target)? {
        printer.emit(Doc::new().status(Role::Info, "Aborted").with_data(
            &BackupRestoreDeclinedOutput {
                name: args.name.to_string(),
                snapshot: selected.name.clone(),
                restored_to: target.resolved_display(),
                restored: false,
                declined: true,
            },
        ));
        return Ok(None);
    }

    let started = std::time::Instant::now();
    let outcome = cfgd_core::backup::restore_backup(&unit, state, printer, selected, args.to)?;

    // The same skeleton `backup run` settles through — owner group, then the
    // run's own verdict — so the command's two mutating verbs read as one
    // command rather than two.
    let tally = cfgd_core::backup::report_restore(printer, &outcome);
    cfgd_core::reconciler::render_run_rollup(
        &tally,
        cfgd_core::reconciler::RunTitle::Restore,
        printer,
        Some(started.elapsed()),
    );

    printer.emit(Doc::new().with_data(BackupRestoreOutput::from(&outcome)));
    Ok(Some(outcome))
}

/// Ask before overwriting live data.
///
/// A refusal to prompt (piped stdin, structured output) is an ERROR, not a
/// silent decline: the caller asked for a restore, and quietly reporting
/// "aborted" for a run that could never have been confirmed would read as the
/// operator's choice. The remedy — `--yes` / `CFGD_YES` — rides along.
fn confirm_restore(
    printer: &Printer,
    name: &str,
    snapshot: &SnapshotInfo,
    target: &cfgd_core::backup::RestoreTarget,
) -> anyhow::Result<bool> {
    // The RESOLVED path is what gets overwritten, so it is what the operator
    // agrees to. A symlinked source is named both ways — agreeing to a path you
    // did not type is its own kind of surprise.
    let into = if target.was_redirected_by_a_link() {
        format!(
            "{} (via {})",
            target.resolved_display(),
            target.requested_display()
        )
    } else {
        target.resolved_display()
    };
    let question = format!(
        "Restore '{}' from snapshot {} into {}?",
        name, snapshot.name, into
    );
    printer.prompt_confirm(&question).map_err(|e| {
        let hint = "pass --yes (or set CFGD_YES=1) to restore without a prompt".to_string();
        // The prompt's own refusal quotes the whole question back; nesting that
        // inside this message renders the prompt twice and reads as two
        // different problems. Only a prompt that was actually reached and then
        // failed has a cause worth repeating.
        let message = if printer.can_prompt() {
            cfgd_core::output::collapse_to_subject_line(&e)
        } else {
            format!("Restore of '{name}' needs confirmation, and this session cannot prompt")
        };
        cli_error_with_hints(
            name,
            "confirmation_required",
            message,
            serde_json::json!({ "hint": hint, "snapshot": snapshot.name }),
            vec![hint],
        )
    })
}

pub fn cmd_backup_run(cli: &Cli, printer: &Printer, name: Option<&str>) -> anyhow::Result<()> {
    let outcome = run_backup_run(cli, printer, name)?;

    // A scripted consumer must be able to detect a failed, dirty, or refused
    // backup from the exit code alone — `run_backup_run` already emitted the
    // per-unit status lines and the summary Doc; exit nonzero directly here
    // (mirroring `cmd_source_update`) so the failure isn't re-rendered as a
    // SECOND top-level document after the payload, which would leave
    // `-o json` stdout unparseable by any single-document reader. Kept out of
    // the core so the body stays in-process testable (`process::exit` would
    // abort the test binary).
    if !outcome.fully_clean() {
        cfgd_core::exit::ExitCode::Error.exit();
    }

    Ok(())
}

/// What a `backup run` invocation did, as the exit-code decision needs it.
///
/// One report per requested unit, in request order. A `Vec<BackupRunRecord>`
/// cannot answer the question: a unit refused for a held lock produces no
/// record at all, so "every record is clean" would be vacuously true for it,
/// and both shapes must exit nonzero.
#[derive(Debug, Default)]
pub struct BackupRunOutcome {
    pub reports: Vec<cfgd_core::backup::BackupRunReport>,
}

impl BackupRunOutcome {
    /// True when every requested unit ran and produced an intact snapshot.
    pub fn fully_clean(&self) -> bool {
        self.reports
            .iter()
            .all(cfgd_core::backup::BackupRunReport::is_clean)
    }
}

/// Core of `backup run`: resolves the target unit(s) and runs them as one run —
/// a `Backup` header, the `Backups` pseudo-phase with a `backup:<name>` group
/// per unit, and a rollup. `name = None` runs every declared backup (scheduled
/// or not — the schedule only gates the automatic run inside `cfgd apply`; an
/// explicit `backup run` always runs).
///
/// A unit another process is already backing up is reported in the returned
/// outcome, NOT as an `Err`: the summary `Doc` has been emitted by then, and an
/// error returned past it would be rendered by the central sink as a second
/// top-level document on the same stdout.
pub fn run_backup_run(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
) -> anyhow::Result<BackupRunOutcome> {
    let ctx = RunContext::new(cli, printer);
    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    // Cache-only composition (no network refresh), but Enforce constraint mode:
    // `backup run` executes user-declared hooks and writes snapshots, so it is a
    // mutating surface like apply/plan/daemon and must abort on a source
    // violation rather than record it and continue. Only `backup list`, which
    // reads, composes in Report.
    let composition = compose_with_sources(
        &ctx,
        cfg,
        local_resolved,
        printer,
        false,
        composition::ConstraintMode::Enforce,
    )?;
    let backups = composition.resolved.merged.backups;

    let targets: Vec<&config::BackupSpec> = match name {
        Some(n) => vec![find_backup_spec(&backups, n)?],
        None => backups.iter().collect(),
    };

    if targets.is_empty() {
        printer.emit(
            Doc::new()
                .status(Role::Info, "No backups configured")
                .with_data(Vec::<BackupRunOutput>::new()),
        );
        return Ok(BackupRunOutcome::default());
    }

    let (config_dir, state, state_dir) = unit_context(&ctx)?;
    let units: Vec<BackupUnit<'_>> = targets
        .iter()
        .map(|spec| BackupUnit::new(spec, &config_dir, profile_name, &state_dir))
        .collect();

    // `run_ctx`, not a second `ctx`: `cli::RunContext` (bound above) and
    // `reconciler::RunContext` are both in scope in this module, and one name
    // for both makes the reader check which is which at every use.
    let run_ctx = cfgd_core::reconciler::RunContext {
        title: cfgd_core::reconciler::RunTitle::Backup,
        config_path: Some(cli.config.as_path()),
        profile: Some(profile_name),
        sources: &[],
        modules: &[],
        trigger: None,
    };
    let (_status, reports) = cfgd_core::reconciler::ApplyRun::backups(run_ctx, &units, state)
        .execute_backups(printer)?;

    // One report per unit, in unit order — `render_backups` pushes them as it
    // walks the same slice. A silent `zip` truncation here would drop payload
    // entries for units that did run.
    debug_assert_eq!(targets.len(), reports.len(), "one report per target unit");
    let outputs: Vec<BackupRunOutput> = targets
        .iter()
        .zip(&reports)
        .map(|(spec, report)| BackupRunOutput::from_report(&spec.name, report))
        .collect();
    printer.emit(Doc::new().with_data(&outputs));
    Ok(BackupRunOutcome { reports })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::backup::RestoreOutcome;

    fn outcome(error: Option<&str>, restored: bool) -> RestoreOutcome {
        RestoreOutcome {
            name: "docs".to_string(),
            snapshot: "notes.txt.20260101T000000Z".to_string(),
            restored_to: "/home/u/notes.txt".to_string(),
            restored,
            size_bytes: 12,
            safety_snapshot: None,
            error: error.map(str::to_string),
        }
    }

    /// Drive the real renderer: only a successful restore is easy to stage from
    /// a fixture, so the two trouble arms are reached here rather than through a
    /// golden that could never go red for them.
    fn rendered(outcome: &RestoreOutcome) -> (String, cfgd_core::reconciler::RunTally) {
        let (printer, cap) = Printer::for_test_doc();
        let tally = cfgd_core::backup::report_restore(&printer, outcome);
        drop(printer);
        (cfgd_core::output::strip_ansi(&cap.human()), tally)
    }

    /// The one line naming the restore itself, out of the group's rows.
    fn restore_row(human: &str) -> String {
        human
            .lines()
            .find(|l| l.contains("Restored from"))
            .unwrap_or_else(|| panic!("no restore row in:\n{human}"))
            .to_string()
    }

    #[test]
    fn a_restore_settles_under_its_owner_with_the_destination_on_a_row_of_its_own() {
        let (human, tally) = rendered(&outcome(None, true));
        assert!(human.contains("backup:docs"), "{human}");
        let row = restore_row(&human);
        assert!(
            row.contains("Restored from notes.txt.20260101T000000Z") && row.contains("12 B"),
            "{row}"
        );
        // The destination is a `Label: value` fact, not a clause hung off the
        // detail dash the size already occupies.
        assert!(
            !row.contains("/home/u/notes.txt"),
            "the destination belongs on its own row: {row}"
        );
        assert!(
            human.contains("Destination") && human.contains("/home/u/notes.txt"),
            "{human}"
        );
        assert_eq!(tally.status, cfgd_core::state::ApplyStatus::Success);
        assert_eq!(
            (tally.succeeded, tally.failed, tally.planned_total),
            (1, 0, 1)
        );
    }

    #[test]
    fn a_restore_whose_hooks_failed_leads_with_the_failure_and_keeps_the_size() {
        let (human, tally) = rendered(&outcome(Some("hook exited 1"), true));
        let row = restore_row(&human);
        assert!(
            row.contains("hook exited 1") && row.contains("(12 B)"),
            "{row}"
        );
        // The renderer supplies the one " — " between subject and detail; a
        // second one inside the detail would read as the same separator twice.
        assert_eq!(row.matches(" — ").count(), 1, "{row}");
        // Warn, not Fail: the data is back, and something still needs attention
        // — the same split a dirty `backup run` settles through.
        assert_eq!(tally.status, cfgd_core::state::ApplyStatus::Partial);
        assert_eq!(tally.succeeded, 1);
    }

    #[test]
    fn a_restore_that_did_not_happen_fails_the_run() {
        let (_human, tally) = rendered(&outcome(Some("target busy"), false));
        assert_eq!(tally.status, cfgd_core::state::ApplyStatus::Failed);
        assert_eq!((tally.succeeded, tally.failed), (0, 1));
    }
}

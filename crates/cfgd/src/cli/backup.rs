//! `cfgd backup` — run, inspect, or restore declarative backups
//! (`spec.backups[]`).

use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::backup::{BackupUnit, SnapshotInfo};
use cfgd_core::format_bytes;
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};
use cfgd_core::state::BackupRunRecord;

/// How a rollback copy comes to exist, read by both the empty-listing hint
/// and the "nothing to roll back to" error: a `cfgd backup restore` or an
/// adopting `cfgd apply` is what leaves one beside a source, never the
/// rollback itself.
const ROLLBACK_COPY_ORIGIN: &str = "A copy is left beside a source by `cfgd backup restore <name>`, and by any file `cfgd apply` adopts";

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
        vec![hint.into()],
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

/// What `cfgd backup restore` / `cfgd backup rollback` report under: the
/// composed sources, the resolved profile's modules for the header row, and the
/// units to choose from.
///
/// A module-resolution failure DEGRADES here rather than refusing. Resolution
/// clones or fetches every module's git file source, and none of a restore's
/// actual work depends on it — it is spent on a header row — so an unreachable
/// module remote would otherwise veto putting data back, plausibly during the
/// very incident that made it unreachable. The row is dropped and the reason is
/// stated instead. `cfgd backup run` stays fatal: it executes the profile's own
/// hooks, so a profile it cannot resolve is a run it cannot make.
///
/// A COMPOSITION failure still refuses, as its own error: a source constraint
/// violation is about the config the run would act under, not about a header
/// row. Which is why the composition happens HERE and the resolution over it is
/// a separate step — `compose_with_sources` renders the `Source Conflicts`
/// section and records every conflict it found, so answering "which half
/// failed" by composing a second time would print that section twice and write
/// a duplicate conflict row per attempt.
fn restoring_verb_state(
    ctx: &RunContext<'_>,
    cfg: &config::CfgdConfig,
    local_resolved: &cfgd_core::config::ResolvedProfile,
    printer: &Printer,
) -> anyhow::Result<(
    Vec<cfgd_core::reconciler::ComposedSource>,
    Vec<cfgd_core::output::HeaderModule>,
    Vec<config::BackupSpec>,
)> {
    let composition = compose_with_sources(
        ctx,
        cfg,
        local_resolved,
        printer,
        false,
        composition::ConstraintMode::Enforce,
    )?;
    let sources = cfgd_core::reconciler::ComposedSource::from_declared(&cfg.spec.sources);
    let backups = composition.resolved.merged.backups.clone();

    match resolve_desired_from_composition(ctx, cfg, composition, &[], false, printer) {
        Ok(desired) => Ok((
            sources,
            cfgd_core::output::HeaderModule::of_resolved(&desired.modules),
            backups,
        )),
        Err(e) => {
            printer.status_simple(
                Role::Warn,
                format!(
                    "Modules not resolved — {}",
                    cfgd_core::output::collapse_to_subject_line(&e)
                ),
            );
            Ok((sources, Vec::new(), backups))
        }
    }
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
            None => (cfgd_core::ABSENT.to_string(), None),
        };
        t = t.row_styled(vec![
            (e.name.clone(), None),
            (cfgd_core::fold_home_in_text(&e.source), None),
            (
                e.schedule
                    .clone()
                    .unwrap_or_else(|| cfgd_core::ABSENT.into()),
                None,
            ),
            (e.retention.to_string(), None),
            (
                e.snapshots
                    .map_or_else(|| cfgd_core::ABSENT.to_string(), |n| n.to_string()),
                None,
            ),
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
    // `Schedule` on a catalog of unscheduled units, `Status` and `Next Run`
    // before the first run: a column of `-` pushes `Snapshots` and `Last Run`,
    // the two cells a reader compares across listings, off to the right.
    doc = doc.table(t.without_unfillable_columns());
    doc.with_data(entries)
}

/// Build the `cfgd backup list <name> --snapshots` Doc. Pure; the caller
/// assembles the entries from the unit's recorded runs.
///
/// Columns and payload keys are the snapshot analogue of
/// [`build_backup_list_doc`]: `Created` carries the age, and `Size` goes
/// through the CLI's one byte renderer so it reads the same as `cfgd
/// upgrade`'s asset size. The payload keeps the ISO 8601 stamp.
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

    let mut t = Table::new(["Snapshot", "Created", "Size"]);
    for e in entries {
        t = t.row([
            e.name.clone(),
            cfgd_core::humanize_age_cell(Some(&e.created), now),
            format_bytes(e.size_bytes),
        ]);
    }
    doc = doc.table(t.without_unfillable_columns());
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
            vec!["cfgd backup list <name> --snapshots".into()],
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
            let snapshots =
                unit_dirs
                    .as_ref()
                    .zip(state)
                    .and_then(|((config_dir, state_dir), state)| {
                        let unit = BackupUnit::new(spec, config_dir, profile_name, state_dir);
                        cfgd_core::backup::list_snapshots(&unit, state)
                            .ok()
                            .map(|s| s.len())
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
                snapshots,
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
        vec![hint.into()],
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
    let ctx = RunContext::new(cli, printer);
    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    let (sources, header_modules, backups) =
        restoring_verb_state(&ctx, cfg, local_resolved, printer)?;

    let spec = find_backup_spec(&backups, args.name)?;

    let (config_dir, state, state_dir) = unit_context(&ctx)?;
    let unit = BackupUnit::new(spec, &config_dir, profile_name, &state_dir);

    let snapshots = cfgd_core::backup::list_snapshots(&unit, state)?;
    let selected: &SnapshotInfo =
        cfgd_core::backup::select_snapshot(args.name, &snapshots, args.at)
            .map_err(|e| snapshot_selection_error(args.name, e))?;

    let target = cfgd_core::backup::restore_target(&unit, args.to);

    // Ahead of the prompt, not after it: the operator agreeing to overwrite
    // live data is agreeing under a config and a profile, and the header is
    // where a run states them. `run_ctx`, not a second `ctx` — `cli::RunContext`
    // is already bound above.
    // The declared path as `backup list`'s Source column spells it: the run
    // header states what the unit READS, the action row what the restore
    // WRITES, so a `--to` or a followed link shows as the two disagreeing.
    let unit_source = spec.source.posix().to_string();
    let profile_inherits = local_resolved.inherits_chain();
    let run_ctx = cfgd_core::reconciler::RunContext {
        title: cfgd_core::reconciler::RunTitle::Restore,
        config_path: Some(cli.config.as_path()),
        profile: Some(profile_name),
        sources: &sources,
        modules: &header_modules,
        profile_inherits: &profile_inherits,
        trigger: None,
        subject: Some(args.name),
        unit_source: Some(&unit_source),
    };
    cfgd_core::reconciler::ApplyRun::unplanned(run_ctx, cfgd_core::backup::RESTORE_ACTION_COUNT)
        .header(printer);

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
            cfgd_core::fold_home_in_text(&target.resolved_display()),
            cfgd_core::fold_home_in_text(&target.requested_display())
        )
    } else {
        cfgd_core::fold_home_in_text(&target.resolved_display())
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
            vec![hint.into()],
        )
    })
}

/// Turn "nothing displaced this source" into the CLI's structured error shape.
///
/// The hint never instructs the destructive write that would create a copy —
/// an operator with nothing to undo is not told to overwrite live data on
/// the strength of a rollback error. It states how a copy comes to exist and
/// points at the read-only surface that shows its snapshots.
fn no_rollback_copy_error(name: &str, source: &Path) -> anyhow::Error {
    let hint = format!("{ROLLBACK_COPY_ORIGIN}; see `cfgd backup list {name}` for its snapshots");
    cli_error_ctx_with_hints(
        cfgd_core::errors::CfgdError::Backup(cfgd_core::errors::BackupError::NoRollbackCopy {
            name: name.to_string(),
            source_path: source.to_path_buf(),
        })
        .into(),
        name,
        "no_rollback_copy",
        format!("Backup '{name}' has no copy to roll back to"),
        serde_json::json!({ "hint": hint }),
        vec![hint.into()],
    )
}

/// Build the `cfgd backup rollback` listing Doc from a populated entries
/// vector. Pure; the caller assembles the entries and passes `now`, so a render
/// pins in a test rather than reading a clock inside the builder.
///
/// `Created` is an age for the same reason `build_backup_snapshot_list_doc`'s
/// is: the reader is choosing whether the copy is the one they want back, and
/// how long ago it was written is that question. The payload keeps the ISO 8601
/// stamp.
pub fn build_backup_rollback_list_doc(entries: &[BackupRollbackEntry], now: &str) -> Doc {
    let mut doc = Doc::new().heading("Rollback Copies");

    if entries.is_empty() {
        doc = doc.status(Role::Info, "Nothing to roll back");
        doc = doc.hint(ROLLBACK_COPY_ORIGIN);
        return doc.with_data(entries);
    }

    let mut t = Table::new(["Name", "Copy", "Created", "Size"]);
    for e in entries {
        t = t.row([
            e.name.clone(),
            cfgd_core::fold_home_in_text(&e.copy),
            cfgd_core::humanize_age_cell(Some(&e.created), now),
            format_bytes(e.size_bytes),
        ]);
    }
    doc = doc.table(t.without_unfillable_columns());
    doc.with_data(entries)
}

pub fn cmd_backup_rollback(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    yes: bool,
) -> anyhow::Result<()> {
    let Some(name) = name else {
        return list_rollback_copies(cli, printer);
    };
    // The same split `cmd_backup_restore` uses: the payload Doc is already out
    // by the time the exit code is decided, so a failed rollback is not
    // rendered as a SECOND top-level document.
    match run_backup_rollback(cli, printer, name, yes)? {
        Some(outcome) if !outcome.is_clean() => cfgd_core::exit::ExitCode::Error.exit(),
        _ => Ok(()),
    }
}

/// The no-name arm: what a rollback COULD put back, over every declared unit.
///
/// A read surface, so it composes in `Report` alongside `backup list` rather
/// than in the `Enforce` the two mutating verbs take.
fn list_rollback_copies(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let ctx = RunContext::new(cli, printer);
    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    let composition = compose_with_sources(
        &ctx,
        cfg,
        local_resolved,
        printer,
        false,
        composition::ConstraintMode::Report,
    )?;
    let backups = composition.resolved.merged.backups;

    let (config_dir, _state, state_dir) = unit_context(&ctx)?;
    let entries: Vec<BackupRollbackEntry> = backups
        .iter()
        .filter_map(|spec| {
            let unit = BackupUnit::new(spec, &config_dir, profile_name, &state_dir);
            cfgd_core::backup::rollback_copy(&unit).map(|copy| BackupRollbackEntry {
                name: spec.name.clone(),
                copy: copy.path.posix().to_string(),
                created: copy.created,
                size_bytes: copy.size_bytes,
            })
        })
        .collect();

    printer.emit(build_backup_rollback_list_doc(
        &entries,
        &cfgd_core::utc_now_iso8601(),
    ));
    Ok(())
}

/// Core of `backup rollback <name>`. `Ok(None)` means the operator declined at
/// the confirmation prompt — nothing ran, and that is a success.
///
/// Kept out of [`cmd_backup_rollback`] so the body stays in-process testable
/// (`process::exit` would abort the test binary).
pub fn run_backup_rollback(
    cli: &Cli,
    printer: &Printer,
    name: &str,
    yes: bool,
) -> anyhow::Result<Option<cfgd_core::backup::RollbackOutcome>> {
    let ctx = RunContext::new(cli, printer);
    let (cfg, profile_name, local_resolved) = ctx.config_and_profile()?;
    let (sources, header_modules, backups) =
        restoring_verb_state(&ctx, cfg, local_resolved, printer)?;

    let spec = find_backup_spec(&backups, name)?;

    let (config_dir, _state, state_dir) = unit_context(&ctx)?;
    let unit = BackupUnit::new(spec, &config_dir, profile_name, &state_dir);

    // Resolved before the prompt so the operator is told which copy they are
    // agreeing to, and so a unit with nothing to put back is refused without
    // asking. `rollback_backup` looks it up again under the lock.
    let copy = cfgd_core::backup::rollback_copy(&unit)
        .ok_or_else(|| no_rollback_copy_error(name, &unit.source()))?;

    let unit_source = spec.source.posix().to_string();
    let profile_inherits = local_resolved.inherits_chain();
    let run_ctx = cfgd_core::reconciler::RunContext {
        title: cfgd_core::reconciler::RunTitle::Rollback,
        config_path: Some(cli.config.as_path()),
        profile: Some(profile_name),
        sources: &sources,
        modules: &header_modules,
        profile_inherits: &profile_inherits,
        trigger: None,
        subject: Some(name),
        unit_source: Some(&unit_source),
    };
    cfgd_core::reconciler::ApplyRun::unplanned(run_ctx, cfgd_core::backup::RESTORE_ACTION_COUNT)
        .header(printer);

    let copy_display = copy.path.posix().to_string();
    let target = cfgd_core::backup::restore_target(&unit, None);
    if !yes && !confirm_rollback(printer, name, &copy_display, &target)? {
        printer.emit(Doc::new().status(Role::Info, "Aborted").with_data(
            &BackupRollbackDeclinedOutput {
                name: name.to_string(),
                copy: copy_display,
                restored_to: target.resolved_display(),
                restored: false,
                declined: true,
            },
        ));
        return Ok(None);
    }

    let started = std::time::Instant::now();
    let outcome = cfgd_core::backup::rollback_backup(&unit, printer)?;

    let tally = cfgd_core::backup::report_rollback(printer, &outcome);
    cfgd_core::reconciler::render_run_rollup(
        &tally,
        cfgd_core::reconciler::RunTitle::Rollback,
        printer,
        Some(started.elapsed()),
    );
    if outcome.is_clean() {
        printer.hint(success_next_step(Mutation::BackupRolledBack { unit: name }));
    }

    printer.emit(Doc::new().with_data(BackupRollbackOutput::from(&outcome)));
    Ok(Some(outcome))
}

/// Ask before overwriting live data, on the terms [`confirm_restore`] asks on:
/// a session that cannot prompt is an error carrying the `--yes` remedy, never
/// a silent decline, and the RESOLVED destination is named because that is what
/// gets overwritten. A symlinked source is named both ways, for the reason
/// [`confirm_restore`] states.
fn confirm_rollback(
    printer: &Printer,
    name: &str,
    copy: &str,
    target: &cfgd_core::backup::RestoreTarget,
) -> anyhow::Result<bool> {
    let into = if target.was_redirected_by_a_link() {
        format!(
            "{} (via {})",
            cfgd_core::fold_home_in_text(&target.resolved_display()),
            cfgd_core::fold_home_in_text(&target.requested_display())
        )
    } else {
        cfgd_core::fold_home_in_text(&target.resolved_display())
    };
    let question = format!(
        "Roll '{name}' back to {into} from {}?",
        cfgd_core::fold_home_in_text(copy)
    );
    printer.prompt_confirm(&question).map_err(|e| {
        let hint = "pass --yes (or set CFGD_YES=1) to roll back without a prompt".to_string();
        let message = if printer.can_prompt() {
            cfgd_core::output::collapse_to_subject_line(&e)
        } else {
            format!("Rollback of '{name}' needs confirmation, and this session cannot prompt")
        };
        cli_error_with_hints(
            name,
            "confirmation_required",
            message,
            serde_json::json!({ "hint": hint, "copy": copy }),
            vec![hint.into()],
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
    // The whole desired state rather than the composition alone: `spec.backups[]`
    // is profile-declared, so this run reports under a resolved profile and its
    // header names that profile's modules like every other run does. One
    // resolution — `resolve_desired_state` composes internally.
    let desired = resolve_desired_state(
        &ctx,
        cfg,
        local_resolved,
        &[],
        false,
        printer,
        false,
        composition::ConstraintMode::Enforce,
    )?;
    let sources = desired.sources;
    let header_modules = cfgd_core::output::HeaderModule::of_resolved(&desired.modules);
    // Read off `.resolved` before the `.merged.backups` move below partially
    // moves it.
    let profile_inherits = desired.resolved.inherits_chain();
    let backups = desired.resolved.merged.backups;

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

    // A run of ONE named unit is titled and sourced like its restore
    // (`Backup: docs` / `Source …`); a run over every declared unit names them
    // in its owner groups and has no one source to state.
    let named = name.and_then(|n| targets.iter().find(|spec| spec.name == n));
    let unit_source = named.map(|spec| spec.source.posix().to_string());
    // `run_ctx`, not a second `ctx`: `cli::RunContext` (bound above) and
    // `reconciler::RunContext` are both in scope in this module, and one name
    // for both makes the reader check which is which at every use.
    let run_ctx = cfgd_core::reconciler::RunContext {
        title: cfgd_core::reconciler::RunTitle::Backup,
        config_path: Some(cli.config.as_path()),
        profile: Some(profile_name),
        sources: &sources,
        modules: &header_modules,
        profile_inherits: &profile_inherits,
        trigger: None,
        subject: named.map(|spec| spec.name.as_str()),
        unit_source: unit_source.as_deref(),
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
    use cfgd_core::backup::{RestoreOutcome, RollbackOutcome};

    fn outcome(error: Option<&str>, restored: bool) -> RestoreOutcome {
        RestoreOutcome {
            name: "docs".to_string(),
            snapshot: "notes.txt.20260101T000000Z".to_string(),
            restored_to: "/home/u/notes.txt".to_string(),
            restored,
            size_bytes: 12,
            safety_copy: None,
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
            .find(|l| l.contains("restore /home"))
            .unwrap_or_else(|| panic!("no restore row in:\n{human}"))
            .to_string()
    }

    #[test]
    fn a_restore_settles_under_its_owner_naming_its_target_in_the_action_row() {
        let (human, tally) = rendered(&outcome(None, true));
        assert!(human.contains("backup:docs"), "{human}");
        let row = restore_row(&human);
        assert!(
            row.contains("restore /home/u/notes.txt from notes.txt.20260101T000000Z")
                && row.contains("12 B"),
            "{row}"
        );
        // The target is IN the subject, the way every file action names what it
        // writes; a `Destination` row under the action is the shape this pins
        // against.
        assert!(
            !human.contains("Destination"),
            "the target belongs in the action row, not on a row under it: {human}"
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

    fn rollback_outcome(error: Option<&str>, restored: bool) -> RollbackOutcome {
        RollbackOutcome {
            name: "docs".to_string(),
            copy: "/home/u/notes.txt.cfgd-backup".to_string(),
            restored_to: "/home/u/notes.txt".to_string(),
            restored,
            size_bytes: 12,
            safety_copy: None,
            error: error.map(str::to_string),
        }
    }

    /// The rollback twin of [`rendered`], and for the same reason: the two
    /// trouble arms cannot be staged from a fixture.
    fn rendered_rollback(outcome: &RollbackOutcome) -> (String, cfgd_core::reconciler::RunTally) {
        let (printer, cap) = Printer::for_test_doc();
        let tally = cfgd_core::backup::report_rollback(&printer, outcome);
        drop(printer);
        (cfgd_core::output::strip_ansi(&cap.human()), tally)
    }

    fn rollback_row(human: &str) -> String {
        human
            .lines()
            .find(|l| l.contains("rollback /home"))
            .unwrap_or_else(|| panic!("no rollback row in:\n{human}"))
            .to_string()
    }

    #[test]
    fn a_rollback_whose_hooks_failed_leads_with_the_failure_and_keeps_the_size() {
        let (human, tally) = rendered_rollback(&rollback_outcome(Some("hook exited 1"), true));
        let row = rollback_row(&human);
        assert!(
            row.contains("hook exited 1") && row.contains("(12 B)"),
            "{row}"
        );
        assert_eq!(row.matches(" — ").count(), 1, "{row}");
        assert_eq!(tally.status, cfgd_core::state::ApplyStatus::Partial);
        assert_eq!(tally.succeeded, 1);
    }

    #[test]
    fn a_rollback_that_did_not_happen_fails_the_run() {
        let (_human, tally) = rendered_rollback(&rollback_outcome(Some("target busy"), false));
        assert_eq!(tally.status, cfgd_core::state::ApplyStatus::Failed);
        assert_eq!((tally.succeeded, tally.failed), (0, 1));
    }

    /// The arm where the hint matters most: the copy landed and the overlay
    /// then failed, which for a directory unit leaves a mixed tree — so the
    /// line naming the complete copy IS the recovery instruction, and it must
    /// be on screen beside the failure rather than only beside a success.
    #[test]
    fn a_failed_rollback_still_says_where_the_copy_it_took_went() {
        let mut outcome = rollback_outcome(Some("target busy"), false);
        outcome.safety_copy = Some(cfgd_core::reconciler::SidecarOutcome {
            path: std::path::PathBuf::from("/home/u/notes.txt.cfgd-backup.20260101T000000Z"),
            reused: false,
        });
        let (human, tally) = rendered_rollback(&outcome);
        assert_eq!(tally.status, cfgd_core::state::ApplyStatus::Failed);
        assert!(rollback_row(&human).contains("target busy"), "{human}");
        assert!(
            human.contains(
                "Previous contents backed up to /home/u/notes.txt.cfgd-backup.20260101T000000Z"
            ),
            "{human}"
        );
    }

    /// The closing hint is a DISPLAY slot like the rows above it, so a path
    /// under home folds to `~/` there too — a restore that printed
    /// `restore ~/notes.md …` and then `backed up to /home/tj/notes.md.cfgd-backup`
    /// one line below spelled `$HOME` two ways in one report. `-o json` keeps
    /// the absolute path on both verbs.
    #[test]
    fn no_closing_hint_spells_the_home_directory_absolutely() {
        let home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(home.path());
        let home_posix = cfgd_core::to_posix_string(home.path());
        let target = format!("{home_posix}/notes.txt");
        let copy = format!("{target}.cfgd-backup");
        let sidecar = || {
            Some(cfgd_core::reconciler::SidecarOutcome {
                path: std::path::PathBuf::from(&copy),
                reused: false,
            })
        };

        let mut restore = outcome(None, true);
        restore.restored_to = target.clone();
        restore.safety_copy = sidecar();
        let (restore_human, _) = rendered(&restore);
        let restore_payload = serde_json::to_string(&BackupRestoreOutput::from(&restore)).unwrap();

        let mut rollback = rollback_outcome(None, true);
        rollback.copy = copy.clone();
        rollback.restored_to = target.clone();
        rollback.safety_copy = sidecar();
        let (rollback_human, _) = rendered_rollback(&rollback);
        let rollback_payload =
            serde_json::to_string(&BackupRollbackOutput::from(&rollback)).unwrap();

        for (verb, human, payload) in [
            ("restore", restore_human, restore_payload),
            ("rollback", rollback_human, rollback_payload),
        ] {
            assert!(
                human.contains("Previous contents backed up to ~/notes.txt.cfgd-backup"),
                "{verb}'s closing hint folds the home directory:\n{human}"
            );
            assert!(
                !human.contains(&home_posix),
                "{verb} spells a path under home absolutely somewhere its rows fold it:\n{human}"
            );
            assert!(
                payload.contains(&home_posix),
                "{verb}'s `-o json` payload keeps the absolute path:\n{payload}"
            );
        }
    }

    /// Where the displaced contents went is the one thing an operator who
    /// regrets a rollback needs, and the sidecar's own outcome words it.
    #[test]
    fn a_rollback_says_where_the_contents_it_displaced_went() {
        let mut outcome = rollback_outcome(None, true);
        outcome.safety_copy = Some(cfgd_core::reconciler::SidecarOutcome {
            path: std::path::PathBuf::from("/home/u/notes.txt.cfgd-backup.20260101T000000Z"),
            reused: false,
        });
        let (human, _tally) = rendered_rollback(&outcome);
        assert!(
            human.contains(
                "Previous contents backed up to /home/u/notes.txt.cfgd-backup.20260101T000000Z"
            ) && human.contains("cfgd backup rollback docs"),
            "{human}"
        );
    }
}

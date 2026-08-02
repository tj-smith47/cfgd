//! `cfgd backup` — run, inspect, or restore declarative backups
//! (`spec.backups[]`).

use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::backup::{BackupUnit, SnapshotInfo, run_backup};
use cfgd_core::output::{Doc, Printer, Role, renderer::Table};
use cfgd_core::state::{BackupRunRecord, BackupRunStatus};

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

/// Build the `cfgd backup list` Doc from a populated entries vector. Pure; the
/// caller assembles the entries from config + the state store.
pub fn build_backup_list_doc(entries: &[BackupListEntry]) -> Doc {
    let mut doc = Doc::new().heading("Backups");

    if entries.is_empty() {
        doc = doc.status(Role::Info, "No backups configured");
        return doc.with_data(entries);
    }

    let mut t = Table::new([
        "Name",
        "Source",
        "Schedule",
        "Retention",
        "Last Run",
        "Next Run",
    ]);
    for e in entries {
        let last_run = match (&e.last_run_status, &e.last_run_at) {
            (Some(status), Some(at)) if e.last_run_clean == Some(false) => {
                format!("{status} (dirty) @ {at}")
            }
            (Some(status), Some(at)) => format!("{status} @ {at}"),
            _ => "never".to_string(),
        };
        t = t.row([
            e.name.clone(),
            e.source.clone(),
            e.schedule.clone().unwrap_or_else(|| "-".into()),
            e.retention.to_string(),
            last_run,
            e.next_run_at.clone().unwrap_or_else(|| "-".into()),
        ]);
    }
    doc = doc.table(t);
    doc.with_data(entries)
}

/// Build the `cfgd backup list <name> --snapshots` Doc. Pure; the caller
/// assembles the entries from the unit's recorded runs.
///
/// Columns and payload keys are the snapshot analogue of
/// [`build_backup_list_doc`]: `Created` is the ISO 8601 UTC stamp
/// `BackupListEntry::last_run_at` uses, and `Size` goes through the CLI's one
/// byte renderer so it reads the same as `cfgd upgrade`'s asset size.
pub fn build_backup_snapshot_list_doc(name: &str, entries: &[BackupSnapshotEntry]) -> Doc {
    let mut doc = Doc::new().heading(format!("Snapshots: {name}"));

    if entries.is_empty() {
        doc = doc.status(Role::Info, format!("Backup '{name}' has no snapshots"));
        return doc.with_data(entries);
    }

    let mut t = Table::new(["Snapshot", "Created", "Size"]);
    for e in entries {
        t = t.row([
            e.name.clone(),
            e.created.clone(),
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

    let (cfg, profile_name, local_resolved) = load_config_and_profile(cli)?;
    // Cache-only composition (no network refresh) and Report constraint mode:
    // listing backups is a read surface, the same class as
    // `status`/`diff`/`compliance`. `backup run` is not — it composes in
    // Enforce because it runs hooks and writes snapshots.
    let composition = compose_with_sources(
        cli,
        &cfg,
        &local_resolved,
        printer,
        false,
        composition::ConstraintMode::Report,
    )?;
    let backups = composition.resolved.merged.backups;

    // A named lookup resolves before the inventory is assembled so an unknown
    // name is the same typed, name-listing error `backup run` returns rather
    // than an empty table the caller has to interpret.
    let selected: Vec<&config::BackupSpec> = match name {
        Some(n) => match backups.iter().find(|b| b.name == n) {
            Some(b) => vec![b],
            None => {
                let valid: Vec<String> = backups.iter().map(|b| b.name.clone()).collect();
                return Err(backup_not_found_error(n, valid));
            }
        },
        None => backups.iter().collect(),
    };

    if snapshots {
        let spec = selected
            .first()
            .copied()
            // Unreachable: `snapshots` implies a name, and a name that resolves
            // to nothing returned above.
            .ok_or_else(|| backup_not_found_error(name.unwrap_or_default(), Vec::new()))?;
        return list_unit_snapshots(cli, printer, spec, &profile_name);
    }

    if selected.is_empty() {
        printer.emit(build_backup_list_doc(&[]));
        return Ok(());
    }

    // A state-store failure costs the run HISTORY, not the inventory: the
    // declared units come from config, which loaded fine. Degrading to
    // "never" with a warning keeps the config half of the command useful when
    // `state.db` is unreadable, matching how `resolve_backup_tasks` treats the
    // same failure.
    let state = match open_state_store(cli.state_dir.as_deref()) {
        Ok(state) => Some(state),
        Err(e) => {
            printer
                .status(Role::Warn, "backup history unavailable")
                .detail(cfgd_core::output::collapse_to_subject_line(&e));
            None
        }
    };
    let entries: Vec<BackupListEntry> = selected
        .iter()
        .map(|spec| {
            let last = state
                .as_ref()
                .and_then(|state| state.latest_backup_run(&spec.name).ok().flatten());
            BackupListEntry {
                name: spec.name.clone(),
                source: spec.source.posix().to_string(),
                schedule: spec.schedule.clone(),
                retention: spec.retention,
                last_run_status: last.as_ref().map(|r| match r.status {
                    BackupRunStatus::Success => "success".to_string(),
                    BackupRunStatus::Failed => "failed".to_string(),
                }),
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
            }
        })
        .collect();

    printer.emit(build_backup_list_doc(&entries));
    Ok(())
}

/// Emit the `--snapshots` view for one already-resolved unit.
///
/// Unlike the inventory, this cannot degrade when the state store is
/// unreadable: the run records ARE the snapshot list — there is no config half
/// left to render — so a store failure is the command's failure.
fn list_unit_snapshots(
    cli: &Cli,
    printer: &Printer,
    spec: &config::BackupSpec,
    profile_name: &str,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_dir = cfgd_core::resolve_state_dir(cli.state_dir.as_deref(), cli.scope())?;
    let unit = BackupUnit::new(spec, &config_dir, profile_name, &state_dir);

    let entries: Vec<BackupSnapshotEntry> = cfgd_core::backup::list_snapshots(&unit, &state)?
        .iter()
        .map(BackupSnapshotEntry::from)
        .collect();

    printer.emit(build_backup_snapshot_list_doc(&spec.name, &entries));
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
    printer.heading("Restore Backup");

    let (cfg, profile_name, local_resolved) = load_config_and_profile(cli)?;
    // Enforce, like `backup run`: a restore executes the unit's hooks and
    // overwrites live data, so a source constraint violation must abort rather
    // than be recorded and stepped over.
    let composition = compose_with_sources(
        cli,
        &cfg,
        &local_resolved,
        printer,
        false,
        composition::ConstraintMode::Enforce,
    )?;
    let backups = composition.resolved.merged.backups;

    let spec = match backups.iter().find(|b| b.name == args.name) {
        Some(spec) => spec,
        None => {
            let valid: Vec<String> = backups.iter().map(|b| b.name.clone()).collect();
            return Err(backup_not_found_error(args.name, valid));
        }
    };

    let config_dir = config_dir(cli);
    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_dir = cfgd_core::resolve_state_dir(cli.state_dir.as_deref(), cli.scope())?;
    let unit = BackupUnit::new(spec, &config_dir, &profile_name, &state_dir);

    let snapshots = cfgd_core::backup::list_snapshots(&unit, &state)?;
    let selected: &SnapshotInfo =
        cfgd_core::backup::select_snapshot(args.name, &snapshots, args.at)
            .map_err(|e| snapshot_selection_error(args.name, e))?;

    let target = cfgd_core::backup::restore_target(&unit, args.to);

    if !args.yes && !confirm_restore(printer, args.name, selected, &target)? {
        printer.emit(Doc::new().status(Role::Info, "Aborted").with_data(
            &BackupRestoreDeclinedOutput {
                name: args.name.to_string(),
                snapshot: selected.name.clone(),
                restored_to: cfgd_core::to_posix_string(&target),
                restored: false,
                declined: true,
            },
        ));
        return Ok(None);
    }

    let outcome = cfgd_core::backup::restore_backup(&unit, &state, printer, selected, args.to)?;

    // The same three-way split `backup run` renders: a clean restore is Ok, a
    // completed restore whose hooks failed is Warn (the data is back, but
    // something needs attention), and a restore that did not happen is Fail.
    let role = if outcome.is_clean() {
        Role::Ok
    } else if outcome.restored {
        Role::Warn
    } else {
        Role::Fail
    };
    let subject = format!(
        "backup '{}' restored from {}",
        outcome.name, outcome.snapshot
    );
    // `outcome.restored_to`, not the requested target: a symlinked source is
    // followed, and the operator needs to be told where the bytes actually went.
    let detail = match &outcome.error {
        Some(e) => format!(
            "into {} — {}",
            outcome.restored_to,
            cfgd_core::output::collapse_to_subject_line(e)
        ),
        None => format!("into {}", outcome.restored_to),
    };
    printer.status(role, subject).detail(detail);
    // `hint`, not `note`: where the overwritten data went is the one thing an
    // operator needs after a restore they regret, and `note` is Verbose-only.
    if let Some(safety) = &outcome.safety_snapshot {
        printer.hint(format!("previous contents saved to {safety}"));
    }

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
    target: &Path,
) -> anyhow::Result<bool> {
    let question = format!(
        "Restore '{}' from snapshot {} into {}?",
        name,
        snapshot.name,
        target.posix()
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
/// A refused unit produces no [`BackupRunRecord`] — nothing ran — so the
/// records alone cannot distinguish "everything succeeded" from "one unit was
/// already being backed up by someone else". Both must exit nonzero.
#[derive(Debug, Default)]
pub struct BackupRunOutcome {
    /// One record per unit that actually ran.
    pub records: Vec<BackupRunRecord>,
    /// Units skipped because another process held their per-unit lock.
    pub busy: Vec<String>,
}

impl BackupRunOutcome {
    /// True when every requested unit ran and produced an intact snapshot.
    ///
    /// Named apart from [`BackupRunRecord::is_clean`] because it covers the
    /// case that has no record at all: a unit refused for a held lock never
    /// ran, so "every record is clean" would be vacuously true for it.
    pub fn fully_clean(&self) -> bool {
        self.busy.is_empty() && self.records.iter().all(BackupRunRecord::is_clean)
    }
}

/// Core of `backup run`: resolves the target unit(s), runs each through the
/// backup engine, and returns what happened. `name = None` runs every
/// declared backup (scheduled or not — the schedule only gates the automatic
/// run inside `cfgd apply`; an explicit `backup run` always runs).
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
    printer.heading("Run Backups");

    let (cfg, profile_name, local_resolved) = load_config_and_profile(cli)?;
    // Cache-only composition (no network refresh), but Enforce constraint mode:
    // `backup run` executes user-declared hooks and writes snapshots, so it is a
    // mutating surface like apply/plan/daemon and must abort on a source
    // violation rather than record it and continue. Only `backup list`, which
    // reads, composes in Report.
    let composition = compose_with_sources(
        cli,
        &cfg,
        &local_resolved,
        printer,
        false,
        composition::ConstraintMode::Enforce,
    )?;
    let backups = composition.resolved.merged.backups;

    let targets: Vec<&config::BackupSpec> = match name {
        Some(n) => match backups.iter().find(|b| b.name == n) {
            Some(b) => vec![b],
            None => {
                let valid: Vec<String> = backups.iter().map(|b| b.name.clone()).collect();
                return Err(backup_not_found_error(n, valid));
            }
        },
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

    let config_dir = config_dir(cli);
    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_dir = cfgd_core::resolve_state_dir(cli.state_dir.as_deref(), cli.scope())?;

    let mut outcome = BackupRunOutcome::default();
    let mut outputs: Vec<BackupRunOutput> = Vec::with_capacity(targets.len());
    for spec in targets {
        let unit = BackupUnit::new(spec, &config_dir, &profile_name, &state_dir);
        let record = match run_backup(&unit, &state, printer) {
            Ok(record) => record,
            // Another surface (a daemon timer fire, an apply) holds this unit's
            // lock. The unit IS being backed up, just not by us, so the line
            // reads `Skipped` — the same word `cfgd apply` uses for the same
            // event and the same `"skipped"` the payload carries. Only the exit
            // code differs between the two surfaces, because here the user
            // named a run that did not happen. Every OTHER unit still runs: the
            // collision is one unit's, not the command's.
            Err(cfgd_core::errors::CfgdError::Backup(cfgd_core::errors::BackupError::Busy {
                holder,
                ..
            })) => {
                printer
                    .status(Role::Skipped, format!("backup '{}'", spec.name))
                    .detail(format!("already running ({holder})"));
                outputs.push(BackupRunOutput {
                    name: spec.name.clone(),
                    status: "skipped".to_string(),
                    destination_path: None,
                    clean: false,
                    error: Some(format!("already running ({holder})")),
                });
                outcome.busy.push(spec.name.clone());
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let subject = format!("backup '{}'", record.name);
        // `is_clean()` is the exit-code predicate; the human status line uses
        // the same three-way split: a fully clean run is Ok, a Success run
        // with a failed postBackup hook is Warn (the snapshot is fine, but
        // something needs attention), and no artifact at all is Fail.
        let role = if record.is_clean() {
            Role::Ok
        } else if record.status == BackupRunStatus::Success {
            Role::Warn
        } else {
            Role::Fail
        };
        match &record.error {
            Some(e) => {
                printer
                    .status(role, subject)
                    .detail(cfgd_core::output::collapse_to_subject_line(e));
            }
            None => printer.status_simple(role, subject),
        }
        outputs.push(BackupRunOutput::from(&record));
        outcome.records.push(record);
    }

    printer.emit(Doc::new().with_data(&outputs));
    Ok(outcome)
}

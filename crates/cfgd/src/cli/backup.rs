//! `cfgd backup` — run or inspect declarative backups (`spec.backups[]`).

use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::backup::{BackupUnit, run_backup};
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

    let mut t = Table::new(["Name", "Source", "Schedule", "Retention", "Last Run"]);
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
        ]);
    }
    doc = doc.table(t);
    doc.with_data(entries)
}

pub fn cmd_backup_list(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    if !config_path.exists() {
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

    let (cfg, _profile_name, local_resolved) = load_config_and_profile(cli)?;
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

    if backups.is_empty() {
        printer.emit(build_backup_list_doc(&[]));
        return Ok(());
    }

    let state = open_state_store(cli.state_dir.as_deref())?;
    let entries: Vec<BackupListEntry> = backups
        .iter()
        .map(|spec| {
            let last = state.latest_backup_run(&spec.name).ok().flatten();
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
            }
        })
        .collect();

    printer.emit(build_backup_list_doc(&entries));
    Ok(())
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

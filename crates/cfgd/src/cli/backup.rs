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
    // listing/running backups is a read/maintenance surface, the same class
    // as `status`/`diff`/`compliance`, not a mutating surface like
    // apply/plan/daemon that composes in Enforce mode.
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
    let records = run_backup_run(cli, printer, name)?;

    // A scripted consumer must be able to detect a failed or dirty backup from
    // the exit code alone — `run_backup_run` already emitted the per-unit
    // status lines and the summary Doc; exit nonzero directly here (mirroring
    // `cmd_source_update`) so the failure isn't re-rendered as a second error
    // line by the central sink. Kept out of the core so the body stays
    // in-process testable (`process::exit` would abort the test binary).
    if records.iter().any(|r| !r.is_clean()) {
        cfgd_core::exit::ExitCode::Error.exit();
    }

    Ok(())
}

/// Core of `backup run`: resolves the target unit(s), runs each through the
/// backup engine, and returns every recorded run. `name = None` runs every
/// declared backup (scheduled or not — the schedule only gates the automatic
/// run inside `cfgd apply`; an explicit `backup run` always runs).
pub(crate) fn run_backup_run(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
) -> anyhow::Result<Vec<BackupRunRecord>> {
    printer.heading("Run Backups");

    let (cfg, profile_name, local_resolved) = load_config_and_profile(cli)?;
    let composition = compose_with_sources(
        cli,
        &cfg,
        &local_resolved,
        printer,
        false,
        composition::ConstraintMode::Report,
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
        return Ok(Vec::new());
    }

    let config_dir = config_dir(cli);
    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_dir = cfgd_core::resolve_state_dir(cli.state_dir.as_deref(), cli.scope())?;

    let mut records = Vec::with_capacity(targets.len());
    let mut outputs: Vec<BackupRunOutput> = Vec::with_capacity(targets.len());
    // The first unit found already running. Reported after the loop so a
    // `backup run` with no name still runs every OTHER unit — the collision is
    // one unit's, not the command's — while the exit code stays nonzero,
    // because the user asked for a run of that unit and did not get one.
    let mut busy: Option<cfgd_core::errors::BackupError> = None;
    for spec in targets {
        let unit = BackupUnit::new(spec, &config_dir, &profile_name, &state_dir);
        let record = match run_backup(&unit, &state, printer) {
            Ok(record) => record,
            Err(cfgd_core::errors::CfgdError::Backup(
                e @ cfgd_core::errors::BackupError::Busy { .. },
            )) => {
                let holder = match &e {
                    cfgd_core::errors::BackupError::Busy { holder, .. } => holder.clone(),
                    _ => String::new(),
                };
                printer
                    .status(Role::Fail, format!("backup '{}'", spec.name))
                    .detail(format!("already running ({holder})"));
                outputs.push(BackupRunOutput {
                    name: spec.name.clone(),
                    status: "skipped".to_string(),
                    destination_path: None,
                    clean: false,
                    error: Some(format!("already running ({holder})")),
                });
                busy.get_or_insert(e);
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
        records.push(record);
    }

    printer.emit(Doc::new().with_data(&outputs));
    if let Some(e) = busy {
        return Err(cfgd_core::errors::CfgdError::from(e).into());
    }
    Ok(records)
}

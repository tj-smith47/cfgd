use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

/// One profile's migration outcome, emitted in the structured payload.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrationRecord {
    name: String,
    from: Option<String>,
    to: Option<String>,
    action: String,
    reason: Option<String>,
}

enum PlanItem {
    Move {
        name: String,
        from: PathBuf,
        to: PathBuf,
    },
    AlreadyCanonical {
        name: String,
    },
    Failed {
        name: String,
        reason: String,
    },
}

fn plan_for_name(
    profiles_dir: &Path,
    name: &str,
) -> Result<PlanItem, cfgd_core::errors::ConfigError> {
    let canonical = cfgd_core::config::canonical_profile_path(profiles_dir, name);
    match cfgd_core::config::find_profile_path(profiles_dir, name) {
        Ok(path) if path == canonical => Ok(PlanItem::AlreadyCanonical {
            name: name.to_string(),
        }),
        Ok(path) => Ok(PlanItem::Move {
            name: name.to_string(),
            from: path,
            to: canonical,
        }),
        Err(e) => Err(e),
    }
}

fn in_git_work_tree(dir: &Path) -> bool {
    cfgd_core::git_cmd_local()
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && cfgd_core::stdout_lossy_trimmed(&o) == "true")
        .unwrap_or(false)
}

enum GitMvOutcome {
    /// git staged the rename; history is preserved.
    Moved,
    /// git isn't applicable (file untracked, or tracking couldn't be
    /// confirmed) — a plain rename is the correct path, not a degradation.
    NotApplicable,
    /// The file IS tracked and `git mv` still failed (index.lock contention,
    /// permissions, …) — falling back loses history, so the caller must warn.
    Failed(String),
}

/// `git mv` inside `work_dir`, distinguishing "git declined because the file
/// isn't tracked" from "git should have worked and didn't".
fn git_mv(work_dir: &Path, from: &Path, to: &Path) -> GitMvOutcome {
    let tracked = cfgd_core::git_cmd_local()
        .arg("-C")
        .arg(work_dir)
        .args(["ls-files", "--error-unmatch"])
        .arg(from)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !tracked {
        return GitMvOutcome::NotApplicable;
    }
    match cfgd_core::git_cmd_local()
        .arg("-C")
        .arg(work_dir)
        .arg("mv")
        .arg(from)
        .arg(to)
        .output()
    {
        Ok(o) if o.status.success() => GitMvOutcome::Moved,
        Ok(o) => GitMvOutcome::Failed(cfgd_core::stderr_lossy_trimmed(&o)),
        Err(e) => GitMvOutcome::Failed(e.to_string()),
    }
}

pub fn cmd_profile_migrate(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    all: bool,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let failed = run_profile_migrate(cli, printer, name, all, dry_run, yes)?;
    // A scripted consumer must detect per-profile failures from the exit code
    // alone; the summary Doc is already emitted, so exit directly (mirroring
    // cmd_source_update) instead of returning an error the central sink would
    // re-render. Kept out of the core so the body stays unit-testable.
    if failed > 0 {
        cfgd_core::exit::ExitCode::Error.exit();
    }
    Ok(())
}

/// Core of `profile migrate`: plans and executes the legacy-flat → canonical
/// bundle moves, emits the summary Doc, and returns the failure count.
pub(crate) fn run_profile_migrate(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    all: bool,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<usize> {
    let pdir = profiles_dir(cli);

    let plan: Vec<PlanItem> = if let Some(name) = name {
        validate_resource_name(name, "Profile")?;
        printer.heading(format!("Migrate Profile: {}", name));
        match plan_for_name(&pdir, name) {
            Ok(item) => vec![item],
            Err(e @ cfgd_core::errors::ConfigError::ProfileNotFound { .. }) => {
                return Err(crate::cli::cli_error_ctx(
                    cfgd_core::errors::CfgdError::Config(e).into(),
                    name,
                    "not_found",
                    format!("Profile '{}' not found", name),
                    serde_json::json!({}),
                ));
            }
            Err(e) if dry_run => vec![PlanItem::Failed {
                name: name.to_string(),
                reason: cfgd_core::output::collapse_to_subject_line(&e),
            }],
            Err(e) => return Err(cfgd_core::errors::CfgdError::Config(e).into()),
        }
    } else {
        debug_assert!(all, "clap requires NAME or --all");
        printer.heading("Migrate Profiles");
        // An unreadable profiles dir must fail the command, not silently
        // migrate nothing; scan_profiles_tolerant only errors on real I/O
        // failures (a missing dir is an empty list).
        cfgd_core::config::scan_profiles_tolerant(&pdir)
            .map_err(cfgd_core::errors::CfgdError::Config)?
            .into_iter()
            .map(|entry| match entry {
                cfgd_core::config::ProfileScanEntry::Found(found) => match found.form {
                    cfgd_core::config::ProfileForm::Canonical => {
                        PlanItem::AlreadyCanonical { name: found.name }
                    }
                    cfgd_core::config::ProfileForm::LegacyFlat => PlanItem::Move {
                        to: cfgd_core::config::canonical_profile_path(&pdir, &found.name),
                        name: found.name,
                        from: found.path,
                    },
                },
                cfgd_core::config::ProfileScanEntry::Ambiguous { name, error, .. } => {
                    PlanItem::Failed {
                        name,
                        reason: cfgd_core::output::collapse_to_subject_line(&error),
                    }
                }
            })
            .collect()
    };

    let move_count = plan
        .iter()
        .filter(|i| matches!(i, PlanItem::Move { .. }))
        .count();

    if dry_run {
        let mut records = Vec::new();
        for item in &plan {
            match item {
                PlanItem::Move { name, from, to } => {
                    printer.status_simple(
                        Role::Pending,
                        format!("Would move {} → {}", from.posix(), to.posix()),
                    );
                    records.push(MigrationRecord {
                        name: name.clone(),
                        from: Some(cfgd_core::to_posix_string(from)),
                        to: Some(cfgd_core::to_posix_string(to)),
                        action: "planned".into(),
                        reason: None,
                    });
                }
                other => records.push(report_no_move(printer, other)),
            }
        }
        // The exit code must mirror a real run's: a scripted
        // `migrate --dry-run && migrate` would otherwise proceed into a
        // guaranteed partial failure.
        let failed = count_action(&records, "failed");
        let (role, summary) = if failed > 0 {
            (
                Role::Warn,
                format!(
                    "Dry run: {} profile(s) would be migrated, {} failed",
                    move_count, failed
                ),
            )
        } else {
            (
                Role::Info,
                format!("Dry run: {} profile(s) would be migrated", move_count),
            )
        };
        printer.emit(
            Doc::new()
                .status(role, summary)
                .with_data(summary_payload(&records, true)),
        );
        return Ok(failed);
    }

    if move_count > 0 && !yes {
        for item in &plan {
            if let PlanItem::Move { from, to, .. } = item {
                printer.status_simple(Role::Pending, format!("{} → {}", from.posix(), to.posix()));
            }
        }
        if !printer.prompt_confirm(&format!("Migrate {} profile(s)?", move_count))? {
            printer.emit(
                Doc::new()
                    .status(Role::Info, "Cancelled")
                    .with_data(serde_json::json!({ "cancelled": true, "profiles": [] })),
            );
            return Ok(0);
        }
    }

    let config_dir = config_dir(cli);
    let use_git = in_git_work_tree(&config_dir);

    let mut records = Vec::new();
    for item in &plan {
        match item {
            PlanItem::Move { name, from, to } => {
                match execute_move(printer, use_git, &config_dir, from, to) {
                    Ok(()) => {
                        printer.status_simple(
                            Role::Ok,
                            format!("Migrated '{}' → {}", name, to.posix()),
                        );
                        records.push(MigrationRecord {
                            name: name.clone(),
                            from: Some(cfgd_core::to_posix_string(from)),
                            to: Some(cfgd_core::to_posix_string(to)),
                            action: "migrated".into(),
                            reason: None,
                        });
                    }
                    Err(e) => {
                        let reason = cfgd_core::output::collapse_to_subject_line(&e);
                        printer.status_simple(
                            Role::Fail,
                            format!("Failed to migrate '{}': {}", name, reason),
                        );
                        records.push(MigrationRecord {
                            name: name.clone(),
                            from: Some(cfgd_core::to_posix_string(from)),
                            to: Some(cfgd_core::to_posix_string(to)),
                            action: "failed".into(),
                            reason: Some(reason),
                        });
                    }
                }
            }
            other => records.push(report_no_move(printer, other)),
        }
    }

    let migrated = count_action(&records, "migrated");
    let failed = count_action(&records, "failed");
    let (role, summary) = match (migrated, failed) {
        (0, 0) => (Role::Ok, "All profiles already canonical".to_string()),
        (m, 0) => (Role::Ok, format!("Migrated {} profile(s)", m)),
        (0, f) => (Role::Fail, format!("{} profile(s) failed to migrate", f)),
        (m, f) => (Role::Warn, format!("Migrated {}, failed {}", m, f)),
    };
    printer.emit(
        Doc::new()
            .status(role, summary)
            .with_data(summary_payload(&records, false)),
    );

    if migrated > 0 {
        update_workflow_best_effort(cli, printer);
    }

    Ok(failed)
}

/// Render + record a non-move plan item (already canonical, or failed during
/// planning — e.g. ambiguous forms on disk).
fn report_no_move(printer: &Printer, item: &PlanItem) -> MigrationRecord {
    match item {
        PlanItem::AlreadyCanonical { name } => {
            printer.status_simple(Role::Ok, format!("Profile '{}' already canonical", name));
            MigrationRecord {
                name: name.clone(),
                from: None,
                to: None,
                action: "already-canonical".into(),
                reason: None,
            }
        }
        PlanItem::Failed { name, reason } => {
            printer.status_simple(Role::Fail, format!("Cannot migrate '{}': {}", name, reason));
            MigrationRecord {
                name: name.clone(),
                from: None,
                to: None,
                action: "failed".into(),
                reason: Some(reason.clone()),
            }
        }
        PlanItem::Move { .. } => unreachable!("move items are handled by the caller"),
    }
}

fn count_action(records: &[MigrationRecord], action: &str) -> usize {
    records.iter().filter(|r| r.action == action).count()
}

fn summary_payload(records: &[MigrationRecord], dry_run: bool) -> serde_json::Value {
    serde_json::json!({
        "profiles": records,
        "migrated": count_action(records, "migrated"),
        "planned": count_action(records, "planned"),
        "alreadyCanonical": count_action(records, "already-canonical"),
        "failed": count_action(records, "failed"),
        "dryRun": dry_run,
    })
}

/// Move one manifest into its bundle dir. The dir may already exist (holding
/// `files/`). `git mv` preserves history when the config dir is a work tree;
/// an untracked manifest falls back silently to a plain rename, while a
/// tracked one whose `git mv` failed warns first — the rename still succeeds
/// but history is lost.
fn execute_move(
    printer: &Printer,
    use_git: bool,
    config_dir: &Path,
    from: &Path,
    to: &Path,
) -> anyhow::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if use_git {
        match git_mv(config_dir, from, to) {
            GitMvOutcome::Moved => return Ok(()),
            GitMvOutcome::NotApplicable => {}
            GitMvOutcome::Failed(stderr) => {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "git mv failed for {} ({}); falling back to plain rename — git history not preserved",
                        from.posix(),
                        cfgd_core::output::collapse_to_subject_line(&stderr),
                    ),
                );
            }
        }
    }
    std::fs::rename(from, to)?;
    Ok(())
}

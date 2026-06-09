use cfgd_core::PathDisplayExt;
use cfgd_core::output::Role;
use tracing::warn;

use super::*;

pub(crate) fn collect_module_file_targets(
    module_name: &str,
    config_dir: &Path,
    cache_over: Option<&Path>,
) -> Vec<PathBuf> {
    // Try local module first
    let module_dir = config_dir.join("modules").join(module_name);
    if let Ok(loaded) = modules::load_module(&module_dir) {
        return loaded
            .spec
            .files
            .iter()
            .map(|f| cfgd_core::expand_tilde(&PathBuf::from(&f.target)))
            .collect();
    }

    // Try cached remote module
    if let Ok(cache_base) = module_cache_dir_for(cache_over) {
        let lockfile = modules::load_lockfile(config_dir).unwrap_or_default();
        if let Some(entry) = lockfile.modules.iter().find(|e| e.name == module_name)
            && let Ok(git_src) = modules::parse_git_source(&entry.url)
        {
            let cache_dir = modules::git_cache_dir(&cache_base, &git_src.repo_url);
            if let Ok(loaded) = modules::load_module(&cache_dir) {
                return loaded
                    .spec
                    .files
                    .iter()
                    .map(|f| cfgd_core::expand_tilde(&PathBuf::from(&f.target)))
                    .collect();
            }
        }
    }

    Vec::new()
}

/// Restore (or remove) each deployed file during module cleanup, routing
/// through the shared reconciler restore path so `existed`/content/symlink
/// semantics stay identical to the `rollback` command.
///
/// For a path with a recorded backup, the restore outcome drives the status
/// line; with no backup, the deployed file is simply removed. Status lines
/// render inside `section`; restore-path warnings go through `printer`.
pub(crate) fn restore_or_remove_deployed_files(
    paths: &[&str],
    state: &cfgd_core::state::StateStore,
    section: &cfgd_core::output::section_guard::SectionGuard<'_>,
    printer: &cfgd_core::output::Printer,
) {
    for file_path in paths {
        let path = Path::new(file_path);
        if let Ok(Some(backup)) = state.latest_backup_for_path(file_path) {
            match cfgd_core::reconciler::restore_file_from_backup(path, &backup, printer) {
                cfgd_core::reconciler::RestoreOutcome::Restored => {
                    section.status_simple(Role::Ok, format!("Restored: {file_path}"));
                }
                cfgd_core::reconciler::RestoreOutcome::Removed => {
                    section.status_simple(Role::Ok, format!("Removed: {file_path}"));
                }
                // Skipped: target already matched, nothing to report.
                // Failed: a warning was already emitted by the restore path.
                cfgd_core::reconciler::RestoreOutcome::Skipped
                | cfgd_core::reconciler::RestoreOutcome::Failed => {}
            }
        } else if path.exists() || path.symlink_metadata().is_ok() {
            // No backup recorded — just remove the deployed file.
            if let Err(e) = std::fs::remove_file(path) {
                section.status_simple(
                    Role::Warn,
                    format!("rollback: failed to remove {file_path}: {e}"),
                );
            } else {
                section.status_simple(Role::Ok, format!("Removed: {file_path}"));
            }
        }
    }
}

/// After removing a module, check if any of its file targets have `.cfgd-backup` files
/// and prompt the user to restore them.
pub(crate) fn prompt_restore_backups(
    targets: &[PathBuf],
    printer: &cfgd_core::output::Printer,
) -> anyhow::Result<()> {
    for target in targets {
        let backup_path = PathBuf::from(format!("{}.cfgd-backup", target.display()));
        if backup_path.exists() {
            // Decline-on-IO-error is the safe default for an unattended path,
            // but the underlying failure is logged so it's not invisible.
            let confirmed = match printer.prompt_confirm(&format!(
                "Restore backup {} to {}?",
                backup_path.posix(),
                target.posix()
            )) {
                Ok(answer) => answer,
                Err(e) => {
                    warn!(
                        target = %target.posix(),
                        backup = %backup_path.posix(),
                        error = %e,
                        "prompt_restore_backups: prompt failed; declining restore"
                    );
                    false
                }
            };
            if confirmed {
                // Remove the cfgd-managed file/symlink if it exists
                if target.symlink_metadata().is_ok() {
                    std::fs::remove_file(target)?;
                }
                std::fs::rename(&backup_path, target)?;
                printer.status_simple(Role::Ok, format!("Restored {}", target.posix()));
            }
        }
    }
    Ok(())
}

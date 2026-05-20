use cfgd_core::output::Role;
use tracing::warn;

use super::*;

pub(crate) fn collect_module_file_targets(module_name: &str, config_dir: &Path) -> Vec<PathBuf> {
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
    if let Ok(cache_base) = modules::default_module_cache_dir() {
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
                backup_path.display(),
                target.display()
            )) {
                Ok(answer) => answer,
                Err(e) => {
                    warn!(
                        target = %target.display(),
                        backup = %backup_path.display(),
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
                printer.status_simple(Role::Ok, format!("Restored {}", target.display()));
            }
        }
    }
    Ok(())
}

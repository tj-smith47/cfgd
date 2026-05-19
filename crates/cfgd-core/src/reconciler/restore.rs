use crate::output_v2::Role;
use crate::providers::FileAction;

use super::types::{Action, EnvAction};

/// Outcome of a single file restoration during rollback.
pub(super) enum RestoreOutcome {
    Restored,
    Removed,
    Skipped,
    Failed,
}

/// Restore a single file from a backup record. Used by `rollback_apply`.
pub(super) fn restore_file_from_backup(
    target: &std::path::Path,
    bk: &crate::state::FileBackupRecord,
    printer: &crate::output_v2::Printer,
) -> RestoreOutcome {
    // Backup has content — write it (works for both regular files and symlink snapshots
    // where the resolved content was captured)
    if !bk.oversized && !bk.content.is_empty() {
        // Check if the current resolved content already matches the backup — skip if so
        if let Ok(Some(current)) = crate::capture_file_resolved_state(target)
            && current.content == bk.content
        {
            return RestoreOutcome::Skipped;
        }
        // Remove existing target (might be a symlink or regular file)
        if target.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(target);
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = crate::atomic_write(target, &bk.content) {
            printer.status_simple(
                Role::Warn,
                format!("rollback: failed to restore {}: {}", target.display(), e),
            );
            return RestoreOutcome::Failed;
        }
        // Restore permissions if recorded
        if let Some(mode) = bk.permissions {
            let _ = crate::set_file_permissions(target, mode);
        }
        return RestoreOutcome::Restored;
    }

    // Symlink with no content (only link target recorded — legacy backup)
    if bk.was_symlink
        && let Some(ref link_target) = bk.symlink_target
    {
        let _ = std::fs::remove_file(target);
        if let Err(e) = crate::create_symlink(std::path::Path::new(link_target), target) {
            printer.status_simple(
                Role::Warn,
                format!(
                    "rollback: failed to restore symlink {}: {}",
                    target.display(),
                    e
                ),
            );
            return RestoreOutcome::Failed;
        }
        return RestoreOutcome::Restored;
    }

    // Empty content, not symlink, not oversized — file didn't exist before
    if bk.content.is_empty() && !bk.was_symlink && !bk.oversized && target.exists() {
        if let Err(e) = std::fs::remove_file(target) {
            printer.status_simple(
                Role::Warn,
                format!("rollback: failed to remove {}: {}", target.display(), e),
            );
            return RestoreOutcome::Failed;
        }
        return RestoreOutcome::Removed;
    }

    RestoreOutcome::Skipped
}

/// Extract the target file path from an action, if it writes to a file.
/// Used for pre-apply backup capture.
pub(super) fn action_target_path(action: &Action) -> Option<std::path::PathBuf> {
    match action {
        Action::File(
            FileAction::Create { target, .. }
            | FileAction::Update { target, .. }
            | FileAction::Delete { target, .. },
        ) => Some(target.clone()),
        Action::Env(EnvAction::WriteEnvFile { path, .. }) => Some(path.clone()),
        // Module deploys multiple files — backup handled per-file in apply_module_action
        _ => None,
    }
}

pub(super) fn content_hash_if_exists(path: &std::path::Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| crate::sha256_hex(&bytes))
}

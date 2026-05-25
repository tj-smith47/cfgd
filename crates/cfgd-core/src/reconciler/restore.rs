use crate::output::Role;
use crate::providers::FileAction;

use super::types::{Action, EnvAction};

/// Outcome of a single file restoration during rollback.
#[derive(Debug, PartialEq, Eq)]
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
    printer: &crate::output::Printer,
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
        // Remove existing target (might be a symlink or regular file). A
        // remove failure here means we cannot atomically replace — propagate
        // as Failed rather than silently continuing with stale content.
        if target.symlink_metadata().is_ok()
            && let Err(e) = std::fs::remove_file(target)
        {
            printer.status_simple(
                Role::Warn,
                format!(
                    "rollback: failed to clear {} before restore: {}",
                    target.display(),
                    e
                ),
            );
            return RestoreOutcome::Failed;
        }
        if let Some(parent) = target.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            printer.status_simple(
                Role::Warn,
                format!(
                    "rollback: failed to create parent dir {}: {}",
                    parent.display(),
                    e
                ),
            );
            return RestoreOutcome::Failed;
        }
        if let Err(e) = crate::atomic_write(target, &bk.content) {
            printer.status_simple(
                Role::Warn,
                format!("rollback: failed to restore {}: {}", target.display(), e),
            );
            return RestoreOutcome::Failed;
        }
        // Restore permissions if recorded. A perm-set failure is treated as
        // hard-fail because rollback exists precisely to revert to a known
        // state — leaving SSH/age keys at 0644 because chmod failed silently
        // is exactly the security-relevant bug this guard prevents.
        if let Some(mode) = bk.permissions
            && let Err(e) = crate::set_file_permissions(target, mode)
        {
            printer.status_simple(
                Role::Warn,
                format!(
                    "rollback: restored {} but failed to set permissions {:o}: {}",
                    target.display(),
                    mode,
                    e
                ),
            );
            return RestoreOutcome::Failed;
        }
        return RestoreOutcome::Restored;
    }

    // Symlink with no content (only link target recorded — legacy backup)
    if bk.was_symlink
        && let Some(ref link_target) = bk.symlink_target
    {
        if target.symlink_metadata().is_ok()
            && let Err(e) = std::fs::remove_file(target)
        {
            printer.status_simple(
                Role::Warn,
                format!(
                    "rollback: failed to clear {} before symlink restore: {}",
                    target.display(),
                    e
                ),
            );
            return RestoreOutcome::Failed;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FileBackupRecord;

    fn record(path: &std::path::Path, content: &[u8], perms: Option<u32>) -> FileBackupRecord {
        FileBackupRecord {
            id: 1,
            apply_id: 1,
            file_path: path.display().to_string(),
            content_hash: crate::sha256_hex(content),
            content: content.to_vec(),
            permissions: perms,
            was_symlink: false,
            symlink_target: None,
            oversized: false,
            backed_up_at: crate::utc_now_iso8601(),
        }
    }

    use crate::test_helpers::test_printer as quiet_printer;

    #[test]
    fn restore_writes_content_and_marks_restored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("config.txt");
        std::fs::write(&target, b"current").unwrap();

        let bk = record(&target, b"original", None);
        let printer = quiet_printer();

        assert_eq!(
            restore_file_from_backup(&target, &bk, &printer),
            RestoreOutcome::Restored
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn restore_with_perm_request_applies_them_and_marks_restored() {
        // Regression guard for MEDIUM #2: rollback used to fall through to
        // `Restored` even on perm-set failure. The happy path proves perms
        // ARE applied — the failure-path equivalent is enforced by the
        // logic in `restore_file_from_backup` returning Failed when
        // `set_file_permissions` errs (covered by error-path tests in the
        // reconciler integration suite).
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("ssh-key");
        std::fs::write(&target, b"current").unwrap();

        let bk = record(&target, b"original", Some(0o600));
        let printer = quiet_printer();

        assert_eq!(
            restore_file_from_backup(&target, &bk, &printer),
            RestoreOutcome::Restored
        );
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "restore must apply requested permission bits");
    }

    #[test]
    fn restore_skips_when_current_already_matches_backup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("same.txt");
        std::fs::write(&target, b"identical").unwrap();

        let bk = record(&target, b"identical", None);
        let printer = quiet_printer();

        assert_eq!(
            restore_file_from_backup(&target, &bk, &printer),
            RestoreOutcome::Skipped
        );
    }
}

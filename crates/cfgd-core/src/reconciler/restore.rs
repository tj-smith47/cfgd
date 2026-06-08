use crate::PathDisplayExt;
use crate::output::Role;
use crate::providers::FileAction;

use super::types::{Action, EnvAction};

/// Outcome of a single file restoration during rollback.
#[derive(Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// The backup content (or symlink) was written to the target.
    Restored,
    /// The target was deleted because the backup marks it as not-yet-existing.
    Removed,
    /// No change: the target already matched, or the record can't be restored
    /// (oversized content) and the target is absent.
    Skipped,
    /// Restore was attempted but failed; a warning was already emitted.
    Failed,
}

/// Restore a single file from a backup record.
///
/// Drives both the `rollback` command and the profile-update module cleanup
/// path. An `existed=false` backup removes the target (undoing a later CREATE);
/// otherwise the recorded content, symlink, and permissions are restored.
/// Warnings are emitted via `printer`; the caller maps the returned
/// [`RestoreOutcome`] onto its own status lines.
pub fn restore_file_from_backup(
    target: &std::path::Path,
    bk: &crate::state::FileBackupRecord,
    printer: &crate::output::Printer,
) -> RestoreOutcome {
    // The file did not exist at backup time (an absent marker) — remove it so
    // rollback undoes a later apply's CREATE rather than restoring stale
    // content. This is the durable replacement for the old empty-content
    // heuristic, which both missed real CREATEs and wrongly removed genuinely
    // empty managed files.
    if !bk.existed {
        if target.exists() {
            if let Err(e) = std::fs::remove_file(target) {
                printer.status_simple(
                    Role::Warn,
                    format!("rollback: failed to remove {}: {}", target.posix(), e),
                );
                return RestoreOutcome::Failed;
            }
            return RestoreOutcome::Removed;
        }
        return RestoreOutcome::Skipped;
    }

    // Backup has restorable content — write it (works for both regular files
    // and symlink snapshots where the resolved content was captured). Empty
    // content for an existed file is a genuine 0-byte managed file and must be
    // written, not removed. Excluded: oversized rows (content not captured) and
    // content-less legacy symlink backups (handled by the symlink branch below).
    if !(bk.oversized || bk.was_symlink && bk.content.is_empty()) {
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
                    target.posix(),
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
                    parent.posix(),
                    e
                ),
            );
            return RestoreOutcome::Failed;
        }
        if let Err(e) = crate::atomic_write(target, &bk.content) {
            printer.status_simple(
                Role::Warn,
                format!("rollback: failed to restore {}: {}", target.posix(), e),
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
                    target.posix(),
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
                    target.posix(),
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
                    target.posix(),
                    e
                ),
            );
            return RestoreOutcome::Failed;
        }
        return RestoreOutcome::Restored;
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
            existed: true,
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
    fn restore_removes_target_when_backup_marked_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("created-later.txt");
        std::fs::write(&target, b"some content").unwrap();

        let mut bk = record(&target, b"", None);
        bk.existed = false;
        let printer = quiet_printer();

        assert_eq!(
            restore_file_from_backup(&target, &bk, &printer),
            RestoreOutcome::Removed
        );
        assert!(
            !target.exists(),
            "absent-marked backup must remove the file"
        );
    }

    #[test]
    fn restore_writes_empty_file_for_existed_empty_backup() {
        // A genuinely empty managed file (existed=true, content empty) must be
        // written as a 0-byte file, NOT removed — guards the latent bug where
        // empty content was treated as "did not exist".
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("empty-managed.txt");
        std::fs::write(&target, b"stale content").unwrap();

        let bk = record(&target, b"", None);
        let printer = quiet_printer();

        assert_eq!(
            restore_file_from_backup(&target, &bk, &printer),
            RestoreOutcome::Restored
        );
        assert!(target.exists(), "existed empty backup must write a file");
        assert_eq!(std::fs::read(&target).unwrap(), b"");
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

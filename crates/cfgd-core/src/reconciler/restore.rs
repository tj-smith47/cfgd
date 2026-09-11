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
        // A row that recorded both a link and its content came from a write
        // that went THROUGH the link, so the rollback goes through it too:
        // clearing the link and dropping a regular file in its place would
        // strand the dotfile repo the link points into and lose the rolled-back
        // content at the next re-link. Restoring the link first covers the case
        // where something replaced it since.
        if bk.was_symlink
            && let Some(ref link_target) = bk.symlink_target
        {
            return restore_through_link(target, std::path::Path::new(link_target), bk, printer);
        }
        // Remove existing target (might be a symlink or regular file). A
        // remove failure here means the replacement cannot be atomic — propagate
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
        // The symlink case returned above, so this target is the regular file
        // the write just created: a link here could only have been planted
        // between the two, which is the swap the no-follow chmod refuses.
        if let Some(mode) = bk.permissions
            && let Err(e) = crate::set_file_permissions_nofollow(target, mode)
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

/// Roll back a write that followed a symlink: put the link back if it is gone,
/// then write the recorded content through it.
fn restore_through_link(
    target: &std::path::Path,
    link_target: &std::path::Path,
    bk: &crate::state::FileBackupRecord,
    printer: &crate::output::Printer,
) -> RestoreOutcome {
    let link_intact = std::fs::read_link(target).is_ok_and(|current| current == link_target);
    if !link_intact {
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
        if let Err(e) = crate::create_symlink(link_target, target) {
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
    }
    if let Err(e) = crate::atomic_write_resolved(target, &bk.content) {
        printer.status_simple(
            Role::Warn,
            format!(
                "rollback: failed to restore {} through its symlink: {}",
                target.posix(),
                e
            ),
        );
        return RestoreOutcome::Failed;
    }
    // The recorded mode is the RESOLVED file's, so the chmod names whatever the
    // write above landed on, through the same resolution that write used: one
    // recorded hop is not the file where the chain is longer than one link, and a
    // relative destination (stow's default shape) belongs to the link's own
    // directory rather than the process cwd. The whole step runs AFTER the write
    // because where the recorded destination is gone the write lands at the link
    // path itself: only then is there a regular file to chmod, and a live symlink
    // is what the no-follow chmod refuses. Naming the resolved file is also what
    // keeps the chmod off the link, which whoever owns the target's directory can
    // re-point between the two calls.
    if let Some(mode) = bk.permissions
        && let Err(e) = crate::resolve_write_target(target)
            .and_then(|resolved| crate::set_file_permissions_nofollow(&resolved, mode))
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
    RestoreOutcome::Restored
}

/// The file a pre-apply backup of an action must capture.
#[derive(Debug, Clone)]
pub(super) struct BackupTarget {
    pub path: std::path::PathBuf,
    /// Whether the action's write follows a symlink at `path` rather than
    /// replacing it.
    ///
    /// The capture has to make the same choice the write does. A link-only
    /// snapshot of a target the write goes *through* records zero bytes, so the
    /// backup row exists and rollback restores nothing — the failure mode is
    /// indistinguishable from a working backup until someone needs it.
    pub follow_symlink: bool,
}

/// Extract the target file of an action, if it writes to a file.
/// Used for pre-apply backup capture.
pub(super) fn action_target_path(action: &Action) -> Option<BackupTarget> {
    match action {
        Action::File(
            FileAction::Create { target, .. }
            | FileAction::Update { target, .. }
            | FileAction::Delete { target, .. },
        ) => Some(BackupTarget {
            path: target.clone(),
            follow_symlink: false,
        }),
        // Both env writes resolve a symlinked target and write through it, so
        // both capture through it. The generated file is as likely to be
        // symlinked into a dotfile repo as the rc file is.
        Action::Env(EnvAction::WriteEnvFile { path, .. }) => Some(BackupTarget {
            path: path.clone(),
            follow_symlink: true,
        }),
        // A source-line injection rewrites a user-owned dotfile in full. It is
        // the one managed write whose target cfgd did not author, so it is the
        // one that most needs a pre-write backup row to roll back to.
        Action::Env(EnvAction::InjectSourceLine { rc_path, .. }) => Some(BackupTarget {
            path: rc_path.clone(),
            follow_symlink: true,
        }),
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
        // Rollback used to fall through to `Restored` even when setting
        // permissions failed. The happy path proves perms
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

    #[cfg(unix)]
    #[test]
    fn restore_through_link_applies_recorded_permissions() {
        // Symlink is cfgd's default file strategy, so this is the backup
        // shape rollback restores for an ordinary managed file: the mode
        // belongs to the file the content was written to (the link's
        // destination), so it must land there too, not just the link.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let real_file = tmp.path().join("real-target.txt");
        std::fs::write(&real_file, b"current").unwrap();
        std::fs::set_permissions(&real_file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let link = tmp.path().join("linked-target.txt");
        std::os::unix::fs::symlink(&real_file, &link).unwrap();

        let mut bk = record(&link, b"original", Some(0o600));
        bk.was_symlink = true;
        bk.symlink_target = Some(real_file.display().to_string());

        let printer = quiet_printer();

        assert_eq!(
            restore_file_from_backup(&link, &bk, &printer),
            RestoreOutcome::Restored
        );
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "rollback through a link must leave the link intact"
        );
        assert_eq!(std::fs::read(&link).unwrap(), b"original");
        let mode = std::fs::metadata(&real_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "rollback through a link must restore the destination's mode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restore_through_link_applies_recorded_permissions_to_a_relative_destination() {
        // `read_link` hands back whatever the link stores, and stow's default
        // shape is relative, so the recorded destination is too. Resolved
        // against the process cwd it names either nothing or a same-named
        // stranger; only the link's own directory answers.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let real_file = tmp.path().join("real-target.txt");
        std::fs::write(&real_file, b"current").unwrap();
        std::fs::set_permissions(&real_file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let link = tmp.path().join("linked-target.txt");
        std::os::unix::fs::symlink("real-target.txt", &link).unwrap();

        let mut bk = record(&link, b"original", Some(0o600));
        bk.was_symlink = true;
        bk.symlink_target = Some("real-target.txt".to_string());

        let printer = quiet_printer();

        assert_eq!(
            restore_file_from_backup(&link, &bk, &printer),
            RestoreOutcome::Restored
        );
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "rollback through a link must leave the link intact"
        );
        assert_eq!(std::fs::read(&link).unwrap(), b"original");
        let mode = std::fs::metadata(&real_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "a relative destination resolves against the link's directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restore_through_a_chain_of_links_applies_the_mode_to_the_file_at_its_end() {
        // A link pointing at a second link is an ordinary stow shape, and the
        // write follows the whole chain, so the recorded mode belongs to the file
        // at the end of it. The recorded destination is the FIRST hop, which is
        // itself a link, and the no-follow chmod refuses a link outright.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let real_file = tmp.path().join("real-target.txt");
        std::fs::write(&real_file, b"current").unwrap();
        std::fs::set_permissions(&real_file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mid = tmp.path().join("mid");
        std::os::unix::fs::symlink("real-target.txt", &mid).unwrap();
        let link = tmp.path().join("linked-target.txt");
        std::os::unix::fs::symlink("mid", &link).unwrap();

        let mut bk = record(&link, b"original", Some(0o600));
        bk.was_symlink = true;
        bk.symlink_target = Some("mid".to_string());

        let printer = quiet_printer();

        assert_eq!(
            restore_file_from_backup(&link, &bk, &printer),
            RestoreOutcome::Restored
        );
        assert!(
            mid.symlink_metadata().unwrap().file_type().is_symlink(),
            "the middle hop stays a link, it is not the file the mode belongs to"
        );
        assert_eq!(std::fs::read(&real_file).unwrap(), b"original");
        let mode = std::fs::metadata(&real_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the recorded mode lands on the file at the end of the chain"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restore_through_a_dangling_link_applies_the_mode_to_the_file_that_replaced_it() {
        // The recorded destination is gone, so the write lands at the link path
        // itself and severs the link (`atomic_write_resolved`'s dangling rule).
        // Only after that write is there a regular file to carry the mode:
        // chmodded first, the path is still a live symlink, which the no-follow
        // chmod refuses, failing a rollback whose content already landed.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("linked-target.txt");
        std::os::unix::fs::symlink("gone.txt", &target).unwrap();

        // Not 0o600: that is the mode the temp file already carries, so a pin
        // asserting it stays green with the chmod deleted.
        let mut bk = record(&target, b"original", Some(0o640));
        bk.was_symlink = true;
        bk.symlink_target = Some("gone.txt".to_string());

        let printer = quiet_printer();

        assert_eq!(
            restore_file_from_backup(&target, &bk, &printer),
            RestoreOutcome::Restored
        );
        assert!(
            target.symlink_metadata().unwrap().file_type().is_file(),
            "a write through a dangling link lands at the link path itself"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o640,
            "the recorded mode lands on the file the write left behind"
        );
        assert!(
            !tmp.path().join("gone.txt").exists(),
            "a dangling destination is never created to satisfy the mode"
        );
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

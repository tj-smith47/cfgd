use std::collections::HashSet;

use crate::errors::Result;
use crate::output::Printer;
use crate::state::{ApplyStatus, FileBackupRecord};

use super::restore::{RestoreOutcome, restore_file_from_backup};
use super::types::RollbackResult;

impl<'a> super::Reconciler<'a> {
    /// Roll back completed file actions from a previous apply.
    ///
    /// Restores files to the state immediately after the target apply. Files
    /// created by a later apply (recorded as absent markers) are deleted.
    /// Package installs and system changes are NOT rolled back — they are
    /// listed in the output as requiring manual review.
    pub fn rollback_apply(&self, apply_id: i64, printer: &Printer) -> Result<RollbackResult> {
        // Rollback restores the system to the state that existed AFTER the target apply.
        //
        // Primary source: post-apply snapshots stored with the target apply_id.
        // These capture the resolved content of all managed files (following symlinks)
        // at the moment the target apply completed. For each file path, the LAST
        // backup entry (highest id) for the target apply is the post-apply snapshot.
        //
        // Fallback: for files not covered by the target apply's snapshots, use the
        // earliest backup from applies AFTER the target (pre-action backups from
        // later applies, which represent the state right after the target).
        let target_backups = self.state.get_apply_backups(apply_id)?;
        let after_backups = self.state.file_backups_after_apply(apply_id)?;
        let after_entries = self.state.journal_entries_after_apply(apply_id)?;

        // Build the post-apply snapshot AND fix the restore order in the same
        // pass: `target_backups` is `ORDER BY id` (chronological), so walking
        // it in REVERSE means the first record seen for a path is the one
        // with the highest id — the post-apply snapshot the comment above
        // describes — and every later (older) duplicate for that path is
        // dropped. The resulting `Vec` is therefore already in
        // reverse-apply order: most-recently-written files first, working
        // backward toward the oldest. A `HashMap` here previously threw that
        // order away and restored files in `RandomState` order, which is
        // doubly wrong for a destructive operation — the on-screen warning
        // order reshuffled per run, and so did the order files were actually
        // overwritten or deleted in. Reverse-apply order is chosen to match
        // ordinary undo semantics: if this rollback is interrupted partway
        // through, the newest overwrites are the ones already undone rather
        // than the oldest.
        let mut seen_paths: HashSet<String> = HashSet::new();
        let mut target_snapshot: Vec<&FileBackupRecord> = Vec::new();
        for bk in target_backups.iter().rev() {
            if seen_paths.insert(bk.file_path.clone()) {
                target_snapshot.push(bk);
            }
        }

        let mut files_restored = 0usize;
        let mut files_removed = 0usize;
        let mut non_file_actions = Vec::new();

        // Collect non-file actions from subsequent applies
        for entry in &after_entries {
            let is_file = entry.is_file_work();
            let already_listed = non_file_actions
                .iter()
                .any(|(_, rid): &(String, String)| rid == &entry.resource_id);
            if !is_file && !already_listed {
                non_file_actions.push((entry.action_type.clone(), entry.resource_id.clone()));
            }
        }

        // Track which file paths we've already restored (avoid duplicate restores)
        let mut restored_paths = HashSet::new();

        // Restore from target apply's post-apply snapshots, in reverse-apply
        // (newest-first) order — see the comment above `target_snapshot`.
        for bk in &target_snapshot {
            restored_paths.insert(bk.file_path.clone());
            let target = std::path::Path::new(&bk.file_path);
            let result = restore_file_from_backup(target, bk, printer);
            match result {
                RestoreOutcome::Restored => files_restored += 1,
                RestoreOutcome::Removed => files_removed += 1,
                RestoreOutcome::Skipped | RestoreOutcome::Failed => {}
            }
        }

        // Fall back to the earliest backup after target for remaining paths,
        // walked FORWARD (ascending id) — the opposite of `target_snapshot`
        // above, and deliberately so. `file_backups_after_apply` dedupes
        // ACROSS applies (one apply_id per path: the earliest apply after
        // target) but NOT within that one apply: an apply that both backs up
        // a path pre-write and stores a post-apply resolved snapshot for it
        // contributes two rows here under the same apply_id. The EARLIEST of
        // those (lowest id) is the pre-action backup — the state that
        // existed the instant before that apply touched the path, which is
        // exactly the target apply's own settled output, since nothing ran
        // between target completing and this apply starting. The latest
        // (post-apply) row is that LATER apply's own result and must lose.
        // Walking forward with first-seen-wins picks the earliest row per
        // path; reversing here (as `target_snapshot` correctly does for ITS
        // own, opposite, latest-wins requirement) silently restores files to
        // a later apply's content instead of the target's — observed as
        // `rollback_removes_files_created_by_later_apply` restoring a
        // later-apply's created file instead of removing it. Files created
        // by a later apply surface here as absent markers (existed=0), which
        // restore_file_from_backup removes — undoing the CREATE.
        for bk in &after_backups {
            if restored_paths.contains(&bk.file_path) {
                continue;
            }
            restored_paths.insert(bk.file_path.clone());
            let target = std::path::Path::new(&bk.file_path);
            let result = restore_file_from_backup(target, bk, printer);
            match result {
                RestoreOutcome::Restored => files_restored += 1,
                RestoreOutcome::Removed => files_removed += 1,
                RestoreOutcome::Skipped | RestoreOutcome::Failed => {}
            }
        }

        // Record rollback as a new apply
        self.state.record_apply(
            "rollback",
            &format!("rollback-of-{}", apply_id),
            ApplyStatus::Success,
            Some(
                &crate::state::ApplySummary::Rollback {
                    rollback_of: apply_id,
                    restored: files_restored,
                    removed: files_removed,
                }
                .to_column(),
            ),
        )?;

        Ok(RollbackResult {
            files_restored,
            files_removed,
            non_file_actions,
        })
    }
}

use std::collections::HashMap;

use crate::errors::Result;
use crate::output_v2::{Printer, Role};
use crate::state::ApplyStatus;

use super::restore::{RestoreOutcome, restore_file_from_backup};
use super::types::RollbackResult;

impl<'a> super::Reconciler<'a> {
    /// Roll back completed file actions from a previous apply.
    ///
    /// Restores files from backups in reverse order. Newly created files (no backup)
    /// are deleted. Package installs and system changes are NOT rolled back — they
    /// are listed in the output as requiring manual review.
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

        // Build a map of file_path -> last backup from the target apply
        // (last = post-apply snapshot, which has the highest id)
        let mut target_snapshot: HashMap<String, &crate::state::FileBackupRecord> = HashMap::new();
        for bk in &target_backups {
            target_snapshot.insert(bk.file_path.clone(), bk);
        }

        let mut files_restored = 0usize;
        let mut files_removed = 0usize;
        let mut non_file_actions = Vec::new();

        // Collect non-file actions from subsequent applies
        for entry in &after_entries {
            let is_file = entry.phase == "files"
                || entry.action_type == "file"
                || entry.resource_id.starts_with("file:");
            if !is_file && !non_file_actions.contains(&entry.resource_id) {
                non_file_actions.push(entry.resource_id.clone());
            }
        }

        // Track which file paths we've already restored (avoid duplicate restores)
        let mut restored_paths = std::collections::HashSet::new();

        // Phase 1: restore from target apply's post-apply snapshots
        for (path, bk) in &target_snapshot {
            restored_paths.insert(path.clone());
            let target = std::path::Path::new(path);
            let result = restore_file_from_backup(target, bk, printer);
            match result {
                RestoreOutcome::Restored => files_restored += 1,
                RestoreOutcome::Removed => files_removed += 1,
                RestoreOutcome::Skipped | RestoreOutcome::Failed => {}
            }
        }

        // Phase 2: fallback to earliest backup after target for remaining paths
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

        // Phase 3: handle files created by subsequent applies but not in target's snapshot
        for entry in &after_entries {
            let is_file = entry.phase == "files"
                || entry.action_type == "file"
                || entry.resource_id.starts_with("file:");
            if !is_file {
                continue;
            }

            let actual_path = entry
                .resource_id
                .strip_prefix("file:create:")
                .or_else(|| entry.resource_id.strip_prefix("file:update:"))
                .or_else(|| entry.resource_id.strip_prefix("file:delete:"))
                .unwrap_or(&entry.resource_id);

            if restored_paths.contains(actual_path) {
                continue;
            }
            restored_paths.insert(actual_path.to_string());

            // If the file is in the target apply's snapshot, it was already handled in phase 1.
            // If not, check the journal to see if it existed at the target apply.
            let target_entries = self.state.journal_completed_actions(apply_id)?;
            let target_had_file = target_entries.iter().any(|e| {
                let target_path = e
                    .resource_id
                    .strip_prefix("file:create:")
                    .or_else(|| e.resource_id.strip_prefix("file:update:"))
                    .or_else(|| e.resource_id.strip_prefix("file:delete:"))
                    .unwrap_or(&e.resource_id);
                target_path == actual_path
            });

            if !target_had_file && entry.resource_id.starts_with("file:create:") {
                let target = std::path::Path::new(actual_path);
                if target.exists() {
                    if let Err(e) = std::fs::remove_file(target) {
                        printer.status_simple(
                            Role::Warn,
                            format!("rollback: failed to remove {}: {}", target.display(), e),
                        );
                    } else {
                        files_removed += 1;
                    }
                }
            }
        }

        // Record rollback as a new apply
        self.state.record_apply(
            "rollback",
            &format!("rollback-of-{}", apply_id),
            ApplyStatus::Success,
            Some(&format!(
                "{{\"rollback_of\":{},\"restored\":{},\"removed\":{}}}",
                apply_id, files_restored, files_removed
            )),
        )?;

        Ok(RollbackResult {
            files_restored,
            files_removed,
            non_file_actions,
        })
    }
}

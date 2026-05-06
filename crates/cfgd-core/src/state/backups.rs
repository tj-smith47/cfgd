use rusqlite::params;

use super::StateStore;
use super::types::FileBackupRecord;
use crate::errors::{Result, StateError};

impl StateStore {
    /// Store a file backup before overwriting.
    pub fn store_file_backup(
        &self,
        apply_id: i64,
        file_path: &str,
        state: &crate::FileState,
    ) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO file_backups (apply_id, file_path, content_hash, content, permissions, was_symlink, symlink_target, oversized, backed_up_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                apply_id,
                file_path,
                state.content_hash,
                state.content,
                state.permissions.map(|p| p as i64),
                state.is_symlink as i64,
                state.symlink_target.as_ref().map(|p| p.display().to_string()),
                state.oversized as i64,
                timestamp,
            ],
        )?;
        Ok(())
    }

    /// Get a file backup by apply_id and path.
    pub fn get_file_backup(
        &self,
        apply_id: i64,
        file_path: &str,
    ) -> Result<Option<FileBackupRecord>> {
        let result = self.conn.query_row(
            "SELECT id, apply_id, file_path, content_hash, content, permissions, was_symlink, symlink_target, oversized, backed_up_at
             FROM file_backups WHERE apply_id = ?1 AND file_path = ?2",
            params![apply_id, file_path],
            |row| {
                Ok(FileBackupRecord {
                    id: row.get(0)?,
                    apply_id: row.get(1)?,
                    file_path: row.get(2)?,
                    content_hash: row.get(3)?,
                    content: row.get(4)?,
                    permissions: row.get::<_, Option<i64>>(5)?.map(|p| p as u32),
                    was_symlink: row.get::<_, i64>(6)? != 0,
                    symlink_target: row.get(7)?,
                    oversized: row.get::<_, i64>(8)? != 0,
                    backed_up_at: row.get(9)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Get all file backups for a specific apply (for full rollback).
    pub fn get_apply_backups(&self, apply_id: i64) -> Result<Vec<FileBackupRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, apply_id, file_path, content_hash, content, permissions, was_symlink, symlink_target, oversized, backed_up_at
             FROM file_backups WHERE apply_id = ?1 ORDER BY id",
        )?;

        let records = stmt
            .query_map(params![apply_id], |row| {
                Ok(FileBackupRecord {
                    id: row.get(0)?,
                    apply_id: row.get(1)?,
                    file_path: row.get(2)?,
                    content_hash: row.get(3)?,
                    content: row.get(4)?,
                    permissions: row.get::<_, Option<i64>>(5)?.map(|p| p as u32),
                    was_symlink: row.get::<_, i64>(6)? != 0,
                    symlink_target: row.get(7)?,
                    oversized: row.get::<_, i64>(8)? != 0,
                    backed_up_at: row.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    /// Get the most recent backup for a file path (for restore after removal).
    pub fn latest_backup_for_path(&self, file_path: &str) -> Result<Option<FileBackupRecord>> {
        let result = self.conn.query_row(
            "SELECT id, apply_id, file_path, content_hash, content, permissions, was_symlink, symlink_target, oversized, backed_up_at
             FROM file_backups WHERE file_path = ?1 ORDER BY id DESC LIMIT 1",
            params![file_path],
            |row| {
                Ok(FileBackupRecord {
                    id: row.get(0)?,
                    apply_id: row.get(1)?,
                    file_path: row.get(2)?,
                    content_hash: row.get(3)?,
                    content: row.get(4)?,
                    permissions: row.get::<_, Option<i64>>(5)?.map(|p| p as u32),
                    was_symlink: row.get::<_, i64>(6)? != 0,
                    symlink_target: row.get(7)?,
                    oversized: row.get::<_, i64>(8)? != 0,
                    backed_up_at: row.get(9)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Get the earliest file backup for each unique file path from applies after the given ID.
    /// This captures the state that existed right after the target apply completed, for each
    /// file that was subsequently modified. Used by rollback to restore to a prior apply's state.
    pub fn file_backups_after_apply(&self, after_apply_id: i64) -> Result<Vec<FileBackupRecord>> {
        // For each distinct file_path in backups from applies after `after_apply_id`,
        // pick the backup with the smallest apply_id (earliest apply after target).
        let mut stmt = self.conn.prepare(
            "SELECT b.id, b.apply_id, b.file_path, b.content_hash, b.content, b.permissions,
                    b.was_symlink, b.symlink_target, b.oversized, b.backed_up_at
             FROM file_backups b
             INNER JOIN (
                 SELECT file_path, MIN(apply_id) AS min_apply_id
                 FROM file_backups
                 WHERE apply_id > ?1
                 GROUP BY file_path
             ) earliest ON b.file_path = earliest.file_path AND b.apply_id = earliest.min_apply_id
             ORDER BY b.id",
        )?;

        let records = stmt
            .query_map(params![after_apply_id], |row| {
                Ok(FileBackupRecord {
                    id: row.get(0)?,
                    apply_id: row.get(1)?,
                    file_path: row.get(2)?,
                    content_hash: row.get(3)?,
                    content: row.get(4)?,
                    permissions: row.get::<_, Option<i64>>(5)?.map(|p| p as u32),
                    was_symlink: row.get::<_, i64>(6)? != 0,
                    symlink_target: row.get(7)?,
                    oversized: row.get::<_, i64>(8)? != 0,
                    backed_up_at: row.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    /// Prune old backups, keeping only the last N applies' worth.
    pub fn prune_old_backups(&self, keep_last_n: usize) -> Result<usize> {
        let deleted: usize = self.conn.execute(
            "DELETE FROM file_backups WHERE apply_id NOT IN (
                SELECT id FROM applies ORDER BY id DESC LIMIT ?1
            )",
            params![keep_last_n as i64],
        )?;
        Ok(deleted)
    }
}

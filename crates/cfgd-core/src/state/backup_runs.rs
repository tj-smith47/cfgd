//! Persistence for declarative backup runs (`spec.backups[]`).

use rusqlite::params;

use super::StateStore;
use super::types::{BackupRunDraft, BackupRunRecord, BackupRunStatus};
use crate::errors::{Result, StateError};

/// Column list shared by every `backup_runs` read, so the `row.get` indices
/// below can never drift from the projection.
const BACKUP_RUN_COLUMNS: &str =
    "id, name, source, destination_path, size_bytes, status, error, started_at, finished_at";

fn map_backup_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<BackupRunRecord> {
    Ok(BackupRunRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        source: row.get(2)?,
        destination_path: row.get(3)?,
        size_bytes: row.get::<_, Option<i64>>(4)?.map(|v| v.max(0) as u64),
        status: BackupRunStatus::from_str(&row.get::<_, String>(5)?),
        error: row.get(6)?,
        started_at: row.get(7)?,
        finished_at: row.get(8)?,
    })
}

impl StateStore {
    /// Persist one backup run and return it with its assigned id.
    pub fn record_backup_run(&self, draft: &BackupRunDraft) -> Result<BackupRunRecord> {
        self.conn.execute(
            "INSERT INTO backup_runs (name, source, destination_path, size_bytes, status, error, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                draft.name,
                draft.source,
                draft.destination_path,
                draft.size_bytes.map(|v| v as i64),
                draft.status.as_str(),
                draft.error,
                draft.started_at,
                draft.finished_at,
            ],
        )?;
        Ok(BackupRunRecord {
            id: self.conn.last_insert_rowid(),
            name: draft.name.clone(),
            source: draft.source.clone(),
            destination_path: draft.destination_path.clone(),
            size_bytes: draft.size_bytes,
            status: draft.status,
            error: draft.error.clone(),
            started_at: draft.started_at.clone(),
            finished_at: draft.finished_at.clone(),
        })
    }

    /// Every run recorded for `name`, newest first.
    pub fn backup_runs(&self, name: &str) -> Result<Vec<BackupRunRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {BACKUP_RUN_COLUMNS} FROM backup_runs WHERE name = ?1 ORDER BY id DESC"
        ))?;
        let records = stmt
            .query_map(params![name], map_backup_run)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(records)
    }

    /// The most recent run for `name`, if any.
    pub fn latest_backup_run(&self, name: &str) -> Result<Option<BackupRunRecord>> {
        let result = self.conn.query_row(
            &format!(
                "SELECT {BACKUP_RUN_COLUMNS} FROM backup_runs WHERE name = ?1 ORDER BY id DESC LIMIT 1"
            ),
            params![name],
            map_backup_run,
        );
        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Drop a run row. Called once its artifact has been removed (or was
    /// already gone) by retention pruning.
    pub fn delete_backup_run(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM backup_runs WHERE id = ?1", params![id])?;
        Ok(())
    }
}

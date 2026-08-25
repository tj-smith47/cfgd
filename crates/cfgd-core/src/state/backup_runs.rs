//! Persistence for declarative backup runs (`spec.backups[]`).

use rusqlite::params;

use super::StateStore;
use super::types::{BackupRunDraft, BackupRunKind, BackupRunRecord, BackupRunStatus};
use crate::errors::{Result, StateError};

/// Column list shared by every `backup_runs` read, so the `row.get` indices
/// below can never drift from the projection.
const BACKUP_RUN_COLUMNS: &str =
    "id, name, kind, source, destination_path, size_bytes, status, error, started_at, finished_at";

fn map_backup_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<BackupRunRecord> {
    Ok(BackupRunRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: BackupRunKind::from_str(&row.get::<_, String>(2)?),
        source: row.get(3)?,
        destination_path: row.get(4)?,
        size_bytes: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
        status: BackupRunStatus::from_str(&row.get::<_, String>(6)?),
        error: row.get(7)?,
        started_at: row.get(8)?,
        finished_at: row.get(9)?,
    })
}

impl StateStore {
    /// Persist one backup run and return it with its assigned id.
    pub fn record_backup_run(&self, draft: &BackupRunDraft) -> Result<BackupRunRecord> {
        self.conn.execute(
            "INSERT INTO backup_runs (name, kind, source, destination_path, size_bytes, status, error, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                draft.name,
                draft.kind.as_str(),
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
            kind: draft.kind,
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

    /// The most recent BACKUP of `name`, if any.
    ///
    /// A restore's safety snapshot is not a run: it is the copy taken of what
    /// the restore is about to overwrite, so answering with it would make a
    /// restore read as the unit's last backup — and re-anchor an interval
    /// schedule on the restore's clock. [`Self::backup_runs`] is the read that
    /// still returns every row of every kind, which is what retention pruning
    /// and the snapshot list walk.
    pub fn latest_backup_run(&self, name: &str) -> Result<Option<BackupRunRecord>> {
        let result = self.conn.query_row(
            &format!(
                "SELECT {BACKUP_RUN_COLUMNS} FROM backup_runs WHERE name = ?1 AND kind = ?2 ORDER BY id DESC LIMIT 1"
            ),
            params![name, BackupRunKind::Run.as_str()],
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

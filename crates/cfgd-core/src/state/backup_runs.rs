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

    /// The most recent run of `name`, if any: the row `backup list`'s Last Run
    /// reads and an interval schedule anchors on. A restore writes no row, so
    /// nothing here can answer with the restore's clock.
    pub fn latest_backup_run(&self, name: &str) -> Result<Option<BackupRunRecord>> {
        let result = self.conn.query_row(
            &format!("SELECT {BACKUP_RUN_COLUMNS} FROM backup_runs WHERE name = ?1 ORDER BY id DESC LIMIT 1"),
            params![name],
            map_backup_run,
        );
        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Re-classify a run as [`BackupRunStatus::Orphaned`]: its recorded
    /// snapshot is no longer inside the unit's destination.
    ///
    /// The retention prune's answer to a row it must not delete and must not
    /// drop. Dropping it threw away the only proof the recorded path was ever
    /// cfgd's, which is why nothing could collect the payload afterwards; the
    /// row survives so `cfgd backup gc` has something to act on.
    pub fn mark_backup_run_orphaned(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE backup_runs SET status = ?1 WHERE id = ?2",
            params![BackupRunStatus::Orphaned.as_str(), id],
        )?;
        Ok(())
    }

    /// Drop a run row. Called once its artifact has been removed (or was
    /// already gone) by retention pruning.
    pub fn delete_backup_run(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM backup_runs WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Replace the cluster-owned cadences with what a check-in just answered,
    /// answering whether that changed the set.
    ///
    /// A REPLACE rather than a merge: the gateway sends the whole set every
    /// time, so a unit a policy stopped scheduling has to lose its projection
    /// here or it would run on a cadence nothing in the cluster still asks for.
    ///
    /// The comparison is taken inside the same transaction as the replace, so
    /// the `true` a caller acts on describes the write it just made. The daemon
    /// re-arms its backup timers on that answer, and re-arming on every check-in
    /// instead would re-resolve the whole profile once per tick.
    pub fn record_cluster_backup_schedules(
        &self,
        projections: &crate::backup::ScheduleProjections,
    ) -> Result<bool> {
        let checked_in_at = crate::utc_now_iso8601();
        self.in_transaction(|| {
            let changed = self.cluster_backup_schedules()? != *projections;
            self.conn
                .execute("DELETE FROM cluster_backup_schedules", [])?;
            let mut insert = self.conn.prepare(
                "INSERT INTO cluster_backup_schedules (name, schedule, retention, checked_in_at)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (name, projection) in projections {
                insert.execute(params![
                    name,
                    projection.schedule,
                    projection.retention,
                    checked_in_at
                ])?;
            }
            Ok(changed)
        })
    }

    /// The cluster-owned cadences the last check-in answered with. Empty when
    /// no check-in has run, or when the cluster owns none of this machine's
    /// units — both of which leave every unit on its declared cadence.
    pub fn cluster_backup_schedules(&self) -> Result<crate::backup::ScheduleProjections> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, schedule, retention FROM cluster_backup_schedules")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                crate::backup::BackupScheduleProjection {
                    schedule: row.get::<_, String>(1)?,
                    retention: row.get::<_, Option<u32>>(2)?,
                },
            ))
        })?;
        let mut out = crate::backup::ScheduleProjections::new();
        for row in rows {
            let (name, projection) = row.map_err(|e| StateError::Database(e.to_string()))?;
            out.insert(name, projection);
        }
        Ok(out)
    }
}

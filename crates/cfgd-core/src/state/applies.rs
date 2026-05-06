use rusqlite::params;

use super::StateStore;
use super::types::{ApplyRecord, ApplyStatus};
use crate::errors::{Result, StateError};

impl StateStore {
    /// Record a completed apply operation.
    pub fn record_apply(
        &self,
        profile: &str,
        plan_hash: &str,
        status: ApplyStatus,
        summary: Option<&str>,
    ) -> Result<i64> {
        let timestamp = crate::utc_now_iso8601();
        self.conn
            .execute(
                "INSERT INTO applies (timestamp, profile, plan_hash, status, summary) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![timestamp, profile, plan_hash, status.as_str(), summary],
            )
            ?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get the most recent apply record.
    pub fn last_apply(&self) -> Result<Option<ApplyRecord>> {
        let result = self.conn.query_row(
            "SELECT id, timestamp, profile, plan_hash, status, summary FROM applies ORDER BY id DESC LIMIT 1",
            [],
            |row| {
                Ok(ApplyRecord {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    profile: row.get(2)?,
                    plan_hash: row.get(3)?,
                    status: ApplyStatus::from_str(&row.get::<_, String>(4)?),
                    summary: row.get(5)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Get a specific apply record by ID.
    pub fn get_apply(&self, apply_id: i64) -> Result<Option<ApplyRecord>> {
        let result = self.conn.query_row(
            "SELECT id, timestamp, profile, plan_hash, status, summary FROM applies WHERE id = ?1",
            params![apply_id],
            |row| {
                Ok(ApplyRecord {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    profile: row.get(2)?,
                    plan_hash: row.get(3)?,
                    status: ApplyStatus::from_str(&row.get::<_, String>(4)?),
                    summary: row.get(5)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Get apply history (most recent first), limited to `limit` entries.
    pub fn history(&self, limit: u32) -> Result<Vec<ApplyRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, timestamp, profile, plan_hash, status, summary FROM applies ORDER BY id DESC LIMIT ?1",
            )
            ?;

        let records = stmt
            .query_map(params![limit], |row| {
                Ok(ApplyRecord {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    profile: row.get(2)?,
                    plan_hash: row.get(3)?,
                    status: ApplyStatus::from_str(&row.get::<_, String>(4)?),
                    summary: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    /// Update apply status (for changing "in-progress" to final status).
    pub fn update_apply_status(
        &self,
        apply_id: i64,
        status: ApplyStatus,
        summary: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE applies SET status = ?1, summary = ?2 WHERE id = ?3",
            params![status.as_str(), summary, apply_id],
        )?;
        Ok(())
    }
}

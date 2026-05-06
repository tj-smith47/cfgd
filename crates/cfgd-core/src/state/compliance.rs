use rusqlite::params;

use super::StateStore;
use super::types::ComplianceHistoryRow;
use crate::errors::{Result, StateError};

impl StateStore {
    /// Store a compliance snapshot. The caller provides the content hash
    /// (typically `sha256_hex` of the serialized JSON).
    pub fn store_compliance_snapshot(
        &self,
        snapshot: &crate::compliance::ComplianceSnapshot,
        hash: &str,
    ) -> Result<()> {
        let json = serde_json::to_string(snapshot)
            .map_err(|e| StateError::Database(format!("failed to serialize snapshot: {}", e)))?;
        self.conn.execute(
            "INSERT INTO compliance_snapshots (timestamp, content_hash, snapshot_json, summary_compliant, summary_warning, summary_violation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                snapshot.timestamp,
                hash,
                json,
                snapshot.summary.compliant as i64,
                snapshot.summary.warning as i64,
                snapshot.summary.violation as i64,
            ],
        )?;
        Ok(())
    }

    /// Get the content hash of the most recently stored compliance snapshot.
    pub fn latest_compliance_hash(&self) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT content_hash FROM compliance_snapshots ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        );

        match result {
            Ok(hash) => Ok(Some(hash)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Get compliance snapshot history as summary rows.
    /// If `since` is provided, only return snapshots after that ISO 8601 timestamp.
    pub fn compliance_history(
        &self,
        since: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ComplianceHistoryRow>> {
        if let Some(since_ts) = since {
            let mut stmt = self.conn.prepare(
                "SELECT id, timestamp, summary_compliant, summary_warning, summary_violation
                 FROM compliance_snapshots WHERE timestamp > ?1 ORDER BY id DESC LIMIT ?2",
            )?;

            let rows = stmt
                .query_map(params![since_ts, limit], |row| {
                    Ok(ComplianceHistoryRow {
                        id: row.get(0)?,
                        timestamp: row.get(1)?,
                        compliant: row.get(2)?,
                        warning: row.get(3)?,
                        violation: row.get(4)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, timestamp, summary_compliant, summary_warning, summary_violation
                 FROM compliance_snapshots ORDER BY id DESC LIMIT ?1",
            )?;

            let rows = stmt
                .query_map(params![limit], |row| {
                    Ok(ComplianceHistoryRow {
                        id: row.get(0)?,
                        timestamp: row.get(1)?,
                        compliant: row.get(2)?,
                        warning: row.get(3)?,
                        violation: row.get(4)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        }
    }

    /// Retrieve a full compliance snapshot by ID.
    pub fn get_compliance_snapshot(
        &self,
        id: i64,
    ) -> Result<Option<crate::compliance::ComplianceSnapshot>> {
        let result = self.conn.query_row(
            "SELECT snapshot_json FROM compliance_snapshots WHERE id = ?1",
            params![id],
            |row| row.get::<_, String>(0),
        );

        match result {
            Ok(json) => {
                let snapshot: crate::compliance::ComplianceSnapshot = serde_json::from_str(&json)
                    .map_err(|e| {
                    StateError::Database(format!("failed to deserialize snapshot: {}", e))
                })?;
                Ok(Some(snapshot))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Remove compliance snapshots older than the given ISO 8601 timestamp.
    /// Returns the number of rows deleted.
    pub fn prune_compliance_snapshots(&self, before_timestamp: &str) -> Result<usize> {
        let deleted = self.conn.execute(
            "DELETE FROM compliance_snapshots WHERE timestamp < ?1",
            params![before_timestamp],
        )?;
        Ok(deleted)
    }
}

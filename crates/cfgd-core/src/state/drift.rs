use rusqlite::params;

use super::StateStore;
use super::types::DriftEvent;
use crate::errors::Result;

impl StateStore {
    /// Record a drift event.
    pub fn record_drift(
        &self,
        resource_type: &str,
        resource_id: &str,
        expected: Option<&str>,
        actual: Option<&str>,
        source: &str,
    ) -> Result<i64> {
        let timestamp = crate::utc_now_iso8601();
        self.conn
            .execute(
                "INSERT INTO drift_events (timestamp, resource_type, resource_id, expected, actual, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![timestamp, resource_type, resource_id, expected, actual, source],
            )
            ?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Resolve drift events by linking them to an apply.
    pub fn resolve_drift(
        &self,
        apply_id: i64,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE drift_events SET resolved_by = ?1 WHERE resource_type = ?2 AND resource_id = ?3 AND resolved_by IS NULL",
                params![apply_id, resource_type, resource_id],
            )
            ?;
        Ok(())
    }

    /// Get unresolved drift events.
    pub fn unresolved_drift(&self) -> Result<Vec<DriftEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, timestamp, resource_type, resource_id, expected, actual, resolved_by, source FROM drift_events WHERE resolved_by IS NULL ORDER BY timestamp DESC",
            )
            ?;

        let events = stmt
            .query_map([], |row| {
                Ok(DriftEvent {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    resource_type: row.get(2)?,
                    resource_id: row.get(3)?,
                    expected: row.get(4)?,
                    actual: row.get(5)?,
                    resolved_by: row.get(6)?,
                    source: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(events)
    }
}

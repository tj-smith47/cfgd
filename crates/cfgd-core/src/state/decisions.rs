use rusqlite::params;

use super::StateStore;
use super::types::PendingDecision;
use crate::errors::Result;

impl StateStore {
    /// Upsert a pending decision. If an unresolved decision already exists for this
    /// (source, resource) pair, updates the summary and resets the timestamp.
    pub fn upsert_pending_decision(
        &self,
        source: &str,
        resource: &str,
        tier: &str,
        action: &str,
        summary: &str,
    ) -> Result<i64> {
        let timestamp = crate::utc_now_iso8601();
        // Try to update an existing unresolved row first
        let updated = self.conn.execute(
            "UPDATE pending_decisions SET tier = ?1, action = ?2, summary = ?3, created_at = ?4
                 WHERE source = ?5 AND resource = ?6 AND resolved_at IS NULL",
            params![tier, action, summary, timestamp, source, resource],
        )?;

        if updated > 0 {
            let id = self
                .conn
                .query_row(
                    "SELECT id FROM pending_decisions WHERE source = ?1 AND resource = ?2 AND resolved_at IS NULL",
                    params![source, resource],
                    |row| row.get(0),
                )
                ?;
            return Ok(id);
        }

        self.conn.execute(
            "INSERT INTO pending_decisions (source, resource, tier, action, summary, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![source, resource, tier, action, summary, timestamp],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get all unresolved pending decisions.
    pub fn pending_decisions(&self) -> Result<Vec<PendingDecision>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, source, resource, tier, action, summary, created_at, resolved_at, resolution
                 FROM pending_decisions WHERE resolved_at IS NULL ORDER BY created_at DESC",
            )
            ?;

        let rows = stmt
            .query_map([], |row| {
                Ok(PendingDecision {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    resource: row.get(2)?,
                    tier: row.get(3)?,
                    action: row.get(4)?,
                    summary: row.get(5)?,
                    created_at: row.get(6)?,
                    resolved_at: row.get(7)?,
                    resolution: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Get pending decisions for a specific source.
    pub fn pending_decisions_for_source(&self, source: &str) -> Result<Vec<PendingDecision>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, source, resource, tier, action, summary, created_at, resolved_at, resolution
                 FROM pending_decisions WHERE source = ?1 AND resolved_at IS NULL ORDER BY created_at DESC",
            )
            ?;

        let rows = stmt
            .query_map(params![source], |row| {
                Ok(PendingDecision {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    resource: row.get(2)?,
                    tier: row.get(3)?,
                    action: row.get(4)?,
                    summary: row.get(5)?,
                    created_at: row.get(6)?,
                    resolved_at: row.get(7)?,
                    resolution: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Resolve a pending decision by resource path.
    pub fn resolve_decision(&self, resource: &str, resolution: &str) -> Result<bool> {
        let timestamp = crate::utc_now_iso8601();
        let updated = self.conn.execute(
            "UPDATE pending_decisions SET resolved_at = ?1, resolution = ?2
                 WHERE resource = ?3 AND resolved_at IS NULL",
            params![timestamp, resolution, resource],
        )?;
        Ok(updated > 0)
    }

    /// Resolve all pending decisions for a source.
    pub fn resolve_decisions_for_source(&self, source: &str, resolution: &str) -> Result<usize> {
        let timestamp = crate::utc_now_iso8601();
        let updated = self.conn.execute(
            "UPDATE pending_decisions SET resolved_at = ?1, resolution = ?2
                 WHERE source = ?3 AND resolved_at IS NULL",
            params![timestamp, resolution, source],
        )?;
        Ok(updated)
    }

    /// Resolve all pending decisions.
    pub fn resolve_all_decisions(&self, resolution: &str) -> Result<usize> {
        let timestamp = crate::utc_now_iso8601();
        let updated = self.conn.execute(
            "UPDATE pending_decisions SET resolved_at = ?1, resolution = ?2
                 WHERE resolved_at IS NULL",
            params![timestamp, resolution],
        )?;
        Ok(updated)
    }
}

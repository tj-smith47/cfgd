use rusqlite::params;

use super::StateStore;
use super::types::DriftEvent;
use crate::errors::Result;

impl StateStore {
    /// Record a drift event for a resource currently diverging from desired
    /// state. Upserts: if an unresolved row already exists for
    /// `(resource_type, resource_id)`, its timestamp and expected/actual values
    /// are refreshed instead of inserting a duplicate, so a resource that drifts
    /// across N reconcile ticks keeps exactly one outstanding row.
    pub fn record_drift(
        &self,
        resource_type: &str,
        resource_id: &str,
        expected: Option<&str>,
        actual: Option<&str>,
        source: &str,
    ) -> Result<i64> {
        let timestamp = crate::utc_now_iso8601();
        let updated = self.conn.execute(
            "UPDATE drift_events SET timestamp = ?1, expected = ?2, actual = ?3, source = ?4
                 WHERE resource_type = ?5 AND resource_id = ?6
                 AND resolved_by IS NULL AND resolved_at IS NULL",
            params![
                timestamp,
                expected,
                actual,
                source,
                resource_type,
                resource_id
            ],
        )?;

        if updated > 0 {
            let id = self.conn.query_row(
                "SELECT id FROM drift_events
                     WHERE resource_type = ?1 AND resource_id = ?2
                     AND resolved_by IS NULL AND resolved_at IS NULL",
                params![resource_type, resource_id],
                |row| row.get(0),
            )?;
            return Ok(id);
        }

        self.conn.execute(
            "INSERT INTO drift_events (timestamp, resource_type, resource_id, expected, actual, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![timestamp, resource_type, resource_id, expected, actual, source],
        )?;
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
                "UPDATE drift_events SET resolved_by = ?1 WHERE resource_type = ?2 AND resource_id = ?3 AND resolved_by IS NULL AND resolved_at IS NULL",
                params![apply_id, resource_type, resource_id],
            )
            ?;
        Ok(())
    }

    /// Mark every unresolved drift row whose `(resource_type, resource_id)` is
    /// NOT in `current` as resolved. Used by the daemon reconcile snapshot: the
    /// plan's action set is the ground truth for what is drifting right now, so
    /// any outstanding row not in that set has healed and must clear.
    ///
    /// `resolved_at` (not `resolved_by`) carries the marker because no apply ran
    /// — `resolved_by` is a foreign key into applies(id) and cannot take a
    /// synthetic value.
    pub fn resolve_drift_not_in(&self, current: &[(String, String)]) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();

        // Empty current set → every unresolved row healed.
        if current.is_empty() {
            return self.resolve_all_drift();
        }

        // Single set-based UPDATE: keep rows whose (resource_type, resource_id)
        // is in `current`, resolve the rest. The composite key is matched via a
        // `\x1f`-joined concatenation (unit-separator never appears in a
        // resource type or POSIX-folded id), and all values are bound params —
        // no value is interpolated into the SQL, so it stays injection-safe.
        let placeholders = std::iter::repeat_n("?", current.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE drift_events SET resolved_at = ?1
                 WHERE resolved_by IS NULL AND resolved_at IS NULL
                 AND (resource_type || char(31) || resource_id) NOT IN ({placeholders})",
        );

        let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(current.len() + 1);
        bound.push(Box::new(timestamp));
        for (rtype, rid) in current {
            bound.push(Box::new(format!("{rtype}\u{1f}{rid}")));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(|b| b.as_ref()).collect();
        self.conn.execute(&sql, refs.as_slice())?;
        Ok(())
    }

    /// Mark every unresolved drift row as resolved. Used by the daemon reconcile
    /// snapshot when a tick detects no drift at all.
    pub fn resolve_all_drift(&self) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();
        self.conn.execute(
            "UPDATE drift_events SET resolved_at = ?1 WHERE resolved_by IS NULL AND resolved_at IS NULL",
            params![timestamp],
        )?;
        Ok(())
    }

    /// Get unresolved drift events.
    pub fn unresolved_drift(&self) -> Result<Vec<DriftEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, timestamp, resource_type, resource_id, expected, actual, resolved_by, source FROM drift_events WHERE resolved_by IS NULL AND resolved_at IS NULL ORDER BY timestamp DESC",
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

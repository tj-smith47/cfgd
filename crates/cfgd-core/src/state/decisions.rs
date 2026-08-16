use rusqlite::params;

use super::StateStore;
use super::types::PendingDecision;
use crate::errors::Result;

/// The column list every decision query selects, in the order
/// [`decision_from_row`] reads them. One constant so a query cannot select a
/// different shape than the mapper expects.
const DECISION_COLUMNS: &str =
    "id, source, resource, tier, action, summary, created_at, resolved_at, resolution";

/// The `pending_decisions.resolution` value a row carries when installed state
/// answered the question — accepted because the machine already satisfies the
/// item, not by the operator's hand.
pub const RESOLUTION_AUTO_ACCEPTED: &str = "auto-accepted";

fn decision_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingDecision> {
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
}

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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM pending_decisions
                 WHERE resolved_at IS NULL ORDER BY created_at DESC"
        ))?;

        let rows = stmt
            .query_map([], decision_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Every decision that currently withholds its resource from reconciliation.
    ///
    /// Two decision states withhold, and the plan cannot tell them apart: an
    /// unresolved row is awaiting the operator's answer, and a `rejected` row
    /// already has it. `docs/sources.md` gives both the same effect — "awaiting
    /// user action" is not applied, "user declined" is "excluded from
    /// reconciliation" — so one query answers "may this resource be planned",
    /// and `accepted` is the only resolution that releases it. The whole row is
    /// returned rather than the path alone because every withheld resource must
    /// also be NAMED on the surface the operator reads; a caller that renders
    /// and a caller that prunes have to work from one list or the plan will
    /// hide a resource nothing explains.
    ///
    /// Only the NEWEST row per `(source, resource)` is consulted. A rejection
    /// does not persist across source versions: an update to the item mints a
    /// fresh decision beside the resolved one, and the answer to that fresh
    /// decision is the operator's current intent. Reading every row instead
    /// would let a stale rejection quietly overrule the acceptance that
    /// replaced it.
    pub fn withheld_decisions(&self) -> Result<Vec<PendingDecision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM pending_decisions AS d
                 WHERE (resolved_at IS NULL OR resolution = 'rejected')
                   AND id = (SELECT MAX(id) FROM pending_decisions AS newer
                             WHERE newer.source = d.source AND newer.resource = d.resource)
                 ORDER BY created_at DESC"
        ))?;
        let rows = stmt
            .query_map([], decision_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Drop every decision belonging to a source that is no longer subscribed.
    ///
    /// Runs on every reconcile, not only when auto-apply is on and a source
    /// remains: a decision outliving its subscription is a row the operator can
    /// never answer, because `cfgd decide` acts against a source that is gone.
    /// An empty `subscribed` therefore clears the table rather than being a
    /// no-op — dropping the last source is exactly the case that leaves rows
    /// behind.
    pub fn discard_decisions_not_in(&self, subscribed: &[String]) -> Result<usize> {
        if subscribed.is_empty() {
            let deleted = self.conn.execute("DELETE FROM pending_decisions", [])?;
            return Ok(deleted);
        }
        let placeholders = (1..=subscribed.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let params = rusqlite::params_from_iter(subscribed.iter());
        let deleted = self.conn.execute(
            &format!("DELETE FROM pending_decisions WHERE source NOT IN ({placeholders})"),
            params,
        )?;
        Ok(deleted)
    }

    /// Drop every decision belonging to `source`, resolved or not.
    ///
    /// For a source the subscriber no longer has: its decisions describe items
    /// that are gone with it, so they are discarded rather than resolved.
    /// Resolving them as `rejected` would be a lasting exclusion of the
    /// resource PATHS they name — a later local declaration of the same file or
    /// package would be withheld by a source the machine no longer subscribes
    /// to — and re-subscribing would find the items already answered rather
    /// than asking again.
    pub fn discard_decisions_for_source(&self, source: &str) -> Result<usize> {
        let deleted = self.conn.execute(
            "DELETE FROM pending_decisions WHERE source = ?1",
            params![source],
        )?;
        Ok(deleted)
    }

    /// Whether `(source, resource)` has ever been asked about, in any state.
    ///
    /// The idempotence guard on minting: a `Notify` policy asks once and then
    /// respects whatever answer the row carries, so an item is re-asked only
    /// when the source's delivered set actually changed. Resolved rows count —
    /// a rejection must not be re-minted the tick after it is recorded.
    pub fn has_decision(&self, source: &str, resource: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM pending_decisions WHERE source = ?1 AND resource = ?2",
            params![source, resource],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get pending decisions for a specific source.
    pub fn pending_decisions_for_source(&self, source: &str) -> Result<Vec<PendingDecision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM pending_decisions
                 WHERE source = ?1 AND resolved_at IS NULL ORDER BY created_at DESC"
        ))?;

        let rows = stmt
            .query_map(params![source], decision_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Record that installed state answered a decision — the machine already
    /// satisfies the item, so the question is closed without the operator's
    /// hand.
    ///
    /// The resolution value is [`RESOLUTION_AUTO_ACCEPTED`], distinguishable
    /// from an operator's `accepted` wherever the row is read; any
    /// non-`rejected` resolution releases the resource from
    /// [`Self::withheld_decisions`] by construction. An open row is resolved
    /// in place (its summary rewritten to say why); with no open row, an
    /// already-resolved row is inserted so the provenance exists even for an
    /// item nothing had asked about yet. Re-observing an unchanged fact is a
    /// no-op rather than a fresh history row per run.
    pub fn record_auto_accepted_decision(
        &self,
        source: &str,
        resource: &str,
        tier: &str,
        action: &str,
        summary: &str,
    ) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();
        let updated = self.conn.execute(
            "UPDATE pending_decisions
                 SET tier = ?1, action = ?2, summary = ?3, resolved_at = ?4, resolution = ?5
                 WHERE source = ?6 AND resource = ?7 AND resolved_at IS NULL",
            params![
                tier,
                action,
                summary,
                timestamp,
                RESOLUTION_AUTO_ACCEPTED,
                source,
                resource
            ],
        )?;
        if updated > 0 {
            return Ok(());
        }

        let already_recorded: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM pending_decisions AS d
                 WHERE source = ?1 AND resource = ?2
                   AND resolution = ?3 AND summary = ?4
                   AND id = (SELECT MAX(id) FROM pending_decisions AS newer
                             WHERE newer.source = d.source AND newer.resource = d.resource)",
            params![source, resource, RESOLUTION_AUTO_ACCEPTED, summary],
            |row| row.get(0),
        )?;
        if already_recorded > 0 {
            return Ok(());
        }

        self.conn.execute(
            "INSERT INTO pending_decisions
                 (source, resource, tier, action, summary, created_at, resolved_at, resolution)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                source,
                resource,
                tier,
                action,
                summary,
                timestamp,
                timestamp,
                RESOLUTION_AUTO_ACCEPTED
            ],
        )?;
        Ok(())
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

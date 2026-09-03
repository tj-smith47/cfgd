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
    ///
    /// A `None` operand LEAVES the stored one alone (`COALESCE`) rather than
    /// blanking it. Producers know their findings to different depths: a live
    /// check words a missing package `installed` / `not installed`, while a
    /// daemon tick re-affirming the same row from its plan knows only that the
    /// package is planned. Overwriting with NULL let the tick erase the words a
    /// reader acts on, and the row then rendered as a version mismatch on a
    /// package that was simply absent. Clearing an operand deliberately is not
    /// a thing any producer needs; re-wording one is, and passing `Some` still
    /// does it.
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
            "UPDATE drift_events
                 SET timestamp = ?1,
                     expected = COALESCE(?2, expected),
                     actual = COALESCE(?3, actual),
                     source = ?4
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

    /// The ONE composite-key UPDATE the three set-based resolvers share: set
    /// `set_clause` on every unresolved row whose `(resource_type,
    /// resource_id)` satisfies `membership` (`IN` / `NOT IN`) against `keys`.
    /// The key is matched as a row value — `(resource_type, resource_id)
    /// IN (VALUES (?,?), …)` — rather than a concatenation, because only the
    /// row-value form is a shape `idx_drift_events_resource` can seek; an
    /// expression over the columns forces a scan however the index is built.
    /// (`NOT IN` still examines every unresolved row — inherent to a
    /// complement, not to the SQL shape.) Every value is a bound param —
    /// nothing is interpolated into the SQL, so it stays injection-safe.
    /// Empty-set semantics are each caller's to decide BEFORE calling; an
    /// empty `keys` here is a caller bug (`VALUES` with no rows is a syntax
    /// error), and each of the three callers short-circuits it.
    fn update_unresolved_by_keys(
        &self,
        set_clause: &str,
        membership: &str,
        first_param: &dyn rusqlite::ToSql,
        keys: &[(String, String)],
    ) -> Result<()> {
        let placeholders = std::iter::repeat_n("(?, ?)", keys.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE drift_events SET {set_clause}
                 WHERE resolved_by IS NULL AND resolved_at IS NULL
                 AND (resource_type, resource_id) {membership} (VALUES {placeholders})",
        );

        let mut refs: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(2 * keys.len() + 1);
        refs.push(first_param);
        for (rtype, rid) in keys {
            refs.push(rtype);
            refs.push(rid);
        }
        self.conn.execute(&sql, refs.as_slice())?;
        Ok(())
    }

    /// Resolve every unresolved drift row whose `(resource_type, resource_id)`
    /// IS in `keys`, linking each to `apply_id`.
    ///
    /// The set-based counterpart of [`Self::resolve_drift`], for a caller
    /// holding many keys at once: one statement whose row-value `IN` seeks
    /// `idx_drift_events_resource`, instead of a statement per merged env var
    /// and alias, paid inside the apply transaction. Only `resolved_by` is
    /// written — the stored operands describe the row they were recorded with
    /// and must stay byte-exact.
    pub fn resolve_drift_keys(&self, apply_id: i64, keys: &[(String, String)]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        self.update_unresolved_by_keys("resolved_by = ?1", "IN", &apply_id, keys)
    }

    /// Mark every unresolved drift row whose `(resource_type, resource_id)`
    /// IS in `healed` as resolved, with no apply to link it to. The SCOPED
    /// counterpart of [`Self::resolve_drift_not_in`]: a `--module` live check
    /// proves clean only the rows it actually re-checked, so it names them
    /// outright — the complement of a scoped scan's findings is mostly rows
    /// the scan never looked at, which it cannot vouch for either way.
    ///
    /// `resolved_at` (not `resolved_by`) carries the marker because no apply
    /// ran, exactly as in the complement method. The empty-set meaning
    /// INVERTS between the pair: an empty `healed` set is a scan that
    /// verified nothing clean, and resolves nothing.
    pub fn resolve_drift_in(&self, healed: &[(String, String)]) -> Result<()> {
        if healed.is_empty() {
            return Ok(());
        }
        let timestamp = crate::utc_now_iso8601();
        self.update_unresolved_by_keys("resolved_at = ?1", "IN", &timestamp, healed)
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
        // Empty current set → every unresolved row healed.
        if current.is_empty() {
            return self.resolve_all_drift();
        }
        let timestamp = crate::utc_now_iso8601();
        self.update_unresolved_by_keys("resolved_at = ?1", "NOT IN", &timestamp, current)
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

    /// Record that a live drift scan just ran against this machine (a
    /// `diff`/`verify`/`status --scan` pass, or a daemon reconcile tick) —
    /// the recorded-state `status` header's staleness signal on a host with
    /// no outstanding drift rows to date it by. Upserts the single row.
    ///
    /// Infallible by design, unlike every other write on this store: all four
    /// callers are read-only commands that already have their answer by the
    /// time they stamp, and a store that refuses the stamp must cost the NEXT
    /// run's header its age line rather than fail the run that found it. The
    /// warning lives here so the four cannot drift into four policies and
    /// four spellings of the same message.
    ///
    /// Returns the timestamp it stamped, so a caller rendering a payload in
    /// the same breath can describe THIS scan rather than re-reading the row
    /// or reporting the previous one — and `None` when the write was refused,
    /// because a stamp no row holds would report the machine as scanned more
    /// recently than the store can prove and then go backwards on the next
    /// run that reads the row instead. A caller that ignores the value is
    /// unaffected either way.
    pub fn record_scan(&self) -> Option<String> {
        let timestamp = crate::utc_now_iso8601();
        let written = self.conn.execute(
            "INSERT INTO last_scan (id, timestamp) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET timestamp = excluded.timestamp",
            params![timestamp],
        );
        if let Err(e) = written {
            tracing::warn!(error = %e, "failed to record scan timestamp");
            return None;
        }
        Some(timestamp)
    }

    /// Pin the recorded scan stamp at `timestamp` and refuse every later
    /// write to it, so a crate that cannot reach the connection can still
    /// drive the refused-write branch of [`record_scan`] and see what its own
    /// fallback renders. Reached as
    /// [`crate::test_helpers::freeze_last_scan_at`], the crate's surface for
    /// every test-only affordance; crate-visible here so the shipped type
    /// carries no public method a reader could mistake for product API.
    ///
    /// A pair of `RAISE(ABORT)` triggers is the one refusal that is selective:
    /// dropping the table would also fail the read a caller makes before it
    /// scans, and a read-only database file refuses nothing to a process
    /// running as root. Both the INSERT and the UPDATE half are installed
    /// because the write is an upsert and either half can be the one that
    /// lands.
    ///
    /// Repeatable, and re-pinnable at a new stamp: the triggers are dropped
    /// before the seed and re-created after it, so the seed itself is not
    /// refused by the freeze a previous call installed.
    ///
    /// [`record_scan`]: StateStore::record_scan
    #[cfg(any(test, feature = "test-helpers"))]
    pub(crate) fn freeze_last_scan_at(&self, timestamp: &str) -> Result<()> {
        self.conn.execute_batch(
            "DROP TRIGGER IF EXISTS cfgd_frozen_last_scan_insert;
             DROP TRIGGER IF EXISTS cfgd_frozen_last_scan_update;",
        )?;
        self.conn.execute(
            "INSERT INTO last_scan (id, timestamp) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET timestamp = excluded.timestamp",
            params![timestamp],
        )?;
        self.conn.execute_batch(
            "CREATE TRIGGER cfgd_frozen_last_scan_insert BEFORE INSERT ON last_scan
                 BEGIN SELECT RAISE(ABORT, 'last_scan is frozen'); END;
             CREATE TRIGGER cfgd_frozen_last_scan_update BEFORE UPDATE ON last_scan
                 BEGIN SELECT RAISE(ABORT, 'last_scan is frozen'); END;",
        )?;
        Ok(())
    }

    /// The timestamp of the most recent [`record_scan`], `None` if this
    /// machine has never been scanned.
    ///
    /// [`record_scan`]: StateStore::record_scan
    pub fn last_scan_at(&self) -> Result<Option<String>> {
        let result =
            self.conn
                .query_row("SELECT timestamp FROM last_scan WHERE id = 1", [], |row| {
                    row.get(0)
                });
        match result {
            Ok(ts) => Ok(Some(ts)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(crate::errors::StateError::Database(e.to_string()).into()),
        }
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
                    want: None,
                    have: None,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(events)
    }
}

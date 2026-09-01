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

    /// Resolve every unresolved drift row whose `(resource_type, resource_id)`
    /// IS in `keys`, linking each to `apply_id`.
    ///
    /// The set-based counterpart of [`Self::resolve_drift`], for a caller
    /// holding many keys at once: `drift_events` carries no index on those two
    /// columns, so a per-key call is a full table scan per merged env var and
    /// alias, paid inside the apply transaction. Only `resolved_by` is
    /// written — the stored operands describe the row they were recorded with
    /// and must stay byte-exact.
    pub fn resolve_drift_keys(&self, apply_id: i64, keys: &[(String, String)]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }

        // Same composite-key match `resolve_drift_not_in` uses: a `\x1f`-joined
        // concatenation (the unit separator never appears in a resource type or
        // a POSIX-folded id), every value a bound param.
        let placeholders = std::iter::repeat_n("?", keys.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE drift_events SET resolved_by = ?1
                 WHERE resolved_by IS NULL AND resolved_at IS NULL
                 AND (resource_type || char(31) || resource_id) IN ({placeholders})",
        );

        let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(keys.len() + 1);
        bound.push(Box::new(apply_id));
        for (rtype, rid) in keys {
            bound.push(Box::new(format!("{rtype}\u{1f}{rid}")));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(|b| b.as_ref()).collect();
        self.conn.execute(&sql, refs.as_slice())?;
        Ok(())
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

        // Same composite-key match as `resolve_drift_not_in`: a `\x1f`-joined
        // concatenation, every value a bound param.
        let placeholders = std::iter::repeat_n("?", healed.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE drift_events SET resolved_at = ?1
                 WHERE resolved_by IS NULL AND resolved_at IS NULL
                 AND (resource_type || char(31) || resource_id) IN ({placeholders})",
        );

        let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(healed.len() + 1);
        bound.push(Box::new(timestamp));
        for (rtype, rid) in healed {
            bound.push(Box::new(format!("{rtype}\u{1f}{rid}")));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(|b| b.as_ref()).collect();
        self.conn.execute(&sql, refs.as_slice())?;
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

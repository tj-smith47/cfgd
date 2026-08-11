use rusqlite::params;

use super::StateStore;
use super::types::JournalEntry;
use crate::errors::Result;

impl StateStore {
    /// Record the start of a journal action.
    pub fn journal_begin(
        &self,
        apply_id: i64,
        action_index: usize,
        phase: &str,
        action_type: &str,
        resource_id: &str,
        pre_state: Option<&str>,
    ) -> Result<i64> {
        let timestamp = crate::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO apply_journal (apply_id, action_index, phase, action_type, resource_id, pre_state, status, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            params![apply_id, action_index as i64, phase, action_type, resource_id, pre_state, timestamp],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Mark a journal action as completed, optionally storing script output.
    ///
    /// `completion_index` is the run's monotonic counter of finishes, assigned
    /// where the action is collected. `completed_at` cannot answer the same
    /// question: [`crate::utc_now_iso8601`] truncates to whole seconds, so two
    /// lanes finishing inside one second are unordered by it — which is the
    /// common case for exactly the short actions lanes exist to overlap.
    pub fn journal_complete(
        &self,
        journal_id: i64,
        completion_index: usize,
        post_state: Option<&str>,
        script_output: Option<&str>,
    ) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();
        self.conn.execute(
            "UPDATE apply_journal SET status = 'completed', post_state = ?1, completed_at = ?2, script_output = ?3, completion_index = ?4 WHERE id = ?5",
            params![post_state, timestamp, script_output, completion_index as i64, journal_id],
        )?;
        Ok(())
    }

    /// Mark a journal action as failed. Takes the same `completion_index` a
    /// success does: a failure is a finish, and the run's execution record is
    /// only total if it carries one.
    pub fn journal_fail(
        &self,
        journal_id: i64,
        completion_index: usize,
        error: &str,
    ) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();
        self.conn.execute(
            "UPDATE apply_journal SET status = 'failed', error = ?1, completed_at = ?2, completion_index = ?3 WHERE id = ?4",
            params![error, timestamp, completion_index as i64, journal_id],
        )?;
        Ok(())
    }

    /// Get all journal entries for an apply (all statuses), in plan order —
    /// which is what `cfgd log --show-output` reads and wants.
    pub fn journal_entries(&self, apply_id: i64) -> Result<Vec<JournalEntry>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOURNAL_COLUMNS} FROM apply_journal WHERE apply_id = ?1 ORDER BY action_index"
        ))?;

        let records = stmt
            .query_map(params![apply_id], journal_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    /// Get all journal entries from applies after the given ID, for rollback
    /// tracking — most-recent-first.
    ///
    /// Ordered by `completion_index`, because once package work runs in lanes
    /// `action_index DESC` stops meaning "most recent first". `COALESCE` is for
    /// in-flight rows, not for old databases: every `StateStore` open migrates
    /// before returning, so no binary reads a pre-migration schema, but a run
    /// killed between [`Self::journal_begin`] and its collection leaves rows
    /// NULL forever and the report across them must still order by something.
    pub fn journal_entries_after_apply(&self, after_apply_id: i64) -> Result<Vec<JournalEntry>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOURNAL_COLUMNS} FROM apply_journal WHERE apply_id > ?1 AND status = 'completed'
             ORDER BY apply_id DESC, COALESCE(completion_index, action_index) DESC"
        ))?;

        let records = stmt
            .query_map(params![after_apply_id], journal_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }
}

/// The `apply_journal` projection [`journal_row`] decodes, positionally. One
/// constant so a column added to one query cannot shift the other's indices.
const JOURNAL_COLUMNS: &str = "id, apply_id, action_index, completion_index, phase, action_type, \
                               resource_id, pre_state, post_state, status, error, started_at, \
                               completed_at, script_output";

fn journal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JournalEntry> {
    Ok(JournalEntry {
        id: row.get(0)?,
        apply_id: row.get(1)?,
        action_index: row.get(2)?,
        completion_index: row.get(3)?,
        phase: row.get(4)?,
        action_type: row.get(5)?,
        resource_id: row.get(6)?,
        pre_state: row.get(7)?,
        post_state: row.get(8)?,
        status: row.get(9)?,
        error: row.get(10)?,
        started_at: row.get(11)?,
        completed_at: row.get(12)?,
        script_output: row.get(13)?,
    })
}

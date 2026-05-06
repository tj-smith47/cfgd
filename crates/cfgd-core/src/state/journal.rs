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
    pub fn journal_complete(
        &self,
        journal_id: i64,
        post_state: Option<&str>,
        script_output: Option<&str>,
    ) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();
        self.conn.execute(
            "UPDATE apply_journal SET status = 'completed', post_state = ?1, completed_at = ?2, script_output = ?3 WHERE id = ?4",
            params![post_state, timestamp, script_output, journal_id],
        )?;
        Ok(())
    }

    /// Mark a journal action as failed.
    pub fn journal_fail(&self, journal_id: i64, error: &str) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();
        self.conn.execute(
            "UPDATE apply_journal SET status = 'failed', error = ?1, completed_at = ?2 WHERE id = ?3",
            params![error, timestamp, journal_id],
        )?;
        Ok(())
    }

    /// Get completed actions for an apply (for rollback).
    pub fn journal_completed_actions(&self, apply_id: i64) -> Result<Vec<JournalEntry>> {
        self.query_journal(apply_id, Some("completed"))
    }

    /// Get all journal entries for an apply (all statuses).
    pub fn journal_entries(&self, apply_id: i64) -> Result<Vec<JournalEntry>> {
        self.query_journal(apply_id, None)
    }

    /// Get all journal entries from applies after the given ID, for rollback tracking.
    pub fn journal_entries_after_apply(&self, after_apply_id: i64) -> Result<Vec<JournalEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, apply_id, action_index, phase, action_type, resource_id, pre_state, post_state, status, error, started_at, completed_at, script_output
             FROM apply_journal WHERE apply_id > ?1 AND status = 'completed' ORDER BY apply_id DESC, action_index DESC",
        )?;

        let records = stmt
            .query_map(params![after_apply_id], |row| {
                Ok(JournalEntry {
                    id: row.get(0)?,
                    apply_id: row.get(1)?,
                    action_index: row.get(2)?,
                    phase: row.get(3)?,
                    action_type: row.get(4)?,
                    resource_id: row.get(5)?,
                    pre_state: row.get(6)?,
                    post_state: row.get(7)?,
                    status: row.get(8)?,
                    error: row.get(9)?,
                    started_at: row.get(10)?,
                    completed_at: row.get(11)?,
                    script_output: row.get(12)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    fn query_journal(
        &self,
        apply_id: i64,
        status_filter: Option<&str>,
    ) -> Result<Vec<JournalEntry>> {
        let base_sql = if status_filter.is_some() {
            "SELECT id, apply_id, action_index, phase, action_type, resource_id, pre_state, post_state, status, error, started_at, completed_at, script_output
             FROM apply_journal WHERE apply_id = ?1 AND status = ?2 ORDER BY action_index"
        } else {
            "SELECT id, apply_id, action_index, phase, action_type, resource_id, pre_state, post_state, status, error, started_at, completed_at, script_output
             FROM apply_journal WHERE apply_id = ?1 ORDER BY action_index"
        };

        let mut stmt = self.conn.prepare(base_sql)?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<JournalEntry> {
            Ok(JournalEntry {
                id: row.get(0)?,
                apply_id: row.get(1)?,
                action_index: row.get(2)?,
                phase: row.get(3)?,
                action_type: row.get(4)?,
                resource_id: row.get(5)?,
                pre_state: row.get(6)?,
                post_state: row.get(7)?,
                status: row.get(8)?,
                error: row.get(9)?,
                started_at: row.get(10)?,
                completed_at: row.get(11)?,
                script_output: row.get(12)?,
            })
        };

        let entries: Vec<JournalEntry> = if let Some(status) = status_filter {
            stmt.query_map(params![apply_id, status], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![apply_id], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };

        Ok(entries)
    }
}

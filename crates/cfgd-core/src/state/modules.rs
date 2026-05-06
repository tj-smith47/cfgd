use rusqlite::params;

use super::StateStore;
use super::types::{ModuleFileRecord, ModuleStateRecord};
use crate::errors::{Result, StateError};

impl StateStore {
    /// Insert or update module state.
    pub fn upsert_module_state(
        &self,
        module_name: &str,
        last_applied: Option<i64>,
        packages_hash: &str,
        files_hash: &str,
        git_sources: Option<&str>,
        status: &str,
    ) -> Result<()> {
        let now = crate::utc_now_iso8601();
        self.conn
            .execute(
                "INSERT INTO module_state (module_name, installed_at, last_applied, packages_hash, files_hash, git_sources, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(module_name) DO UPDATE SET
                    last_applied = ?3,
                    packages_hash = ?4,
                    files_hash = ?5,
                    git_sources = ?6,
                    status = ?7",
                params![module_name, now, last_applied, packages_hash, files_hash, git_sources, status],
            )
            ?;
        Ok(())
    }

    /// Get all module states.
    pub fn module_states(&self) -> Result<Vec<ModuleStateRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT module_name, installed_at, last_applied, packages_hash, files_hash, git_sources, status
                 FROM module_state ORDER BY module_name",
            )
            ?;

        let records = stmt
            .query_map([], |row| {
                Ok(ModuleStateRecord {
                    module_name: row.get(0)?,
                    installed_at: row.get(1)?,
                    last_applied: row.get(2)?,
                    packages_hash: row.get(3)?,
                    files_hash: row.get(4)?,
                    git_sources: row.get(5)?,
                    status: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    /// Get module state by name.
    pub fn module_state_by_name(&self, module_name: &str) -> Result<Option<ModuleStateRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT module_name, installed_at, last_applied, packages_hash, files_hash, git_sources, status
                 FROM module_state WHERE module_name = ?1",
            )
            ?;

        let mut rows = stmt.query_map(params![module_name], |row| {
            Ok(ModuleStateRecord {
                module_name: row.get(0)?,
                installed_at: row.get(1)?,
                last_applied: row.get(2)?,
                packages_hash: row.get(3)?,
                files_hash: row.get(4)?,
                git_sources: row.get(5)?,
                status: row.get(6)?,
            })
        })?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(StateError::Database(e.to_string()).into()),
            None => Ok(None),
        }
    }

    /// Remove module state by name.
    pub fn remove_module_state(&self, module_name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM module_state WHERE module_name = ?1",
            params![module_name],
        )?;
        Ok(())
    }

    /// Record a file deployed by a module.
    pub fn upsert_module_file(
        &self,
        module_name: &str,
        file_path: &str,
        content_hash: &str,
        strategy: &str,
        apply_id: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO module_file_manifest (module_name, file_path, content_hash, strategy, last_applied)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(module_name, file_path) DO UPDATE SET
                content_hash = excluded.content_hash,
                strategy = excluded.strategy,
                last_applied = excluded.last_applied",
            params![module_name, file_path, content_hash, strategy, apply_id],
        )?;
        Ok(())
    }

    /// Get all files deployed by a module.
    pub fn module_deployed_files(&self, module_name: &str) -> Result<Vec<ModuleFileRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT module_name, file_path, content_hash, strategy, last_applied
             FROM module_file_manifest WHERE module_name = ?1 ORDER BY file_path",
        )?;

        let records = stmt
            .query_map(params![module_name], |row| {
                Ok(ModuleFileRecord {
                    module_name: row.get(0)?,
                    file_path: row.get(1)?,
                    content_hash: row.get(2)?,
                    strategy: row.get(3)?,
                    last_applied: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
    }

    /// Delete all manifest entries for a module.
    pub fn delete_module_files(&self, module_name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM module_file_manifest WHERE module_name = ?1",
            params![module_name],
        )?;
        Ok(())
    }
}

use rusqlite::params;

use super::StateStore;
use super::types::ManagedResource;
use crate::errors::Result;

impl StateStore {
    /// Upsert a managed resource record.
    pub fn upsert_managed_resource(
        &self,
        resource_type: &str,
        resource_id: &str,
        source: &str,
        hash: Option<&str>,
        apply_id: Option<i64>,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO managed_resources (resource_type, resource_id, source, last_hash, last_applied)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(resource_type, resource_id) DO UPDATE SET
                    source = excluded.source,
                    last_hash = excluded.last_hash,
                    last_applied = excluded.last_applied",
                params![resource_type, resource_id, source, hash, apply_id],
            )
            ?;
        Ok(())
    }

    /// Check if a resource is tracked in managed_resources.
    pub fn is_resource_managed(&self, resource_type: &str, resource_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM managed_resources WHERE resource_type = ?1 AND resource_id = ?2",
            params![resource_type, resource_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get all managed resources.
    pub fn managed_resources(&self) -> Result<Vec<ManagedResource>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT resource_type, resource_id, source, last_hash, last_applied FROM managed_resources ORDER BY resource_type, resource_id",
            )
            ?;

        let resources = stmt
            .query_map([], |row| {
                Ok(ManagedResource {
                    resource_type: row.get(0)?,
                    resource_id: row.get(1)?,
                    source: row.get(2)?,
                    last_hash: row.get(3)?,
                    last_applied: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(resources)
    }

    /// Get managed resources from a specific source.
    pub fn managed_resources_by_source(&self, source_name: &str) -> Result<Vec<ManagedResource>> {
        let mut stmt = self.conn.prepare(
            "SELECT resource_type, resource_id, source, last_hash, last_applied
                 FROM managed_resources WHERE source = ?1 ORDER BY resource_type, resource_id",
        )?;

        let resources = stmt
            .query_map(params![source_name], |row| {
                Ok(ManagedResource {
                    resource_type: row.get(0)?,
                    resource_id: row.get(1)?,
                    source: row.get(2)?,
                    last_hash: row.get(3)?,
                    last_applied: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(resources)
    }
}

use std::collections::HashSet;

use rusqlite::params;

use super::StateStore;
use super::types::ManagedResource;
use crate::errors::Result;
use crate::providers::OrphanedPackage;

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

    /// Upsert a package tracking row, persisting the manager's uninstall command.
    ///
    /// Like [`upsert_managed_resource`](Self::upsert_managed_resource) but fixed to
    /// `resource_type = "package"` with a NULL `last_hash`, and it records
    /// `uninstall_cmd` — the scripted uninstall template (`Some` only for
    /// user-defined custom managers; `None` for built-ins, whose uninstall derives
    /// from code). On conflict it refreshes source, `last_applied`, AND
    /// `uninstall_cmd`, so a re-install picks up a changed script. This persistence
    /// lets the package still be pruned after its custom-manager block leaves the
    /// config (the script would otherwise vanish with it).
    pub fn upsert_package_resource(
        &self,
        resource_id: &str,
        source: &str,
        apply_id: Option<i64>,
        uninstall_cmd: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO managed_resources (resource_type, resource_id, source, last_hash, last_applied, uninstall_cmd)
                 VALUES ('package', ?1, ?2, NULL, ?3, ?4)
                 ON CONFLICT(resource_type, resource_id) DO UPDATE SET
                    source = excluded.source,
                    last_applied = excluded.last_applied,
                    uninstall_cmd = excluded.uninstall_cmd",
            params![resource_id, source, apply_id, uninstall_cmd],
        )?;
        Ok(())
    }

    /// Package tracking rows whose `<manager>` is not in `known_managers` — i.e.
    /// rows for a custom/scripted manager whose definition has left the config.
    ///
    /// The manager is parsed from `resource_id` by splitting on the first `/`
    /// (matching [`managed_package_ids`](Self::managed_package_ids)). Built-in
    /// managers are always present in the registry, so they never appear here; the
    /// only orphans are custom-manager packages. Each row carries its persisted
    /// `uninstall_cmd` (`None` for rows tracked before the column existed) so the
    /// caller can run the script and prune, or warn when there is no script.
    pub fn orphaned_package_resources(
        &self,
        known_managers: &HashSet<String>,
    ) -> Result<Vec<OrphanedPackage>> {
        let mut stmt = self.conn.prepare(
            "SELECT resource_id, uninstall_cmd FROM managed_resources
                 WHERE resource_type = 'package' ORDER BY resource_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, uninstall_cmd)| {
                id.split_once('/').and_then(|(mgr, pkg)| {
                    (!known_managers.contains(mgr)).then(|| OrphanedPackage {
                        manager: mgr.to_string(),
                        package: pkg.to_string(),
                        uninstall_cmd,
                    })
                })
            })
            .collect())
    }

    /// Remove a managed resource record. Idempotent: deleting a row that is not
    /// tracked is a no-op, not an error — so an uninstall of an already-untracked
    /// package succeeds cleanly.
    pub fn remove_managed_resource(&self, resource_type: &str, resource_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM managed_resources WHERE resource_type = ?1 AND resource_id = ?2",
            params![resource_type, resource_id],
        )?;
        Ok(())
    }

    /// Tracked cfgd-installed packages as `(manager, package)` pairs.
    ///
    /// Rows have `resource_type = "package"` and `resource_id = "<manager>/<package>"`;
    /// the id is split on the first `/` so package names containing `/` (none today,
    /// but defensive) keep their tail intact. Rows whose id has no `/` are skipped.
    pub fn managed_package_ids(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT resource_id FROM managed_resources WHERE resource_type = 'package' ORDER BY resource_id",
        )?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|id| {
                id.split_once('/')
                    .map(|(mgr, pkg)| (mgr.to_string(), pkg.to_string()))
            })
            .collect())
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

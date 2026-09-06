use rusqlite::params;

use super::StateStore;
use crate::errors::{Result, StateError};

/// The storage half of the `providers/` capability: `providers/` declares what
/// a `PackageManager` may reach and this supplies it, so the trait layer never
/// imports the store.
impl crate::providers::PackageStateStore for StateStore {
    fn resolved_prefix(&self, manager: &str) -> Result<Option<(String, bool)>> {
        self.package_manager_prefix(manager)
    }

    fn record_resolved_prefix(&self, manager: &str, prefix: &str, is_fallback: bool) -> Result<()> {
        self.record_package_manager_prefix(manager, prefix, is_fallback)
    }
}

impl StateStore {
    /// Record the global-install prefix `manager` resolved, replacing any
    /// earlier record for it. Every subsequent operation for `manager` reads
    /// this back instead of re-resolving, so a package installed under one
    /// resolved prefix stays visible even if a later run's live inputs
    /// (elevation, write-probe, project-local config) would resolve
    /// differently.
    pub fn record_package_manager_prefix(
        &self,
        manager: &str,
        prefix: &str,
        is_fallback: bool,
    ) -> Result<()> {
        let now = crate::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO package_manager_prefixes (manager, prefix, is_fallback, resolved_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(manager) DO UPDATE SET
                prefix = excluded.prefix,
                is_fallback = excluded.is_fallback,
                resolved_at = excluded.resolved_at",
            params![manager, prefix, is_fallback, now],
        )?;
        Ok(())
    }

    /// The prefix previously resolved for `manager`, paired with whether it
    /// was the fallback (vs. npm's own writable configured prefix). `None`
    /// when nothing has been resolved yet.
    pub fn package_manager_prefix(&self, manager: &str) -> Result<Option<(String, bool)>> {
        let mut stmt = self.conn.prepare(
            "SELECT prefix, is_fallback FROM package_manager_prefixes WHERE manager = ?1",
        )?;

        let mut rows = stmt.query_map(params![manager], |row| {
            let prefix: String = row.get(0)?;
            let is_fallback: bool = row.get(1)?;
            Ok((prefix, is_fallback))
        })?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(StateError::Database(e.to_string()).into()),
            None => Ok(None),
        }
    }
}

use rusqlite::params;

use super::StateStore;
use crate::errors::{Result, StateError};

impl StateStore {
    /// Record the PATH directories cfgd owns for `manager`, replacing any
    /// earlier record for it.
    ///
    /// "Owns" is broader than "bootstrapped", which the table name still says:
    /// a directory cfgd created during an `install()` is recorded here too, so
    /// a manager the user installed themselves still contributes the prefix
    /// cfgd made for it. Renaming the table would buy a migration and no
    /// behavior.
    ///
    /// `dirs` is stored in the order given and read back in that order. The
    /// shell env file generated from these entries is hashed and compared on
    /// every reconcile tick, so a reordering between two reads would surface as
    /// permanent drift.
    pub fn record_bootstrapped_path_dirs(&self, manager: &str, dirs: &[String]) -> Result<()> {
        let encoded = serde_json::to_string(dirs).map_err(|source| StateError::Serialize {
            context: "bootstrapped path dirs",
            source,
        })?;
        let now = crate::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO bootstrapped_managers (manager, path_dirs, bootstrapped_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(manager) DO UPDATE SET
                path_dirs = excluded.path_dirs,
                bootstrapped_at = excluded.bootstrapped_at",
            params![manager, encoded, now],
        )?;
        Ok(())
    }

    /// Every package manager cfgd holds PATH directories for — bootstrapped by
    /// cfgd, or handed a prefix cfgd created during an install — paired with
    /// those directories, ordered by manager name.
    pub fn bootstrapped_managers(&self) -> Result<Vec<(String, Vec<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT manager, path_dirs FROM bootstrapped_managers ORDER BY manager")?;

        let rows = stmt
            .query_map([], |row| {
                let manager: String = row.get(0)?;
                let encoded: String = row.get(1)?;
                Ok((manager, encoded))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut records = Vec::with_capacity(rows.len());
        for (manager, encoded) in rows {
            match serde_json::from_str::<Vec<String>>(&encoded) {
                Ok(dirs) => records.push((manager, dirs)),
                // A row this build cannot decode contributes nothing rather than
                // failing the caller: this feeds `cfgd plan`, `cfgd status`, and
                // the daemon tick, and one unreadable row must not wedge all
                // three. The next bootstrap of that manager rewrites it.
                Err(e) => tracing::warn!("ignoring unreadable bootstrap record for {manager}: {e}"),
            }
        }
        Ok(records)
    }
}

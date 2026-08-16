use rusqlite::params;

use super::StateStore;
use crate::errors::{Result, StateError};

impl StateStore {
    /// Record the PATH directories cfgd owns for `manager`, replacing any
    /// earlier record for it.
    ///
    /// "Owns" is broader than "bootstrapped", which the table name still says:
    /// a directory cfgd created during an `install()` lands in the same row, so
    /// a manager the user installed themselves still contributes the prefix
    /// cfgd made for it. Renaming the table would buy a migration and no
    /// behavior.
    ///
    /// This is the REPLACING write, for a manager restating everything it needs
    /// on PATH. A caller naming one directory it created reaches for
    /// [`StateStore::add_bootstrapped_path_dirs`] instead.
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

    /// Add PATH directories cfgd created for `manager` to whatever is already
    /// recorded, keeping the recorded order and appending only what is new.
    ///
    /// Additive where [`StateStore::record_bootstrapped_path_dirs`] replaces,
    /// because the two answer different questions. A bootstrap declares every
    /// directory its manager needs and may correct an earlier answer, so its
    /// record is the whole row. An install names only the one prefix it had to
    /// create, which is a fact ABOUT the manager rather than a new declaration
    /// for it: writing that narrower value over the row would drop the rest of
    /// a provision's directories out of the generated env file, and nothing
    /// puts them back until the manager is bootstrapped again.
    pub fn add_bootstrapped_path_dirs(&self, manager: &str, dirs: &[String]) -> Result<()> {
        let mut merged = self.manager_path_dirs(manager)?;
        for dir in dirs {
            if !merged.iter().any(|recorded| recorded == dir) {
                merged.push(dir.clone());
            }
        }
        self.record_bootstrapped_path_dirs(manager, &merged)
    }

    /// The PATH directories recorded for one manager, empty when it has no row.
    fn manager_path_dirs(&self, manager: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path_dirs FROM bootstrapped_managers WHERE manager = ?1")?;
        let mut rows = stmt.query_map(params![manager], |row| row.get::<_, String>(0))?;
        let encoded = match rows.next() {
            Some(Ok(encoded)) => encoded,
            Some(Err(e)) => return Err(StateError::Database(e.to_string()).into()),
            None => return Ok(Vec::new()),
        };
        match serde_json::from_str::<Vec<String>>(&encoded) {
            Ok(dirs) => Ok(dirs),
            // Same degrade as `bootstrapped_managers`: a row this build cannot
            // decode contributes nothing and is rewritten by what follows,
            // rather than failing an apply that is otherwise succeeding.
            Err(e) => {
                tracing::warn!("ignoring unreadable bootstrap record for {manager}: {e}");
                Ok(Vec::new())
            }
        }
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

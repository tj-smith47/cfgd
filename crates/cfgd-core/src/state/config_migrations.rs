use std::path::Path;

use rusqlite::{OptionalExtension, params};

use super::StateStore;
use crate::errors::{Result, StateError};

/// The row key for a config file. Folded lexically so `--config ../cfg/cfgd.yaml` and
/// its absolute spelling are one row; never canonicalized, so a symlinked config keeps
/// the name the reader gave it (`absolutize_path`'s own rule). A key, never rendered.
fn config_row_key(config_path: &Path) -> String {
    crate::to_posix_string(crate::lexically_normalized(&crate::absolutize_path(
        config_path,
    )))
}

impl StateStore {
    /// Record what a reader answered about this document, and what they were
    /// asked about. A later answer replaces an earlier one: the question
    /// moved, so the verdict on the old one is spent.
    pub fn record_migration_answer(
        &self,
        config_path: &Path,
        api_version: &str,
        accepted: bool,
        offered_keys: &[String],
    ) -> Result<()> {
        let keys = serde_json::to_string(offered_keys).map_err(|source| StateError::Serialize {
            context: "migration offered keys",
            source,
        })?;
        self.conn.execute(
            "INSERT INTO config_migrations
                 (config_path, api_version, accepted, offered_keys, answered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(config_path, api_version) DO UPDATE SET
                     accepted = excluded.accepted,
                     offered_keys = excluded.offered_keys,
                     answered_at = excluded.answered_at",
            params![
                config_row_key(config_path),
                api_version,
                accepted,
                keys,
                crate::utc_now_iso8601()
            ],
        )?;
        Ok(())
    }

    /// `Some(accepted)` when this exact question was already answered: the
    /// recorded `offered_keys` covers every key `offered` names. `None` when
    /// it was never asked, or asked about less, so a release that adds a
    /// field asks about it rather than inheriting a verdict on a different
    /// question. A question naming no key is answered by nothing. An
    /// `offered_keys` column that no longer decodes is read as covering
    /// nothing, so the reader is asked again rather than answered for.
    pub fn migration_answer(
        &self,
        config_path: &Path,
        api_version: &str,
        offered: &[String],
    ) -> Result<Option<bool>> {
        // `all` over an empty set is true, so a corrupt row would answer a
        // question nobody was asked.
        if offered.is_empty() {
            return Ok(None);
        }
        let row: Option<(bool, String)> = self
            .conn
            .query_row(
                "SELECT accepted, offered_keys FROM config_migrations
                     WHERE config_path = ?1 AND api_version = ?2",
                params![config_row_key(config_path), api_version],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((accepted, keys)) = row else {
            return Ok(None);
        };
        let recorded: Vec<String> = serde_json::from_str(&keys).unwrap_or_default();
        Ok(offered
            .iter()
            .all(|k| recorded.contains(k))
            .then_some(accepted))
    }

    /// Remove the `config_migrations` table so the next read or write of it
    /// fails.
    ///
    /// The seam for a caller's state-store-failure arm, which in production is
    /// reached only by a refused query (a full disk, a locked or corrupt DB)
    /// and is otherwise untestable: the connection is private to this module,
    /// so a consumer's test cannot break the schema by hand. The migrations
    /// are gated on the schema version, so a reopen does not put the table
    /// back.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn drop_config_migrations_table(&self) -> Result<()> {
        self.conn.execute("DROP TABLE config_migrations", [])?;
        Ok(())
    }
}

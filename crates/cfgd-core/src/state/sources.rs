use rusqlite::params;

use super::StateStore;
use super::types::{ConfigSourceRecord, SourceConfigHash};
use crate::errors::{Result, StateError};

/// What one successful fetch writes to a source's `config_sources` row.
///
/// A struct rather than a parameter list because the row is all-optional past
/// its identity: a caller that learned nothing new about the checkout says so
/// by leaving the field at its default, and cannot silently transpose two
/// same-typed `Option<&str>` columns on the way in.
#[derive(Debug, Default, Clone)]
pub struct ConfigSourceUpsert<'a> {
    pub name: &'a str,
    pub origin_url: &'a str,
    pub origin_branch: &'a str,
    pub last_commit: Option<&'a str>,
    pub source_version: Option<&'a str>,
    pub pinned_version: Option<&'a str>,
    /// Whether HEAD carried a signature cfgd accepts. `None` is "cannot say".
    pub last_commit_signed: Option<bool>,
}

impl StateStore {
    /// Upsert a config source record.
    ///
    /// Every caller reaches here having just fetched the source successfully,
    /// so the row's status returns to [`SOURCE_STATUS_ACTIVE`] on the way in.
    /// Without that, a single failed `cfgd source update` left the row reading
    /// `error` for the rest of the source's life: nothing else ever wrote the
    /// column back, and the schema default only applies on INSERT.
    ///
    /// [`SOURCE_STATUS_ACTIVE`]: super::types::SOURCE_STATUS_ACTIVE
    pub fn upsert_config_source(&self, source: &ConfigSourceUpsert<'_>) -> Result<i64> {
        let ConfigSourceUpsert {
            name,
            origin_url,
            origin_branch,
            last_commit,
            source_version,
            pinned_version,
            last_commit_signed,
        } = *source;
        let timestamp = crate::utc_now_iso8601();
        self.conn
            .execute(
                "INSERT INTO config_sources (name, origin_url, origin_branch, last_fetched, last_commit, source_version, pinned_version, last_commit_signed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(name) DO UPDATE SET
                    origin_url = excluded.origin_url,
                    origin_branch = excluded.origin_branch,
                    last_fetched = excluded.last_fetched,
                    last_commit = excluded.last_commit,
                    source_version = excluded.source_version,
                    pinned_version = excluded.pinned_version,
                    last_commit_signed = excluded.last_commit_signed,
                    status = 'active'",
                params![name, origin_url, origin_branch, timestamp, last_commit, source_version, pinned_version, last_commit_signed],
            )
            ?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get all config sources.
    pub fn config_sources(&self) -> Result<Vec<ConfigSourceRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, origin_url, origin_branch, last_fetched, last_commit, source_version, pinned_version, status, last_commit_signed
                 FROM config_sources ORDER BY name",
            )
            ?;

        let sources = stmt
            .query_map([], |row| {
                Ok(ConfigSourceRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    origin_url: row.get(2)?,
                    origin_branch: row.get(3)?,
                    last_fetched: row.get(4)?,
                    last_commit: row.get(5)?,
                    source_version: row.get(6)?,
                    pinned_version: row.get(7)?,
                    status: row.get(8)?,
                    last_commit_signed: row.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(sources)
    }

    /// Get a config source by name.
    pub fn config_source_by_name(&self, name: &str) -> Result<Option<ConfigSourceRecord>> {
        let result = self.conn.query_row(
            "SELECT id, name, origin_url, origin_branch, last_fetched, last_commit, source_version, pinned_version, status, last_commit_signed
             FROM config_sources WHERE name = ?1",
            params![name],
            |row| {
                Ok(ConfigSourceRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    origin_url: row.get(2)?,
                    origin_branch: row.get(3)?,
                    last_fetched: row.get(4)?,
                    last_commit: row.get(5)?,
                    source_version: row.get(6)?,
                    pinned_version: row.get(7)?,
                    status: row.get(8)?,
                    last_commit_signed: row.get(9)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Remove a config source from state.
    pub fn remove_config_source(&self, name: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM config_sources WHERE name = ?1", params![name])?;
        Ok(())
    }

    /// Update the status of a config source.
    pub fn update_config_source_status(&self, name: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE config_sources SET status = ?1 WHERE name = ?2",
            params![status, name],
        )?;
        Ok(())
    }

    /// Record a source apply (links a source's commit to an apply).
    pub fn record_source_apply(
        &self,
        source_name: &str,
        apply_id: i64,
        source_commit: &str,
    ) -> Result<()> {
        let source = self.config_source_by_name(source_name)?;
        if let Some(src) = source {
            self.conn.execute(
                "INSERT INTO source_applies (source_id, apply_id, source_commit)
                     VALUES (?1, ?2, ?3)",
                params![src.id, apply_id, source_commit],
            )?;
        }
        Ok(())
    }

    /// Record a composition conflict.
    pub fn record_source_conflict(
        &self,
        source_name: &str,
        resource_type: &str,
        resource_id: &str,
        resolution: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();
        self.conn
            .execute(
                "INSERT INTO source_conflicts (timestamp, source_name, resource_type, resource_id, resolution, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![timestamp, source_name, resource_type, resource_id, resolution, detail],
            )
            ?;
        Ok(())
    }

    /// Get the stored config hash for a source.
    pub fn source_config_hash(&self, source: &str) -> Result<Option<SourceConfigHash>> {
        let result = self.conn.query_row(
            "SELECT source, config_hash, merged_at FROM source_config_hashes WHERE source = ?1",
            params![source],
            |row| {
                Ok(SourceConfigHash {
                    source: row.get(0)?,
                    config_hash: row.get(1)?,
                    merged_at: row.get(2)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Upsert a source config hash.
    pub fn set_source_config_hash(&self, source: &str, config_hash: &str) -> Result<()> {
        let timestamp = crate::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO source_config_hashes (source, config_hash, merged_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(source) DO UPDATE SET
                    config_hash = excluded.config_hash,
                    merged_at = excluded.merged_at",
            params![source, config_hash, timestamp],
        )?;
        Ok(())
    }

    /// Remove the config hash for a source.
    pub fn remove_source_config_hash(&self, source: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM source_config_hashes WHERE source = ?1",
            params![source],
        )?;
        Ok(())
    }
}

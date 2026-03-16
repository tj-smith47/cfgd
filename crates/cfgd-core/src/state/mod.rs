use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use crate::errors::{Result, StateError};

const MIGRATIONS: &[&str] = &[
    // Migration 0: Initial schema
    "CREATE TABLE IF NOT EXISTS applies (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT NOT NULL,
        profile TEXT NOT NULL,
        plan_hash TEXT NOT NULL,
        status TEXT NOT NULL,
        summary TEXT
    );

    CREATE TABLE IF NOT EXISTS drift_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT NOT NULL,
        resource_type TEXT NOT NULL,
        resource_id TEXT NOT NULL,
        expected TEXT,
        actual TEXT,
        resolved_by INTEGER,
        FOREIGN KEY (resolved_by) REFERENCES applies(id)
    );

    CREATE TABLE IF NOT EXISTS managed_resources (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        resource_type TEXT NOT NULL,
        resource_id TEXT NOT NULL,
        source TEXT NOT NULL DEFAULT 'local',
        last_hash TEXT,
        last_applied INTEGER,
        UNIQUE(resource_type, resource_id),
        FOREIGN KEY (last_applied) REFERENCES applies(id)
    );

    CREATE TABLE IF NOT EXISTS schema_version (
        version INTEGER NOT NULL
    );

    INSERT INTO schema_version (version) VALUES (1);",
    // Migration 1: Config sources (Phase 9)
    "CREATE TABLE IF NOT EXISTS config_sources (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL UNIQUE,
        origin_url TEXT NOT NULL,
        origin_branch TEXT NOT NULL DEFAULT 'main',
        last_fetched TEXT,
        last_commit TEXT,
        source_version TEXT,
        pinned_version TEXT,
        status TEXT NOT NULL DEFAULT 'active'
    );

    CREATE TABLE IF NOT EXISTS source_applies (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        source_id INTEGER NOT NULL,
        apply_id INTEGER NOT NULL,
        source_commit TEXT NOT NULL,
        FOREIGN KEY (source_id) REFERENCES config_sources(id),
        FOREIGN KEY (apply_id) REFERENCES applies(id)
    );

    CREATE TABLE IF NOT EXISTS source_conflicts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT NOT NULL,
        source_name TEXT NOT NULL,
        resource_type TEXT NOT NULL,
        resource_id TEXT NOT NULL,
        resolution TEXT NOT NULL,
        detail TEXT
    );

    UPDATE schema_version SET version = 2;",
    // Migration 2: Add source column to drift_events
    "ALTER TABLE drift_events ADD COLUMN source TEXT NOT NULL DEFAULT 'local';

    UPDATE schema_version SET version = 3;",
    // Migration 3: Auto-apply decisions
    "CREATE TABLE IF NOT EXISTS pending_decisions (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        source      TEXT NOT NULL,
        resource    TEXT NOT NULL,
        tier        TEXT NOT NULL,
        action      TEXT NOT NULL,
        summary     TEXT NOT NULL,
        created_at  TEXT NOT NULL,
        resolved_at TEXT,
        resolution  TEXT
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_decisions_source_resource
        ON pending_decisions (source, resource)
        WHERE resolved_at IS NULL;

    CREATE TABLE IF NOT EXISTS source_config_hashes (
        source      TEXT PRIMARY KEY,
        config_hash TEXT NOT NULL,
        merged_at   TEXT NOT NULL
    );

    UPDATE schema_version SET version = 4;",
    // Migration 4: Module state tracking
    "CREATE TABLE IF NOT EXISTS module_state (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        module_name     TEXT NOT NULL UNIQUE,
        installed_at    TEXT NOT NULL,
        last_applied    INTEGER,
        packages_hash   TEXT NOT NULL,
        files_hash      TEXT NOT NULL,
        git_sources     TEXT,
        status          TEXT NOT NULL DEFAULT 'installed',
        FOREIGN KEY (last_applied) REFERENCES applies(id)
    );

    UPDATE schema_version SET version = 5;",
];

/// Apply status for a reconciliation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStatus {
    Success,
    Partial,
    Failed,
}

impl ApplyStatus {
    fn as_str(&self) -> &str {
        match self {
            ApplyStatus::Success => "success",
            ApplyStatus::Partial => "partial",
            ApplyStatus::Failed => "failed",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "success" => ApplyStatus::Success,
            "partial" => ApplyStatus::Partial,
            "failed" => ApplyStatus::Failed,
            _ => ApplyStatus::Failed,
        }
    }
}

/// A recorded apply operation.
#[derive(Debug, Clone)]
pub struct ApplyRecord {
    pub id: i64,
    pub timestamp: String,
    pub profile: String,
    pub plan_hash: String,
    pub status: ApplyStatus,
    pub summary: Option<String>,
}

/// A recorded drift event.
#[derive(Debug, Clone)]
pub struct DriftEvent {
    pub id: i64,
    pub timestamp: String,
    pub resource_type: String,
    pub resource_id: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub resolved_by: Option<i64>,
    pub source: String,
}

/// A managed resource tracked in the state store.
#[derive(Debug, Clone)]
pub struct ManagedResource {
    pub resource_type: String,
    pub resource_id: String,
    pub source: String,
    pub last_hash: Option<String>,
    pub last_applied: Option<i64>,
}

/// A tracked config source.
#[derive(Debug, Clone)]
pub struct ConfigSourceRecord {
    pub id: i64,
    pub name: String,
    pub origin_url: String,
    pub origin_branch: String,
    pub last_fetched: Option<String>,
    pub last_commit: Option<String>,
    pub source_version: Option<String>,
    pub pinned_version: Option<String>,
    pub status: String,
}

/// A conflict record from composition.
#[derive(Debug, Clone)]
pub struct SourceConflictRecord {
    pub id: i64,
    pub timestamp: String,
    pub source_name: String,
    pub resource_type: String,
    pub resource_id: String,
    pub resolution: String,
    pub detail: Option<String>,
}

/// A pending decision for a source item needing user review.
#[derive(Debug, Clone)]
pub struct PendingDecision {
    pub id: i64,
    pub source: String,
    pub resource: String,
    pub tier: String,
    pub action: String,
    pub summary: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub resolution: Option<String>,
}

/// A stored config hash for detecting source changes.
#[derive(Debug, Clone)]
pub struct SourceConfigHash {
    pub source: String,
    pub config_hash: String,
    pub merged_at: String,
}

/// A module's state in the state store.
#[derive(Debug, Clone)]
pub struct ModuleStateRecord {
    pub module_name: String,
    pub installed_at: String,
    pub last_applied: Option<i64>,
    pub packages_hash: String,
    pub files_hash: String,
    pub git_sources: Option<String>,
    pub status: String,
}

/// SQLite-backed state store for cfgd.
pub struct StateStore {
    conn: Connection,
}

impl StateStore {
    /// Open or create a state store at the default location.
    /// Uses `~/.local/share/cfgd/state.db`.
    pub fn open_default() -> Result<Self> {
        let data_dir = default_state_dir()?;
        std::fs::create_dir_all(&data_dir).map_err(|_| StateError::DirectoryNotWritable {
            path: data_dir.clone(),
        })?;
        let db_path = data_dir.join("state.db");
        Self::open(&db_path)
    }

    /// Open or create a state store at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let mut store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    /// Create an in-memory state store (for testing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        let mut store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&mut self) -> Result<()> {
        let current_version = self.schema_version();

        for (i, migration) in MIGRATIONS.iter().enumerate() {
            if i >= current_version {
                self.conn
                    .execute_batch(migration)
                    .map_err(|e| StateError::MigrationFailed {
                        message: format!("migration {}: {}", i, e),
                    })?;
            }
        }

        Ok(())
    }

    fn schema_version(&self) -> usize {
        self.conn
            .query_row("SELECT version FROM schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|v| v as usize)
            .unwrap_or(0)
    }

    /// Record a completed apply operation.
    pub fn record_apply(
        &self,
        profile: &str,
        plan_hash: &str,
        status: ApplyStatus,
        summary: Option<&str>,
    ) -> Result<i64> {
        let timestamp = crate::utc_now_iso8601();
        self.conn
            .execute(
                "INSERT INTO applies (timestamp, profile, plan_hash, status, summary) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![timestamp, profile, plan_hash, status.as_str(), summary],
            )
            ?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Record a drift event.
    pub fn record_drift(
        &self,
        resource_type: &str,
        resource_id: &str,
        expected: Option<&str>,
        actual: Option<&str>,
        source: &str,
    ) -> Result<i64> {
        let timestamp = crate::utc_now_iso8601();
        self.conn
            .execute(
                "INSERT INTO drift_events (timestamp, resource_type, resource_id, expected, actual, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![timestamp, resource_type, resource_id, expected, actual, source],
            )
            ?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Resolve drift events by linking them to an apply.
    pub fn resolve_drift(
        &self,
        apply_id: i64,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE drift_events SET resolved_by = ?1 WHERE resource_type = ?2 AND resource_id = ?3 AND resolved_by IS NULL",
                params![apply_id, resource_type, resource_id],
            )
            ?;
        Ok(())
    }

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

    /// Get the most recent apply record.
    pub fn last_apply(&self) -> Result<Option<ApplyRecord>> {
        let result = self.conn.query_row(
            "SELECT id, timestamp, profile, plan_hash, status, summary FROM applies ORDER BY id DESC LIMIT 1",
            [],
            |row| {
                Ok(ApplyRecord {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    profile: row.get(2)?,
                    plan_hash: row.get(3)?,
                    status: ApplyStatus::from_str(&row.get::<_, String>(4)?),
                    summary: row.get(5)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StateError::Database(e.to_string()).into()),
        }
    }

    /// Get apply history (most recent first), limited to `limit` entries.
    pub fn history(&self, limit: u32) -> Result<Vec<ApplyRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, timestamp, profile, plan_hash, status, summary FROM applies ORDER BY id DESC LIMIT ?1",
            )
            ?;

        let records = stmt
            .query_map(params![limit], |row| {
                Ok(ApplyRecord {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    profile: row.get(2)?,
                    plan_hash: row.get(3)?,
                    status: ApplyStatus::from_str(&row.get::<_, String>(4)?),
                    summary: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(records)
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

    /// Get unresolved drift events.
    pub fn unresolved_drift(&self) -> Result<Vec<DriftEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, timestamp, resource_type, resource_id, expected, actual, resolved_by, source FROM drift_events WHERE resolved_by IS NULL ORDER BY timestamp DESC",
            )
            ?;

        let events = stmt
            .query_map([], |row| {
                Ok(DriftEvent {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    resource_type: row.get(2)?,
                    resource_id: row.get(3)?,
                    expected: row.get(4)?,
                    actual: row.get(5)?,
                    resolved_by: row.get(6)?,
                    source: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(events)
    }

    // --- Config source state methods (Phase 9) ---

    /// Upsert a config source record.
    pub fn upsert_config_source(
        &self,
        name: &str,
        origin_url: &str,
        origin_branch: &str,
        last_commit: Option<&str>,
        source_version: Option<&str>,
        pinned_version: Option<&str>,
    ) -> Result<i64> {
        let timestamp = crate::utc_now_iso8601();
        self.conn
            .execute(
                "INSERT INTO config_sources (name, origin_url, origin_branch, last_fetched, last_commit, source_version, pinned_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(name) DO UPDATE SET
                    origin_url = excluded.origin_url,
                    origin_branch = excluded.origin_branch,
                    last_fetched = excluded.last_fetched,
                    last_commit = excluded.last_commit,
                    source_version = excluded.source_version,
                    pinned_version = excluded.pinned_version",
                params![name, origin_url, origin_branch, timestamp, last_commit, source_version, pinned_version],
            )
            ?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get all config sources.
    pub fn config_sources(&self) -> Result<Vec<ConfigSourceRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, origin_url, origin_branch, last_fetched, last_commit, source_version, pinned_version, status
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
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(sources)
    }

    /// Get a config source by name.
    pub fn config_source_by_name(&self, name: &str) -> Result<Option<ConfigSourceRecord>> {
        let result = self.conn.query_row(
            "SELECT id, name, origin_url, origin_branch, last_fetched, last_commit, source_version, pinned_version, status
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

    // --- Pending decisions ---

    /// Upsert a pending decision. If an unresolved decision already exists for this
    /// (source, resource) pair, updates the summary and resets the timestamp.
    pub fn upsert_pending_decision(
        &self,
        source: &str,
        resource: &str,
        tier: &str,
        action: &str,
        summary: &str,
    ) -> Result<i64> {
        let timestamp = crate::utc_now_iso8601();
        // Try to update an existing unresolved row first
        let updated = self.conn.execute(
            "UPDATE pending_decisions SET tier = ?1, action = ?2, summary = ?3, created_at = ?4
                 WHERE source = ?5 AND resource = ?6 AND resolved_at IS NULL",
            params![tier, action, summary, timestamp, source, resource],
        )?;

        if updated > 0 {
            let id = self
                .conn
                .query_row(
                    "SELECT id FROM pending_decisions WHERE source = ?1 AND resource = ?2 AND resolved_at IS NULL",
                    params![source, resource],
                    |row| row.get(0),
                )
                ?;
            return Ok(id);
        }

        self.conn.execute(
            "INSERT INTO pending_decisions (source, resource, tier, action, summary, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![source, resource, tier, action, summary, timestamp],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get all unresolved pending decisions.
    pub fn pending_decisions(&self) -> Result<Vec<PendingDecision>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, source, resource, tier, action, summary, created_at, resolved_at, resolution
                 FROM pending_decisions WHERE resolved_at IS NULL ORDER BY created_at DESC",
            )
            ?;

        let rows = stmt
            .query_map([], |row| {
                Ok(PendingDecision {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    resource: row.get(2)?,
                    tier: row.get(3)?,
                    action: row.get(4)?,
                    summary: row.get(5)?,
                    created_at: row.get(6)?,
                    resolved_at: row.get(7)?,
                    resolution: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Get pending decisions for a specific source.
    pub fn pending_decisions_for_source(&self, source: &str) -> Result<Vec<PendingDecision>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, source, resource, tier, action, summary, created_at, resolved_at, resolution
                 FROM pending_decisions WHERE source = ?1 AND resolved_at IS NULL ORDER BY created_at DESC",
            )
            ?;

        let rows = stmt
            .query_map(params![source], |row| {
                Ok(PendingDecision {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    resource: row.get(2)?,
                    tier: row.get(3)?,
                    action: row.get(4)?,
                    summary: row.get(5)?,
                    created_at: row.get(6)?,
                    resolved_at: row.get(7)?,
                    resolution: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Resolve a pending decision by resource path.
    pub fn resolve_decision(&self, resource: &str, resolution: &str) -> Result<bool> {
        let timestamp = crate::utc_now_iso8601();
        let updated = self.conn.execute(
            "UPDATE pending_decisions SET resolved_at = ?1, resolution = ?2
                 WHERE resource = ?3 AND resolved_at IS NULL",
            params![timestamp, resolution, resource],
        )?;
        Ok(updated > 0)
    }

    /// Resolve all pending decisions for a source.
    pub fn resolve_decisions_for_source(&self, source: &str, resolution: &str) -> Result<usize> {
        let timestamp = crate::utc_now_iso8601();
        let updated = self.conn.execute(
            "UPDATE pending_decisions SET resolved_at = ?1, resolution = ?2
                 WHERE source = ?3 AND resolved_at IS NULL",
            params![timestamp, resolution, source],
        )?;
        Ok(updated)
    }

    /// Resolve all pending decisions.
    pub fn resolve_all_decisions(&self, resolution: &str) -> Result<usize> {
        let timestamp = crate::utc_now_iso8601();
        let updated = self.conn.execute(
            "UPDATE pending_decisions SET resolved_at = ?1, resolution = ?2
                 WHERE resolved_at IS NULL",
            params![timestamp, resolution],
        )?;
        Ok(updated)
    }

    // --- Source config hashes ---

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

    // --- Module state ---

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
}

/// Compute SHA256 hash of a serializable plan for deduplication.
pub fn plan_hash(data: &str) -> String {
    format!("{:x}", Sha256::digest(data.as_bytes()))
}

pub fn default_state_dir() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| StateError::DirectoryNotWritable {
        path: PathBuf::from("~/.local/share/cfgd"),
    })?;
    Ok(base.data_local_dir().join("cfgd"))
}

const PENDING_CONFIG_FILENAME: &str = "pending-server-config.json";

/// Save a desired config received from the device gateway for later reconciliation.
pub fn save_pending_server_config(config: &serde_json::Value) -> Result<PathBuf> {
    let dir = default_state_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|_| StateError::DirectoryNotWritable { path: dir.clone() })?;
    let path = dir.join(PENDING_CONFIG_FILENAME);
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| StateError::Database(format!("failed to serialize pending config: {}", e)))?;
    std::fs::write(&path, json)
        .map_err(|_| StateError::DirectoryNotWritable { path: path.clone() })?;
    Ok(path)
}

/// Load a pending server config, if one exists.
pub fn load_pending_server_config() -> Result<Option<serde_json::Value>> {
    let dir = default_state_dir()?;
    let path = dir.join(PENDING_CONFIG_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&path)
        .map_err(|_| StateError::DirectoryNotWritable { path: path.clone() })?;
    let value: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|e| StateError::Database(format!("failed to parse pending config: {}", e)))?;
    Ok(Some(value))
}

/// Remove the pending server config file after it has been consumed.
pub fn clear_pending_server_config() -> Result<()> {
    let dir = default_state_dir()?;
    let path = dir.join(PENDING_CONFIG_FILENAME);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|_| StateError::DirectoryNotWritable { path: path.clone() })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory() {
        let store = StateStore::open_in_memory().unwrap();
        assert!(store.last_apply().unwrap().is_none());
    }

    #[test]
    fn record_and_retrieve_apply() {
        let store = StateStore::open_in_memory().unwrap();
        let id = store
            .record_apply(
                "default",
                "abc123",
                ApplyStatus::Success,
                Some("{\"files\": 3}"),
            )
            .unwrap();
        assert!(id > 0);

        let last = store.last_apply().unwrap().unwrap();
        assert_eq!(last.id, id);
        assert_eq!(last.profile, "default");
        assert_eq!(last.plan_hash, "abc123");
        assert_eq!(last.status, ApplyStatus::Success);
        assert_eq!(last.summary.as_deref(), Some("{\"files\": 3}"));
    }

    #[test]
    fn history_returns_most_recent_first() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .record_apply("p1", "h1", ApplyStatus::Success, None)
            .unwrap();
        store
            .record_apply("p2", "h2", ApplyStatus::Partial, None)
            .unwrap();
        store
            .record_apply("p3", "h3", ApplyStatus::Failed, None)
            .unwrap();

        let history = store.history(10).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].profile, "p3");
        assert_eq!(history[1].profile, "p2");
        assert_eq!(history[2].profile, "p1");
    }

    #[test]
    fn history_respects_limit() {
        let store = StateStore::open_in_memory().unwrap();
        for i in 0..10 {
            store
                .record_apply(&format!("p{}", i), "h", ApplyStatus::Success, None)
                .unwrap();
        }

        let history = store.history(3).unwrap();
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn record_and_retrieve_drift() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .record_drift(
                "file",
                "/home/user/.zshrc",
                Some("abc"),
                Some("def"),
                "local",
            )
            .unwrap();

        let events = store.unresolved_drift().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].resource_type, "file");
        assert_eq!(events[0].resource_id, "/home/user/.zshrc");
        assert_eq!(events[0].expected.as_deref(), Some("abc"));
        assert_eq!(events[0].actual.as_deref(), Some("def"));
        assert!(events[0].resolved_by.is_none());
    }

    #[test]
    fn resolve_drift_links_to_apply() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .record_drift("file", "/test", Some("a"), Some("b"), "local")
            .unwrap();

        let apply_id = store
            .record_apply("default", "h", ApplyStatus::Success, None)
            .unwrap();
        store.resolve_drift(apply_id, "file", "/test").unwrap();

        let events = store.unresolved_drift().unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn upsert_managed_resource() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_managed_resource("file", "/home/.zshrc", "local", Some("hash1"), None)
            .unwrap();

        let resources = store.managed_resources().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].resource_type, "file");
        assert_eq!(resources[0].resource_id, "/home/.zshrc");
        assert_eq!(resources[0].last_hash.as_deref(), Some("hash1"));

        // Update with new hash
        store
            .upsert_managed_resource("file", "/home/.zshrc", "local", Some("hash2"), None)
            .unwrap();

        let resources = store.managed_resources().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].last_hash.as_deref(), Some("hash2"));
    }

    #[test]
    fn is_resource_managed() {
        let store = StateStore::open_in_memory().unwrap();

        assert!(!store.is_resource_managed("file", "/home/.zshrc").unwrap());

        store
            .upsert_managed_resource("file", "/home/.zshrc", "local", Some("hash1"), None)
            .unwrap();

        assert!(store.is_resource_managed("file", "/home/.zshrc").unwrap());
        assert!(!store.is_resource_managed("file", "/home/.bashrc").unwrap());
        assert!(
            !store
                .is_resource_managed("package", "/home/.zshrc")
                .unwrap()
        );
    }

    #[test]
    fn managed_resources_unique_constraint() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_managed_resource("file", "/a", "local", None, None)
            .unwrap();
        store
            .upsert_managed_resource("package", "/a", "local", None, None)
            .unwrap();

        let resources = store.managed_resources().unwrap();
        assert_eq!(resources.len(), 2);
    }

    #[test]
    fn plan_hash_is_deterministic() {
        let h1 = plan_hash("test plan data");
        let h2 = plan_hash("test plan data");
        assert_eq!(h1, h2);
        assert_ne!(h1, plan_hash("different data"));
    }

    #[test]
    fn now_iso8601_format() {
        let ts = crate::utc_now_iso8601();
        assert!(ts.contains('T'));
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
    }

    #[test]
    fn open_file_based_store() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.db");

        let store = StateStore::open(&db_path).unwrap();
        store
            .record_apply("test", "hash", ApplyStatus::Success, None)
            .unwrap();

        // Reopen and verify persistence
        let store2 = StateStore::open(&db_path).unwrap();
        let last = store2.last_apply().unwrap().unwrap();
        assert_eq!(last.profile, "test");
    }

    // --- Config source state tests ---

    #[test]
    fn upsert_and_list_config_sources() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_config_source(
                "acme",
                "git@github.com:acme/config.git",
                "main",
                Some("abc123"),
                Some("2.1.0"),
                Some("~2"),
            )
            .unwrap();

        let sources = store.config_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "acme");
        assert_eq!(sources[0].origin_url, "git@github.com:acme/config.git");
        assert_eq!(sources[0].last_commit.as_deref(), Some("abc123"));
        assert_eq!(sources[0].source_version.as_deref(), Some("2.1.0"));
        assert_eq!(sources[0].status, "active");
    }

    #[test]
    fn config_source_by_name() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_config_source("acme", "url", "main", None, None, None)
            .unwrap();

        let found = store.config_source_by_name("acme").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "acme");

        let not_found = store.config_source_by_name("nonexistent").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn remove_config_source() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_config_source("acme", "url", "main", None, None, None)
            .unwrap();

        store.remove_config_source("acme").unwrap();
        let sources = store.config_sources().unwrap();
        assert!(sources.is_empty());
    }

    #[test]
    fn update_config_source_status() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_config_source("acme", "url", "main", None, None, None)
            .unwrap();

        store
            .update_config_source_status("acme", "inactive")
            .unwrap();
        let source = store.config_source_by_name("acme").unwrap().unwrap();
        assert_eq!(source.status, "inactive");
    }

    #[test]
    fn record_source_conflict() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .record_source_conflict(
                "acme",
                "package",
                "git-secrets (brew)",
                "REQUIRED",
                Some("team requirement"),
            )
            .unwrap();
    }

    #[test]
    fn managed_resources_by_source() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_managed_resource("file", "/a", "local", None, None)
            .unwrap();
        store
            .upsert_managed_resource("file", "/b", "acme", None, None)
            .unwrap();
        store
            .upsert_managed_resource("package", "git-secrets", "acme", None, None)
            .unwrap();

        let acme_resources = store.managed_resources_by_source("acme").unwrap();
        assert_eq!(acme_resources.len(), 2);

        let local_resources = store.managed_resources_by_source("local").unwrap();
        assert_eq!(local_resources.len(), 1);
    }

    #[test]
    fn upsert_config_source_updates_on_conflict() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_config_source("acme", "url1", "main", Some("commit1"), Some("1.0.0"), None)
            .unwrap();
        store
            .upsert_config_source(
                "acme",
                "url2",
                "dev",
                Some("commit2"),
                Some("2.0.0"),
                Some("~2"),
            )
            .unwrap();

        let sources = store.config_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].origin_url, "url2");
        assert_eq!(sources[0].origin_branch, "dev");
        assert_eq!(sources[0].last_commit.as_deref(), Some("commit2"));
        assert_eq!(sources[0].source_version.as_deref(), Some("2.0.0"));
    }

    // --- Pending decision tests ---

    #[test]
    fn upsert_and_list_pending_decisions() {
        let store = StateStore::open_in_memory().unwrap();
        let id = store
            .upsert_pending_decision(
                "acme",
                "packages.brew.k9s",
                "recommended",
                "install",
                "install k9s (recommended by acme)",
            )
            .unwrap();
        assert!(id > 0);

        let decisions = store.pending_decisions().unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].source, "acme");
        assert_eq!(decisions[0].resource, "packages.brew.k9s");
        assert_eq!(decisions[0].tier, "recommended");
        assert_eq!(decisions[0].action, "install");
        assert!(decisions[0].resolved_at.is_none());
    }

    #[test]
    fn upsert_pending_decision_updates_existing() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_pending_decision(
                "acme",
                "packages.brew.k9s",
                "recommended",
                "install",
                "original summary",
            )
            .unwrap();
        store
            .upsert_pending_decision(
                "acme",
                "packages.brew.k9s",
                "recommended",
                "update",
                "updated summary",
            )
            .unwrap();

        let decisions = store.pending_decisions().unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, "update");
        assert_eq!(decisions[0].summary, "updated summary");
    }

    #[test]
    fn resolve_decision_by_resource() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_pending_decision("acme", "packages.brew.k9s", "recommended", "install", "k9s")
            .unwrap();

        let resolved = store
            .resolve_decision("packages.brew.k9s", "accepted")
            .unwrap();
        assert!(resolved);

        let pending = store.pending_decisions().unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn resolve_decision_nonexistent_returns_false() {
        let store = StateStore::open_in_memory().unwrap();
        let resolved = store
            .resolve_decision("nonexistent.resource", "accepted")
            .unwrap();
        assert!(!resolved);
    }

    #[test]
    fn resolve_decisions_for_source() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_pending_decision("acme", "packages.brew.k9s", "recommended", "install", "k9s")
            .unwrap();
        store
            .upsert_pending_decision(
                "acme",
                "packages.brew.stern",
                "recommended",
                "install",
                "stern",
            )
            .unwrap();
        store
            .upsert_pending_decision("other", "packages.brew.bat", "optional", "install", "bat")
            .unwrap();

        let count = store
            .resolve_decisions_for_source("acme", "accepted")
            .unwrap();
        assert_eq!(count, 2);

        let pending = store.pending_decisions().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].source, "other");
    }

    #[test]
    fn resolve_all_decisions() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_pending_decision("a", "r1", "recommended", "install", "s1")
            .unwrap();
        store
            .upsert_pending_decision("b", "r2", "optional", "install", "s2")
            .unwrap();

        let count = store.resolve_all_decisions("accepted").unwrap();
        assert_eq!(count, 2);

        let pending = store.pending_decisions().unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_decisions_for_source() {
        let store = StateStore::open_in_memory().unwrap();
        store
            .upsert_pending_decision("acme", "r1", "recommended", "install", "s1")
            .unwrap();
        store
            .upsert_pending_decision("other", "r2", "optional", "install", "s2")
            .unwrap();

        let acme = store.pending_decisions_for_source("acme").unwrap();
        assert_eq!(acme.len(), 1);
        assert_eq!(acme[0].resource, "r1");
    }

    // --- Source config hash tests ---

    #[test]
    fn set_and_get_source_config_hash() {
        let store = StateStore::open_in_memory().unwrap();
        store.set_source_config_hash("acme", "hash123").unwrap();

        let hash = store.source_config_hash("acme").unwrap().unwrap();
        assert_eq!(hash.config_hash, "hash123");
    }

    #[test]
    fn source_config_hash_upsert() {
        let store = StateStore::open_in_memory().unwrap();
        store.set_source_config_hash("acme", "hash1").unwrap();
        store.set_source_config_hash("acme", "hash2").unwrap();

        let hash = store.source_config_hash("acme").unwrap().unwrap();
        assert_eq!(hash.config_hash, "hash2");
    }

    #[test]
    fn source_config_hash_not_found() {
        let store = StateStore::open_in_memory().unwrap();
        let hash = store.source_config_hash("nonexistent").unwrap();
        assert!(hash.is_none());
    }

    #[test]
    fn remove_source_config_hash() {
        let store = StateStore::open_in_memory().unwrap();
        store.set_source_config_hash("acme", "hash1").unwrap();
        store.remove_source_config_hash("acme").unwrap();

        let hash = store.source_config_hash("acme").unwrap();
        assert!(hash.is_none());
    }
}

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::errors::{Result, StateError};

mod applies;
mod backups;
mod compliance;
mod decisions;
mod drift;
mod journal;
mod managed;
mod modules;
mod pending_config;
mod sources;
mod types;

pub use pending_config::{
    clear_pending_server_config, load_pending_server_config, save_pending_server_config,
};
pub use types::{
    ApplyRecord, ApplyStatus, ComplianceHistoryRow, ConfigSourceRecord, DriftEvent,
    FileBackupRecord, JournalEntry, ManagedResource, ModuleFileRecord, ModuleStateRecord,
    PendingDecision, SourceConfigHash, SourceConflictRecord,
};

const MIGRATIONS: &[&str] = &[
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
        source TEXT NOT NULL DEFAULT 'local',
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

    CREATE TABLE IF NOT EXISTS config_sources (
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

    CREATE TABLE IF NOT EXISTS pending_decisions (
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

    CREATE TABLE IF NOT EXISTS module_state (
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

    CREATE TABLE IF NOT EXISTS schema_version (
        version INTEGER NOT NULL
    );

    INSERT INTO schema_version (version) VALUES (0);",
    // Migration 2: File safety — backup store, transaction journal, module file manifest
    "CREATE TABLE IF NOT EXISTS file_backups (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        apply_id        INTEGER NOT NULL,
        file_path       TEXT NOT NULL,
        content_hash    TEXT NOT NULL,
        content         BLOB NOT NULL,
        permissions     INTEGER,
        was_symlink     INTEGER NOT NULL DEFAULT 0,
        symlink_target  TEXT,
        oversized       INTEGER NOT NULL DEFAULT 0,
        backed_up_at    TEXT NOT NULL,
        FOREIGN KEY (apply_id) REFERENCES applies(id)
    );

    CREATE INDEX IF NOT EXISTS idx_file_backups_apply ON file_backups (apply_id);
    CREATE INDEX IF NOT EXISTS idx_file_backups_path ON file_backups (file_path);

    CREATE TABLE IF NOT EXISTS apply_journal (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        apply_id        INTEGER NOT NULL,
        action_index    INTEGER NOT NULL,
        phase           TEXT NOT NULL,
        action_type     TEXT NOT NULL,
        resource_id     TEXT NOT NULL,
        pre_state       TEXT,
        post_state      TEXT,
        status          TEXT NOT NULL DEFAULT 'pending',
        error           TEXT,
        started_at      TEXT NOT NULL,
        completed_at    TEXT,
        FOREIGN KEY (apply_id) REFERENCES applies(id)
    );

    CREATE INDEX IF NOT EXISTS idx_apply_journal_apply ON apply_journal (apply_id);

    CREATE TABLE IF NOT EXISTS module_file_manifest (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        module_name     TEXT NOT NULL,
        file_path       TEXT NOT NULL,
        content_hash    TEXT NOT NULL,
        strategy        TEXT NOT NULL,
        last_applied    INTEGER,
        UNIQUE(module_name, file_path),
        FOREIGN KEY (last_applied) REFERENCES applies(id)
    );

    CREATE INDEX IF NOT EXISTS idx_module_file_manifest_module ON module_file_manifest (module_name);",
    // Migration 3: Script output capture — store stdout/stderr from script actions
    "ALTER TABLE apply_journal ADD COLUMN script_output TEXT;",
    // Migration 4: Compliance snapshots — periodic machine state snapshots
    "CREATE TABLE IF NOT EXISTS compliance_snapshots (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        snapshot_json TEXT NOT NULL,
        summary_compliant INTEGER NOT NULL,
        summary_warning INTEGER NOT NULL,
        summary_violation INTEGER NOT NULL
    );",
];

/// SQLite-backed state store for cfgd.
pub struct StateStore {
    pub(in crate::state) conn: Connection,
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
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

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
        // Use EXCLUSIVE transaction to serialize concurrent migration attempts
        // (e.g. parallel cargo test processes sharing the same state DB).
        self.conn
            .execute_batch("BEGIN EXCLUSIVE")
            .map_err(|e| StateError::MigrationFailed {
                message: format!("failed to acquire migration lock: {e}"),
            })?;

        let current_version = self.schema_version();

        for (i, migration) in MIGRATIONS.iter().enumerate() {
            if i >= current_version {
                self.conn.execute_batch(migration).map_err(|e| {
                    if let Err(rb) = self.conn.execute_batch("ROLLBACK") {
                        tracing::error!("rollback after migration {i} failure also failed: {rb}");
                    }
                    StateError::MigrationFailed {
                        message: format!("migration {}: {}", i, e),
                    }
                })?;
                // Set version automatically — no hardcoded UPDATE in migration SQL
                let new_version = (i + 1) as i64;
                self.conn
                    .execute(
                        "UPDATE schema_version SET version = ?1",
                        rusqlite::params![new_version],
                    )
                    .map_err(|e| {
                        if let Err(rb) = self.conn.execute_batch("ROLLBACK") {
                            tracing::error!(
                                "rollback after schema_version update failure also failed: {rb}"
                            );
                        }
                        StateError::MigrationFailed {
                            message: format!("migration {}: failed to update version: {}", i, e),
                        }
                    })?;
            }
        }

        self.conn
            .execute_batch("COMMIT")
            .map_err(|e| StateError::MigrationFailed {
                message: format!("failed to commit migrations: {e}"),
            })?;

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
}

/// Compute SHA256 hash of a serializable plan for deduplication.
pub fn plan_hash(data: &str) -> String {
    crate::sha256_hex(data.as_bytes())
}

pub fn default_state_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CFGD_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    // Resolve home through the same policy config discovery uses (HOME on Unix,
    // USERPROFILE/HOME on Windows). `directories` would otherwise fall back to
    // the passwd database when HOME is unset, resolving a home that config
    // discovery cannot — the two subsystems must agree so an unset HOME fails
    // uniformly instead of creating an orphan state.db beside a config error.
    if crate::home_dir_var().is_none() {
        return Err(StateError::DirectoryNotWritable {
            path: PathBuf::from("~/.local/share/cfgd"),
        }
        .into());
    }
    let base = directories::BaseDirs::new().ok_or_else(|| StateError::DirectoryNotWritable {
        path: PathBuf::from("~/.local/share/cfgd"),
    })?;
    Ok(base.data_local_dir().join("cfgd"))
}

#[cfg(test)]
mod tests;

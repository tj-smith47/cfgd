use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::Scope;
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
    PENDING_CONFIG_FILENAME, clear_pending_server_config, load_pending_server_config,
    save_pending_server_config,
};
pub use types::{
    ApplyRecord, ApplyStatus, ComplianceHistoryRow, ConfigSourceRecord, DriftEvent,
    FileBackupRecord, JournalEntry, ManagedResource, ModuleFileRecord, ModuleStateRecord,
    PendingDecision, SourceConfigHash, SourceConflictRecord,
};

/// Canonical state DB filename. The single source of truth so the default and
/// explicit-`--state-dir`/`CFGD_STATE_DIR` paths can never diverge onto sibling
/// files (the divergence silently read an empty DB and reported "no history").
pub const STATE_DB_FILENAME: &str = "state.db";

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
        FOREIGN KEY (source_id) REFERENCES config_sources(id) ON DELETE CASCADE,
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
    // Migration 5: persist the scripted uninstall command alongside each package
    // tracking row, so a custom/scripted manager's packages can still be pruned
    // after its definition leaves the config (the script vanishes with it).
    "ALTER TABLE managed_resources ADD COLUMN uninstall_cmd TEXT;",
    // Migration 6: source_applies.source_id gains ON DELETE CASCADE. An apply
    // records a source_applies row referencing config_sources(id); the bare
    // DELETE on config_sources then violated the foreign key (foreign_keys=ON),
    // so `source remove`/`source replace` failed after any apply — and the
    // cfgd.yaml mutation had already landed, leaving config and state out of
    // sync. SQLite cannot ALTER a constraint, so rebuild the child table (no
    // other table references source_applies, so the drop/rename is safe inside
    // the migration transaction; copied rows keep valid source_id references).
    "CREATE TABLE source_applies_new (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        source_id INTEGER NOT NULL,
        apply_id INTEGER NOT NULL,
        source_commit TEXT NOT NULL,
        FOREIGN KEY (source_id) REFERENCES config_sources(id) ON DELETE CASCADE,
        FOREIGN KEY (apply_id) REFERENCES applies(id)
    );
    INSERT INTO source_applies_new (id, source_id, apply_id, source_commit)
        SELECT id, source_id, apply_id, source_commit FROM source_applies;
    DROP TABLE source_applies;
    ALTER TABLE source_applies_new RENAME TO source_applies;",
    // Migration 7: daemon-snapshot drift resolution. `resolved_by` is a foreign
    // key into applies(id), so it cannot mark a row resolved when no apply ran
    // (the no-drift / healed-complement reconcile snapshots). A nullable
    // timestamp column — mirroring pending_decisions.resolved_at — carries that
    // marker without a synthetic apply row. "resolved" is now
    // `resolved_by IS NOT NULL OR resolved_at IS NOT NULL`.
    "ALTER TABLE drift_events ADD COLUMN resolved_at TEXT;",
    // Migration 8: durably record whether a file existed at backup time. A
    // pre-action backup of a CREATE action records existed=0 (an absent
    // marker); rollback then removes such files instead of restoring stale
    // content. DEFAULT 1 keeps every legacy row at today's content-restore
    // behavior.
    "ALTER TABLE file_backups ADD COLUMN existed INTEGER NOT NULL DEFAULT 1;",
];

/// SQLite-backed state store for cfgd.
pub struct StateStore {
    pub(in crate::state) conn: Connection,
}

impl StateStore {
    /// Open or create a state store at the default location.
    /// Uses `~/.local/state/cfgd/state.db`.
    pub fn open_default() -> Result<Self> {
        Self::open_in_dir(&default_state_dir()?)
    }

    /// Open or create the canonical [`STATE_DB_FILENAME`] DB inside `dir`,
    /// creating the directory if needed. Every state-dir override resolves the
    /// DB through here so it cannot drift from the default-location filename.
    pub fn open_in_dir(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|_| StateError::DirectoryNotWritable {
            path: dir.to_path_buf(),
        })?;
        // create_dir_all is a no-op when the dir already exists, so an existing
        // read-only state dir would otherwise reach Connection::open and fail with
        // a generic "state database error" that names no path. Probe real write
        // access first so a read-only dir surfaces the typed DirectoryNotWritable.
        if let crate::DirWritable::NotWritable = crate::probe_dir_writable(dir) {
            return Err(StateError::DirectoryNotWritable {
                path: dir.to_path_buf(),
            }
            .into());
        }
        Self::open(&dir.join(STATE_DB_FILENAME))
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

/// Default per-user state directory (SQLite state DB, backups).
///
/// Resolution order:
/// 1. `CFGD_STATE_DIR` when set (verbatim) — back-compat short-circuit, wins
///    over everything.
/// 2. the platform-native state location with a `cfgd` segment, honoring
///    `XDG_STATE_HOME`:
///    - Linux: `$XDG_STATE_HOME/cfgd` (default `~/.local/state/cfgd`)
///    - macOS/Windows: `BaseDirs::state_dir()` is `None`, so fall back to the
///      data-local dir nested under a `state/` segment
///      (macOS `~/Library/Application Support/cfgd/state`,
///      Windows `%LOCALAPPDATA%\cfgd\state`).
///
/// Resolves home through the same policy config discovery uses (HOME on Unix,
/// USERPROFILE/HOME on Windows) so an unset HOME fails uniformly instead of
/// creating an orphan state.db beside a config error.
pub fn default_state_dir() -> Result<PathBuf> {
    default_state_dir_for(Scope::User)
}

/// Scope-aware state directory.
///
/// Precedence (highest first): `CFGD_STATE_DIR` (verbatim), systemd's
/// `$STATE_DIRECTORY`, then the scope default. [`Scope::User`] is the frozen
/// resolution documented on [`default_state_dir`]. [`Scope::System`] is the
/// absolute machine-wide state root (Linux `/var/lib/cfgd`, macOS
/// `/Library/Application Support/cfgd/state`, Windows `%ProgramData%\cfgd\state`)
/// and consults no home directory, so it never errors. Pure path logic.
pub fn default_state_dir_for(scope: Scope) -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CFGD_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = crate::systemd_dir("STATE_DIRECTORY") {
        return Ok(dir);
    }
    if scope.is_system() {
        return Ok(system_state_dir());
    }
    // `directories` would otherwise fall back to the passwd database when HOME
    // is unset, resolving a home that config discovery cannot — the two
    // subsystems must agree.
    if crate::home_dir_var().is_none() {
        return Err(StateError::DirectoryNotWritable {
            path: PathBuf::from("~/.local/state/cfgd"),
        }
        .into());
    }
    let base = directories::BaseDirs::new().ok_or_else(|| StateError::DirectoryNotWritable {
        path: PathBuf::from("~/.local/state/cfgd"),
    })?;
    Ok(match base.state_dir() {
        Some(state) => state.join("cfgd"),
        None => base.data_local_dir().join("cfgd").join("state"),
    })
}

/// The machine-wide state root: Linux `/var/lib/cfgd`, macOS
/// `/Library/Application Support/cfgd/state`, Windows `%ProgramData%\cfgd\state`.
/// Absolute on every platform — never consults a home directory.
fn system_state_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/var/lib/cfgd")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/cfgd/state")
    }
    #[cfg(windows)]
    {
        crate::program_data_dir().join("cfgd").join("state")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        PathBuf::from("/var/lib/cfgd")
    }
}

/// Crash-safe move of the state DB (and any unfolded WAL/SHM sidecars) from a
/// legacy directory to a new one. Returns `true` when a DB was migrated,
/// `false` when there was nothing to do.
///
/// Never clobbers: when a DB already exists at `new_dir` the legacy DB is left
/// in place and `false` is returned, so a partial earlier migration (or a fresh
/// install that already wrote to the new location) is never overwritten.
///
/// Before moving, a best-effort `wal_checkpoint(TRUNCATE)` is run against a bare
/// connection (no schema migrations) to fold the WAL into the main DB file. When
/// the checkpoint succeeds the folded sidecars are removed; when it fails (a
/// locked or degraded DB) the sidecars — snapshotted before any connection opens,
/// since opening one would let SQLite delete them — are recreated beside the
/// moved DB so no committed-but-unfolded data is lost.
pub fn migrate_state_db(legacy_dir: &Path, new_dir: &Path) -> Result<bool> {
    let legacy_db = legacy_dir.join(STATE_DB_FILENAME);
    let new_db = new_dir.join(STATE_DB_FILENAME);
    if !legacy_db.exists() {
        return Ok(false);
    }
    // A DB already at the destination is authoritative — never overwrite it.
    if new_db.exists() {
        return Ok(false);
    }

    // Snapshot the sidecar bytes BEFORE opening any connection: opening (and
    // dropping) a connection to the DB makes SQLite delete what it judges to be
    // stale `-wal`/`-shm` files, which would destroy the recovery copies before
    // the checkpoint-failure branch below could carry them across. Read them up
    // front so a degraded/locked DB never loses committed-but-unfolded data.
    let sidecars: Vec<(&str, Vec<u8>)> = ["-wal", "-shm"]
        .into_iter()
        .filter_map(|suffix| {
            let path = legacy_dir.join(format!("{STATE_DB_FILENAME}{suffix}"));
            std::fs::read(&path).ok().map(|bytes| (suffix, bytes))
        })
        .collect();

    // Fold the WAL into the main DB so the moved file is self-contained. A bare
    // connection is used (not StateStore::open) so this never runs schema
    // migrations on an old DB mid-move. Failure (locked/degraded DB) is captured
    // and recovered from below by writing the snapshotted sidecars instead.
    let checkpointed = match Connection::open(&legacy_db) {
        Ok(conn) => conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .is_ok(),
        Err(_) => false,
    };

    std::fs::create_dir_all(new_dir).map_err(|e| StateError::FilesystemIo {
        path: new_dir.to_path_buf(),
        source: e,
    })?;
    crate::move_file(&legacy_db, &new_db).map_err(|e| StateError::FilesystemIo {
        path: new_db.clone(),
        source: e,
    })?;

    // When the checkpoint succeeded the WAL is folded and truncated, so the moved
    // DB is self-contained and the sidecars hold nothing extra — drop any that
    // survive at legacy. When it failed, recreate the snapshotted sidecars beside
    // the moved DB so committed-but-unfolded data is preserved.
    if checkpointed {
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(legacy_dir.join(format!("{STATE_DB_FILENAME}{suffix}")));
        }
    } else {
        for (suffix, bytes) in &sidecars {
            let new_sidecar = new_dir.join(format!("{STATE_DB_FILENAME}{suffix}"));
            let _ = std::fs::write(&new_sidecar, bytes);
            let _ = std::fs::remove_file(legacy_dir.join(format!("{STATE_DB_FILENAME}{suffix}")));
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests;

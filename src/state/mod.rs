#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
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
        let conn = Connection::open(path).map_err(|e| StateError::Database(e.to_string()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| StateError::Database(e.to_string()))?;

        let mut store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    /// Create an in-memory state store (for testing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| StateError::Database(e.to_string()))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| StateError::Database(e.to_string()))?;

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
        let timestamp = now_iso8601();
        self.conn
            .execute(
                "INSERT INTO applies (timestamp, profile, plan_hash, status, summary) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![timestamp, profile, plan_hash, status.as_str(), summary],
            )
            .map_err(|e| StateError::Database(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Record a drift event.
    pub fn record_drift(
        &self,
        resource_type: &str,
        resource_id: &str,
        expected: Option<&str>,
        actual: Option<&str>,
    ) -> Result<i64> {
        let timestamp = now_iso8601();
        self.conn
            .execute(
                "INSERT INTO drift_events (timestamp, resource_type, resource_id, expected, actual) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![timestamp, resource_type, resource_id, expected, actual],
            )
            .map_err(|e| StateError::Database(e.to_string()))?;
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
            .map_err(|e| StateError::Database(e.to_string()))?;
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
            .map_err(|e| StateError::Database(e.to_string()))?;
        Ok(())
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
            .map_err(|e| StateError::Database(e.to_string()))?;

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
            })
            .map_err(|e| StateError::Database(e.to_string()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StateError::Database(e.to_string()))?;

        Ok(records)
    }

    /// Get all managed resources.
    pub fn managed_resources(&self) -> Result<Vec<ManagedResource>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT resource_type, resource_id, source, last_hash, last_applied FROM managed_resources ORDER BY resource_type, resource_id",
            )
            .map_err(|e| StateError::Database(e.to_string()))?;

        let resources = stmt
            .query_map([], |row| {
                Ok(ManagedResource {
                    resource_type: row.get(0)?,
                    resource_id: row.get(1)?,
                    source: row.get(2)?,
                    last_hash: row.get(3)?,
                    last_applied: row.get(4)?,
                })
            })
            .map_err(|e| StateError::Database(e.to_string()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StateError::Database(e.to_string()))?;

        Ok(resources)
    }

    /// Get unresolved drift events.
    pub fn unresolved_drift(&self) -> Result<Vec<DriftEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, timestamp, resource_type, resource_id, expected, actual, resolved_by FROM drift_events WHERE resolved_by IS NULL ORDER BY timestamp DESC",
            )
            .map_err(|e| StateError::Database(e.to_string()))?;

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
                })
            })
            .map_err(|e| StateError::Database(e.to_string()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StateError::Database(e.to_string()))?;

        Ok(events)
    }
}

/// Compute SHA256 hash of a serializable plan for deduplication.
pub fn plan_hash(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn default_state_dir() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| StateError::DirectoryNotWritable {
        path: PathBuf::from("~/.local/share/cfgd"),
    })?;
    Ok(base.data_local_dir().join("cfgd"))
}

pub fn now_iso8601() -> String {
    // Use a simple timestamp without external chrono dependency
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Format as ISO 8601 UTC manually
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Approximate date calculation (good enough for logging, not for calendar apps)
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
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
            .record_drift("file", "/home/user/.zshrc", Some("abc"), Some("def"))
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
            .record_drift("file", "/test", Some("a"), Some("b"))
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
        let ts = now_iso8601();
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
}

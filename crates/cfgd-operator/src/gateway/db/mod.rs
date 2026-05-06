use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex as PlMutex;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};

use super::errors::GatewayError;
use crate::metrics::Metrics;

mod bootstrap;
mod cleanup;
mod credentials;
mod devices;
mod enrollment;
mod events;
mod types;
mod user_keys;

// _tx free fns are re-exported only when called from outside `gateway::db` (api, web).
// Sub-files reach each other directly via `super::<sub>::*`; internal-only _tx fns stay
// at their definition site.
pub(in crate::gateway) use bootstrap::validate_and_consume_bootstrap_token_tx;
pub(in crate::gateway) use credentials::create_device_credential_tx;
pub(in crate::gateway) use devices::{
    get_device_tx, get_fleet_status_tx, list_devices_paginated_tx, register_device_tx,
    update_checkin_tx,
};
pub(in crate::gateway) use events::{
    list_checkin_events_tx, list_drift_events_tx, record_checkin_tx, record_drift_event_tx,
};
pub use types::{
    BootstrapToken, CheckinEvent, Device, DeviceCredential, DeviceStatus, DriftEvent,
    EnrollmentChallenge, FleetEvent, UserPublicKey,
};

pub(super) type ReaderPool = Pool<SqliteConnectionManager>;
pub(super) type ReaderConn = PooledConnection<SqliteConnectionManager>;

const DEFAULT_READER_POOL_SIZE: u32 = 16;
const POOL_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SQLITE_MMAP_SIZE: i64 = 268_435_456; // 256 MiB

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS devices (
        id TEXT PRIMARY KEY,
        hostname TEXT NOT NULL,
        os TEXT NOT NULL,
        arch TEXT NOT NULL,
        last_checkin TEXT NOT NULL,
        config_hash TEXT NOT NULL,
        status TEXT NOT NULL,
        desired_config TEXT
    );
    CREATE TABLE IF NOT EXISTS drift_events (
        id TEXT PRIMARY KEY,
        device_id TEXT NOT NULL,
        timestamp TEXT NOT NULL,
        details TEXT NOT NULL,
        FOREIGN KEY (device_id) REFERENCES devices(id)
    );
    CREATE TABLE IF NOT EXISTS checkin_events (
        id TEXT PRIMARY KEY,
        device_id TEXT NOT NULL,
        timestamp TEXT NOT NULL,
        config_hash TEXT NOT NULL,
        config_changed INTEGER NOT NULL DEFAULT 0,
        FOREIGN KEY (device_id) REFERENCES devices(id)
    );
    CREATE TABLE IF NOT EXISTS bootstrap_tokens (
        id TEXT PRIMARY KEY,
        token_hash TEXT NOT NULL UNIQUE,
        username TEXT NOT NULL,
        team TEXT,
        created_at TEXT NOT NULL,
        expires_at TEXT NOT NULL,
        used_at TEXT,
        used_by_device TEXT
    );
    CREATE TABLE IF NOT EXISTS device_credentials (
        device_id TEXT PRIMARY KEY,
        api_key_hash TEXT NOT NULL UNIQUE,
        username TEXT NOT NULL,
        team TEXT,
        created_at TEXT NOT NULL,
        last_used TEXT,
        revoked INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS user_public_keys (
        id TEXT PRIMARY KEY,
        username TEXT NOT NULL,
        key_type TEXT NOT NULL,
        public_key TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        label TEXT,
        created_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS enrollment_challenges (
        id TEXT PRIMARY KEY,
        username TEXT NOT NULL,
        device_id TEXT NOT NULL,
        hostname TEXT NOT NULL,
        os TEXT NOT NULL,
        arch TEXT NOT NULL,
        nonce TEXT NOT NULL,
        created_at TEXT NOT NULL,
        expires_at TEXT NOT NULL,
        consumed INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_drift_events_device_id ON drift_events(device_id);
    CREATE INDEX IF NOT EXISTS idx_drift_events_timestamp ON drift_events(timestamp);
    CREATE INDEX IF NOT EXISTS idx_checkin_events_device_id ON checkin_events(device_id);
    CREATE INDEX IF NOT EXISTS idx_checkin_events_timestamp ON checkin_events(timestamp);
    CREATE INDEX IF NOT EXISTS idx_user_public_keys_username ON user_public_keys(username);
    CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
    INSERT INTO schema_version (version) VALUES (0);",
    // Migration 1: add desired_config column for databases created prior to this schema.
    "ALTER TABLE devices ADD COLUMN desired_config TEXT;",
    // Migration 2: add compliance_summary column to store last-reported compliance data.
    "ALTER TABLE devices ADD COLUMN compliance_summary TEXT;",
];

// --- Connection init / pool helpers ---

/// Applied to every reader connection.
fn init_reader(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA foreign_keys=ON;
         PRAGMA temp_store=MEMORY;
         PRAGMA query_only=ON;",
    )?;
    conn.pragma_update(None, "mmap_size", SQLITE_MMAP_SIZE)?;
    Ok(())
}

/// Applied to the dedicated writer connection.
fn init_writer(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA foreign_keys=ON;
         PRAGMA temp_store=MEMORY;",
    )?;
    conn.pragma_update(None, "mmap_size", SQLITE_MMAP_SIZE)?;
    Ok(())
}

#[derive(Debug)]
struct ReaderCustomizer;

impl r2d2::CustomizeConnection<Connection, rusqlite::Error> for ReaderCustomizer {
    fn on_acquire(&self, conn: &mut Connection) -> Result<(), rusqlite::Error> {
        init_reader(conn)
    }
    fn on_release(&self, conn: Connection) {
        // Fire PRAGMA optimize on release; ignore errors (best-effort hint).
        let _ = conn.execute_batch("PRAGMA optimize;");
    }
}

fn reader_pool_size_from_env() -> u32 {
    std::env::var("CFGD_GATEWAY_DB_READ_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n: &u32| n >= 1)
        .unwrap_or(DEFAULT_READER_POOL_SIZE)
}

async fn spawn_blocking_db<F, R>(f: F) -> Result<R, GatewayError>
where
    F: FnOnce() -> Result<R, GatewayError> + Send + 'static,
    R: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "db task panicked or was cancelled");
            Err(GatewayError::DatabaseTaskPanicked)
        }
    }
}

fn map_pool_err(e: r2d2::Error) -> GatewayError {
    GatewayError::PoolExhausted(e.to_string())
}

fn acquire_reader(
    pool: &ReaderPool,
    metrics: &Option<Metrics>,
) -> Result<ReaderConn, GatewayError> {
    let t0 = Instant::now();
    let conn = pool.get().map_err(map_pool_err)?;
    if let Some(m) = metrics {
        m.db_pool_wait_seconds.observe(t0.elapsed().as_secs_f64());
    }
    Ok(conn)
}

fn acquire_writer<'a>(
    writer: &'a PlMutex<Connection>,
    metrics: &Option<Metrics>,
) -> parking_lot::MutexGuard<'a, Connection> {
    let t0 = Instant::now();
    let guard = writer.lock();
    if let Some(m) = metrics {
        m.db_writer_wait_seconds.observe(t0.elapsed().as_secs_f64());
    }
    guard
}

#[derive(Clone)]
pub struct ServerDb {
    pub(in crate::gateway::db) readers: Arc<ReaderPool>,
    pub(in crate::gateway::db) writer: Arc<PlMutex<Connection>>,
    pub(in crate::gateway::db) metrics: Option<Metrics>,
}

impl ServerDb {
    pub fn open(path: &str) -> Result<Self, GatewayError> {
        Self::open_with_config(path, reader_pool_size_from_env(), POOL_CONNECTION_TIMEOUT)
    }

    /// Open with explicit reader pool size and connection timeout.
    /// Exposed for tests that need to trigger pool exhaustion deterministically
    /// or exercise concurrency shapes that depend on pool sizing.
    ///
    /// Ordering: the writer connection is established and migrations run
    /// BEFORE the reader pool is built. This avoids a race where a short
    /// `connection_timeout` (e.g. 50 ms for pool-exhaustion tests) would cause
    /// the initial reader pool connection to time out while SQLite was still
    /// creating the database file.
    pub fn open_with_config(
        path: &str,
        reader_pool_size: u32,
        connection_timeout: std::time::Duration,
    ) -> Result<Self, GatewayError> {
        let mut writer = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(GatewayError::Database)?;
        init_writer(&mut writer).map_err(GatewayError::Database)?;
        run_migrations_on_conn(&mut writer)?;

        let manager = SqliteConnectionManager::file(path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE);
        let min_idle = if reader_pool_size >= 2 { Some(2) } else { None };
        let readers = Pool::builder()
            .max_size(reader_pool_size)
            .min_idle(min_idle)
            .connection_timeout(connection_timeout)
            .connection_customizer(Box::new(ReaderCustomizer))
            .build(manager)
            .map_err(|e| GatewayError::Internal(format!("db pool build failed: {e}")))?;

        Ok(Self {
            readers: Arc::new(readers),
            writer: Arc::new(PlMutex::new(writer)),
            metrics: None,
        })
    }

    pub fn with_metrics(mut self, metrics: Option<Metrics>) -> Self {
        self.metrics = metrics;
        self
    }

    pub fn readers_handle(&self) -> Arc<ReaderPool> {
        self.readers.clone()
    }

    pub async fn with_read_tx<F, R>(&self, f: F) -> Result<R, GatewayError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<R, GatewayError> + Send + 'static,
        R: Send + 'static,
    {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let mut conn = acquire_reader(&pool, &metrics)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let out = f(&tx)?;
            drop(tx);
            Ok(out)
        })
        .await
    }

    pub async fn with_write_tx<F, R>(&self, f: F) -> Result<R, GatewayError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<R, GatewayError> + Send + 'static,
        R: Send + 'static,
    {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let mut conn = acquire_writer(&writer, &metrics);
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let out = f(&tx)?;
            tx.commit()?;
            Ok(out)
        })
        .await
    }

    #[cfg(test)]
    pub(super) fn schema_version_blocking(&self) -> usize {
        let conn = acquire_writer(&self.writer, &self.metrics);
        schema_version_inner(&conn).unwrap_or(0)
    }
}

fn schema_version_inner(conn: &Connection) -> std::result::Result<usize, rusqlite::Error> {
    conn.query_row("SELECT version FROM schema_version", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|v| v as usize)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(0),
        rusqlite::Error::SqliteFailure(_, Some(ref msg)) if msg.contains("no such table") => Ok(0),
        other => Err(other),
    })
}

/// Run schema migrations against a writer connection. Called at open time
/// before the reader pool is built so the schema is in place for readers.
fn run_migrations_on_conn(conn: &mut Connection) -> Result<(), GatewayError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;

    let current_version = match schema_version_inner(conn) {
        Ok(v) => v,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e.into());
        }
    };

    for (i, migration) in MIGRATIONS.iter().enumerate() {
        if i >= current_version {
            if let Err(e) = conn.execute_batch(migration) {
                let is_dup_column = matches!(
                    &e,
                    rusqlite::Error::SqliteFailure(_, Some(msg))
                        if msg.contains("duplicate column name")
                );
                if !is_dup_column {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e.into());
                }
            }
            let new_version = (i + 1) as i64;
            if let Err(e) = conn.execute(
                "UPDATE schema_version SET version = ?1",
                params![new_version],
            ) {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e.into());
            }
        }
    }

    if let Err(e) = conn.execute_batch("COMMIT") {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

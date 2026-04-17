use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex as PlMutex;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::errors::GatewayError;
use crate::metrics::Metrics;

pub(super) type ReaderPool = Pool<SqliteConnectionManager>;
pub(super) type ReaderConn = PooledConnection<SqliteConnectionManager>;

const DEFAULT_READER_POOL_SIZE: u32 = 16;
const POOL_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SQLITE_MMAP_SIZE: i64 = 268_435_456; // 256 MiB

/// Device status as a proper enum with well-defined states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceStatus {
    Healthy,
    Drifted,
    Offline,
    PendingReconcile,
}

impl DeviceStatus {
    pub fn as_str(&self) -> &str {
        match self {
            DeviceStatus::Healthy => "healthy",
            DeviceStatus::Drifted => "drifted",
            DeviceStatus::Offline => "offline",
            DeviceStatus::PendingReconcile => "pending-reconcile",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "healthy" => DeviceStatus::Healthy,
            "drifted" => DeviceStatus::Drifted,
            "offline" => DeviceStatus::Offline,
            "pending-reconcile" => DeviceStatus::PendingReconcile,
            _ => DeviceStatus::Offline,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub last_checkin: String,
    pub config_hash: String,
    pub status: DeviceStatus,
    pub desired_config: Option<serde_json::Value>,
    pub compliance_summary: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftEvent {
    pub id: String,
    pub device_id: String,
    pub timestamp: String,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinEvent {
    pub id: String,
    pub device_id: String,
    pub timestamp: String,
    pub config_hash: String,
    pub config_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetEvent {
    pub timestamp: String,
    pub device_id: String,
    pub event_type: String,
    pub summary: String,
}

/// A bootstrap token record — admin-created, one-time use for device enrollment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapToken {
    pub id: String,
    pub username: String,
    pub team: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    pub used_at: Option<String>,
    pub used_by_device: Option<String>,
}

/// A device credential record — permanent API key for an enrolled device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCredential {
    pub device_id: String,
    pub username: String,
    pub team: Option<String>,
    pub created_at: String,
    pub last_used: Option<String>,
    pub revoked: bool,
}

/// A user's public key for key-based enrollment verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPublicKey {
    pub id: String,
    pub username: String,
    pub key_type: String, // "ssh" or "gpg"
    pub public_key: String,
    pub fingerprint: String,
    pub label: Option<String>,
    pub created_at: String,
}

/// A short-lived enrollment challenge for key-based enrollment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentChallenge {
    pub id: String,
    pub username: String,
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub nonce: String,
    pub created_at: String,
    pub expires_at: String,
}

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

// --- Row mappers ---

fn map_device_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    let config_str: Option<String> = row.get(7)?;
    let desired_config = config_str.and_then(|s| serde_json::from_str(&s).ok());
    let compliance_str: Option<String> = row.get(8)?;
    let compliance_summary = compliance_str.and_then(|s| serde_json::from_str(&s).ok());
    let status_str: String = row.get(6)?;
    Ok(Device {
        id: row.get(0)?,
        hostname: row.get(1)?,
        os: row.get(2)?,
        arch: row.get(3)?,
        last_checkin: row.get(4)?,
        config_hash: row.get(5)?,
        status: DeviceStatus::parse(&status_str),
        desired_config,
        compliance_summary,
    })
}

fn map_drift_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DriftEvent> {
    Ok(DriftEvent {
        id: row.get(0)?,
        device_id: row.get(1)?,
        timestamp: row.get(2)?,
        details: row.get(3)?,
    })
}

fn map_checkin_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckinEvent> {
    let changed: i32 = row.get(4)?;
    Ok(CheckinEvent {
        id: row.get(0)?,
        device_id: row.get(1)?,
        timestamp: row.get(2)?,
        config_hash: row.get(3)?,
        config_changed: changed != 0,
    })
}

fn map_fleet_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FleetEvent> {
    Ok(FleetEvent {
        timestamp: row.get(0)?,
        device_id: row.get(1)?,
        event_type: row.get(2)?,
        summary: row.get(3)?,
    })
}

fn map_bootstrap_token_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BootstrapToken> {
    Ok(BootstrapToken {
        id: row.get(0)?,
        username: row.get(1)?,
        team: row.get(2)?,
        created_at: row.get(3)?,
        expires_at: row.get(4)?,
        used_at: row.get(5)?,
        used_by_device: row.get(6)?,
    })
}

fn map_device_credential_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceCredential> {
    let revoked: i32 = row.get(5)?;
    Ok(DeviceCredential {
        device_id: row.get(0)?,
        username: row.get(1)?,
        team: row.get(2)?,
        created_at: row.get(3)?,
        last_used: row.get(4)?,
        revoked: revoked != 0,
    })
}

fn map_user_public_key_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserPublicKey> {
    Ok(UserPublicKey {
        id: row.get(0)?,
        username: row.get(1)?,
        key_type: row.get(2)?,
        public_key: row.get(3)?,
        fingerprint: row.get(4)?,
        label: row.get(5)?,
        created_at: row.get(6)?,
    })
}

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
    readers: Arc<ReaderPool>,
    writer: Arc<PlMutex<Connection>>,
    metrics: Option<Metrics>,
}

impl ServerDb {
    pub fn open(path: &str) -> Result<Self, GatewayError> {
        let manager = SqliteConnectionManager::file(path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE);
        let readers = Pool::builder()
            .max_size(reader_pool_size_from_env())
            .min_idle(Some(2))
            .connection_timeout(POOL_CONNECTION_TIMEOUT)
            .connection_customizer(Box::new(ReaderCustomizer))
            .build(manager)
            .map_err(|e| GatewayError::Internal(format!("db pool build failed: {e}")))?;

        let mut writer = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(GatewayError::Database)?;
        init_writer(&mut writer).map_err(GatewayError::Database)?;

        let db = Self {
            readers: Arc::new(readers),
            writer: Arc::new(PlMutex::new(writer)),
            metrics: None,
        };
        db.run_migrations_blocking()?;
        Ok(db)
    }

    pub fn with_metrics(mut self, metrics: Option<Metrics>) -> Self {
        self.metrics = metrics;
        self
    }

    pub fn readers_handle(&self) -> Arc<ReaderPool> {
        self.readers.clone()
    }

    fn run_migrations_blocking(&self) -> Result<(), GatewayError> {
        let conn = self.writer.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;

        let current_version = match schema_version_inner(&conn) {
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

    // --- async public methods: reads go through the reader pool ---

    pub async fn get_device(&self, id: &str) -> Result<Device, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        let id = id.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            get_device_tx(&conn, &id)
        })
        .await
    }

    /// Convenience wrapper over [`Self::list_devices_paginated`]; used by tests
    /// and callers that want an unbounded list.
    pub async fn list_devices(&self) -> Result<Vec<Device>, GatewayError> {
        self.list_devices_paginated(1000, 0).await
    }

    pub async fn list_devices_paginated(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Device>, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            list_devices_paginated_tx(&conn, limit, offset)
        })
        .await
    }

    pub async fn list_drift_events(
        &self,
        device_id: &str,
    ) -> Result<Vec<DriftEvent>, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        let device_id = device_id.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            list_drift_events_tx(&conn, &device_id)
        })
        .await
    }

    pub async fn list_checkin_events(
        &self,
        device_id: &str,
    ) -> Result<Vec<CheckinEvent>, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        let device_id = device_id.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            list_checkin_events_tx(&conn, &device_id)
        })
        .await
    }

    pub async fn list_fleet_events(&self, limit: u32) -> Result<Vec<FleetEvent>, GatewayError> {
        self.list_fleet_events_paginated(limit, 0).await
    }

    pub async fn list_fleet_events_paginated(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<FleetEvent>, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            list_fleet_events_paginated_tx(&conn, limit, offset)
        })
        .await
    }

    pub async fn list_bootstrap_tokens(&self) -> Result<Vec<BootstrapToken>, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            list_bootstrap_tokens_tx(&conn)
        })
        .await
    }

    pub async fn list_user_public_keys(
        &self,
        username: &str,
    ) -> Result<Vec<UserPublicKey>, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        let username = username.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            list_user_public_keys_tx(&conn, &username)
        })
        .await
    }

    pub async fn get_fleet_status(&self) -> Result<super::fleet::FleetStatus, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            get_fleet_status_tx(&conn)
        })
        .await
    }

    // --- async public methods: writes acquire the writer ---

    pub async fn register_device(
        &self,
        id: &str,
        hostname: &str,
        os: &str,
        arch: &str,
        config_hash: &str,
        compliance_summary: Option<&serde_json::Value>,
    ) -> Result<Device, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let id = id.to_string();
        let hostname = hostname.to_string();
        let os = os.to_string();
        let arch = arch.to_string();
        let config_hash = config_hash.to_string();
        let compliance = compliance_summary.cloned();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            register_device_tx(
                &conn,
                &id,
                &hostname,
                &os,
                &arch,
                &config_hash,
                compliance.as_ref(),
            )
        })
        .await
    }

    pub async fn update_checkin(
        &self,
        id: &str,
        config_hash: &str,
        compliance_summary: Option<&serde_json::Value>,
    ) -> Result<(), GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let id = id.to_string();
        let config_hash = config_hash.to_string();
        let compliance = compliance_summary.cloned();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            update_checkin_tx(&conn, &id, &config_hash, compliance.as_ref())
        })
        .await
    }

    pub async fn set_device_config(
        &self,
        id: &str,
        config: &serde_json::Value,
    ) -> Result<(), GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let id = id.to_string();
        let config = config.clone();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            set_device_config_tx(&conn, &id, &config)
        })
        .await
    }

    pub async fn record_drift_event(
        &self,
        device_id: &str,
        details: &str,
    ) -> Result<DriftEvent, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let device_id = device_id.to_string();
        let details = details.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            record_drift_event_tx(&conn, &device_id, &details)
        })
        .await
    }

    pub async fn record_checkin(
        &self,
        device_id: &str,
        config_hash: &str,
        config_changed: bool,
    ) -> Result<CheckinEvent, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let device_id = device_id.to_string();
        let config_hash = config_hash.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            record_checkin_tx(&conn, &device_id, &config_hash, config_changed)
        })
        .await
    }

    pub async fn set_force_reconcile(&self, device_id: &str) -> Result<(), GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let device_id = device_id.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            set_force_reconcile_tx(&conn, &device_id)
        })
        .await
    }

    pub async fn create_bootstrap_token(
        &self,
        token_hash: &str,
        username: &str,
        team: Option<&str>,
        expires_at: &str,
    ) -> Result<BootstrapToken, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let token_hash = token_hash.to_string();
        let username = username.to_string();
        let team = team.map(|s| s.to_string());
        let expires_at = expires_at.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            create_bootstrap_token_tx(&conn, &token_hash, &username, team.as_deref(), &expires_at)
        })
        .await
    }

    pub async fn validate_and_consume_bootstrap_token(
        &self,
        token_hash: &str,
        device_id: &str,
    ) -> Result<BootstrapToken, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let token_hash = token_hash.to_string();
        let device_id = device_id.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            validate_and_consume_bootstrap_token_tx(&conn, &token_hash, &device_id)
        })
        .await
    }

    pub async fn delete_bootstrap_token(&self, id: &str) -> Result<(), GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let id = id.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            delete_bootstrap_token_tx(&conn, &id)
        })
        .await
    }

    pub async fn create_device_credential(
        &self,
        device_id: &str,
        api_key_hash: &str,
        username: &str,
        team: Option<&str>,
    ) -> Result<DeviceCredential, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let device_id = device_id.to_string();
        let api_key_hash = api_key_hash.to_string();
        let username = username.to_string();
        let team = team.map(|s| s.to_string());
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            create_device_credential_tx(
                &conn,
                &device_id,
                &api_key_hash,
                &username,
                team.as_deref(),
            )
        })
        .await
    }

    /// Validate a device API key. Goes through the writer because this path also
    /// updates `last_used` on success.
    pub async fn validate_device_credential(
        &self,
        api_key_hash: &str,
    ) -> Result<DeviceCredential, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let api_key_hash = api_key_hash.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            validate_device_credential_tx(&conn, &api_key_hash)
        })
        .await
    }

    pub async fn revoke_device_credential(&self, device_id: &str) -> Result<(), GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let device_id = device_id.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            revoke_device_credential_tx(&conn, &device_id)
        })
        .await
    }

    pub async fn add_user_public_key(
        &self,
        username: &str,
        key_type: &str,
        public_key: &str,
        fingerprint: &str,
        label: Option<&str>,
    ) -> Result<UserPublicKey, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let username = username.to_string();
        let key_type = key_type.to_string();
        let public_key = public_key.to_string();
        let fingerprint = fingerprint.to_string();
        let label = label.map(|s| s.to_string());
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            add_user_public_key_tx(
                &conn,
                &username,
                &key_type,
                &public_key,
                &fingerprint,
                label.as_deref(),
            )
        })
        .await
    }

    pub async fn delete_user_public_key(&self, id: &str) -> Result<(), GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let id = id.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            delete_user_public_key_tx(&conn, &id)
        })
        .await
    }

    pub async fn create_enrollment_challenge(
        &self,
        username: &str,
        device_id: &str,
        hostname: &str,
        os_arch: (&str, &str),
        nonce: &str,
        ttl_secs: u64,
    ) -> Result<EnrollmentChallenge, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        let username = username.to_string();
        let device_id = device_id.to_string();
        let hostname = hostname.to_string();
        let os = os_arch.0.to_string();
        let arch = os_arch.1.to_string();
        let nonce = nonce.to_string();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            create_enrollment_challenge_tx(
                &conn,
                &username,
                &device_id,
                &hostname,
                (&os, &arch),
                &nonce,
                ttl_secs,
            )
        })
        .await
    }

    pub async fn consume_enrollment_challenge(
        &self,
        challenge_id: &str,
    ) -> Result<EnrollmentChallenge, GatewayError> {
        let challenge_id = challenge_id.to_string();
        self.with_write_tx(move |tx| consume_enrollment_challenge_tx(tx, &challenge_id))
            .await
    }

    pub async fn cleanup_old_events(&self, max_age_days: u32) -> Result<usize, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            cleanup_old_events_tx(&conn, max_age_days)
        })
        .await
    }

    pub async fn reset_data(&self) -> Result<usize, GatewayError> {
        let writer = self.writer.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let conn = acquire_writer(&writer, &metrics);
            reset_data_tx(&conn)
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

// --- Device _tx free functions ---

pub(super) fn register_device_tx(
    conn: &Connection,
    id: &str,
    hostname: &str,
    os: &str,
    arch: &str,
    config_hash: &str,
    compliance_summary: Option<&serde_json::Value>,
) -> Result<Device, GatewayError> {
    let now = cfgd_core::utc_now_iso8601();
    let compliance_str = compliance_summary
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| GatewayError::Internal(format!("failed to serialize compliance: {e}")))?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO devices (id, hostname, os, arch, last_checkin, config_hash, status, compliance_summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            hostname = excluded.hostname,
            os = excluded.os,
            arch = excluded.arch,
            last_checkin = excluded.last_checkin,
            config_hash = excluded.config_hash,
            status = excluded.status,
            compliance_summary = excluded.compliance_summary",
    )?;
    stmt.execute(params![
        id,
        hostname,
        os,
        arch,
        &now,
        config_hash,
        DeviceStatus::Healthy.as_str(),
        compliance_str,
    ])?;
    get_device_tx(conn, id)
}

pub(super) fn update_checkin_tx(
    conn: &Connection,
    id: &str,
    config_hash: &str,
    compliance_summary: Option<&serde_json::Value>,
) -> Result<(), GatewayError> {
    let now = cfgd_core::utc_now_iso8601();
    let compliance_str = compliance_summary
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| GatewayError::Internal(format!("failed to serialize compliance: {e}")))?;
    let mut stmt = conn.prepare_cached(
        "UPDATE devices SET last_checkin = ?1, config_hash = ?2, status = ?3, compliance_summary = ?4 WHERE id = ?5",
    )?;
    let rows = stmt.execute(params![
        &now,
        config_hash,
        DeviceStatus::Healthy.as_str(),
        compliance_str,
        id
    ])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!("device not found: {id}")));
    }
    Ok(())
}

pub(super) fn get_device_tx(conn: &Connection, id: &str) -> Result<Device, GatewayError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, hostname, os, arch, last_checkin, config_hash, status, desired_config, compliance_summary FROM devices WHERE id = ?1",
    )?;
    stmt.query_row(params![id], map_device_row)
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                GatewayError::NotFound(format!("device not found: {id}"))
            }
            other => GatewayError::Database(other),
        })
}

pub(super) fn set_device_config_tx(
    conn: &Connection,
    id: &str,
    config: &serde_json::Value,
) -> Result<(), GatewayError> {
    let config_str = serde_json::to_string(config)
        .map_err(|e| GatewayError::Internal(format!("failed to serialize config: {e}")))?;
    let mut stmt = conn.prepare_cached("UPDATE devices SET desired_config = ?1 WHERE id = ?2")?;
    let rows = stmt.execute(params![&config_str, id])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!("device not found: {id}")));
    }
    Ok(())
}

pub(super) fn list_devices_paginated_tx(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> Result<Vec<Device>, GatewayError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, hostname, os, arch, last_checkin, config_hash, status, desired_config, compliance_summary FROM devices ORDER BY hostname LIMIT ?1 OFFSET ?2",
    )?;
    let devices = stmt
        .query_map(params![limit, offset], map_device_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(devices)
}

pub(super) fn record_drift_event_tx(
    conn: &Connection,
    device_id: &str,
    details: &str,
) -> Result<DriftEvent, GatewayError> {
    get_device_tx(conn, device_id)?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = cfgd_core::utc_now_iso8601();
    {
        let mut stmt = conn.prepare_cached(
            "INSERT INTO drift_events (id, device_id, timestamp, details) VALUES (?1, ?2, ?3, ?4)",
        )?;
        stmt.execute(params![&id, device_id, &now, details])?;
    }
    {
        let mut stmt = conn.prepare_cached("UPDATE devices SET status = ?1 WHERE id = ?2")?;
        stmt.execute(params![DeviceStatus::Drifted.as_str(), device_id])?;
    }

    Ok(DriftEvent {
        id,
        device_id: device_id.to_string(),
        timestamp: now,
        details: details.to_string(),
    })
}

pub(super) fn list_drift_events_tx(
    conn: &Connection,
    device_id: &str,
) -> Result<Vec<DriftEvent>, GatewayError> {
    get_device_tx(conn, device_id)?;

    let mut stmt = conn.prepare_cached(
        "SELECT id, device_id, timestamp, details FROM drift_events WHERE device_id = ?1 ORDER BY timestamp DESC",
    )?;
    let events = stmt
        .query_map(params![device_id], map_drift_event_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(events)
}

pub(super) fn record_checkin_tx(
    conn: &Connection,
    device_id: &str,
    config_hash: &str,
    config_changed: bool,
) -> Result<CheckinEvent, GatewayError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = cfgd_core::utc_now_iso8601();
    let mut stmt = conn.prepare_cached(
        "INSERT INTO checkin_events (id, device_id, timestamp, config_hash, config_changed) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    stmt.execute(params![
        &id,
        device_id,
        &now,
        config_hash,
        config_changed as i32
    ])?;
    Ok(CheckinEvent {
        id,
        device_id: device_id.to_string(),
        timestamp: now,
        config_hash: config_hash.to_string(),
        config_changed,
    })
}

pub(super) fn list_checkin_events_tx(
    conn: &Connection,
    device_id: &str,
) -> Result<Vec<CheckinEvent>, GatewayError> {
    get_device_tx(conn, device_id)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, device_id, timestamp, config_hash, config_changed FROM checkin_events WHERE device_id = ?1 ORDER BY timestamp DESC LIMIT 100",
    )?;
    let events = stmt
        .query_map(params![device_id], map_checkin_event_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(events)
}

pub(super) fn list_fleet_events_paginated_tx(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> Result<Vec<FleetEvent>, GatewayError> {
    let mut stmt = conn.prepare_cached(
        "SELECT timestamp, device_id, 'drift' as event_type, details as summary FROM drift_events
         UNION ALL
         SELECT timestamp, device_id, CASE WHEN config_changed THEN 'config-changed' ELSE 'checkin' END, config_hash FROM checkin_events
         ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt
        .query_map(params![limit, offset], map_fleet_event_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) fn set_force_reconcile_tx(
    conn: &Connection,
    device_id: &str,
) -> Result<(), GatewayError> {
    let mut stmt = conn.prepare_cached("UPDATE devices SET status = ?1 WHERE id = ?2")?;
    let rows = stmt.execute(params![DeviceStatus::PendingReconcile.as_str(), device_id])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!(
            "device not found: {device_id}"
        )));
    }
    Ok(())
}

// --- Bootstrap Token _tx ---

pub(super) fn create_bootstrap_token_tx(
    conn: &Connection,
    token_hash: &str,
    username: &str,
    team: Option<&str>,
    expires_at: &str,
) -> Result<BootstrapToken, GatewayError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = cfgd_core::utc_now_iso8601();
    let mut stmt = conn.prepare_cached(
        "INSERT INTO bootstrap_tokens (id, token_hash, username, team, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    stmt.execute(params![&id, token_hash, username, team, &now, expires_at])?;
    Ok(BootstrapToken {
        id,
        username: username.to_string(),
        team: team.map(String::from),
        created_at: now,
        expires_at: expires_at.to_string(),
        used_at: None,
        used_by_device: None,
    })
}

pub(super) fn validate_and_consume_bootstrap_token_tx(
    conn: &Connection,
    token_hash: &str,
    device_id: &str,
) -> Result<BootstrapToken, GatewayError> {
    let now = cfgd_core::utc_now_iso8601();

    let rows = {
        let mut stmt = conn.prepare_cached(
            "UPDATE bootstrap_tokens SET used_at = ?1, used_by_device = ?2
             WHERE token_hash = ?3 AND used_at IS NULL AND expires_at >= ?1",
        )?;
        stmt.execute(params![&now, device_id, token_hash])?
    };

    if rows == 0 {
        let mut stmt = conn.prepare_cached(
            "SELECT used_at, expires_at FROM bootstrap_tokens WHERE token_hash = ?1",
        )?;
        return match stmt.query_row(params![token_hash], |row| {
            let used_at: Option<String> = row.get(0)?;
            let expires_at: String = row.get(1)?;
            Ok((used_at, expires_at))
        }) {
            Ok((Some(_), _)) => Err(GatewayError::InvalidRequest(
                "bootstrap token has already been used".to_string(),
            )),
            Ok((None, expires_at)) if expires_at < now => Err(GatewayError::InvalidRequest(
                "bootstrap token has expired".to_string(),
            )),
            Ok(_) => Err(GatewayError::Internal(
                "bootstrap token validation failed unexpectedly".to_string(),
            )),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(GatewayError::Unauthorized),
            Err(e) => Err(GatewayError::Database(e)),
        };
    }

    let mut stmt = conn.prepare_cached(
        "SELECT id, username, team, created_at, expires_at, used_at, used_by_device FROM bootstrap_tokens WHERE token_hash = ?1",
    )?;
    stmt.query_row(params![token_hash], map_bootstrap_token_row)
        .map_err(GatewayError::Database)
}

pub(super) fn list_bootstrap_tokens_tx(
    conn: &Connection,
) -> Result<Vec<BootstrapToken>, GatewayError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, username, team, created_at, expires_at, used_at, used_by_device FROM bootstrap_tokens ORDER BY created_at DESC",
    )?;
    let tokens = stmt
        .query_map([], map_bootstrap_token_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(tokens)
}

pub(super) fn delete_bootstrap_token_tx(conn: &Connection, id: &str) -> Result<(), GatewayError> {
    let mut stmt = conn.prepare_cached("DELETE FROM bootstrap_tokens WHERE id = ?1")?;
    let rows = stmt.execute(params![id])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!(
            "bootstrap token not found: {id}"
        )));
    }
    Ok(())
}

// --- Device Credential _tx ---

pub(super) fn create_device_credential_tx(
    conn: &Connection,
    device_id: &str,
    api_key_hash: &str,
    username: &str,
    team: Option<&str>,
) -> Result<DeviceCredential, GatewayError> {
    let now = cfgd_core::utc_now_iso8601();
    let mut stmt = conn.prepare_cached(
        "INSERT INTO device_credentials (device_id, api_key_hash, username, team, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(device_id) DO UPDATE SET api_key_hash = excluded.api_key_hash, username = excluded.username, team = excluded.team, created_at = excluded.created_at, revoked = 0, last_used = NULL",
    )?;
    stmt.execute(params![device_id, api_key_hash, username, team, &now])?;
    Ok(DeviceCredential {
        device_id: device_id.to_string(),
        username: username.to_string(),
        team: team.map(String::from),
        created_at: now,
        last_used: None,
        revoked: false,
    })
}

pub(super) fn validate_device_credential_tx(
    conn: &Connection,
    api_key_hash: &str,
) -> Result<DeviceCredential, GatewayError> {
    let cred = {
        let mut stmt = conn.prepare_cached(
            "SELECT device_id, username, team, created_at, last_used, revoked FROM device_credentials WHERE api_key_hash = ?1",
        )?;
        stmt.query_row(params![api_key_hash], map_device_credential_row)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => GatewayError::Unauthorized,
                other => GatewayError::Database(other),
            })?
    };

    if cred.revoked {
        return Err(GatewayError::Unauthorized);
    }

    let now = cfgd_core::utc_now_iso8601();
    {
        let mut stmt = conn
            .prepare_cached("UPDATE device_credentials SET last_used = ?1 WHERE device_id = ?2")?;
        if let Err(e) = stmt.execute(params![&now, &cred.device_id]) {
            tracing::warn!(device_id = %cred.device_id, error = %e, "failed to update credential last_used");
        }
    }

    Ok(cred)
}

pub(super) fn revoke_device_credential_tx(
    conn: &Connection,
    device_id: &str,
) -> Result<(), GatewayError> {
    let mut stmt =
        conn.prepare_cached("UPDATE device_credentials SET revoked = 1 WHERE device_id = ?1")?;
    let rows = stmt.execute(params![device_id])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!(
            "device credential not found: {device_id}"
        )));
    }
    Ok(())
}

// --- User Public Key _tx ---

pub(super) fn add_user_public_key_tx(
    conn: &Connection,
    username: &str,
    key_type: &str,
    public_key: &str,
    fingerprint: &str,
    label: Option<&str>,
) -> Result<UserPublicKey, GatewayError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = cfgd_core::utc_now_iso8601();
    let mut stmt = conn.prepare_cached(
        "INSERT INTO user_public_keys (id, username, key_type, public_key, fingerprint, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    stmt.execute(params![
        &id,
        username,
        key_type,
        public_key,
        fingerprint,
        label,
        &now
    ])?;
    Ok(UserPublicKey {
        id,
        username: username.to_string(),
        key_type: key_type.to_string(),
        public_key: public_key.to_string(),
        fingerprint: fingerprint.to_string(),
        label: label.map(String::from),
        created_at: now,
    })
}

pub(super) fn list_user_public_keys_tx(
    conn: &Connection,
    username: &str,
) -> Result<Vec<UserPublicKey>, GatewayError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, username, key_type, public_key, fingerprint, label, created_at FROM user_public_keys WHERE username = ?1 ORDER BY created_at DESC",
    )?;
    let keys = stmt
        .query_map(params![username], map_user_public_key_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(keys)
}

pub(super) fn delete_user_public_key_tx(conn: &Connection, id: &str) -> Result<(), GatewayError> {
    let mut stmt = conn.prepare_cached("DELETE FROM user_public_keys WHERE id = ?1")?;
    let rows = stmt.execute(params![id])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!(
            "public key not found: {id}"
        )));
    }
    Ok(())
}

// --- Enrollment Challenge _tx ---

pub(super) fn create_enrollment_challenge_tx(
    conn: &Connection,
    username: &str,
    device_id: &str,
    hostname: &str,
    os_arch: (&str, &str),
    nonce: &str,
    ttl_secs: u64,
) -> Result<EnrollmentChallenge, GatewayError> {
    let (os, arch) = os_arch;
    let id = uuid::Uuid::new_v4().to_string();
    let now_secs = cfgd_core::unix_secs_now();
    let created_at = cfgd_core::unix_secs_to_iso8601(now_secs);
    let expires_at = cfgd_core::unix_secs_to_iso8601(now_secs + ttl_secs);

    let mut stmt = conn.prepare_cached(
        "INSERT INTO enrollment_challenges (id, username, device_id, hostname, os, arch, nonce, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    stmt.execute(params![
        &id,
        username,
        device_id,
        hostname,
        os,
        arch,
        nonce,
        &created_at,
        &expires_at
    ])?;

    Ok(EnrollmentChallenge {
        id,
        username: username.to_string(),
        device_id: device_id.to_string(),
        hostname: hostname.to_string(),
        os: os.to_string(),
        arch: arch.to_string(),
        nonce: nonce.to_string(),
        created_at,
        expires_at,
    })
}

pub(super) fn consume_enrollment_challenge_tx(
    conn: &Connection,
    challenge_id: &str,
) -> Result<EnrollmentChallenge, GatewayError> {
    let now = cfgd_core::utc_now_iso8601();

    let (challenge, consumed) = {
        let mut stmt = conn.prepare_cached(
            "SELECT id, username, device_id, hostname, os, arch, nonce, created_at, expires_at, consumed
             FROM enrollment_challenges WHERE id = ?1",
        )?;
        match stmt.query_row(params![challenge_id], |row| {
            let consumed: i32 = row.get(9)?;
            Ok((
                EnrollmentChallenge {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    device_id: row.get(2)?,
                    hostname: row.get(3)?,
                    os: row.get(4)?,
                    arch: row.get(5)?,
                    nonce: row.get(6)?,
                    created_at: row.get(7)?,
                    expires_at: row.get(8)?,
                },
                consumed,
            ))
        }) {
            Ok(pair) => pair,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(GatewayError::NotFound(format!(
                    "enrollment challenge not found: {challenge_id}"
                )));
            }
            Err(e) => return Err(GatewayError::Database(e)),
        }
    };

    if consumed != 0 {
        return Err(GatewayError::InvalidRequest(
            "enrollment challenge has already been used".to_string(),
        ));
    }
    if challenge.expires_at < now {
        return Err(GatewayError::InvalidRequest(
            "enrollment challenge has expired".to_string(),
        ));
    }

    {
        let mut stmt =
            conn.prepare_cached("UPDATE enrollment_challenges SET consumed = 1 WHERE id = ?1")?;
        stmt.execute(params![challenge_id])?;
    }

    Ok(challenge)
}

// --- Cleanup / reset ---

pub(super) fn cleanup_old_events_tx(
    conn: &Connection,
    max_age_days: u32,
) -> Result<usize, GatewayError> {
    let cutoff = {
        let cutoff_secs = cfgd_core::unix_secs_now().saturating_sub(max_age_days as u64 * 86400);
        cfgd_core::unix_secs_to_iso8601(cutoff_secs)
    };

    let drift_deleted = {
        let mut stmt = conn.prepare_cached("DELETE FROM drift_events WHERE timestamp < ?1")?;
        stmt.execute(params![&cutoff])?
    };
    let checkin_deleted = {
        let mut stmt = conn.prepare_cached("DELETE FROM checkin_events WHERE timestamp < ?1")?;
        stmt.execute(params![&cutoff])?
    };

    let total = drift_deleted + checkin_deleted;
    if total > 0 {
        tracing::info!(
            drift = drift_deleted,
            checkin = checkin_deleted,
            max_age_days,
            "cleaned up old events"
        );
    }
    Ok(total)
}

pub(super) fn reset_data_tx(conn: &Connection) -> Result<usize, GatewayError> {
    let tables = [
        "enrollment_challenges",
        "device_credentials",
        "drift_events",
        "checkin_events",
        "bootstrap_tokens",
        "user_public_keys",
        "devices",
    ];
    let mut total = 0usize;
    for table in &tables {
        // Table names come from this fixed list, not from user input — safe to format.
        let deleted = conn.execute(&format!("DELETE FROM {table}"), [])?;
        total += deleted;
    }
    if total > 0 {
        tracing::info!(rows_deleted = total, "admin reset: all data wiped");
    }
    Ok(total)
}

// --- Fleet status (shared with fleet.rs) ---

pub(super) fn get_fleet_status_tx(
    conn: &Connection,
) -> Result<super::fleet::FleetStatus, GatewayError> {
    let devices = list_devices_paginated_tx(conn, 1_000_000, 0)?;
    let total_devices = devices.len();
    let mut healthy = 0usize;
    let mut drifted = 0usize;
    let mut offline = 0usize;

    for device in &devices {
        match device.status {
            DeviceStatus::Healthy => healthy += 1,
            DeviceStatus::Drifted => drifted += 1,
            DeviceStatus::Offline | DeviceStatus::PendingReconcile => offline += 1,
        }
    }

    Ok(super::fleet::FleetStatus {
        total_devices,
        healthy,
        drifted,
        offline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> (ServerDb, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("test.db");
        let db = ServerDb::open(path.to_str().expect("utf8")).expect("open db");
        (db, tmp)
    }

    fn future_expiry() -> String {
        cfgd_core::unix_secs_to_iso8601(cfgd_core::unix_secs_now() + 3600)
    }

    fn past_expiry() -> String {
        cfgd_core::unix_secs_to_iso8601(cfgd_core::unix_secs_now().saturating_sub(3600))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn register_and_get_device() {
        let (db, _tmp) = test_db();
        let device = db
            .register_device("dev-1", "workstation-1", "linux", "x86_64", "abc123", None)
            .await
            .expect("register failed");
        assert_eq!(device.id, "dev-1");
        assert_eq!(device.hostname, "workstation-1");
        assert_eq!(device.status, DeviceStatus::Healthy);
        assert!(device.desired_config.is_none());

        let fetched = db.get_device("dev-1").await.expect("get failed");
        assert_eq!(fetched.hostname, "workstation-1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_devices_empty() {
        let (db, _tmp) = test_db();
        let devices = db.list_devices().await.expect("list failed");
        assert!(devices.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_checkin() {
        let (db, _tmp) = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .await
            .expect("register failed");
        db.update_checkin("dev-1", "hash2", None)
            .await
            .expect("update failed");
        let device = db.get_device("dev-1").await.expect("get failed");
        assert_eq!(device.config_hash, "hash2");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_checkin_not_found() {
        let (db, _tmp) = test_db();
        let result = db.update_checkin("nonexistent", "hash", None).await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drift_events() {
        let (db, _tmp) = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .await
            .expect("register failed");
        let event = db
            .record_drift_event("dev-1", "package vim drifted")
            .await
            .expect("record failed");
        assert_eq!(event.device_id, "dev-1");

        let device = db.get_device("dev-1").await.expect("get failed");
        assert_eq!(device.status, DeviceStatus::Drifted);

        let events = db.list_drift_events("dev-1").await.expect("list failed");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].details, "package vim drifted");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drift_event_for_nonexistent_device() {
        let (db, _tmp) = test_db();
        let result = db.record_drift_event("nonexistent", "details").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn register_device_upsert() {
        let (db, _tmp) = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .await
            .expect("first register");
        let device = db
            .register_device("dev-1", "ws-1-renamed", "linux", "x86_64", "hash2", None)
            .await
            .expect("second register");
        assert_eq!(device.hostname, "ws-1-renamed");
        assert_eq!(device.config_hash, "hash2");

        let all = db.list_devices().await.expect("list");
        assert_eq!(all.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_and_get_device_config() {
        let (db, _tmp) = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .await
            .expect("register failed");

        let config = serde_json::json!({"packages": ["vim", "git"]});
        db.set_device_config("dev-1", &config)
            .await
            .expect("set config failed");

        let device = db.get_device("dev-1").await.expect("get failed");
        assert_eq!(device.desired_config, Some(config));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_config_not_found() {
        let (db, _tmp) = test_db();
        let config = serde_json::json!({"packages": []});
        let result = db.set_device_config("nonexistent", &config).await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_and_list_checkin_events() {
        let (db, _tmp) = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .await
            .expect("register failed");

        let event = db
            .record_checkin("dev-1", "hash1", false)
            .await
            .expect("record checkin failed");
        assert_eq!(event.device_id, "dev-1");
        assert!(!event.config_changed);

        db.record_checkin("dev-1", "hash2", true)
            .await
            .expect("record checkin 2 failed");

        let events = db
            .list_checkin_events("dev-1")
            .await
            .expect("list checkin events failed");
        assert_eq!(events.len(), 2);
        let changed_count = events.iter().filter(|e| e.config_changed).count();
        assert_eq!(changed_count, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_checkin_events_nonexistent_device() {
        let (db, _tmp) = test_db();
        let result = db.list_checkin_events("nonexistent").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_events_combined() {
        let (db, _tmp) = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .await
            .expect("register failed");

        db.record_checkin("dev-1", "hash1", false)
            .await
            .expect("checkin failed");
        db.record_drift_event(
            "dev-1",
            r#"[{"field":"pkg","expected":"vim","actual":"missing"}]"#,
        )
        .await
        .expect("drift failed");
        db.record_checkin("dev-1", "hash2", true)
            .await
            .expect("checkin 2 failed");

        let events = db
            .list_fleet_events(100)
            .await
            .expect("list fleet events failed");
        assert_eq!(events.len(), 3);

        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"drift"));
        assert!(types.contains(&"checkin") || types.contains(&"config-changed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_events_empty() {
        let (db, _tmp) = test_db();
        let events = db
            .list_fleet_events(100)
            .await
            .expect("list fleet events failed");
        assert!(events.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn force_reconcile() {
        let (db, _tmp) = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .await
            .expect("register failed");

        db.set_force_reconcile("dev-1")
            .await
            .expect("force reconcile failed");

        let device = db.get_device("dev-1").await.expect("get failed");
        assert_eq!(device.status, DeviceStatus::PendingReconcile);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn force_reconcile_not_found() {
        let (db, _tmp) = test_db();
        let result = db.set_force_reconcile("nonexistent").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compliance_summary_stored_on_register() {
        let (db, _tmp) = test_db();
        let summary = serde_json::json!({"compliant": 5, "warning": 1, "violation": 0});
        let device = db
            .register_device("dev-c", "ws-c", "linux", "x86_64", "hash1", Some(&summary))
            .await
            .expect("register failed");
        assert_eq!(device.compliance_summary, Some(summary));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compliance_summary_stored_on_checkin_update() {
        let (db, _tmp) = test_db();
        db.register_device("dev-c2", "ws-c2", "linux", "x86_64", "hash1", None)
            .await
            .expect("register failed");

        let summary = serde_json::json!({"compliant": 10, "warning": 0, "violation": 2});
        db.update_checkin("dev-c2", "hash2", Some(&summary))
            .await
            .expect("update failed");

        let device = db.get_device("dev-c2").await.expect("get failed");
        assert_eq!(device.config_hash, "hash2");
        assert_eq!(device.compliance_summary, Some(summary));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compliance_summary_null_when_not_provided() {
        let (db, _tmp) = test_db();
        let device = db
            .register_device("dev-c3", "ws-c3", "linux", "x86_64", "hash1", None)
            .await
            .expect("register failed");
        assert!(device.compliance_summary.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_and_consume_bootstrap_token() {
        let (db, _tmp) = test_db();
        let expires_at = future_expiry();

        let token = db
            .create_bootstrap_token("hash123", "jdoe", Some("acme"), &expires_at)
            .await
            .expect("create failed");
        assert_eq!(token.username, "jdoe");
        assert_eq!(token.team.as_deref(), Some("acme"));
        assert!(token.used_at.is_none());

        let consumed = db
            .validate_and_consume_bootstrap_token("hash123", "dev-1")
            .await
            .expect("validate_and_consume failed");
        assert_eq!(consumed.username, "jdoe");
        assert!(consumed.used_at.is_some());
        assert_eq!(consumed.used_by_device.as_deref(), Some("dev-1"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consume_expired_token_fails() {
        let (db, _tmp) = test_db();
        let expires_at = past_expiry();

        db.create_bootstrap_token("hash_expired", "jdoe", None, &expires_at)
            .await
            .expect("create failed");

        let result = db
            .validate_and_consume_bootstrap_token("hash_expired", "dev-1")
            .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::InvalidRequest(_)),
            "expected InvalidRequest, got: {err}"
        );
        assert!(
            err.to_string().contains("expired"),
            "expected 'expired' in message, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consume_used_token_fails() {
        let (db, _tmp) = test_db();
        let expires_at = future_expiry();

        db.create_bootstrap_token("hash_used", "jdoe", None, &expires_at)
            .await
            .expect("create failed");
        db.validate_and_consume_bootstrap_token("hash_used", "dev-1")
            .await
            .expect("first consume succeeded");

        let result = db
            .validate_and_consume_bootstrap_token("hash_used", "dev-2")
            .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::InvalidRequest(_)),
            "expected InvalidRequest, got: {err}"
        );
        assert!(
            err.to_string().contains("already been used"),
            "expected 'already been used' in message, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consume_nonexistent_token_fails() {
        let (db, _tmp) = test_db();
        let result = db
            .validate_and_consume_bootstrap_token("no_such_hash", "dev-1")
            .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::Unauthorized),
            "expected Unauthorized, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_bootstrap_tokens() {
        let (db, _tmp) = test_db();
        let expires_at = future_expiry();

        db.create_bootstrap_token("h1", "user1", None, &expires_at)
            .await
            .expect("create 1 failed");
        db.create_bootstrap_token("h2", "user2", Some("team"), &expires_at)
            .await
            .expect("create 2 failed");

        let tokens = db.list_bootstrap_tokens().await.expect("list failed");
        assert_eq!(tokens.len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_bootstrap_token() {
        let (db, _tmp) = test_db();
        let expires_at = future_expiry();

        let token = db
            .create_bootstrap_token("h_del", "jdoe", None, &expires_at)
            .await
            .expect("create failed");

        db.delete_bootstrap_token(&token.id)
            .await
            .expect("delete failed");

        let tokens = db.list_bootstrap_tokens().await.expect("list failed");
        assert!(tokens.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_nonexistent_token_fails() {
        let (db, _tmp) = test_db();
        let result = db.delete_bootstrap_token("no-such-id").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_and_validate_device_credential() {
        let (db, _tmp) = test_db();

        db.create_device_credential("dev-1", "keyhash1", "jdoe", Some("acme"))
            .await
            .expect("create failed");

        let cred = db
            .validate_device_credential("keyhash1")
            .await
            .expect("validate failed");
        assert_eq!(cred.device_id, "dev-1");
        assert_eq!(cred.username, "jdoe");
        assert_eq!(cred.team.as_deref(), Some("acme"));
        assert!(!cred.revoked);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn validate_nonexistent_credential_fails() {
        let (db, _tmp) = test_db();
        let result = db.validate_device_credential("no_such_hash").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::Unauthorized),
            "expected Unauthorized, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn revoked_credential_fails_validation() {
        let (db, _tmp) = test_db();

        db.create_device_credential("dev-1", "keyhash_rev", "jdoe", None)
            .await
            .expect("create failed");
        db.revoke_device_credential("dev-1")
            .await
            .expect("revoke failed");

        let result = db.validate_device_credential("keyhash_rev").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::Unauthorized),
            "expected Unauthorized, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn credential_upsert_on_re_enrollment() {
        let (db, _tmp) = test_db();

        db.create_device_credential("dev-1", "key_old", "jdoe", None)
            .await
            .expect("create 1 failed");
        db.create_device_credential("dev-1", "key_new", "jdoe", Some("newteam"))
            .await
            .expect("create 2 (upsert) failed");

        let result = db.validate_device_credential("key_old").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::Unauthorized),
            "expected Unauthorized, got: {err}"
        );

        let cred = db
            .validate_device_credential("key_new")
            .await
            .expect("validate new failed");
        assert_eq!(cred.device_id, "dev-1");
        assert_eq!(cred.team.as_deref(), Some("newteam"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn revoke_nonexistent_credential_fails() {
        let (db, _tmp) = test_db();
        let result = db.revoke_device_credential("no-such-device").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn add_and_list_user_public_keys() {
        let (db, _tmp) = test_db();

        let key = db
            .add_user_public_key(
                "jdoe",
                "ssh",
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG... jdoe@work",
                "SHA256:abc123def456",
                Some("work laptop"),
            )
            .await
            .expect("add key failed");
        assert_eq!(key.username, "jdoe");
        assert_eq!(key.key_type, "ssh");
        assert_eq!(key.label.as_deref(), Some("work laptop"));

        db.add_user_public_key(
            "jdoe",
            "gpg",
            "-----BEGIN PGP PUBLIC KEY BLOCK-----\n...\n-----END PGP PUBLIC KEY BLOCK-----",
            "ABCD1234EFGH5678",
            None,
        )
        .await
        .expect("add gpg key failed");

        let keys = db.list_user_public_keys("jdoe").await.expect("list failed");
        assert_eq!(keys.len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_user_public_keys_empty() {
        let (db, _tmp) = test_db();
        let keys = db
            .list_user_public_keys("nobody")
            .await
            .expect("list failed");
        assert!(keys.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_user_public_key() {
        let (db, _tmp) = test_db();

        let key = db
            .add_user_public_key("jdoe", "ssh", "ssh-ed25519 AAAA...", "SHA256:xyz", None)
            .await
            .expect("add failed");

        db.delete_user_public_key(&key.id)
            .await
            .expect("delete failed");

        let keys = db.list_user_public_keys("jdoe").await.expect("list failed");
        assert!(keys.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_nonexistent_public_key_fails() {
        let (db, _tmp) = test_db();
        let result = db.delete_user_public_key("no-such-id").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_and_consume_enrollment_challenge() {
        let (db, _tmp) = test_db();

        let challenge = db
            .create_enrollment_challenge(
                "jdoe",
                "dev-1",
                "macbook",
                ("macos", "aarch64"),
                "random-nonce-abc",
                300,
            )
            .await
            .expect("create challenge failed");
        assert_eq!(challenge.username, "jdoe");
        assert_eq!(challenge.nonce, "random-nonce-abc");

        let consumed = db
            .consume_enrollment_challenge(&challenge.id)
            .await
            .expect("consume failed");
        assert_eq!(consumed.username, "jdoe");
        assert_eq!(consumed.device_id, "dev-1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consume_challenge_twice_fails() {
        let (db, _tmp) = test_db();

        let challenge = db
            .create_enrollment_challenge(
                "jdoe",
                "dev-1",
                "macbook",
                ("macos", "aarch64"),
                "nonce1",
                300,
            )
            .await
            .expect("create failed");

        db.consume_enrollment_challenge(&challenge.id)
            .await
            .expect("first consume ok");

        let result = db.consume_enrollment_challenge(&challenge.id).await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::InvalidRequest(_)),
            "expected InvalidRequest, got: {err}"
        );
        assert!(
            err.to_string().contains("already been used"),
            "expected 'already been used' in message, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consume_expired_challenge_fails() {
        let (db, _tmp) = test_db();

        // Insert a challenge with an already-past expires_at via the writer tx.
        let id = uuid::Uuid::new_v4().to_string();
        let past = past_expiry();
        let id_c = id.clone();
        let past_c = past.clone();
        db.with_write_tx(move |tx| {
            tx.execute(
                "INSERT INTO enrollment_challenges (id, username, device_id, hostname, os, arch, nonce, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![&id_c, "jdoe", "dev-1", "macbook", "macos", "aarch64", "nonce2", &past_c, &past_c],
            )?;
            Ok(())
        })
        .await
        .expect("insert failed");

        let result = db.consume_enrollment_challenge(&id).await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::InvalidRequest(_)),
            "expected InvalidRequest, got: {err}"
        );
        assert!(
            err.to_string().contains("expired"),
            "expected 'expired' in message, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consume_nonexistent_challenge_fails() {
        let (db, _tmp) = test_db();
        let result = db.consume_enrollment_challenge("no-such-id").await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn schema_version_is_set() {
        let (db, _tmp) = test_db();
        assert_eq!(db.schema_version_blocking(), MIGRATIONS.len());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn migration_upgrades_v1_database() {
        // Simulate a schema at version 1: create tables without desired_config.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upgrade-test.db");
        let path_str = path.to_str().unwrap();

        {
            let conn = Connection::open(path_str).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
                .unwrap();
            conn.execute_batch(
                "CREATE TABLE devices (
                    id TEXT PRIMARY KEY,
                    hostname TEXT NOT NULL,
                    os TEXT NOT NULL,
                    arch TEXT NOT NULL,
                    last_checkin TEXT NOT NULL,
                    config_hash TEXT NOT NULL,
                    status TEXT NOT NULL
                );
                CREATE TABLE schema_version (version INTEGER NOT NULL);
                INSERT INTO schema_version (version) VALUES (1);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO devices (id, hostname, os, arch, last_checkin, config_hash, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params!["dev-1", "host-1", "linux", "amd64", "2026-01-01T00:00:00Z", "abc", "healthy"],
            ).unwrap();
        }

        let db = ServerDb::open(path_str).unwrap();
        assert_eq!(db.schema_version_blocking(), MIGRATIONS.len());

        let device = db.get_device("dev-1").await.unwrap();
        assert_eq!(device.hostname, "host-1");
        assert!(device.desired_config.is_none());
    }
}

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::errors::GatewayError;

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
    // Migration 1: add desired_config column for pre-existing databases.
    // New databases already have this column from migration 0's CREATE TABLE,
    // so the ALTER TABLE will produce a "duplicate column" error — which
    // run_migrations tolerates for ADD COLUMN migrations.
    "ALTER TABLE devices ADD COLUMN desired_config TEXT;",
    // Migration 2: add compliance_summary column to store last-reported compliance data.
    "ALTER TABLE devices ADD COLUMN compliance_summary TEXT;",
];

pub struct ServerDb {
    pub(crate) conn: Connection,
}

impl ServerDb {
    pub fn open(path: &str) -> Result<Self, GatewayError> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let db = Self { conn };
        db.run_migrations()?;
        Ok(db)
    }

    fn run_migrations(&self) -> Result<(), GatewayError> {
        // Use IMMEDIATE to acquire a write lock upfront, preventing concurrent
        // processes from both reading version 0 and running all migrations.
        self.conn.execute_batch("BEGIN IMMEDIATE")?;

        let current_version = match self.schema_version_inner() {
            Ok(v) => v,
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(e.into());
            }
        };

        for (i, migration) in MIGRATIONS.iter().enumerate() {
            if i >= current_version {
                if let Err(e) = self.conn.execute_batch(migration) {
                    // Tolerate "duplicate column name" errors from ADD COLUMN
                    // migrations that run against databases where the column
                    // was already included in the original CREATE TABLE.
                    let is_dup_column = matches!(
                        &e,
                        rusqlite::Error::SqliteFailure(_, Some(msg))
                            if msg.contains("duplicate column name")
                    );
                    if !is_dup_column {
                        let _ = self.conn.execute_batch("ROLLBACK");
                        return Err(e.into());
                    }
                }
                let new_version = (i + 1) as i64;
                if let Err(e) = self.conn.execute(
                    "UPDATE schema_version SET version = ?1",
                    params![new_version],
                ) {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(e.into());
                }
            }
        }

        if let Err(e) = self.conn.execute_batch("COMMIT") {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(e.into());
        }
        Ok(())
    }

    fn schema_version_inner(&self) -> std::result::Result<usize, rusqlite::Error> {
        self.conn
            .query_row("SELECT version FROM schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|v| v as usize)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(0),
                // Table doesn't exist yet (first run) — treat as version 0
                rusqlite::Error::SqliteFailure(_, Some(ref msg))
                    if msg.contains("no such table") =>
                {
                    Ok(0)
                }
                other => Err(other),
            })
    }

    #[cfg(test)]
    fn schema_version(&self) -> usize {
        self.schema_version_inner().unwrap_or(0)
    }

    pub fn register_device(
        &self,
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
        self.conn.execute(
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
            params![
                id,
                hostname,
                os,
                arch,
                &now,
                config_hash,
                DeviceStatus::Healthy.as_str(),
                compliance_str,
            ],
        )?;
        self.get_device(id)
    }

    pub fn update_checkin(
        &self,
        id: &str,
        config_hash: &str,
        compliance_summary: Option<&serde_json::Value>,
    ) -> Result<(), GatewayError> {
        let now = cfgd_core::utc_now_iso8601();
        let compliance_str = compliance_summary
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| GatewayError::Internal(format!("failed to serialize compliance: {e}")))?;
        let rows = self.conn.execute(
            "UPDATE devices SET last_checkin = ?1, config_hash = ?2, status = ?3, compliance_summary = ?4 WHERE id = ?5",
            params![&now, config_hash, DeviceStatus::Healthy.as_str(), compliance_str, id],
        )?;
        if rows == 0 {
            return Err(GatewayError::NotFound(format!("device not found: {id}")));
        }
        Ok(())
    }

    pub fn get_device(&self, id: &str) -> Result<Device, GatewayError> {
        self.conn
            .query_row(
                "SELECT id, hostname, os, arch, last_checkin, config_hash, status, desired_config, compliance_summary FROM devices WHERE id = ?1",
                params![id],
                |row| {
                    let config_str: Option<String> = row.get(7)?;
                    let desired_config = config_str
                        .and_then(|s| serde_json::from_str(&s).ok());
                    let compliance_str: Option<String> = row.get(8)?;
                    let compliance_summary = compliance_str
                        .and_then(|s| serde_json::from_str(&s).ok());
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
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    GatewayError::NotFound(format!("device not found: {id}"))
                }
                other => GatewayError::Database(other),
            })
    }

    pub fn set_device_config(
        &self,
        id: &str,
        config: &serde_json::Value,
    ) -> Result<(), GatewayError> {
        let config_str = serde_json::to_string(config)
            .map_err(|e| GatewayError::Internal(format!("failed to serialize config: {e}")))?;
        let rows = self.conn.execute(
            "UPDATE devices SET desired_config = ?1 WHERE id = ?2",
            params![&config_str, id],
        )?;
        if rows == 0 {
            return Err(GatewayError::NotFound(format!("device not found: {id}")));
        }
        Ok(())
    }

    pub fn list_devices(&self) -> Result<Vec<Device>, GatewayError> {
        self.list_devices_paginated(1000, 0)
    }

    pub fn list_devices_paginated(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Device>, GatewayError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hostname, os, arch, last_checkin, config_hash, status, desired_config, compliance_summary FROM devices ORDER BY hostname LIMIT ?1 OFFSET ?2",
        )?;
        let devices = stmt
            .query_map(params![limit, offset], |row| {
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
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(devices)
    }

    pub fn record_drift_event(
        &self,
        device_id: &str,
        details: &str,
    ) -> Result<DriftEvent, GatewayError> {
        self.get_device(device_id)?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = cfgd_core::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO drift_events (id, device_id, timestamp, details) VALUES (?1, ?2, ?3, ?4)",
            params![&id, device_id, &now, details],
        )?;

        self.conn.execute(
            "UPDATE devices SET status = ?1 WHERE id = ?2",
            params![DeviceStatus::Drifted.as_str(), device_id],
        )?;

        Ok(DriftEvent {
            id,
            device_id: device_id.to_string(),
            timestamp: now,
            details: details.to_string(),
        })
    }

    pub fn list_drift_events(&self, device_id: &str) -> Result<Vec<DriftEvent>, GatewayError> {
        self.get_device(device_id)?;

        let mut stmt = self.conn.prepare(
            "SELECT id, device_id, timestamp, details FROM drift_events WHERE device_id = ?1 ORDER BY timestamp DESC",
        )?;
        let events = stmt
            .query_map(params![device_id], |row| {
                Ok(DriftEvent {
                    id: row.get(0)?,
                    device_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    details: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(events)
    }

    pub fn record_checkin(
        &self,
        device_id: &str,
        config_hash: &str,
        config_changed: bool,
    ) -> Result<CheckinEvent, GatewayError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = cfgd_core::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO checkin_events (id, device_id, timestamp, config_hash, config_changed) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&id, device_id, &now, config_hash, config_changed as i32],
        )?;
        Ok(CheckinEvent {
            id,
            device_id: device_id.to_string(),
            timestamp: now,
            config_hash: config_hash.to_string(),
            config_changed,
        })
    }

    pub fn list_checkin_events(&self, device_id: &str) -> Result<Vec<CheckinEvent>, GatewayError> {
        self.get_device(device_id)?;

        let mut stmt = self.conn.prepare(
            "SELECT id, device_id, timestamp, config_hash, config_changed FROM checkin_events WHERE device_id = ?1 ORDER BY timestamp DESC LIMIT 100",
        )?;
        let events = stmt
            .query_map(params![device_id], |row| {
                let changed: i32 = row.get(4)?;
                Ok(CheckinEvent {
                    id: row.get(0)?,
                    device_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    config_hash: row.get(3)?,
                    config_changed: changed != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(events)
    }

    pub fn list_fleet_events(&self, limit: u32) -> Result<Vec<FleetEvent>, GatewayError> {
        self.list_fleet_events_paginated(limit, 0)
    }

    pub fn list_fleet_events_paginated(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<FleetEvent>, GatewayError> {
        let mut stmt = self.conn.prepare(
            "SELECT timestamp, device_id, 'drift' as event_type, details as summary FROM drift_events
             UNION ALL
             SELECT timestamp, device_id, CASE WHEN config_changed THEN 'config-changed' ELSE 'checkin' END, config_hash FROM checkin_events
             ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(params![limit, offset], |row| {
                Ok(FleetEvent {
                    timestamp: row.get(0)?,
                    device_id: row.get(1)?,
                    event_type: row.get(2)?,
                    summary: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_force_reconcile(&self, device_id: &str) -> Result<(), GatewayError> {
        let rows = self.conn.execute(
            "UPDATE devices SET status = ?1 WHERE id = ?2",
            params![DeviceStatus::PendingReconcile.as_str(), device_id],
        )?;
        if rows == 0 {
            return Err(GatewayError::NotFound(format!(
                "device not found: {device_id}"
            )));
        }
        Ok(())
    }

    // --- Bootstrap Tokens ---

    /// Store a bootstrap token (hash only — the plaintext token is returned to the admin once).
    pub fn create_bootstrap_token(
        &self,
        token_hash: &str,
        username: &str,
        team: Option<&str>,
        expires_at: &str,
    ) -> Result<BootstrapToken, GatewayError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = cfgd_core::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO bootstrap_tokens (id, token_hash, username, team, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![&id, token_hash, username, team, &now, expires_at],
        )?;
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

    /// Atomically validate and consume a bootstrap token.
    /// The token must exist, not be expired, and not already be used.
    /// On success, marks the token as used by the given device and returns the record.
    /// Uses a transaction to prevent TOCTOU races (two concurrent enrollments
    /// with the same token).
    pub fn validate_and_consume_bootstrap_token(
        &self,
        token_hash: &str,
        device_id: &str,
    ) -> Result<BootstrapToken, GatewayError> {
        let now = cfgd_core::utc_now_iso8601();

        // Atomic: UPDATE only if token is unused + not expired, then SELECT the result.
        let rows = self.conn.execute(
            "UPDATE bootstrap_tokens SET used_at = ?1, used_by_device = ?2
             WHERE token_hash = ?3 AND used_at IS NULL AND expires_at >= ?1",
            params![&now, device_id, token_hash],
        )?;

        if rows == 0 {
            // Determine why: token doesn't exist, expired, or already used.
            return match self.conn.query_row(
                "SELECT used_at, expires_at FROM bootstrap_tokens WHERE token_hash = ?1",
                params![token_hash],
                |row| {
                    let used_at: Option<String> = row.get(0)?;
                    let expires_at: String = row.get(1)?;
                    Ok((used_at, expires_at))
                },
            ) {
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

        // Token consumed — read it back.
        self.conn
            .query_row(
                "SELECT id, username, team, created_at, expires_at, used_at, used_by_device FROM bootstrap_tokens WHERE token_hash = ?1",
                params![token_hash],
                |row| {
                    Ok(BootstrapToken {
                        id: row.get(0)?,
                        username: row.get(1)?,
                        team: row.get(2)?,
                        created_at: row.get(3)?,
                        expires_at: row.get(4)?,
                        used_at: row.get(5)?,
                        used_by_device: row.get(6)?,
                    })
                },
            )
            .map_err(GatewayError::Database)
    }

    /// List all bootstrap tokens (active and used).
    pub fn list_bootstrap_tokens(&self) -> Result<Vec<BootstrapToken>, GatewayError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, username, team, created_at, expires_at, used_at, used_by_device FROM bootstrap_tokens ORDER BY created_at DESC",
        )?;
        let tokens = stmt
            .query_map([], |row| {
                Ok(BootstrapToken {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    team: row.get(2)?,
                    created_at: row.get(3)?,
                    expires_at: row.get(4)?,
                    used_at: row.get(5)?,
                    used_by_device: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tokens)
    }

    /// Delete a bootstrap token by ID.
    pub fn delete_bootstrap_token(&self, id: &str) -> Result<(), GatewayError> {
        let rows = self
            .conn
            .execute("DELETE FROM bootstrap_tokens WHERE id = ?1", params![id])?;
        if rows == 0 {
            return Err(GatewayError::NotFound(format!(
                "bootstrap token not found: {id}"
            )));
        }
        Ok(())
    }

    // --- Device Credentials ---

    /// Store a new device credential (hash only — plaintext API key returned to device once).
    pub fn create_device_credential(
        &self,
        device_id: &str,
        api_key_hash: &str,
        username: &str,
        team: Option<&str>,
    ) -> Result<DeviceCredential, GatewayError> {
        let now = cfgd_core::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO device_credentials (device_id, api_key_hash, username, team, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(device_id) DO UPDATE SET api_key_hash = excluded.api_key_hash, username = excluded.username, team = excluded.team, created_at = excluded.created_at, revoked = 0, last_used = NULL",
            params![device_id, api_key_hash, username, team, &now],
        )?;
        Ok(DeviceCredential {
            device_id: device_id.to_string(),
            username: username.to_string(),
            team: team.map(String::from),
            created_at: now,
            last_used: None,
            revoked: false,
        })
    }

    /// Validate a device API key. Returns the credential if valid and not revoked.
    pub fn validate_device_credential(
        &self,
        api_key_hash: &str,
    ) -> Result<DeviceCredential, GatewayError> {
        let cred = self
            .conn
            .query_row(
                "SELECT device_id, username, team, created_at, last_used, revoked FROM device_credentials WHERE api_key_hash = ?1",
                params![api_key_hash],
                |row| {
                    let revoked: i32 = row.get(5)?;
                    Ok(DeviceCredential {
                        device_id: row.get(0)?,
                        username: row.get(1)?,
                        team: row.get(2)?,
                        created_at: row.get(3)?,
                        last_used: row.get(4)?,
                        revoked: revoked != 0,
                    })
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => GatewayError::Unauthorized,
                other => GatewayError::Database(other),
            })?;

        if cred.revoked {
            return Err(GatewayError::Unauthorized);
        }

        // Update last_used timestamp
        let now = cfgd_core::utc_now_iso8601();
        if let Err(e) = self.conn.execute(
            "UPDATE device_credentials SET last_used = ?1 WHERE device_id = ?2",
            params![&now, &cred.device_id],
        ) {
            tracing::warn!(device_id = %cred.device_id, error = %e, "failed to update credential last_used");
        }

        Ok(cred)
    }

    /// Revoke a device credential.
    pub fn revoke_device_credential(&self, device_id: &str) -> Result<(), GatewayError> {
        let rows = self.conn.execute(
            "UPDATE device_credentials SET revoked = 1 WHERE device_id = ?1",
            params![device_id],
        )?;
        if rows == 0 {
            return Err(GatewayError::NotFound(format!(
                "device credential not found: {device_id}"
            )));
        }
        Ok(())
    }

    // --- User Public Keys ---

    /// Add a public key for a user (for key-based enrollment).
    pub fn add_user_public_key(
        &self,
        username: &str,
        key_type: &str,
        public_key: &str,
        fingerprint: &str,
        label: Option<&str>,
    ) -> Result<UserPublicKey, GatewayError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = cfgd_core::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO user_public_keys (id, username, key_type, public_key, fingerprint, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![&id, username, key_type, public_key, fingerprint, label, &now],
        )?;
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

    /// List all public keys for a user.
    pub fn list_user_public_keys(
        &self,
        username: &str,
    ) -> Result<Vec<UserPublicKey>, GatewayError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, username, key_type, public_key, fingerprint, label, created_at FROM user_public_keys WHERE username = ?1 ORDER BY created_at DESC",
        )?;
        let keys = stmt
            .query_map(params![username], |row| {
                Ok(UserPublicKey {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    key_type: row.get(2)?,
                    public_key: row.get(3)?,
                    fingerprint: row.get(4)?,
                    label: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(keys)
    }

    /// Delete a user's public key by ID.
    pub fn delete_user_public_key(&self, id: &str) -> Result<(), GatewayError> {
        let rows = self
            .conn
            .execute("DELETE FROM user_public_keys WHERE id = ?1", params![id])?;
        if rows == 0 {
            return Err(GatewayError::NotFound(format!(
                "public key not found: {id}"
            )));
        }
        Ok(())
    }

    // --- Enrollment Challenges ---

    /// Create a short-lived enrollment challenge for key-based enrollment.
    /// Challenge expires after `ttl_secs` (default: 300 = 5 minutes).
    pub fn create_enrollment_challenge(
        &self,
        username: &str,
        device_id: &str,
        hostname: &str,
        os_arch: (&str, &str),
        nonce: &str,
        ttl_secs: u64,
    ) -> Result<EnrollmentChallenge, GatewayError> {
        let (os, arch) = os_arch;
        let id = uuid::Uuid::new_v4().to_string();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let created_at = cfgd_core::unix_secs_to_iso8601(now_secs);
        let expires_at = cfgd_core::unix_secs_to_iso8601(now_secs + ttl_secs);

        self.conn.execute(
            "INSERT INTO enrollment_challenges (id, username, device_id, hostname, os, arch, nonce, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![&id, username, device_id, hostname, os, arch, nonce, &created_at, &expires_at],
        )?;

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

    /// Atomically consume an enrollment challenge. Returns the challenge if valid
    /// (exists, not expired, not already consumed). Marks it as consumed.
    pub fn consume_enrollment_challenge(
        &self,
        challenge_id: &str,
    ) -> Result<EnrollmentChallenge, GatewayError> {
        let now = cfgd_core::utc_now_iso8601();

        // Single transaction: read challenge data and mark consumed atomically.
        // This prevents the case where UPDATE succeeds but the subsequent SELECT fails,
        // leaving the challenge consumed with no data returned.
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(GatewayError::Database)?;

        let challenge = tx.query_row(
            "SELECT id, username, device_id, hostname, os, arch, nonce, created_at, expires_at, consumed
             FROM enrollment_challenges WHERE id = ?1",
            params![challenge_id],
            |row| {
                let consumed: i32 = row.get(9)?;
                Ok((EnrollmentChallenge {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    device_id: row.get(2)?,
                    hostname: row.get(3)?,
                    os: row.get(4)?,
                    arch: row.get(5)?,
                    nonce: row.get(6)?,
                    created_at: row.get(7)?,
                    expires_at: row.get(8)?,
                }, consumed))
            },
        );

        let (challenge, consumed) = match challenge {
            Ok(pair) => pair,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(GatewayError::NotFound(format!(
                    "enrollment challenge not found: {challenge_id}"
                )));
            }
            Err(e) => return Err(GatewayError::Database(e)),
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

        tx.execute(
            "UPDATE enrollment_challenges SET consumed = 1 WHERE id = ?1",
            params![challenge_id],
        )
        .map_err(GatewayError::Database)?;

        tx.commit().map_err(GatewayError::Database)?;

        Ok(challenge)
    }

    /// Delete drift and checkin events older than `max_age_days`.
    pub fn cleanup_old_events(&self, max_age_days: u32) -> Result<usize, GatewayError> {
        let cutoff = {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let cutoff_secs = now.as_secs().saturating_sub(max_age_days as u64 * 86400);
            cfgd_core::unix_secs_to_iso8601(cutoff_secs)
        };

        let drift_deleted = self.conn.execute(
            "DELETE FROM drift_events WHERE timestamp < ?1",
            params![&cutoff],
        )?;
        let checkin_deleted = self.conn.execute(
            "DELETE FROM checkin_events WHERE timestamp < ?1",
            params![&cutoff],
        )?;

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

    /// Delete all data from every table except `schema_version`.
    /// Used by the admin reset endpoint so E2E runs start with a clean DB
    /// without destroying the SQLite file (avoids Longhorn volume corruption).
    pub fn reset_data(&self) -> Result<usize, GatewayError> {
        // Order matters: child tables with FK references first.
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
            let deleted = self.conn.execute(&format!("DELETE FROM {table}"), [])?;
            total += deleted;
        }
        if total > 0 {
            tracing::info!(rows_deleted = total, "admin reset: all data wiped");
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> ServerDb {
        ServerDb::open(":memory:").expect("failed to open in-memory db")
    }

    #[test]
    fn register_and_get_device() {
        let db = test_db();
        let device = db
            .register_device("dev-1", "workstation-1", "linux", "x86_64", "abc123", None)
            .expect("register failed");
        assert_eq!(device.id, "dev-1");
        assert_eq!(device.hostname, "workstation-1");
        assert_eq!(device.status, DeviceStatus::Healthy);
        assert!(device.desired_config.is_none());

        let fetched = db.get_device("dev-1").expect("get failed");
        assert_eq!(fetched.hostname, "workstation-1");
    }

    #[test]
    fn list_devices_empty() {
        let db = test_db();
        let devices = db.list_devices().expect("list failed");
        assert!(devices.is_empty());
    }

    #[test]
    fn update_checkin() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .expect("register failed");
        db.update_checkin("dev-1", "hash2", None)
            .expect("update failed");
        let device = db.get_device("dev-1").expect("get failed");
        assert_eq!(device.config_hash, "hash2");
    }

    #[test]
    fn update_checkin_not_found() {
        let db = test_db();
        let result = db.update_checkin("nonexistent", "hash", None);
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn drift_events() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .expect("register failed");
        let event = db
            .record_drift_event("dev-1", "package vim drifted")
            .expect("record failed");
        assert_eq!(event.device_id, "dev-1");

        let device = db.get_device("dev-1").expect("get failed");
        assert_eq!(device.status, DeviceStatus::Drifted);

        let events = db.list_drift_events("dev-1").expect("list failed");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].details, "package vim drifted");
    }

    #[test]
    fn drift_event_for_nonexistent_device() {
        let db = test_db();
        let result = db.record_drift_event("nonexistent", "details");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn register_device_upsert() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .expect("first register");
        let device = db
            .register_device("dev-1", "ws-1-renamed", "linux", "x86_64", "hash2", None)
            .expect("second register");
        assert_eq!(device.hostname, "ws-1-renamed");
        assert_eq!(device.config_hash, "hash2");

        let all = db.list_devices().expect("list");
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn set_and_get_device_config() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .expect("register failed");

        let config = serde_json::json!({"packages": ["vim", "git"]});
        db.set_device_config("dev-1", &config)
            .expect("set config failed");

        let device = db.get_device("dev-1").expect("get failed");
        assert_eq!(device.desired_config, Some(config));
    }

    #[test]
    fn set_config_not_found() {
        let db = test_db();
        let config = serde_json::json!({"packages": []});
        let result = db.set_device_config("nonexistent", &config);
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn record_and_list_checkin_events() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .expect("register failed");

        let event = db
            .record_checkin("dev-1", "hash1", false)
            .expect("record checkin failed");
        assert_eq!(event.device_id, "dev-1");
        assert!(!event.config_changed);

        db.record_checkin("dev-1", "hash2", true)
            .expect("record checkin 2 failed");

        let events = db
            .list_checkin_events("dev-1")
            .expect("list checkin events failed");
        assert_eq!(events.len(), 2);
        let changed_count = events.iter().filter(|e| e.config_changed).count();
        assert_eq!(changed_count, 1);
    }

    #[test]
    fn list_checkin_events_nonexistent_device() {
        let db = test_db();
        let result = db.list_checkin_events("nonexistent");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn fleet_events_combined() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .expect("register failed");

        db.record_checkin("dev-1", "hash1", false)
            .expect("checkin failed");
        db.record_drift_event(
            "dev-1",
            r#"[{"field":"pkg","expected":"vim","actual":"missing"}]"#,
        )
        .expect("drift failed");
        db.record_checkin("dev-1", "hash2", true)
            .expect("checkin 2 failed");

        let events = db.list_fleet_events(100).expect("list fleet events failed");
        assert_eq!(events.len(), 3);

        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"drift"));
        assert!(types.contains(&"checkin") || types.contains(&"config-changed"));
    }

    #[test]
    fn fleet_events_empty() {
        let db = test_db();
        let events = db.list_fleet_events(100).expect("list fleet events failed");
        assert!(events.is_empty());
    }

    #[test]
    fn force_reconcile() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .expect("register failed");

        db.set_force_reconcile("dev-1")
            .expect("force reconcile failed");

        let device = db.get_device("dev-1").expect("get failed");
        assert_eq!(device.status, DeviceStatus::PendingReconcile);
    }

    #[test]
    fn force_reconcile_not_found() {
        let db = test_db();
        let result = db.set_force_reconcile("nonexistent");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn compliance_summary_stored_on_register() {
        let db = test_db();
        let summary = serde_json::json!({"compliant": 5, "warning": 1, "violation": 0});
        let device = db
            .register_device("dev-c", "ws-c", "linux", "x86_64", "hash1", Some(&summary))
            .expect("register failed");
        assert_eq!(device.compliance_summary, Some(summary));
    }

    #[test]
    fn compliance_summary_stored_on_checkin_update() {
        let db = test_db();
        db.register_device("dev-c2", "ws-c2", "linux", "x86_64", "hash1", None)
            .expect("register failed");

        let summary = serde_json::json!({"compliant": 10, "warning": 0, "violation": 2});
        db.update_checkin("dev-c2", "hash2", Some(&summary))
            .expect("update failed");

        let device = db.get_device("dev-c2").expect("get failed");
        assert_eq!(device.config_hash, "hash2");
        assert_eq!(device.compliance_summary, Some(summary));
    }

    #[test]
    fn compliance_summary_null_when_not_provided() {
        let db = test_db();
        let device = db
            .register_device("dev-c3", "ws-c3", "linux", "x86_64", "hash1", None)
            .expect("register failed");
        assert!(device.compliance_summary.is_none());
    }

    // --- Bootstrap Token Tests ---

    fn future_expiry() -> String {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        cfgd_core::unix_secs_to_iso8601(now_secs + 3600) // 1 hour from now
    }

    fn past_expiry() -> String {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        cfgd_core::unix_secs_to_iso8601(now_secs.saturating_sub(3600)) // 1 hour ago
    }

    #[test]
    fn create_and_consume_bootstrap_token() {
        let db = test_db();
        let expires_at = future_expiry();

        let token = db
            .create_bootstrap_token("hash123", "jdoe", Some("acme"), &expires_at)
            .expect("create failed");
        assert_eq!(token.username, "jdoe");
        assert_eq!(token.team.as_deref(), Some("acme"));
        assert!(token.used_at.is_none());

        let consumed = db
            .validate_and_consume_bootstrap_token("hash123", "dev-1")
            .expect("validate_and_consume failed");
        assert_eq!(consumed.username, "jdoe");
        assert!(consumed.used_at.is_some());
        assert_eq!(consumed.used_by_device.as_deref(), Some("dev-1"));
    }

    #[test]
    fn consume_expired_token_fails() {
        let db = test_db();
        let expires_at = past_expiry();

        db.create_bootstrap_token("hash_expired", "jdoe", None, &expires_at)
            .expect("create failed");

        let result = db.validate_and_consume_bootstrap_token("hash_expired", "dev-1");
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

    #[test]
    fn consume_used_token_fails() {
        let db = test_db();
        let expires_at = future_expiry();

        db.create_bootstrap_token("hash_used", "jdoe", None, &expires_at)
            .expect("create failed");
        db.validate_and_consume_bootstrap_token("hash_used", "dev-1")
            .expect("first consume succeeded");

        // Second consume should fail — token already used
        let result = db.validate_and_consume_bootstrap_token("hash_used", "dev-2");
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

    #[test]
    fn consume_nonexistent_token_fails() {
        let db = test_db();
        let result = db.validate_and_consume_bootstrap_token("no_such_hash", "dev-1");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::Unauthorized),
            "expected Unauthorized, got: {err}"
        );
    }

    #[test]
    fn list_bootstrap_tokens() {
        let db = test_db();
        let expires_at = future_expiry();

        db.create_bootstrap_token("h1", "user1", None, &expires_at)
            .expect("create 1 failed");
        db.create_bootstrap_token("h2", "user2", Some("team"), &expires_at)
            .expect("create 2 failed");

        let tokens = db.list_bootstrap_tokens().expect("list failed");
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn delete_bootstrap_token() {
        let db = test_db();
        let expires_at = future_expiry();

        let token = db
            .create_bootstrap_token("h_del", "jdoe", None, &expires_at)
            .expect("create failed");

        db.delete_bootstrap_token(&token.id).expect("delete failed");

        let tokens = db.list_bootstrap_tokens().expect("list failed");
        assert!(tokens.is_empty());
    }

    #[test]
    fn delete_nonexistent_token_fails() {
        let db = test_db();
        let result = db.delete_bootstrap_token("no-such-id");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    // --- Device Credential Tests ---

    #[test]
    fn create_and_validate_device_credential() {
        let db = test_db();

        db.create_device_credential("dev-1", "keyhash1", "jdoe", Some("acme"))
            .expect("create failed");

        let cred = db
            .validate_device_credential("keyhash1")
            .expect("validate failed");
        assert_eq!(cred.device_id, "dev-1");
        assert_eq!(cred.username, "jdoe");
        assert_eq!(cred.team.as_deref(), Some("acme"));
        assert!(!cred.revoked);
    }

    #[test]
    fn validate_nonexistent_credential_fails() {
        let db = test_db();
        let result = db.validate_device_credential("no_such_hash");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::Unauthorized),
            "expected Unauthorized, got: {err}"
        );
    }

    #[test]
    fn revoked_credential_fails_validation() {
        let db = test_db();

        db.create_device_credential("dev-1", "keyhash_rev", "jdoe", None)
            .expect("create failed");
        db.revoke_device_credential("dev-1").expect("revoke failed");

        let result = db.validate_device_credential("keyhash_rev");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::Unauthorized),
            "expected Unauthorized, got: {err}"
        );
    }

    #[test]
    fn credential_upsert_on_re_enrollment() {
        let db = test_db();

        db.create_device_credential("dev-1", "key_old", "jdoe", None)
            .expect("create 1 failed");
        db.create_device_credential("dev-1", "key_new", "jdoe", Some("newteam"))
            .expect("create 2 (upsert) failed");

        // Old key should no longer work
        let result = db.validate_device_credential("key_old");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::Unauthorized),
            "expected Unauthorized, got: {err}"
        );

        // New key should work
        let cred = db
            .validate_device_credential("key_new")
            .expect("validate new failed");
        assert_eq!(cred.device_id, "dev-1");
        assert_eq!(cred.team.as_deref(), Some("newteam"));
    }

    #[test]
    fn revoke_nonexistent_credential_fails() {
        let db = test_db();
        let result = db.revoke_device_credential("no-such-device");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    // --- User Public Key Tests ---

    #[test]
    fn add_and_list_user_public_keys() {
        let db = test_db();

        let key = db
            .add_user_public_key(
                "jdoe",
                "ssh",
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG... jdoe@work",
                "SHA256:abc123def456",
                Some("work laptop"),
            )
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
        .expect("add gpg key failed");

        let keys = db.list_user_public_keys("jdoe").expect("list failed");
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn list_user_public_keys_empty() {
        let db = test_db();
        let keys = db.list_user_public_keys("nobody").expect("list failed");
        assert!(keys.is_empty());
    }

    #[test]
    fn delete_user_public_key() {
        let db = test_db();

        let key = db
            .add_user_public_key("jdoe", "ssh", "ssh-ed25519 AAAA...", "SHA256:xyz", None)
            .expect("add failed");

        db.delete_user_public_key(&key.id).expect("delete failed");

        let keys = db.list_user_public_keys("jdoe").expect("list failed");
        assert!(keys.is_empty());
    }

    #[test]
    fn delete_nonexistent_public_key_fails() {
        let db = test_db();
        let result = db.delete_user_public_key("no-such-id");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    // --- Enrollment Challenge Tests ---

    #[test]
    fn create_and_consume_enrollment_challenge() {
        let db = test_db();

        let challenge = db
            .create_enrollment_challenge(
                "jdoe",
                "dev-1",
                "macbook",
                ("macos", "aarch64"),
                "random-nonce-abc",
                300,
            )
            .expect("create challenge failed");
        assert_eq!(challenge.username, "jdoe");
        assert_eq!(challenge.nonce, "random-nonce-abc");

        let consumed = db
            .consume_enrollment_challenge(&challenge.id)
            .expect("consume failed");
        assert_eq!(consumed.username, "jdoe");
        assert_eq!(consumed.device_id, "dev-1");
    }

    #[test]
    fn consume_challenge_twice_fails() {
        let db = test_db();

        let challenge = db
            .create_enrollment_challenge(
                "jdoe",
                "dev-1",
                "macbook",
                ("macos", "aarch64"),
                "nonce1",
                300,
            )
            .expect("create failed");

        db.consume_enrollment_challenge(&challenge.id)
            .expect("first consume ok");

        let result = db.consume_enrollment_challenge(&challenge.id);
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

    #[test]
    fn consume_expired_challenge_fails() {
        let db = test_db();

        // Insert a challenge with an already-past expires_at
        let id = uuid::Uuid::new_v4().to_string();
        let past = past_expiry();
        db.conn.execute(
            "INSERT INTO enrollment_challenges (id, username, device_id, hostname, os, arch, nonce, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![&id, "jdoe", "dev-1", "macbook", "macos", "aarch64", "nonce2", &past, &past],
        ).expect("insert failed");

        let result = db.consume_enrollment_challenge(&id);
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

    #[test]
    fn consume_nonexistent_challenge_fails() {
        let db = test_db();
        let result = db.consume_enrollment_challenge("no-such-id");
        let err = result.unwrap_err();
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn schema_version_is_set() {
        let db = test_db();
        assert_eq!(db.schema_version(), MIGRATIONS.len());
    }

    #[test]
    fn migration_upgrades_v1_database() {
        // Simulate a pre-migration-1 database: create tables without desired_config
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upgrade-test.db");
        let path_str = path.to_str().unwrap();

        // Create the v1 schema manually (devices table without desired_config)
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
            // Insert a device to verify data survives migration
            conn.execute(
                "INSERT INTO devices (id, hostname, os, arch, last_checkin, config_hash, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params!["dev-1", "host-1", "linux", "amd64", "2026-01-01T00:00:00Z", "abc", "healthy"],
            ).unwrap();
        }

        // Open with ServerDb — should run migration 1 and add desired_config
        let db = ServerDb::open(path_str).unwrap();
        assert_eq!(db.schema_version(), MIGRATIONS.len());

        // Verify the device is still there and desired_config is queryable
        let device = db.get_device("dev-1").unwrap();
        assert_eq!(device.hostname, "host-1");
        assert!(device.desired_config.is_none());
    }
}

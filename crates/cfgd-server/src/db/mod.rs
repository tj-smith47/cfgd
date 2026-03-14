use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::errors::ServerError;

/// Device status as a proper enum with well-defined states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

    pub fn from_str(s: &str) -> Self {
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
#[serde(rename_all = "kebab-case")]
pub struct Device {
    pub id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub last_checkin: String,
    pub config_hash: String,
    pub status: DeviceStatus,
    pub desired_config: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DriftEvent {
    pub id: String,
    pub device_id: String,
    pub timestamp: String,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CheckinEvent {
    pub id: String,
    pub device_id: String,
    pub timestamp: String,
    pub config_hash: String,
    pub config_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FleetEvent {
    pub timestamp: String,
    pub device_id: String,
    pub event_type: String,
    pub summary: String,
}

/// A bootstrap token record — admin-created, one-time use for device enrollment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
pub struct DeviceCredential {
    pub device_id: String,
    pub username: String,
    pub team: Option<String>,
    pub created_at: String,
    pub last_used: Option<String>,
    pub revoked: bool,
}

pub struct ServerDb {
    conn: Connection,
}

impl ServerDb {
    pub fn open(path: &str) -> Result<Self, ServerError> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Self { conn };
        db.init_tables()?;
        Ok(db)
    }

    fn init_tables(&self) -> Result<(), ServerError> {
        self.conn.execute_batch(
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
            );",
        )?;

        // Migration: add desired_config column if missing (existing databases)
        let has_col: bool = self
            .conn
            .prepare("SELECT desired_config FROM devices LIMIT 0")
            .is_ok();
        if !has_col {
            self.conn
                .execute_batch("ALTER TABLE devices ADD COLUMN desired_config TEXT")?;
        }

        Ok(())
    }

    pub fn register_device(
        &self,
        id: &str,
        hostname: &str,
        os: &str,
        arch: &str,
        config_hash: &str,
    ) -> Result<Device, ServerError> {
        let now = cfgd_core::utc_now_iso8601();
        self.conn.execute(
            "INSERT INTO devices (id, hostname, os, arch, last_checkin, config_hash, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                hostname = excluded.hostname,
                os = excluded.os,
                arch = excluded.arch,
                last_checkin = excluded.last_checkin,
                config_hash = excluded.config_hash,
                status = excluded.status",
            params![
                id,
                hostname,
                os,
                arch,
                &now,
                config_hash,
                DeviceStatus::Healthy.as_str()
            ],
        )?;
        self.get_device(id)
    }

    pub fn update_checkin(&self, id: &str, config_hash: &str) -> Result<(), ServerError> {
        let now = cfgd_core::utc_now_iso8601();
        let rows = self.conn.execute(
            "UPDATE devices SET last_checkin = ?1, config_hash = ?2, status = ?3 WHERE id = ?4",
            params![&now, config_hash, DeviceStatus::Healthy.as_str(), id],
        )?;
        if rows == 0 {
            return Err(ServerError::NotFound(format!("device not found: {id}")));
        }
        Ok(())
    }

    pub fn get_device(&self, id: &str) -> Result<Device, ServerError> {
        self.conn
            .query_row(
                "SELECT id, hostname, os, arch, last_checkin, config_hash, status, desired_config FROM devices WHERE id = ?1",
                params![id],
                |row| {
                    let config_str: Option<String> = row.get(7)?;
                    let desired_config = config_str
                        .and_then(|s| serde_json::from_str(&s).ok());
                    let status_str: String = row.get(6)?;
                    Ok(Device {
                        id: row.get(0)?,
                        hostname: row.get(1)?,
                        os: row.get(2)?,
                        arch: row.get(3)?,
                        last_checkin: row.get(4)?,
                        config_hash: row.get(5)?,
                        status: DeviceStatus::from_str(&status_str),
                        desired_config,
                    })
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ServerError::NotFound(format!("device not found: {id}"))
                }
                other => ServerError::Database(other),
            })
    }

    pub fn set_device_config(
        &self,
        id: &str,
        config: &serde_json::Value,
    ) -> Result<(), ServerError> {
        let config_str = serde_json::to_string(config)
            .map_err(|e| ServerError::Internal(format!("failed to serialize config: {e}")))?;
        let rows = self.conn.execute(
            "UPDATE devices SET desired_config = ?1 WHERE id = ?2",
            params![&config_str, id],
        )?;
        if rows == 0 {
            return Err(ServerError::NotFound(format!("device not found: {id}")));
        }
        Ok(())
    }

    pub fn list_devices(&self) -> Result<Vec<Device>, ServerError> {
        self.list_devices_paginated(1000, 0)
    }

    pub fn list_devices_paginated(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Device>, ServerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hostname, os, arch, last_checkin, config_hash, status, desired_config FROM devices ORDER BY hostname LIMIT ?1 OFFSET ?2",
        )?;
        let devices = stmt
            .query_map(params![limit, offset], |row| {
                let config_str: Option<String> = row.get(7)?;
                let desired_config = config_str.and_then(|s| serde_json::from_str(&s).ok());
                let status_str: String = row.get(6)?;
                Ok(Device {
                    id: row.get(0)?,
                    hostname: row.get(1)?,
                    os: row.get(2)?,
                    arch: row.get(3)?,
                    last_checkin: row.get(4)?,
                    config_hash: row.get(5)?,
                    status: DeviceStatus::from_str(&status_str),
                    desired_config,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(devices)
    }

    pub fn record_drift_event(
        &self,
        device_id: &str,
        details: &str,
    ) -> Result<DriftEvent, ServerError> {
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

    pub fn list_drift_events(&self, device_id: &str) -> Result<Vec<DriftEvent>, ServerError> {
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
    ) -> Result<CheckinEvent, ServerError> {
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

    pub fn list_checkin_events(&self, device_id: &str) -> Result<Vec<CheckinEvent>, ServerError> {
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

    pub fn list_fleet_events(&self, limit: u32) -> Result<Vec<FleetEvent>, ServerError> {
        self.list_fleet_events_paginated(limit, 0)
    }

    pub fn list_fleet_events_paginated(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<FleetEvent>, ServerError> {
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

    pub fn set_force_reconcile(&self, device_id: &str) -> Result<(), ServerError> {
        let rows = self.conn.execute(
            "UPDATE devices SET status = ?1 WHERE id = ?2",
            params![DeviceStatus::PendingReconcile.as_str(), device_id],
        )?;
        if rows == 0 {
            return Err(ServerError::NotFound(format!(
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
    ) -> Result<BootstrapToken, ServerError> {
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
    ) -> Result<BootstrapToken, ServerError> {
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
                Ok((Some(_), _)) => Err(ServerError::InvalidRequest(
                    "bootstrap token has already been used".to_string(),
                )),
                Ok((None, expires_at)) if expires_at < now => Err(ServerError::InvalidRequest(
                    "bootstrap token has expired".to_string(),
                )),
                Ok(_) => Err(ServerError::Internal(
                    "bootstrap token validation failed unexpectedly".to_string(),
                )),
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(ServerError::Unauthorized),
                Err(e) => Err(ServerError::Database(e)),
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
            .map_err(ServerError::Database)
    }

    /// List all bootstrap tokens (active and used).
    pub fn list_bootstrap_tokens(&self) -> Result<Vec<BootstrapToken>, ServerError> {
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
    pub fn delete_bootstrap_token(&self, id: &str) -> Result<(), ServerError> {
        let rows = self
            .conn
            .execute("DELETE FROM bootstrap_tokens WHERE id = ?1", params![id])?;
        if rows == 0 {
            return Err(ServerError::NotFound(format!(
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
    ) -> Result<DeviceCredential, ServerError> {
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
    ) -> Result<DeviceCredential, ServerError> {
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
                rusqlite::Error::QueryReturnedNoRows => ServerError::Unauthorized,
                other => ServerError::Database(other),
            })?;

        if cred.revoked {
            return Err(ServerError::Unauthorized);
        }

        // Update last_used timestamp
        let now = cfgd_core::utc_now_iso8601();
        let _ = self.conn.execute(
            "UPDATE device_credentials SET last_used = ?1 WHERE device_id = ?2",
            params![&now, &cred.device_id],
        );

        Ok(cred)
    }

    /// Revoke a device credential.
    pub fn revoke_device_credential(&self, device_id: &str) -> Result<(), ServerError> {
        let rows = self.conn.execute(
            "UPDATE device_credentials SET revoked = 1 WHERE device_id = ?1",
            params![device_id],
        )?;
        if rows == 0 {
            return Err(ServerError::NotFound(format!(
                "device credential not found: {device_id}"
            )));
        }
        Ok(())
    }

    /// Delete drift and checkin events older than `max_age_days`.
    pub fn cleanup_old_events(&self, max_age_days: u32) -> Result<usize, ServerError> {
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
                "Cleaned up old events"
            );
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
            .register_device("dev-1", "workstation-1", "linux", "x86_64", "abc123")
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
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1")
            .expect("register failed");
        db.update_checkin("dev-1", "hash2").expect("update failed");
        let device = db.get_device("dev-1").expect("get failed");
        assert_eq!(device.config_hash, "hash2");
    }

    #[test]
    fn update_checkin_not_found() {
        let db = test_db();
        let result = db.update_checkin("nonexistent", "hash");
        assert!(result.is_err());
    }

    #[test]
    fn drift_events() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1")
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
        assert!(result.is_err());
    }

    #[test]
    fn register_device_upsert() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1")
            .expect("first register");
        let device = db
            .register_device("dev-1", "ws-1-renamed", "linux", "x86_64", "hash2")
            .expect("second register");
        assert_eq!(device.hostname, "ws-1-renamed");
        assert_eq!(device.config_hash, "hash2");

        let all = db.list_devices().expect("list");
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn set_and_get_device_config() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1")
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
        assert!(result.is_err());
    }

    #[test]
    fn record_and_list_checkin_events() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1")
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
        assert!(result.is_err());
    }

    #[test]
    fn fleet_events_combined() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1")
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
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1")
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
        assert!(result.is_err());
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
        assert!(result.is_err());
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
        assert!(result.is_err());
    }

    #[test]
    fn consume_nonexistent_token_fails() {
        let db = test_db();
        let result = db.validate_and_consume_bootstrap_token("no_such_hash", "dev-1");
        assert!(result.is_err());
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
        assert!(result.is_err());
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
        assert!(result.is_err());
    }

    #[test]
    fn revoked_credential_fails_validation() {
        let db = test_db();

        db.create_device_credential("dev-1", "keyhash_rev", "jdoe", None)
            .expect("create failed");
        db.revoke_device_credential("dev-1").expect("revoke failed");

        let result = db.validate_device_credential("keyhash_rev");
        assert!(result.is_err());
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
        assert!(result.is_err());

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
        assert!(result.is_err());
    }
}

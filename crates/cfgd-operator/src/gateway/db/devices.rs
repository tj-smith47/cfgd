use rusqlite::{Connection, params};

use super::ServerDb;
use super::types::{Device, DeviceStatus};
use super::{acquire_reader, acquire_writer, spawn_blocking_db};
use crate::gateway::errors::GatewayError;

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
        last_pushed_at: row.get(9)?,
        generation: row.get(10)?,
    })
}

pub fn register_device_tx(
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

pub fn update_checkin_tx(
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

pub fn get_device_tx(conn: &Connection, id: &str) -> Result<Device, GatewayError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, hostname, os, arch, last_checkin, config_hash, status, desired_config, compliance_summary, last_pushed_at, generation FROM devices WHERE id = ?1",
    )?;
    stmt.query_row(params![id], map_device_row)
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                GatewayError::NotFound(format!("device not found: {id}"))
            }
            other => GatewayError::Database(other),
        })
}

pub fn set_device_config_tx(
    conn: &Connection,
    id: &str,
    config: &serde_json::Value,
) -> Result<(), GatewayError> {
    let config_str = serde_json::to_string(config)
        .map_err(|e| GatewayError::Internal(format!("failed to serialize config: {e}")))?;
    let now = cfgd_core::utc_now_iso8601();
    // Bump generation + last_pushed_at on EVERY accepted push so an identical
    // re-push is still observable on the device read path (config_hash alone
    // can't distinguish it from "nothing happened").
    let mut stmt = conn.prepare_cached(
        "UPDATE devices SET desired_config = ?1, last_pushed_at = ?2, generation = generation + 1 WHERE id = ?3",
    )?;
    let rows = stmt.execute(params![&config_str, &now, id])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!("device not found: {id}")));
    }
    Ok(())
}

pub fn list_devices_paginated_tx(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> Result<Vec<Device>, GatewayError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, hostname, os, arch, last_checkin, config_hash, status, desired_config, compliance_summary, last_pushed_at, generation FROM devices ORDER BY hostname LIMIT ?1 OFFSET ?2",
    )?;
    let devices = stmt
        .query_map(params![limit, offset], map_device_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(devices)
}

pub fn set_force_reconcile_tx(conn: &Connection, device_id: &str) -> Result<(), GatewayError> {
    let mut stmt = conn.prepare_cached("UPDATE devices SET status = ?1 WHERE id = ?2")?;
    let rows = stmt.execute(params![DeviceStatus::PendingReconcile.as_str(), device_id])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!(
            "device not found: {device_id}"
        )));
    }
    Ok(())
}

pub fn get_fleet_status_tx(
    conn: &Connection,
) -> Result<crate::gateway::fleet::FleetStatus, GatewayError> {
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

    Ok(crate::gateway::fleet::FleetStatus {
        total_devices,
        healthy,
        drifted,
        offline,
    })
}

impl ServerDb {
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

    pub async fn get_fleet_status(
        &self,
    ) -> Result<crate::gateway::fleet::FleetStatus, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            get_fleet_status_tx(&conn)
        })
        .await
    }

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
}

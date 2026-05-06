use rusqlite::{Connection, params};

use super::ServerDb;
use super::devices::get_device_tx;
use super::types::{CheckinEvent, DeviceStatus, DriftEvent, FleetEvent};
use super::{acquire_reader, acquire_writer, spawn_blocking_db};
use crate::gateway::errors::GatewayError;

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

pub fn record_drift_event_tx(
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

pub fn list_drift_events_tx(
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

pub fn record_checkin_tx(
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

pub fn list_checkin_events_tx(
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

pub fn list_fleet_events_paginated_tx(
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

impl ServerDb {
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
}

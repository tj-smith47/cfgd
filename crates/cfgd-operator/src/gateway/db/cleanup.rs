use rusqlite::{Connection, params};

use super::ServerDb;
use super::{acquire_writer, spawn_blocking_db};
use crate::gateway::errors::GatewayError;

pub fn cleanup_old_events_tx(conn: &Connection, max_age_days: u32) -> Result<usize, GatewayError> {
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

pub fn reset_data_tx(conn: &Connection) -> Result<usize, GatewayError> {
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

impl ServerDb {
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
}

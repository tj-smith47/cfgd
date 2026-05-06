use rusqlite::{Connection, params};

use super::ServerDb;
use super::types::DeviceCredential;
use super::{acquire_writer, spawn_blocking_db};
use crate::gateway::errors::GatewayError;

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

pub fn create_device_credential_tx(
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

pub fn validate_device_credential_tx(
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

pub fn revoke_device_credential_tx(conn: &Connection, device_id: &str) -> Result<(), GatewayError> {
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

impl ServerDb {
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
}

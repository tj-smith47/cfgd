use rusqlite::{Connection, params};

use super::ServerDb;
use super::types::BootstrapToken;
use super::{acquire_reader, acquire_writer, spawn_blocking_db};
use crate::gateway::errors::GatewayError;

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

pub fn create_bootstrap_token_tx(
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

pub fn validate_and_consume_bootstrap_token_tx(
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

pub fn list_bootstrap_tokens_tx(conn: &Connection) -> Result<Vec<BootstrapToken>, GatewayError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, username, team, created_at, expires_at, used_at, used_by_device FROM bootstrap_tokens ORDER BY created_at DESC",
    )?;
    let tokens = stmt
        .query_map([], map_bootstrap_token_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(tokens)
}

pub fn delete_bootstrap_token_tx(conn: &Connection, id: &str) -> Result<(), GatewayError> {
    let mut stmt = conn.prepare_cached("DELETE FROM bootstrap_tokens WHERE id = ?1")?;
    let rows = stmt.execute(params![id])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!(
            "bootstrap token not found: {id}"
        )));
    }
    Ok(())
}

impl ServerDb {
    pub async fn list_bootstrap_tokens(&self) -> Result<Vec<BootstrapToken>, GatewayError> {
        let pool = self.readers.clone();
        let metrics = self.metrics.clone();
        spawn_blocking_db(move || {
            let conn = acquire_reader(&pool, &metrics)?;
            list_bootstrap_tokens_tx(&conn)
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
}

use rusqlite::{Connection, params};

use super::ServerDb;
use super::types::UserPublicKey;
use super::{acquire_reader, acquire_writer, spawn_blocking_db};
use crate::gateway::errors::GatewayError;

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

pub fn add_user_public_key_tx(
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

pub fn list_user_public_keys_tx(
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

pub fn delete_user_public_key_tx(conn: &Connection, id: &str) -> Result<(), GatewayError> {
    let mut stmt = conn.prepare_cached("DELETE FROM user_public_keys WHERE id = ?1")?;
    let rows = stmt.execute(params![id])?;
    if rows == 0 {
        return Err(GatewayError::NotFound(format!(
            "public key not found: {id}"
        )));
    }
    Ok(())
}

impl ServerDb {
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
}

use rusqlite::{Connection, params};

use super::ServerDb;
use super::types::EnrollmentChallenge;
use super::{acquire_writer, spawn_blocking_db};
use crate::gateway::errors::GatewayError;

pub fn create_enrollment_challenge_tx(
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

pub fn consume_enrollment_challenge_tx(
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

impl ServerDb {
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
}

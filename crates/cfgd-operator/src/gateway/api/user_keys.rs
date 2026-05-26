//! Admin user-public-key management endpoints.
//!
//! Add / list / delete SSH and GPG public keys per user. Keys registered
//! here are consumed by the key-based enrollment flow in `enroll.rs`.

use super::*;

fn validate_ssh_public_key(key: &str) -> Result<(), GatewayError> {
    if key.contains('\n') || key.contains('\r') {
        return Err(GatewayError::InvalidRequest(
            "public_key must be a single line (no CR/LF)".to_string(),
        ));
    }
    const SSH_KEY_PREFIXES: &[&str] = &[
        "ssh-ed25519",
        "ssh-rsa",
        "ssh-dss",
        "ecdsa-sha2-nistp256",
        "ecdsa-sha2-nistp384",
        "ecdsa-sha2-nistp521",
        "sk-ssh-ed25519@openssh.com",
        "sk-ecdsa-sha2-nistp256@openssh.com",
    ];
    let first = key.split_whitespace().next().unwrap_or("");
    if !SSH_KEY_PREFIXES.contains(&first) {
        return Err(GatewayError::InvalidRequest(format!(
            "public_key must start with a recognized OpenSSH key type (got '{first}')"
        )));
    }
    if key.split_whitespace().nth(1).is_none() {
        return Err(GatewayError::InvalidRequest(
            "public_key missing base64 payload".to_string(),
        ));
    }
    Ok(())
}

fn validate_gpg_public_key(key: &str) -> Result<(), GatewayError> {
    const ARMOR_HEADER: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----";
    const ARMOR_FOOTER: &str = "-----END PGP PUBLIC KEY BLOCK-----";
    if !key.contains(ARMOR_HEADER) || !key.contains(ARMOR_FOOTER) {
        return Err(GatewayError::InvalidRequest(
            "public_key must be ASCII-armored GPG key (BEGIN/END PGP PUBLIC KEY BLOCK)".to_string(),
        ));
    }
    Ok(())
}

pub(super) async fn add_user_key(
    State(state): State<SharedState>,
    Path(username): Path<String>,
    Json(req): Json<AddKeyRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    validate_username(&username)?;
    if !matches!(req.key_type.as_str(), "ssh" | "gpg") {
        return Err(GatewayError::InvalidRequest(
            "key_type must be 'ssh' or 'gpg'".to_string(),
        ));
    }
    if req.public_key.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "public_key must not be empty".to_string(),
        ));
    }
    if req.fingerprint.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "fingerprint must not be empty".to_string(),
        ));
    }

    match req.key_type.as_str() {
        "ssh" => validate_ssh_public_key(&req.public_key)?,
        "gpg" => validate_gpg_public_key(&req.public_key)?,
        _ => unreachable!("key_type validated above"),
    }

    let key = state
        .db
        .add_user_public_key(
            &username,
            &req.key_type,
            &req.public_key,
            &req.fingerprint,
            req.label.as_deref(),
        )
        .await?;

    tracing::info!(
        username = %username,
        key_type = %req.key_type,
        fingerprint = %req.fingerprint,
        "public key added"
    );

    Ok((StatusCode::CREATED, Json(key)))
}

/// Admin: list all public keys for a user.
pub(super) async fn list_user_keys(
    State(state): State<SharedState>,
    Path(username): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    let keys = state.db.list_user_public_keys(&username).await?;
    Ok(Json(keys))
}

/// Admin: delete a user's public key.
pub(super) async fn delete_user_key(
    State(state): State<SharedState>,
    Path((username, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, GatewayError> {
    state.db.delete_user_public_key(&id).await?;

    tracing::info!(username = %username, key_id = %id, "public key deleted");

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_msg(err: GatewayError) -> String {
        match err {
            GatewayError::InvalidRequest(m) => m,
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn ssh_accepts_valid_ed25519() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleBase64Payload user@host";
        assert!(validate_ssh_public_key(key).is_ok());
    }

    #[test]
    fn ssh_rejects_multiline_key() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample user@host\nssh-ed25519 AAAA otherUser@host";
        let err = validate_ssh_public_key(key).expect_err("multi-line key should be rejected");
        assert!(err_msg(err).contains("single line"));
    }

    #[test]
    fn ssh_rejects_carriage_return() {
        let key = "ssh-ed25519 AAAA user@host\r";
        let err = validate_ssh_public_key(key).expect_err("CR should be rejected");
        assert!(err_msg(err).contains("single line"));
    }

    #[test]
    fn ssh_rejects_unknown_prefix() {
        let key = "ssh-magic AAAA user@host";
        let err = validate_ssh_public_key(key).expect_err("unknown prefix should be rejected");
        let m = err_msg(err);
        assert!(m.contains("recognized OpenSSH key type"));
        assert!(m.contains("ssh-magic"));
    }

    #[test]
    fn ssh_rejects_missing_payload() {
        let key = "ssh-ed25519";
        let err = validate_ssh_public_key(key).expect_err("missing payload should be rejected");
        assert!(err_msg(err).contains("base64 payload"));
    }

    #[test]
    fn gpg_accepts_valid_armored_block() {
        let key = "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmDMEY...payload...\n-----END PGP PUBLIC KEY BLOCK-----\n";
        assert!(validate_gpg_public_key(key).is_ok());
    }

    #[test]
    fn gpg_rejects_without_armor() {
        let key = "mDMEY...payload...without armor markers";
        let err = validate_gpg_public_key(key).expect_err("missing armor should be rejected");
        assert!(err_msg(err).contains("ASCII-armored GPG key"));
    }

    #[test]
    fn gpg_rejects_missing_footer() {
        let key = "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmDMEY...payload...\n";
        let err = validate_gpg_public_key(key).expect_err("missing footer should be rejected");
        assert!(err_msg(err).contains("ASCII-armored GPG key"));
    }
}

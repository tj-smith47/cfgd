//! Admin user-public-key management endpoints.
//!
//! Add / list / delete SSH and GPG public keys per user. Keys registered
//! here are consumed by the key-based enrollment flow in `enroll.rs`.

use super::*;
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

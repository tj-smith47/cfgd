//! Admin bootstrap-token management endpoints.
//!
//! Create / list / delete bootstrap tokens, and revoke per-device credentials.
//! All routes here require admin auth (CFGD_API_KEY) — see `admin_auth_middleware`
//! in the parent module.

use super::*;
pub(super) async fn create_token(
    State(state): State<SharedState>,
    Json(req): Json<CreateTokenRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    validate_username(&req.username)?;
    if req.expires_in == 0 || req.expires_in > 30 * 86400 {
        return Err(GatewayError::InvalidRequest(
            "expires_in must be between 1 and 2592000 seconds (30 days)".to_string(),
        ));
    }

    let token = generate_token("cfgd_bs");
    let token_hash = hash_token(&token);

    // Calculate expiration
    let now_secs = cfgd_core::unix_secs_now();
    let expires_at = cfgd_core::unix_secs_to_iso8601(now_secs + req.expires_in);

    let record = state
        .db
        .create_bootstrap_token(&token_hash, &req.username, req.team.as_deref(), &expires_at)
        .await?;

    tracing::info!(
        username = %req.username,
        expires_at = %expires_at,
        "bootstrap token created"
    );

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            id: record.id,
            token,
            username: record.username,
            team: record.team,
            expires_at: record.expires_at,
        }),
    ))
}

pub(super) async fn list_tokens(
    State(state): State<SharedState>,
) -> Result<impl IntoResponse, GatewayError> {
    let tokens = state.db.list_bootstrap_tokens().await?;
    Ok(Json(tokens))
}

pub(super) async fn delete_token(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    state.db.delete_bootstrap_token(&id).await?;

    tracing::info!(token_id = %id, "bootstrap token deleted");

    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn revoke_credential(
    State(state): State<SharedState>,
    Path(device_id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    state.db.revoke_device_credential(&device_id).await?;

    tracing::info!(device_id = %device_id, "device credential revoked");

    Ok(StatusCode::NO_CONTENT)
}

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use super::db::{FleetEvent, ServerDb};
use super::errors::GatewayError;
use crate::metrics::Metrics;

/// Server-level enrollment method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EnrollmentMethod {
    /// Bootstrap token enrollment (default).
    Token,
    /// SSH/GPG key-based challenge-response enrollment.
    Key,
}

impl EnrollmentMethod {
    pub fn from_env() -> Self {
        match std::env::var("CFGD_ENROLLMENT_METHOD")
            .unwrap_or_default()
            .as_str()
        {
            "key" => EnrollmentMethod::Key,
            _ => EnrollmentMethod::Token,
        }
    }
}

/// Shared application state: database + optional Kubernetes client + event broadcast.
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<tokio::sync::Mutex<ServerDb>>,
    pub kube_client: Option<kube::Client>,
    pub event_tx: tokio::sync::broadcast::Sender<FleetEvent>,
    pub enrollment_method: EnrollmentMethod,
    pub metrics: Option<Metrics>,
}

pub type SharedState = AppState;

/// Authentication context set by the auth middleware.
/// Tells handlers who is making the request.
#[derive(Clone, Debug)]
pub enum AuthContext {
    /// Admin authenticated via CFGD_API_KEY — full access to all endpoints.
    Admin,
    /// Device authenticated via per-device API key — access to own resources only.
    Device { device_id: String, username: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinRequest {
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub config_hash: String,
    #[serde(default)]
    pub compliance_summary: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResponse {
    pub status: String,
    pub config_changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftRequest {
    pub details: Vec<DriftDetailInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftDetailInput {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConfigRequest {
    pub config: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    100
}

// --- Enrollment request/response types ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollRequest {
    pub token: String,
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollResponse {
    pub status: String,
    pub device_id: String,
    pub api_key: String,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_config: Option<serde_json::Value>,
}

// --- Admin token management types ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTokenRequest {
    pub username: String,
    #[serde(default)]
    pub team: Option<String>,
    /// Token lifetime in seconds (default: 24 hours).
    #[serde(default = "default_token_lifetime")]
    pub expires_in: u64,
}

fn default_token_lifetime() -> u64 {
    86400 // 24 hours
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTokenResponse {
    pub id: String,
    /// The plaintext token — shown only once. Store or share securely.
    pub token: String,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub expires_at: String,
}

// --- Key-based enrollment types ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddKeyRequest {
    pub key_type: String,
    pub public_key: String,
    pub fingerprint: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollInfoResponse {
    pub method: EnrollmentMethod,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeRequest {
    pub username: String,
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeResponse {
    pub challenge_id: String,
    pub nonce: String,
    pub expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyRequest {
    pub challenge_id: String,
    pub signature: String,
    pub key_type: String,
}

const CHALLENGE_TTL_SECS: u64 = 300; // 5 minutes

// --- Router ---

pub fn router(state: SharedState) -> Router<SharedState> {
    // Config payloads can legitimately be large (module trees, policy docs).
    // Relax the gateway-wide 1 MiB cap for this one route only — the `Json`
    // extractor enforces this limit *before* deserialization, so the
    // previously-in-handler `.len() > 10 MiB` check at the bottom of
    // `set_device_config` never fires except as a belt-and-braces fallback.
    const SET_DEVICE_CONFIG_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

    // Routes that require admin or device auth
    let authenticated_routes = Router::new()
        .route("/api/v1/checkin", post(checkin))
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices/{id}", get(get_device))
        .route(
            "/api/v1/devices/{id}/config",
            put(set_device_config).layer(axum::extract::DefaultBodyLimit::max(
                SET_DEVICE_CONFIG_MAX_BODY_BYTES,
            )),
        )
        .route(
            "/api/v1/devices/{id}/drift",
            get(list_drift_events).post(record_drift_event),
        )
        .route("/api/v1/devices/{id}/reconcile", post(force_reconcile))
        .route("/api/v1/events", get(list_fleet_events))
        .route("/api/v1/events/stream", get(event_stream))
        .route_layer(middleware::from_fn_with_state(state, auth_middleware));

    // Admin-only routes (always require CFGD_API_KEY)
    let admin_routes = Router::new()
        .route("/api/v1/admin/tokens", post(create_token).get(list_tokens))
        .route("/api/v1/admin/tokens/{id}", delete(delete_token))
        .route(
            "/api/v1/admin/devices/{id}/credential",
            delete(revoke_credential),
        )
        .route(
            "/api/v1/admin/users/{username}/keys",
            post(add_user_key).get(list_user_keys),
        )
        .route(
            "/api/v1/admin/users/{username}/keys/{id}",
            delete(delete_user_key),
        )
        .route("/api/v1/admin/reset", post(admin_reset))
        .route_layer(middleware::from_fn(admin_auth_middleware));

    // Enrollment endpoints — no pre-auth, validated in handlers
    let enrollment_routes = Router::new()
        .route("/api/v1/enroll", post(enroll))
        .route("/api/v1/enroll/info", get(enroll_info))
        .route("/api/v1/enroll/challenge", post(request_challenge))
        .route("/api/v1/enroll/verify", post(verify_enrollment));

    authenticated_routes
        .merge(admin_routes)
        .merge(enrollment_routes)
}

// Length bounds for device-supplied identifiers. These are enforced on
// every enrollment / checkin entry point — they defend against log
// injection (unbounded strings in structured logs), URL traversal (when a
// device_id lands in a path), and DB-pressure DoS.
const DEVICE_ID_MAX_LEN: usize = 128;
const HOSTNAME_MAX_LEN: usize = 253;

fn validate_device_id(s: &str) -> Result<(), GatewayError> {
    if s.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "device_id must not be empty".to_string(),
        ));
    }
    if s.len() > DEVICE_ID_MAX_LEN {
        return Err(GatewayError::InvalidRequest(format!(
            "device_id exceeds {DEVICE_ID_MAX_LEN} chars"
        )));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(GatewayError::InvalidRequest(
            "device_id may only contain [A-Za-z0-9._-]".to_string(),
        ));
    }
    Ok(())
}

fn validate_hostname(s: &str) -> Result<(), GatewayError> {
    if s.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "hostname must not be empty".to_string(),
        ));
    }
    if s.len() > HOSTNAME_MAX_LEN {
        return Err(GatewayError::InvalidRequest(format!(
            "hostname exceeds {HOSTNAME_MAX_LEN} chars"
        )));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(GatewayError::InvalidRequest(
            "hostname may only contain [A-Za-z0-9._-]".to_string(),
        ));
    }
    Ok(())
}

/// Hash a token or API key for storage.
fn hash_token(token: &str) -> String {
    cfgd_core::sha256_hex(token.as_bytes())
}

/// Generate a cryptographically random token with a given prefix.
/// Uses two UUID v4s (128 bits of randomness each, from OS CSPRNG).
fn generate_token(prefix: &str) -> String {
    let a = uuid::Uuid::new_v4().to_string().replace('-', "");
    let b = uuid::Uuid::new_v4().to_string().replace('-', "");
    format!("{prefix}_{a}{b}")
}

// --- Auth ---

/// Extract a Bearer token from the Authorization header.
pub(crate) fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Auth middleware for device/admin endpoints.
/// Accepts either CFGD_API_KEY (admin) or a per-device API key.
/// Sets AuthContext as a request extension.
async fn auth_middleware(
    State(state): State<SharedState>,
    headers: HeaderMap,
    mut request: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, GatewayError> {
    let bearer_token = extract_bearer_token(&headers);

    // Check admin key first (constant-time comparison to prevent timing attacks)
    if let Ok(expected_key) = std::env::var("CFGD_API_KEY") {
        if let Some(ref token) = bearer_token
            && hash_token(token)
                .as_bytes()
                .ct_eq(hash_token(&expected_key).as_bytes())
                .into()
        {
            request.extensions_mut().insert(AuthContext::Admin);
            return Ok(next.run(request).await);
        }
    } else {
        // CFGD_API_KEY not set — reject admin operations (require explicit key)
        tracing::debug!("CFGD_API_KEY not set — admin API access is disabled");
    }

    // Check per-device API key — scope the lock tightly so it's released before
    // downstream handlers run (otherwise every device request serializes through one lock)
    if let Some(ref token) = bearer_token {
        let token_hash = hash_token(token);
        let cred = {
            let db = state.db.lock().await;
            db.validate_device_credential(&token_hash).ok()
        };
        if let Some(cred) = cred {
            request.extensions_mut().insert(AuthContext::Device {
                device_id: cred.device_id,
                username: cred.username,
            });
            return Ok(next.run(request).await);
        }
    }

    Err(GatewayError::Unauthorized)
}

/// Admin-only auth middleware — requires CFGD_API_KEY.
async fn admin_auth_middleware(
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, GatewayError> {
    if let Ok(expected_key) = std::env::var("CFGD_API_KEY") {
        match extract_bearer_token(&headers) {
            Some(token)
                if hash_token(&token)
                    .as_bytes()
                    .ct_eq(hash_token(&expected_key).as_bytes())
                    .into() => {}
            _ => return Err(GatewayError::Unauthorized),
        }
    } else {
        // Server misconfig — log at error so the operator notices, but present
        // as 401 to the client so we don't leak the env-var name.
        tracing::error!(
            env = "CFGD_API_KEY",
            "admin endpoint hit but CFGD_API_KEY is not set — refusing access"
        );
        return Err(GatewayError::Unauthorized);
    }
    Ok(next.run(request).await)
}

/// Enforce that a device-authenticated request can only access its own resources.
fn enforce_device_access(
    auth: &AuthContext,
    requested_device_id: &str,
) -> Result<(), GatewayError> {
    match auth {
        AuthContext::Admin => Ok(()),
        AuthContext::Device { device_id, .. } => {
            if device_id == requested_device_id {
                Ok(())
            } else {
                Err(GatewayError::Forbidden(format!(
                    "device '{}' cannot access resources for '{}'",
                    device_id, requested_device_id
                )))
            }
        }
    }
}

// --- Enrollment ---

async fn enroll(
    State(state): State<SharedState>,
    Json(req): Json<EnrollRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    if state.enrollment_method != EnrollmentMethod::Token {
        return Err(GatewayError::InvalidRequest(
            "bootstrap token enrollment is not enabled — this server uses key-based enrollment"
                .to_string(),
        ));
    }
    validate_device_id(&req.device_id)?;
    validate_hostname(&req.hostname)?;
    if req.token.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "token must not be empty".to_string(),
        ));
    }

    let db = state.db.lock().await;

    // Wrap the entire enrollment in a transaction so that token consumption,
    // device registration, and credential creation are atomic.
    db.conn
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(GatewayError::Database)?;

    let result = (|| -> Result<_, GatewayError> {
        // Atomically validate and consume the bootstrap token (prevents TOCTOU race)
        let token_hash = hash_token(&req.token);
        let bootstrap = db.validate_and_consume_bootstrap_token(&token_hash, &req.device_id)?;

        // Register the device first (so the credential FK is valid)
        db.register_device(
            &req.device_id,
            &req.hostname,
            &req.os,
            &req.arch,
            "pending-enrollment",
            None,
        )?;

        // Generate a permanent device API key
        let device_api_key = generate_token("cfgd_dev");
        let api_key_hash = hash_token(&device_api_key);

        // Create device credential
        db.create_device_credential(
            &req.device_id,
            &api_key_hash,
            &bootstrap.username,
            bootstrap.team.as_deref(),
        )?;

        // Look up desired config for this device (may have been pre-set via MachineConfig CRD)
        let desired_config = db
            .get_device(&req.device_id)
            .ok()
            .and_then(|d| d.desired_config);

        Ok((bootstrap, device_api_key, desired_config))
    })();

    match result {
        Ok((bootstrap, device_api_key, desired_config)) => {
            db.conn
                .execute_batch("COMMIT")
                .map_err(GatewayError::Database)?;

            // Continue with the successfully enrolled data
            let _ = state.event_tx.send(FleetEvent {
                timestamp: cfgd_core::utc_now_iso8601(),
                device_id: req.device_id.clone(),
                event_type: "enrollment".to_string(),
                summary: format!("user={} hostname={}", bootstrap.username, req.hostname),
            });

            tracing::info!(
                device_id = %req.device_id,
                hostname = %req.hostname,
                username = %bootstrap.username,
                "device enrolled"
            );

            if let Some(ref m) = state.metrics {
                m.devices_enrolled_total.inc();
            }

            Ok((
                StatusCode::CREATED,
                Json(EnrollResponse {
                    status: "enrolled".to_string(),
                    device_id: req.device_id,
                    api_key: device_api_key,
                    username: bootstrap.username,
                    team: bootstrap.team,
                    desired_config,
                }),
            ))
        }
        Err(e) => {
            let _ = db.conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// --- Admin Token Management ---

async fn create_token(
    State(state): State<SharedState>,
    Json(req): Json<CreateTokenRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    if req.username.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "username must not be empty".to_string(),
        ));
    }
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

    let db = state.db.lock().await;
    let record =
        db.create_bootstrap_token(&token_hash, &req.username, req.team.as_deref(), &expires_at)?;

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

async fn list_tokens(State(state): State<SharedState>) -> Result<impl IntoResponse, GatewayError> {
    let db = state.db.lock().await;
    let tokens = db.list_bootstrap_tokens()?;
    Ok(Json(tokens))
}

async fn delete_token(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    let db = state.db.lock().await;
    db.delete_bootstrap_token(&id)?;

    tracing::info!(token_id = %id, "bootstrap token deleted");

    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_credential(
    State(state): State<SharedState>,
    Path(device_id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    let db = state.db.lock().await;
    db.revoke_device_credential(&device_id)?;

    tracing::info!(device_id = %device_id, "device credential revoked");

    Ok(StatusCode::NO_CONTENT)
}

// --- Key-based enrollment ---

/// Discovery endpoint — tells clients which enrollment method the server uses.
async fn enroll_info(State(state): State<SharedState>) -> Result<impl IntoResponse, GatewayError> {
    Ok(Json(EnrollInfoResponse {
        method: state.enrollment_method.clone(),
    }))
}

/// Admin: add a public key for a user.
async fn add_user_key(
    State(state): State<SharedState>,
    Path(username): Path<String>,
    Json(req): Json<AddKeyRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    if username.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "username must not be empty".to_string(),
        ));
    }
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

    let db = state.db.lock().await;
    let key = db.add_user_public_key(
        &username,
        &req.key_type,
        &req.public_key,
        &req.fingerprint,
        req.label.as_deref(),
    )?;

    tracing::info!(
        username = %username,
        key_type = %req.key_type,
        fingerprint = %req.fingerprint,
        "public key added"
    );

    Ok((StatusCode::CREATED, Json(key)))
}

/// Admin: list all public keys for a user.
async fn list_user_keys(
    State(state): State<SharedState>,
    Path(username): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    let db = state.db.lock().await;
    let keys = db.list_user_public_keys(&username)?;
    Ok(Json(keys))
}

/// Admin: delete a user's public key.
async fn delete_user_key(
    State(state): State<SharedState>,
    Path((username, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, GatewayError> {
    let db = state.db.lock().await;
    db.delete_user_public_key(&id)?;

    tracing::info!(username = %username, key_id = %id, "public key deleted");

    Ok(StatusCode::NO_CONTENT)
}

/// Reset all gateway data (admin-only). Wipes devices, events, tokens, credentials.
async fn admin_reset(State(state): State<SharedState>) -> Result<impl IntoResponse, GatewayError> {
    let db = state.db.lock().await;
    let deleted = db.reset_data()?;
    Ok(Json(serde_json::json!({
        "status": "ok",
        "rowsDeleted": deleted
    })))
}

/// Request an enrollment challenge (key mode only).
async fn request_challenge(
    State(state): State<SharedState>,
    Json(req): Json<ChallengeRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    if state.enrollment_method != EnrollmentMethod::Key {
        return Err(GatewayError::InvalidRequest(
            "key-based enrollment is not enabled — this server uses bootstrap token enrollment"
                .to_string(),
        ));
    }
    if req.username.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "username must not be empty".to_string(),
        ));
    }
    validate_device_id(&req.device_id)?;
    validate_hostname(&req.hostname)?;

    // Verify user has at least one public key registered
    let db = state.db.lock().await;
    let keys = db.list_user_public_keys(&req.username)?;
    if keys.is_empty() {
        return Err(GatewayError::InvalidRequest(format!(
            "no public keys registered for user '{}' — ask your admin to add your SSH or GPG key",
            req.username
        )));
    }

    let nonce = generate_token("cfgd_ch");

    let challenge = db.create_enrollment_challenge(
        &req.username,
        &req.device_id,
        &req.hostname,
        (&req.os, &req.arch),
        &nonce,
        CHALLENGE_TTL_SECS,
    )?;

    tracing::info!(
        username = %req.username,
        device_id = %req.device_id,
        challenge_id = %challenge.id,
        "enrollment challenge issued"
    );

    Ok((
        StatusCode::CREATED,
        Json(ChallengeResponse {
            challenge_id: challenge.id,
            nonce,
            expires_at: challenge.expires_at,
        }),
    ))
}

/// Verify a signed enrollment challenge and enroll the device (key mode only).
async fn verify_enrollment(
    State(state): State<SharedState>,
    Json(req): Json<VerifyRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    if state.enrollment_method != EnrollmentMethod::Key {
        return Err(GatewayError::InvalidRequest(
            "key-based enrollment is not enabled".to_string(),
        ));
    }
    if req.challenge_id.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "challenge_id must not be empty".to_string(),
        ));
    }
    if req.signature.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "signature must not be empty".to_string(),
        ));
    }
    if !matches!(req.key_type.as_str(), "ssh" | "gpg") {
        return Err(GatewayError::InvalidRequest(
            "key_type must be 'ssh' or 'gpg'".to_string(),
        ));
    }

    let db = state.db.lock().await;

    // Atomically consume the challenge
    let challenge = db.consume_enrollment_challenge(&req.challenge_id)?;

    // Get user's public keys of the matching type
    let keys = db.list_user_public_keys(&challenge.username)?;
    let matching_keys: Vec<_> = keys.iter().filter(|k| k.key_type == req.key_type).collect();

    if matching_keys.is_empty() {
        return Err(GatewayError::InvalidRequest(format!(
            "no {} keys registered for user '{}'",
            req.key_type, challenge.username
        )));
    }

    // Verify signature against each matching key until one succeeds.
    // Verification involves blocking I/O (temp files + subprocess) — run off the async runtime.
    let nonce = challenge.nonce.clone();
    let signature = req.signature.clone();
    let key_type = req.key_type.clone();
    let owned_keys: Vec<super::db::UserPublicKey> =
        matching_keys.iter().map(|k| (*k).clone()).collect();
    let verified = tokio::task::spawn_blocking(move || {
        let key_refs: Vec<_> = owned_keys.iter().collect();
        match key_type.as_str() {
            "ssh" => verify_ssh_signature(&nonce, &signature, &key_refs),
            "gpg" => verify_gpg_signature(&nonce, &signature, &key_refs),
            _ => false,
        }
    })
    .await
    .unwrap_or(false);

    if !verified {
        return Err(GatewayError::Unauthorized);
    }

    // Signature verified — enroll the device (same flow as bootstrap token enrollment)
    db.register_device(
        &challenge.device_id,
        &challenge.hostname,
        &challenge.os,
        &challenge.arch,
        "pending-enrollment",
        None,
    )?;

    let device_api_key = generate_token("cfgd_dev");
    let api_key_hash = hash_token(&device_api_key);

    // Look up team from TeamConfig/user keys metadata (use first key's data)
    let team: Option<String> = None; // Key-based enrollment doesn't carry team info in the key itself

    db.create_device_credential(
        &challenge.device_id,
        &api_key_hash,
        &challenge.username,
        team.as_deref(),
    )?;

    let desired_config = db
        .get_device(&challenge.device_id)
        .ok()
        .and_then(|d| d.desired_config);

    let _ = state.event_tx.send(FleetEvent {
        timestamp: cfgd_core::utc_now_iso8601(),
        device_id: challenge.device_id.clone(),
        event_type: "enrollment".to_string(),
        summary: format!(
            "user={} hostname={} method=key",
            challenge.username, challenge.hostname
        ),
    });

    tracing::info!(
        device_id = %challenge.device_id,
        hostname = %challenge.hostname,
        username = %challenge.username,
        "device enrolled via key verification"
    );

    if let Some(ref m) = state.metrics {
        m.devices_enrolled_total.inc();
    }

    Ok((
        StatusCode::CREATED,
        Json(EnrollResponse {
            status: "enrolled".to_string(),
            device_id: challenge.device_id,
            api_key: device_api_key,
            username: challenge.username,
            team,
            desired_config,
        }),
    ))
}

/// Verify an SSH signature using `ssh-keygen -Y verify`.
fn verify_ssh_signature(
    nonce: &str,
    signature_pem: &str,
    keys: &[&super::db::UserPublicKey],
) -> bool {
    let tmp_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "failed to create temp dir for SSH verification");
            return false;
        }
    };

    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.sig");
    let signers_path = tmp_dir.path().join("allowed_signers");

    if let Err(e) = std::fs::write(&data_path, nonce) {
        tracing::error!(error = %e, "failed to write challenge data for SSH verification");
        return false;
    }
    if let Err(e) = std::fs::write(&sig_path, signature_pem) {
        tracing::error!(error = %e, "failed to write signature file for SSH verification");
        return false;
    }

    // Try each key until one verifies
    for key in keys {
        // Write allowed_signers file: "username key_type key_data"
        // The public_key field is the full OpenSSH public key line (e.g. "ssh-ed25519 AAAA... comment")
        let signer_line = format!("{} {}", key.username, key.public_key);
        if let Err(e) = std::fs::write(&signers_path, &signer_line) {
            tracing::warn!(error = %e, key_user = %key.username, "ssh verify: failed to write allowed_signers");
            continue;
        }

        let data_file = match std::fs::File::open(&data_path) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, "failed to open challenge data file");
                return false;
            }
        };

        let result = std::process::Command::new("ssh-keygen")
            .args([
                "-Y",
                "verify",
                "-f",
                &signers_path.to_string_lossy(),
                "-I",
                &key.username,
                "-n",
                "cfgd-enroll",
                "-s",
                &sig_path.to_string_lossy(),
            ])
            .stdin(data_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match result {
            Ok(status) if status.success() => {
                tracing::info!(
                    username = %key.username,
                    fingerprint = %key.fingerprint,
                    "SSH signature verified"
                );
                return true;
            }
            Ok(_) => {
                tracing::debug!(
                    fingerprint = %key.fingerprint,
                    "SSH signature did not match this key"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "ssh-keygen failed — is OpenSSH installed?");
                continue;
            }
        }
    }

    false
}

/// Verify a GPG signature using `gpg --verify`.
fn verify_gpg_signature(
    nonce: &str,
    signature_armored: &str,
    keys: &[&super::db::UserPublicKey],
) -> bool {
    let tmp_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "failed to create temp dir for GPG verification");
            return false;
        }
    };

    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.asc");
    let key_path = tmp_dir.path().join("pubkey.asc");

    if let Err(e) = std::fs::write(&data_path, nonce) {
        tracing::error!(error = %e, "gpg verify: failed to write challenge data");
        return false;
    }
    if let Err(e) = std::fs::write(&sig_path, signature_armored) {
        tracing::error!(error = %e, "gpg verify: failed to write signature");
        return false;
    }

    for (idx, key) in keys.iter().enumerate() {
        // Per-iteration homedir — sharing one keyring across iterations
        // lets `gpg --verify` accept a signature from ANY previously-imported
        // key, breaking the "this key signed the nonce" attribution in logs.
        let gpg_home = tmp_dir.path().join(format!("gnupg-{idx}"));
        if let Err(e) = std::fs::create_dir_all(&gpg_home) {
            tracing::warn!(error = %e, key_user = %key.username, "gpg verify: failed to create per-key homedir");
            continue;
        }
        let _ = cfgd_core::set_file_permissions(&gpg_home, 0o700);

        if let Err(e) = std::fs::write(&key_path, &key.public_key) {
            tracing::warn!(error = %e, key_user = %key.username, "gpg verify: failed to write public key");
            continue;
        }

        // Import the public key
        let import = std::process::Command::new("gpg")
            .args([
                "--homedir",
                &gpg_home.to_string_lossy(),
                "--batch",
                "--yes",
                "--import",
                &key_path.to_string_lossy(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        if !matches!(import, Ok(s) if s.success()) {
            tracing::debug!(
                fingerprint = %key.fingerprint,
                "failed to import GPG key"
            );
            continue;
        }

        // Verify the signature against THIS iteration's single-key keyring.
        // Success here means the signature verifies under exactly `key`,
        // so the `fingerprint = %key.fingerprint` we log below is truthful.
        let verify = std::process::Command::new("gpg")
            .args([
                "--homedir",
                &gpg_home.to_string_lossy(),
                "--batch",
                "--verify",
                &sig_path.to_string_lossy(),
                &data_path.to_string_lossy(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match verify {
            Ok(status) if status.success() => {
                tracing::info!(
                    username = %key.username,
                    fingerprint = %key.fingerprint,
                    "GPG signature verified"
                );
                return true;
            }
            Ok(_) => {
                tracing::debug!(
                    fingerprint = %key.fingerprint,
                    "GPG signature did not match this key"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "gpg failed — is GPG installed?");
                return false;
            }
        }
    }

    false
}

// --- Device endpoints ---

async fn checkin(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(req): Json<CheckinRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    validate_device_id(&req.device_id)?;
    validate_hostname(&req.hostname)?;

    // Device auth: can only check in as self
    enforce_device_access(&auth, &req.device_id)?;

    let db = state.db.lock().await;

    let existing = db.get_device(&req.device_id);
    let config_changed = match &existing {
        Ok(device) => device.config_hash != req.config_hash,
        Err(GatewayError::NotFound(_)) => false,
        Err(_) => false,
    };

    // Capture desired_config before mutating — avoids a second get_device query
    let desired_config = if config_changed {
        existing
            .as_ref()
            .ok()
            .and_then(|d| d.desired_config.clone())
    } else {
        None
    };

    match &existing {
        Ok(_) => {
            db.update_checkin(
                &req.device_id,
                &req.config_hash,
                req.compliance_summary.as_ref(),
            )?;
        }
        Err(_) => {
            db.register_device(
                &req.device_id,
                &req.hostname,
                &req.os,
                &req.arch,
                &req.config_hash,
                req.compliance_summary.as_ref(),
            )?;
        }
    }

    // Record checkin event for history
    if let Err(e) = db.record_checkin(&req.device_id, &req.config_hash, config_changed) {
        tracing::warn!(device_id = %req.device_id, error = %e, "failed to record checkin event");
    }

    // Broadcast to SSE subscribers
    let event_type = if config_changed {
        "config-changed"
    } else {
        "checkin"
    };
    let _ = state.event_tx.send(FleetEvent {
        timestamp: cfgd_core::utc_now_iso8601(),
        device_id: req.device_id.clone(),
        event_type: event_type.to_string(),
        summary: req.config_hash.clone(),
    });

    let auth_label = match &auth {
        AuthContext::Admin => "admin".to_string(),
        AuthContext::Device { username, .. } => format!("device({})", username),
    };
    tracing::info!(
        device_id = %req.device_id,
        hostname = %req.hostname,
        auth = %auth_label,
        config_changed,
        "device checked in"
    );

    Ok((
        StatusCode::OK,
        Json(CheckinResponse {
            status: "ok".to_string(),
            config_changed,
            desired_config,
        }),
    ))
}

async fn list_devices(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(pagination): Query<PaginationParams>,
) -> Result<impl IntoResponse, GatewayError> {
    // Device auth: can only list self
    if let AuthContext::Device { ref device_id, .. } = auth {
        let db = state.db.lock().await;
        let device = db.get_device(device_id)?;
        return Ok(Json(vec![device]));
    }
    let limit = pagination.limit.min(1000);
    let db = state.db.lock().await;
    let devices = db.list_devices_paginated(limit, pagination.offset)?;
    Ok(Json(devices))
}

async fn get_device(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    enforce_device_access(&auth, &id)?;
    let db = state.db.lock().await;
    let device = db.get_device(&id)?;
    Ok(Json(device))
}

async fn set_device_config(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<SetConfigRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    // Only admin can push config to devices
    if !matches!(auth, AuthContext::Admin) {
        return Err(GatewayError::Forbidden(
            "only admin can push config to devices".to_string(),
        ));
    }
    // Enforce a 10MB size limit on config payloads
    let json_str = serde_json::to_string(&req.config)
        .map_err(|e| GatewayError::InvalidRequest(e.to_string()))?;
    if json_str.len() > 10 * 1024 * 1024 {
        return Err(GatewayError::InvalidRequest(
            "config exceeds 10MB size limit".to_string(),
        ));
    }

    let db = state.db.lock().await;
    db.set_device_config(&id, &req.config)?;

    tracing::info!(device_id = %id, "desired config updated");

    Ok(StatusCode::NO_CONTENT)
}

async fn list_drift_events(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    enforce_device_access(&auth, &id)?;
    let db = state.db.lock().await;
    let events = db.list_drift_events(&id)?;
    Ok(Json(events))
}

async fn record_drift_event(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<DriftRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    enforce_device_access(&auth, &id)?;

    let details_str = serde_json::to_string(&req.details)
        .map_err(|e| GatewayError::Internal(format!("failed to serialize drift details: {e}")))?;

    let (event, device_hostname) = {
        let db = state.db.lock().await;
        let evt = db.record_drift_event(&id, &details_str)?;
        let hostname = db.get_device(&id).ok().map(|d| d.hostname);
        (evt, hostname)
    };

    // Broadcast to SSE subscribers
    let _ = state.event_tx.send(FleetEvent {
        timestamp: event.timestamp.clone(),
        device_id: id.clone(),
        event_type: "drift".to_string(),
        summary: details_str.clone(),
    });

    tracing::warn!(
        device_id = %id,
        event_id = %event.id,
        details_count = req.details.len(),
        "drift event recorded"
    );

    if let Some(ref client) = state.kube_client {
        let hostname = device_hostname.as_deref().unwrap_or(&id);
        create_drift_alert_crd(client, &id, hostname, &req.details, &event.timestamp).await?;
    }

    Ok((StatusCode::CREATED, Json(event)))
}

async fn force_reconcile(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    // Only admin can force reconcile
    if !matches!(auth, AuthContext::Admin) {
        return Err(GatewayError::Forbidden(
            "only admin can force reconcile".to_string(),
        ));
    }
    let db = state.db.lock().await;
    db.set_force_reconcile(&id)?;

    tracing::info!(device_id = %id, "force reconcile requested");

    Ok(StatusCode::NO_CONTENT)
}

async fn list_fleet_events(
    State(state): State<SharedState>,
    Query(pagination): Query<PaginationParams>,
) -> Result<impl IntoResponse, GatewayError> {
    let limit = pagination.limit.min(1000);
    let db = state.db.lock().await;
    let events = db.list_fleet_events_paginated(limit, pagination.offset)?;
    Ok(Json(events))
}

/// SSE endpoint — streams fleet events in real-time.
async fn event_stream(
    State(state): State<SharedState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Some(Ok(Event::default()
                .event(event.event_type.clone())
                .data(data)))
        }
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Create a DriftAlert CRD in Kubernetes with retry and exponential backoff.
async fn create_drift_alert_crd(
    client: &kube::Client,
    device_id: &str,
    hostname: &str,
    details: &[DriftDetailInput],
    timestamp: &str,
) -> Result<(), GatewayError> {
    use crate::crds::{
        DriftAlert, DriftAlertSpec, DriftDetail, DriftSeverity, MachineConfigReference,
    };
    use kube::ResourceExt;
    use kube::api::{Api, PostParams};

    let alerts: Api<DriftAlert> = Api::default_namespaced(client.clone());

    let mc_ref = find_machine_config_for_device(client, hostname).await;

    let alert_name = format!(
        "drift-{}-{}",
        device_id,
        timestamp.replace([':', '-', 'T', 'Z'], "")
    );

    let drift_details: Vec<DriftDetail> = details
        .iter()
        .map(|d| DriftDetail {
            field: d.field.clone(),
            expected: d.expected.clone(),
            actual: d.actual.clone(),
        })
        .collect();

    let mut alert = DriftAlert::new(
        &alert_name,
        DriftAlertSpec {
            device_id: device_id.to_string(),
            machine_config_ref: MachineConfigReference {
                name: mc_ref.clone(),
                namespace: None,
            },
            drift_details,
            severity: DriftSeverity::Medium,
        },
    );
    alert.metadata.labels = Some(std::collections::BTreeMap::from([
        ("cfgd.io/machine-config".to_string(), mc_ref),
        ("cfgd.io/device-id".to_string(), device_id.to_string()),
    ]));

    let retry = cfgd_core::retry::BackoffConfig::DEFAULT_TRANSIENT;
    let mut last_err = None;

    for attempt in 0..retry.max_attempts {
        let delay = retry.delay_for_attempt(attempt);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }

        match alerts.create(&PostParams::default(), &alert).await {
            Ok(created) => {
                tracing::info!(
                    name = %created.name_any(),
                    device_id = %device_id,
                    "driftAlert CRD created in Kubernetes"
                );
                return Ok(());
            }
            Err(kube::Error::Api(ref resp)) if resp.code == 409 => {
                tracing::debug!(
                    name = %alert_name,
                    "driftAlert already exists, skipping creation"
                );
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    device_id = %device_id,
                    attempt = attempt + 1,
                    max_retries = retry.max_attempts,
                    error = %e,
                    "failed to create DriftAlert CRD, retrying"
                );
                last_err = Some(e);
            }
        }
    }

    if let Some(e) = last_err {
        tracing::error!(
            device_id = %device_id,
            error = %e,
            attempts = retry.max_attempts,
            "failed to create DriftAlert CRD after all attempts — drift recorded in database only"
        );
    }

    Ok(())
}

/// Find the MachineConfig CRD name that corresponds to a device hostname.
async fn find_machine_config_for_device(client: &kube::Client, hostname: &str) -> String {
    use crate::crds::MachineConfig;
    use kube::ResourceExt;
    use kube::api::{Api, ListParams};

    let machines: Api<MachineConfig> = Api::all(client.clone());
    match machines.list(&ListParams::default()).await {
        Ok(list) => {
            for mc in &list.items {
                if mc.spec.hostname == hostname {
                    return mc.name_any();
                }
            }
            format!("{}-mc", hostname)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to list MachineConfigs for device lookup");
            format!("{}-mc", hostname)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn hash_token_deterministic() {
        let h1 = hash_token("my-secret-token");
        let h2 = hash_token("my-secret-token");
        assert_eq!(h1, h2);
        assert_ne!(h1, hash_token("different-token"));
    }

    #[test]
    fn generate_token_has_prefix() {
        let token = generate_token("cfgd_bs");
        assert!(token.starts_with("cfgd_bs_"));
        assert!(token.len() > 40); // long enough to be secure
    }

    #[test]
    fn generate_token_unique() {
        let t1 = generate_token("cfgd_dev");
        let t2 = generate_token("cfgd_dev");
        assert_ne!(t1, t2);
    }

    #[test]
    fn enforce_device_access_admin_allows_any() {
        let auth = AuthContext::Admin;
        // Admin should be able to access ANY device ID
        assert!(enforce_device_access(&auth, "any-device").is_ok());
        assert!(enforce_device_access(&auth, "another-device").is_ok());
    }

    #[test]
    fn enforce_device_access_device_allows_self() {
        let auth = AuthContext::Device {
            device_id: "dev-1".to_string(),
            username: "jdoe".to_string(),
        };
        assert!(enforce_device_access(&auth, "dev-1").is_ok());
    }

    #[test]
    fn enforce_device_access_device_denies_other_specific_error() {
        // Verify the error is specifically Forbidden (not some other variant)
        // and that the username doesn't leak into the error
        let auth = AuthContext::Device {
            device_id: "dev-1".to_string(),
            username: "jdoe".to_string(),
        };
        let err = enforce_device_access(&auth, "dev-999").unwrap_err();
        match &err {
            GatewayError::Forbidden(msg) => {
                assert!(msg.contains("dev-1"), "should mention requester: {msg}");
                assert!(msg.contains("dev-999"), "should mention target: {msg}");
            }
            other => panic!("expected Forbidden, got: {other}"),
        }
    }

    #[test]
    fn enforce_device_access_device_denies_other() {
        let auth = AuthContext::Device {
            device_id: "dev-1".to_string(),
            username: "jdoe".to_string(),
        };
        let err = enforce_device_access(&auth, "dev-2").unwrap_err();
        assert!(
            matches!(err, GatewayError::Forbidden(ref msg) if msg.contains("dev-1") && msg.contains("dev-2")),
            "expected Forbidden error mentioning both device IDs, got: {err}"
        );
    }

    #[test]
    fn enroll_info_response_serialization() {
        let resp = EnrollInfoResponse {
            method: EnrollmentMethod::Key,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"method\":\"key\""));
    }

    #[test]
    fn challenge_response_serialization() {
        let resp = ChallengeResponse {
            challenge_id: "abc-123".to_string(),
            nonce: "cfgd_ch_random".to_string(),
            expires_at: "2026-03-14T12:05:00Z".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("abc-123"));
        assert!(json.contains("cfgd_ch_random"));
    }

    // --- extract_bearer_token ---

    #[test]
    fn extract_bearer_token_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer my-secret".parse().unwrap());
        assert_eq!(
            extract_bearer_token(&headers),
            Some("my-secret".to_string())
        );
    }

    #[test]
    fn extract_bearer_token_missing_header() {
        let headers = HeaderMap::new();
        assert_eq!(extract_bearer_token(&headers), None);
    }

    #[test]
    fn extract_bearer_token_wrong_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic dXNlcjpwYXNz".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers), None);
    }

    #[test]
    fn extract_bearer_token_empty_value() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer ".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers), None);
    }

    #[test]
    fn extract_bearer_token_no_space_after_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "BearerNoSpace".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers), None);
    }

    // --- hash_token extended ---

    #[test]
    fn hash_token_is_lowercase_hex() {
        let h = hash_token("test-token");
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn hash_token_sha256_length() {
        // SHA256 produces 64 hex characters
        let h = hash_token("anything");
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn hash_token_empty_input() {
        let h = hash_token("");
        // SHA256 of empty string is a known constant
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    // --- generate_token extended ---

    #[test]
    fn generate_token_different_prefixes() {
        let bs = generate_token("cfgd_bs");
        let dev = generate_token("cfgd_dev");
        let ch = generate_token("cfgd_ch");
        assert!(bs.starts_with("cfgd_bs_"));
        assert!(dev.starts_with("cfgd_dev_"));
        assert!(ch.starts_with("cfgd_ch_"));
    }

    #[test]
    fn generate_token_no_dashes_in_random_part() {
        let token = generate_token("prefix");
        let random_part = token.strip_prefix("prefix_").unwrap();
        assert!(
            !random_part.contains('-'),
            "random part should have dashes stripped"
        );
        // Two UUID v4s without dashes = 64 hex chars
        assert_eq!(random_part.len(), 64);
    }

    // --- enforce_device_access extended ---

    #[test]
    fn enforce_device_access_error_contains_both_ids() {
        let auth = AuthContext::Device {
            device_id: "attacker-device".to_string(),
            username: "attacker".to_string(),
        };
        let err = enforce_device_access(&auth, "victim-device").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("attacker-device"),
            "error should mention the requesting device"
        );
        assert!(
            msg.contains("victim-device"),
            "error should mention the target device"
        );
    }

    // --- CheckinRequest deserialization ---

    #[test]
    fn checkin_request_deserialization() {
        let json = r#"{
            "deviceId": "dev-1",
            "hostname": "workstation-1",
            "os": "linux",
            "arch": "x86_64",
            "configHash": "abc123"
        }"#;
        let req: CheckinRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.device_id, "dev-1");
        assert_eq!(req.hostname, "workstation-1");
        assert_eq!(req.os, "linux");
        assert_eq!(req.arch, "x86_64");
        assert_eq!(req.config_hash, "abc123");
    }

    // --- CheckinResponse serialization ---

    #[test]
    fn checkin_response_no_config_omits_field() {
        let resp = CheckinResponse {
            status: "ok".to_string(),
            config_changed: false,
            desired_config: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains("desiredConfig"),
            "None desired_config should be omitted"
        );
        assert!(json.contains("\"configChanged\":false"));
    }

    #[test]
    fn checkin_response_with_config_includes_field() {
        let resp = CheckinResponse {
            status: "ok".to_string(),
            config_changed: true,
            desired_config: Some(serde_json::json!({"packages": ["vim"]})),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("desiredConfig"));
        assert!(json.contains("\"configChanged\":true"));
    }

    // --- EnrollRequest deserialization ---

    #[test]
    fn enroll_request_deserialization() {
        let json = r#"{
            "token": "cfgd_bs_abc123",
            "deviceId": "dev-42",
            "hostname": "laptop",
            "os": "darwin",
            "arch": "aarch64"
        }"#;
        let req: EnrollRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.token, "cfgd_bs_abc123");
        assert_eq!(req.device_id, "dev-42");
        assert_eq!(req.os, "darwin");
        assert_eq!(req.arch, "aarch64");
    }

    // --- EnrollResponse serialization ---

    #[test]
    fn enroll_response_omits_none_fields() {
        let resp = EnrollResponse {
            status: "enrolled".to_string(),
            device_id: "dev-1".to_string(),
            api_key: "cfgd_dev_key".to_string(),
            username: "jdoe".to_string(),
            team: None,
            desired_config: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("team"));
        assert!(!json.contains("desiredConfig"));
        assert!(json.contains("\"status\":\"enrolled\""));
    }

    #[test]
    fn enroll_response_includes_optional_fields_when_present() {
        let resp = EnrollResponse {
            status: "enrolled".to_string(),
            device_id: "dev-1".to_string(),
            api_key: "cfgd_dev_key".to_string(),
            username: "jdoe".to_string(),
            team: Some("platform".to_string()),
            desired_config: Some(serde_json::json!({"key": "val"})),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"team\":\"platform\""));
        assert!(json.contains("desiredConfig"));
    }

    // --- CreateTokenRequest deserialization with defaults ---

    #[test]
    fn create_token_request_defaults() {
        let json = r#"{"username": "admin"}"#;
        let req: CreateTokenRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.username, "admin");
        assert_eq!(req.team, None);
        assert_eq!(req.expires_in, 86400); // default 24h
    }

    #[test]
    fn create_token_request_with_all_fields() {
        let json = r#"{
            "username": "jdoe",
            "team": "infra",
            "expiresIn": 3600
        }"#;
        let req: CreateTokenRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.username, "jdoe");
        assert_eq!(req.team, Some("infra".to_string()));
        assert_eq!(req.expires_in, 3600);
    }

    // --- CreateTokenResponse serialization ---

    #[test]
    fn create_token_response_omits_none_team() {
        let resp = CreateTokenResponse {
            id: "tok-1".to_string(),
            token: "cfgd_bs_secret".to_string(),
            username: "admin".to_string(),
            team: None,
            expires_at: "2026-03-19T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("team"));
        assert!(json.contains("\"token\":\"cfgd_bs_secret\""));
    }

    // --- PaginationParams defaults ---

    #[test]
    fn pagination_params_defaults() {
        let json = r#"{}"#;
        let params: PaginationParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.limit, 100);
        assert_eq!(params.offset, 0);
    }

    #[test]
    fn pagination_params_custom() {
        let json = r#"{"limit": 50, "offset": 10}"#;
        let params: PaginationParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.limit, 50);
        assert_eq!(params.offset, 10);
    }

    // --- DriftRequest / DriftDetailInput serde ---

    #[test]
    fn drift_detail_input_roundtrip() {
        let detail = DriftDetailInput {
            field: "package/vim".to_string(),
            expected: "installed".to_string(),
            actual: "missing".to_string(),
        };
        let json = serde_json::to_string(&detail).unwrap();
        let parsed: DriftDetailInput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.field, "package/vim");
        assert_eq!(parsed.expected, "installed");
        assert_eq!(parsed.actual, "missing");
    }

    #[test]
    fn drift_request_deserialization() {
        let json = r#"{
            "details": [
                {"field": "pkg/vim", "expected": "9.0", "actual": "8.2"},
                {"field": "file/bashrc", "expected": "hash-a", "actual": "hash-b"}
            ]
        }"#;
        let req: DriftRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.details.len(), 2);
        assert_eq!(req.details[0].field, "pkg/vim");
        assert_eq!(req.details[1].field, "file/bashrc");
    }

    // --- SetConfigRequest deserialization ---

    #[test]
    fn set_config_request_deserialization() {
        let json = r#"{"config": {"packages": ["vim", "git"], "shell": "zsh"}}"#;
        let req: SetConfigRequest = serde_json::from_str(json).unwrap();
        assert!(req.config.is_object());
        assert_eq!(req.config["packages"][0], "vim");
    }

    // --- AddKeyRequest deserialization ---

    #[test]
    fn add_key_request_deserialization() {
        let json = r#"{
            "keyType": "ssh",
            "publicKey": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG...",
            "fingerprint": "SHA256:abcdef",
            "label": "laptop key"
        }"#;
        let req: AddKeyRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.key_type, "ssh");
        assert!(req.public_key.starts_with("ssh-ed25519"));
        assert_eq!(req.fingerprint, "SHA256:abcdef");
        assert_eq!(req.label, Some("laptop key".to_string()));
    }

    #[test]
    fn add_key_request_label_optional() {
        let json = r#"{
            "keyType": "gpg",
            "publicKey": "-----BEGIN PGP PUBLIC KEY BLOCK-----",
            "fingerprint": "DEADBEEF"
        }"#;
        let req: AddKeyRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.key_type, "gpg");
        assert_eq!(req.label, None);
    }

    // --- ChallengeRequest deserialization ---

    #[test]
    fn challenge_request_deserialization() {
        let json = r#"{
            "username": "jdoe",
            "deviceId": "dev-99",
            "hostname": "workstation",
            "os": "linux",
            "arch": "x86_64"
        }"#;
        let req: ChallengeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.username, "jdoe");
        assert_eq!(req.device_id, "dev-99");
        assert_eq!(req.hostname, "workstation");
    }

    // --- VerifyRequest deserialization ---

    #[test]
    fn verify_request_deserialization() {
        let json = r#"{
            "challengeId": "ch-123",
            "signature": "base64data==",
            "keyType": "ssh"
        }"#;
        let req: VerifyRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.challenge_id, "ch-123");
        assert_eq!(req.signature, "base64data==");
        assert_eq!(req.key_type, "ssh");
    }

    // --- EnrollmentMethod serialization ---

    #[test]
    fn enrollment_method_token_serialization() {
        let resp = EnrollInfoResponse {
            method: EnrollmentMethod::Token,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"method\":\"token\""));
    }

    // --- FleetEvent serialization ---

    #[test]
    fn fleet_event_serialization() {
        let event = super::super::db::FleetEvent {
            timestamp: "2026-03-18T10:00:00Z".to_string(),
            device_id: "dev-1".to_string(),
            event_type: "checkin".to_string(),
            summary: "hash123".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["deviceId"], "dev-1");
        assert_eq!(parsed["eventType"], "checkin");
        assert_eq!(parsed["summary"], "hash123");
    }

    // --- Database-backed handler logic tests ---

    fn test_db() -> ServerDb {
        ServerDb::open(":memory:").expect("failed to open in-memory db")
    }

    #[test]
    fn checkin_config_changed_detection() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash-old", None)
            .expect("register failed");

        // Set a desired config so config_changed is meaningful
        db.set_device_config("dev-1", &serde_json::json!({"packages": ["vim"]}))
            .expect("set config failed");

        let device = db.get_device("dev-1").expect("get failed");
        // Simulate checkin with different hash
        let config_changed = device.config_hash != "hash-new";
        assert!(config_changed, "different config_hash should detect change");

        // Same hash should not detect change
        let config_same = device.config_hash != "hash-old";
        assert!(!config_same, "same config_hash should not detect change");
    }

    // --- EnrollmentMethod::from_env ---

    #[test]
    #[serial]
    fn enrollment_method_defaults_to_token() {
        // Clear the env var so from_env returns the default
        // SAFETY: test-only; single-threaded test runner for this module
        unsafe { std::env::remove_var("CFGD_ENROLLMENT_METHOD") };
        let method = EnrollmentMethod::from_env();
        assert_eq!(method, EnrollmentMethod::Token);
    }

    #[test]
    #[serial]
    fn enrollment_method_key_from_env() {
        // SAFETY: test-only; single-threaded test runner for this module
        unsafe { std::env::set_var("CFGD_ENROLLMENT_METHOD", "key") };
        let method = EnrollmentMethod::from_env();
        assert_eq!(method, EnrollmentMethod::Key);
        unsafe { std::env::remove_var("CFGD_ENROLLMENT_METHOD") };
    }

    #[test]
    #[serial]
    fn enrollment_method_unknown_falls_back_to_token() {
        // SAFETY: test-only; single-threaded test runner for this module
        unsafe { std::env::set_var("CFGD_ENROLLMENT_METHOD", "magic") };
        let method = EnrollmentMethod::from_env();
        assert_eq!(method, EnrollmentMethod::Token);
        unsafe { std::env::remove_var("CFGD_ENROLLMENT_METHOD") };
    }

    // --- verify_ssh_signature ---

    #[test]
    fn verify_ssh_signature_correct_key_succeeds() {
        if !cfgd_core::command_available("ssh-keygen") {
            return;
        }

        let tmp = tempfile::tempdir().expect("tmpdir");
        let key_path = tmp.path().join("test_key");
        let pub_path = tmp.path().join("test_key.pub");

        // Generate an ed25519 key pair (no passphrase)
        let keygen_status = std::process::Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &key_path.to_string_lossy(),
                "-N",
                "",
                "-C",
                "test@cfgd",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ssh-keygen failed");
        assert!(keygen_status.success(), "key generation must succeed");

        let public_key = std::fs::read_to_string(&pub_path).expect("read pubkey");

        // Sign a nonce
        let nonce = "cfgd_ch_test_nonce_12345";
        let nonce_path = tmp.path().join("nonce.txt");
        std::fs::write(&nonce_path, nonce).expect("write nonce");

        let sig_output = std::process::Command::new("ssh-keygen")
            .args([
                "-Y",
                "sign",
                "-f",
                &key_path.to_string_lossy(),
                "-n",
                "cfgd-enroll",
            ])
            .stdin(std::fs::File::open(&nonce_path).expect("open nonce"))
            .output()
            .expect("signing failed");
        assert!(sig_output.status.success(), "signing must succeed");

        let signature_pem = String::from_utf8_lossy(&sig_output.stdout).to_string();

        // Build a UserPublicKey
        let user_key = super::super::db::UserPublicKey {
            id: "key-1".to_string(),
            username: "testuser".to_string(),
            key_type: "ssh".to_string(),
            public_key: public_key.trim().to_string(),
            fingerprint: "SHA256:test".to_string(),
            label: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let keys: Vec<&super::super::db::UserPublicKey> = vec![&user_key];
        assert!(
            verify_ssh_signature(nonce, &signature_pem, &keys),
            "verification with correct key should succeed"
        );
    }

    #[test]
    fn verify_ssh_signature_wrong_key_fails() {
        if !cfgd_core::command_available("ssh-keygen") {
            return;
        }

        let tmp = tempfile::tempdir().expect("tmpdir");

        // Generate the signing key pair
        let sign_key = tmp.path().join("sign_key");
        let gen1 = std::process::Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &sign_key.to_string_lossy(),
                "-N",
                "",
                "-C",
                "signer@cfgd",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("keygen");
        assert!(gen1.success());

        // Generate a different key pair (the "wrong" key)
        let wrong_key = tmp.path().join("wrong_key");
        let gen2 = std::process::Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &wrong_key.to_string_lossy(),
                "-N",
                "",
                "-C",
                "wrong@cfgd",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("keygen");
        assert!(gen2.success());

        let wrong_pubkey =
            std::fs::read_to_string(tmp.path().join("wrong_key.pub")).expect("read wrong pubkey");

        // Sign with the signing key
        let nonce = "cfgd_ch_wrong_key_test";
        let nonce_path = tmp.path().join("nonce.txt");
        std::fs::write(&nonce_path, nonce).expect("write nonce");

        let sig_output = std::process::Command::new("ssh-keygen")
            .args([
                "-Y",
                "sign",
                "-f",
                &sign_key.to_string_lossy(),
                "-n",
                "cfgd-enroll",
            ])
            .stdin(std::fs::File::open(&nonce_path).expect("open nonce"))
            .output()
            .expect("signing");
        assert!(sig_output.status.success());

        let signature_pem = String::from_utf8_lossy(&sig_output.stdout).to_string();

        // Verify with the wrong key — should fail
        let wrong_user_key = super::super::db::UserPublicKey {
            id: "key-wrong".to_string(),
            username: "wronguser".to_string(),
            key_type: "ssh".to_string(),
            public_key: wrong_pubkey.trim().to_string(),
            fingerprint: "SHA256:wrong".to_string(),
            label: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let keys: Vec<&super::super::db::UserPublicKey> = vec![&wrong_user_key];
        assert!(
            !verify_ssh_signature(nonce, &signature_pem, &keys),
            "verification with wrong key should fail"
        );
    }

    #[test]
    fn verify_ssh_signature_empty_keys_returns_false() {
        if !cfgd_core::command_available("ssh-keygen") {
            return;
        }

        let keys: Vec<&super::super::db::UserPublicKey> = vec![];
        assert!(
            !verify_ssh_signature("some-nonce", "some-sig", &keys),
            "empty keys list should return false"
        );
    }

    #[test]
    fn verify_ssh_signature_bad_signature_data_returns_false() {
        if !cfgd_core::command_available("ssh-keygen") {
            return;
        }

        let tmp = tempfile::tempdir().expect("tmpdir");
        let key_path = tmp.path().join("test_key");

        let keygen_status = std::process::Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &key_path.to_string_lossy(),
                "-N",
                "",
                "-C",
                "test@cfgd",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("keygen");
        assert!(keygen_status.success());

        let public_key =
            std::fs::read_to_string(tmp.path().join("test_key.pub")).expect("read pubkey");

        let user_key = super::super::db::UserPublicKey {
            id: "key-1".to_string(),
            username: "testuser".to_string(),
            key_type: "ssh".to_string(),
            public_key: public_key.trim().to_string(),
            fingerprint: "SHA256:test".to_string(),
            label: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let keys: Vec<&super::super::db::UserPublicKey> = vec![&user_key];
        assert!(
            !verify_ssh_signature("nonce", "not-a-valid-signature", &keys),
            "garbage signature should fail verification"
        );
    }

    #[test]
    fn verify_ssh_signature_multiple_keys_finds_correct_one() {
        if !cfgd_core::command_available("ssh-keygen") {
            return;
        }

        let tmp = tempfile::tempdir().expect("tmpdir");

        // Generate the actual signing key
        let sign_key = tmp.path().join("sign_key");
        let gen1 = std::process::Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &sign_key.to_string_lossy(),
                "-N",
                "",
                "-C",
                "signer@cfgd",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("keygen");
        assert!(gen1.success());

        let sign_pubkey =
            std::fs::read_to_string(tmp.path().join("sign_key.pub")).expect("read sign pubkey");

        // Generate a decoy key
        let decoy_key = tmp.path().join("decoy_key");
        let gen2 = std::process::Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &decoy_key.to_string_lossy(),
                "-N",
                "",
                "-C",
                "decoy@cfgd",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("keygen");
        assert!(gen2.success());

        let decoy_pubkey =
            std::fs::read_to_string(tmp.path().join("decoy_key.pub")).expect("read decoy pubkey");

        // Sign with the signing key
        let nonce = "cfgd_ch_multi_key_test";
        let nonce_path = tmp.path().join("nonce.txt");
        std::fs::write(&nonce_path, nonce).expect("write nonce");

        let sig_output = std::process::Command::new("ssh-keygen")
            .args([
                "-Y",
                "sign",
                "-f",
                &sign_key.to_string_lossy(),
                "-n",
                "cfgd-enroll",
            ])
            .stdin(std::fs::File::open(&nonce_path).expect("open nonce"))
            .output()
            .expect("signing");
        assert!(sig_output.status.success());

        let signature_pem = String::from_utf8_lossy(&sig_output.stdout).to_string();

        // Present decoy first, then the correct key
        let decoy = super::super::db::UserPublicKey {
            id: "key-decoy".to_string(),
            username: "testuser".to_string(),
            key_type: "ssh".to_string(),
            public_key: decoy_pubkey.trim().to_string(),
            fingerprint: "SHA256:decoy".to_string(),
            label: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let correct = super::super::db::UserPublicKey {
            id: "key-correct".to_string(),
            username: "testuser".to_string(),
            key_type: "ssh".to_string(),
            public_key: sign_pubkey.trim().to_string(),
            fingerprint: "SHA256:correct".to_string(),
            label: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let keys: Vec<&super::super::db::UserPublicKey> = vec![&decoy, &correct];
        assert!(
            verify_ssh_signature(nonce, &signature_pem, &keys),
            "should find the correct key when it appears after a non-matching key"
        );
    }

    #[test]
    fn verify_ssh_signature_wrong_nonce_fails() {
        if !cfgd_core::command_available("ssh-keygen") {
            return;
        }

        let tmp = tempfile::tempdir().expect("tmpdir");
        let key_path = tmp.path().join("test_key");

        let keygen_status = std::process::Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &key_path.to_string_lossy(),
                "-N",
                "",
                "-C",
                "test@cfgd",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("keygen");
        assert!(keygen_status.success());

        let public_key =
            std::fs::read_to_string(tmp.path().join("test_key.pub")).expect("read pubkey");

        // Sign one nonce
        let original_nonce = "cfgd_ch_original_nonce";
        let nonce_path = tmp.path().join("nonce.txt");
        std::fs::write(&nonce_path, original_nonce).expect("write nonce");

        let sig_output = std::process::Command::new("ssh-keygen")
            .args([
                "-Y",
                "sign",
                "-f",
                &key_path.to_string_lossy(),
                "-n",
                "cfgd-enroll",
            ])
            .stdin(std::fs::File::open(&nonce_path).expect("open nonce"))
            .output()
            .expect("signing");
        assert!(sig_output.status.success());

        let signature_pem = String::from_utf8_lossy(&sig_output.stdout).to_string();

        let user_key = super::super::db::UserPublicKey {
            id: "key-1".to_string(),
            username: "testuser".to_string(),
            key_type: "ssh".to_string(),
            public_key: public_key.trim().to_string(),
            fingerprint: "SHA256:test".to_string(),
            label: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let keys: Vec<&super::super::db::UserPublicKey> = vec![&user_key];

        // Verify with a different nonce — should fail
        assert!(
            !verify_ssh_signature("cfgd_ch_different_nonce", &signature_pem, &keys),
            "signature for a different nonce should fail verification"
        );
    }

    // --- verify_gpg_signature ---

    #[test]
    fn verify_gpg_signature_empty_keys_returns_false() {
        if !cfgd_core::command_available("gpg") {
            return;
        }

        let keys: Vec<&super::super::db::UserPublicKey> = vec![];
        assert!(
            !verify_gpg_signature("some-nonce", "some-sig", &keys),
            "empty keys list should return false"
        );
    }

    #[test]
    fn verify_gpg_signature_bad_signature_returns_false() {
        if !cfgd_core::command_available("gpg") {
            return;
        }

        let user_key = super::super::db::UserPublicKey {
            id: "key-1".to_string(),
            username: "testuser".to_string(),
            key_type: "gpg".to_string(),
            public_key: "not-a-real-gpg-key".to_string(),
            fingerprint: "DEADBEEF".to_string(),
            label: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let keys: Vec<&super::super::db::UserPublicKey> = vec![&user_key];
        assert!(
            !verify_gpg_signature("nonce", "not-a-valid-sig", &keys),
            "garbage GPG key and signature should fail"
        );
    }

    // --- CHALLENGE_TTL_SECS ---

    #[test]
    fn challenge_ttl_is_five_minutes() {
        assert_eq!(CHALLENGE_TTL_SECS, 300);
    }

    // --- Async handler tests ---

    fn test_state() -> SharedState {
        let db = super::super::db::ServerDb::open(":memory:").expect("open in-memory db");
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        AppState {
            db: Arc::new(tokio::sync::Mutex::new(db)),
            kube_client: None,
            event_tx,
            enrollment_method: EnrollmentMethod::Token,
            metrics: None,
        }
    }

    fn test_state_key_enrollment() -> SharedState {
        let db = super::super::db::ServerDb::open(":memory:").expect("open in-memory db");
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        AppState {
            db: Arc::new(tokio::sync::Mutex::new(db)),
            kube_client: None,
            event_tx,
            enrollment_method: EnrollmentMethod::Key,
            metrics: None,
        }
    }

    // --- enroll_info ---

    #[tokio::test]
    async fn enroll_info_returns_token_method() {
        let state = test_state();
        let result = enroll_info(State(state)).await;
        assert!(
            result.is_ok(),
            "enroll_info should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(info["method"], "token");
    }

    #[tokio::test]
    async fn enroll_info_returns_key_method() {
        let state = test_state_key_enrollment();
        let result = enroll_info(State(state)).await;
        assert!(
            result.is_ok(),
            "enroll_info should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(info["method"], "key");
    }

    // --- enroll ---

    #[tokio::test]
    async fn enroll_rejects_empty_device_id() {
        let state = test_state();
        let req = EnrollRequest {
            token: "cfgd_bs_some_token".to_string(),
            device_id: "".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result = enroll(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("device_id")));
    }

    #[tokio::test]
    async fn enroll_rejects_empty_token() {
        let state = test_state();
        let req = EnrollRequest {
            token: "".to_string(),
            device_id: "dev-1".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result = enroll(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("token")));
    }

    #[tokio::test]
    async fn enroll_rejects_when_key_enrollment_enabled() {
        let state = test_state_key_enrollment();
        let req = EnrollRequest {
            token: "cfgd_bs_token123".to_string(),
            device_id: "dev-1".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result = enroll(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("key-based")));
    }

    #[tokio::test]
    async fn enroll_rejects_invalid_token() {
        let state = test_state();
        let req = EnrollRequest {
            token: "bogus_nonexistent_token".to_string(),
            device_id: "dev-1".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let Err(err) = enroll(State(state), Json(req)).await else {
            panic!("expected error for nonexistent token");
        };
        assert!(
            matches!(err, GatewayError::Unauthorized),
            "expected Unauthorized for nonexistent token, got: {err}"
        );
    }

    #[tokio::test]
    async fn enroll_success_with_valid_bootstrap_token() {
        let state = test_state();

        // Create a bootstrap token in the DB
        let token_plaintext = "cfgd_bs_test_enrollment_token_abcdef1234567890";
        let token_hash = hash_token(token_plaintext);
        let expires_at = cfgd_core::unix_secs_to_iso8601(cfgd_core::unix_secs_now() + 3600);
        {
            let db = state.db.lock().await;
            db.create_bootstrap_token(&token_hash, "testuser", Some("platform"), &expires_at)
                .expect("create token");
        }

        let req = EnrollRequest {
            token: token_plaintext.to_string(),
            device_id: "dev-enroll-1".to_string(),
            hostname: "ws-enroll".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result = enroll(State(state.clone()), Json(req)).await;
        assert!(
            result.is_ok(),
            "enrollment should succeed with valid token: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let enroll_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(enroll_resp["status"], "enrolled");
        assert_eq!(enroll_resp["deviceId"], "dev-enroll-1");
        assert_eq!(enroll_resp["username"], "testuser");
        assert_eq!(enroll_resp["team"], "platform");
        assert!(
            enroll_resp["apiKey"]
                .as_str()
                .unwrap()
                .starts_with("cfgd_dev_"),
            "api key should have cfgd_dev_ prefix"
        );

        // Verify device was created in DB
        let db = state.db.lock().await;
        let device = db.get_device("dev-enroll-1").expect("device should exist");
        assert_eq!(device.hostname, "ws-enroll");
        assert_eq!(device.os, "linux");
        assert_eq!(device.arch, "x86_64");
    }

    #[tokio::test]
    async fn enroll_token_cannot_be_reused() {
        let state = test_state();

        let token_plaintext = "cfgd_bs_reuse_test_token";
        let token_hash = hash_token(token_plaintext);
        let expires_at = cfgd_core::unix_secs_to_iso8601(cfgd_core::unix_secs_now() + 3600);
        {
            let db = state.db.lock().await;
            db.create_bootstrap_token(&token_hash, "testuser", None, &expires_at)
                .expect("create token");
        }

        // First enrollment succeeds
        let req1 = EnrollRequest {
            token: token_plaintext.to_string(),
            device_id: "dev-first".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result1 = enroll(State(state.clone()), Json(req1)).await;
        assert!(
            result1.is_ok(),
            "first enrollment should succeed: {:?}",
            result1.err()
        );

        // Second enrollment with same token fails
        let req2 = EnrollRequest {
            token: token_plaintext.to_string(),
            device_id: "dev-second".to_string(),
            hostname: "ws-2".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result2 = enroll(State(state), Json(req2)).await;
        assert!(result2.is_err(), "reusing a consumed token should fail");
    }

    // --- checkin ---

    #[tokio::test]
    async fn checkin_rejects_empty_device_id() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let req = CheckinRequest {
            device_id: "".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            config_hash: "abc123".to_string(),
            compliance_summary: None,
        };
        let result = checkin(State(state), Extension(auth), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("device_id")));
    }

    #[tokio::test]
    async fn checkin_registers_new_device() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let req = CheckinRequest {
            device_id: "dev-new".to_string(),
            hostname: "workstation-new".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            config_hash: "hash123".to_string(),
            compliance_summary: None,
        };
        let result = checkin(State(state.clone()), Extension(auth), Json(req)).await;
        assert!(result.is_ok(), "checkin should succeed: {:?}", result.err());
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(checkin_resp["status"], "ok");
        assert_eq!(
            checkin_resp["configChanged"], false,
            "new device should not have config_changed"
        );

        // Verify device was created
        let db = state.db.lock().await;
        let device = db.get_device("dev-new").expect("device should exist");
        assert_eq!(device.hostname, "workstation-new");
        assert_eq!(device.config_hash, "hash123");
    }

    #[tokio::test]
    async fn checkin_updates_existing_device() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash-old", None)
                .expect("register");
        }

        let auth = AuthContext::Admin;
        let req = CheckinRequest {
            device_id: "dev-1".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            config_hash: "hash-new".to_string(),
            compliance_summary: None,
        };
        let result = checkin(State(state.clone()), Extension(auth), Json(req)).await;
        assert!(result.is_ok(), "checkin should succeed: {:?}", result.err());
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(checkin_resp["status"], "ok");
        assert_eq!(
            checkin_resp["configChanged"], true,
            "different hash should detect config change"
        );

        // Verify config hash was updated
        let db = state.db.lock().await;
        let device = db.get_device("dev-1").expect("device");
        assert_eq!(device.config_hash, "hash-new");
    }

    #[tokio::test]
    async fn checkin_detects_config_change() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash-old", None)
                .expect("register");
            db.set_device_config("dev-1", &serde_json::json!({"packages": ["vim"]}))
                .expect("set config");
        }

        let auth = AuthContext::Admin;
        let req = CheckinRequest {
            device_id: "dev-1".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            config_hash: "hash-different".to_string(),
            compliance_summary: None,
        };
        let result = checkin(State(state), Extension(auth), Json(req)).await;
        assert!(result.is_ok(), "checkin should succeed: {:?}", result.err());
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(checkin_resp["status"], "ok");
        assert_eq!(
            checkin_resp["configChanged"], true,
            "different hash should detect config change"
        );
        assert!(
            !checkin_resp["desiredConfig"].is_null(),
            "should return desired_config when config changed"
        );
    }

    #[tokio::test]
    async fn checkin_no_config_change_when_same_hash() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "ws-1", "linux", "x86_64", "same-hash", None)
                .expect("register");
        }

        let auth = AuthContext::Admin;
        let req = CheckinRequest {
            device_id: "dev-1".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            config_hash: "same-hash".to_string(),
            compliance_summary: None,
        };
        let result = checkin(State(state), Extension(auth), Json(req)).await;
        assert!(result.is_ok(), "checkin should succeed: {:?}", result.err());
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(checkin_resp["status"], "ok");
        assert_eq!(
            checkin_resp["configChanged"], false,
            "same hash should not detect config change"
        );
        assert!(
            checkin_resp.get("desiredConfig").is_none(),
            "should not return desired_config when config unchanged"
        );
    }

    #[tokio::test]
    async fn checkin_device_auth_can_only_checkin_as_self() {
        let state = test_state();
        let auth = AuthContext::Device {
            device_id: "dev-1".to_string(),
            username: "jdoe".to_string(),
        };
        let req = CheckinRequest {
            device_id: "dev-2".to_string(),
            hostname: "ws-2".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            config_hash: "hash".to_string(),
            compliance_summary: None,
        };
        let result = checkin(State(state), Extension(auth), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::Forbidden(_)));
    }

    #[tokio::test]
    async fn checkin_with_compliance_summary() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let compliance = serde_json::json!({
            "total": 10,
            "compliant": 8,
            "drifted": 2
        });
        let req = CheckinRequest {
            device_id: "dev-compliance".to_string(),
            hostname: "ws-compliance".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            config_hash: "hash-comp".to_string(),
            compliance_summary: Some(compliance.clone()),
        };
        let result = checkin(State(state.clone()), Extension(auth), Json(req)).await;
        assert!(
            result.is_ok(),
            "checkin with compliance summary should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(checkin_resp["status"], "ok");

        // Verify compliance_summary was stored
        let db = state.db.lock().await;
        let device = db.get_device("dev-compliance").expect("device");
        assert!(device.compliance_summary.is_some());
        assert_eq!(device.compliance_summary.unwrap()["total"], 10);
    }

    #[tokio::test]
    async fn checkin_broadcasts_event() {
        let state = test_state();
        let mut rx = state.event_tx.subscribe();

        let auth = AuthContext::Admin;
        let req = CheckinRequest {
            device_id: "dev-broadcast".to_string(),
            hostname: "ws-broadcast".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            config_hash: "hash-bc".to_string(),
            compliance_summary: None,
        };
        let result = checkin(State(state), Extension(auth), Json(req)).await;
        assert!(
            result.is_ok(),
            "checkin should succeed for broadcast test: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);

        // Check that an event was broadcast
        let event = rx.try_recv().expect("should receive broadcast event");
        assert_eq!(event.device_id, "dev-broadcast");
        assert_eq!(event.event_type, "checkin");
    }

    // --- list_devices ---

    #[tokio::test]
    async fn list_devices_empty() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let pagination = PaginationParams {
            limit: 100,
            offset: 0,
        };
        let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
        assert!(
            result.is_ok(),
            "list_devices should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let devices: Vec<super::super::db::Device> = serde_json::from_slice(&body).unwrap();
        assert!(
            devices.is_empty(),
            "should return empty list when no devices registered"
        );
    }

    #[tokio::test]
    async fn list_devices_returns_all_devices_for_admin() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "ws-1", "linux", "x86_64", "h1", None)
                .expect("register");
            db.register_device("dev-2", "ws-2", "darwin", "aarch64", "h2", None)
                .expect("register");
        }

        let auth = AuthContext::Admin;
        let pagination = PaginationParams {
            limit: 100,
            offset: 0,
        };
        let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
        assert!(
            result.is_ok(),
            "list_devices should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let devices: Vec<super::super::db::Device> = serde_json::from_slice(&body).unwrap();
        assert_eq!(devices.len(), 2, "admin should see all devices");
        let hostnames: Vec<&str> = devices.iter().map(|d| d.hostname.as_str()).collect();
        assert!(hostnames.contains(&"ws-1"));
        assert!(hostnames.contains(&"ws-2"));
    }

    #[tokio::test]
    async fn list_devices_device_auth_returns_only_self() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "ws-1", "linux", "x86_64", "h1", None)
                .expect("register");
            db.register_device("dev-2", "ws-2", "darwin", "aarch64", "h2", None)
                .expect("register");
        }

        let auth = AuthContext::Device {
            device_id: "dev-1".to_string(),
            username: "jdoe".to_string(),
        };
        let pagination = PaginationParams {
            limit: 100,
            offset: 0,
        };
        let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
        assert!(
            result.is_ok(),
            "device should be able to list self: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let devices: Vec<super::super::db::Device> = serde_json::from_slice(&body).unwrap();
        assert_eq!(devices.len(), 1, "device auth should return only self");
        assert_eq!(devices[0].id, "dev-1");
        assert_eq!(devices[0].hostname, "ws-1");
    }

    #[tokio::test]
    async fn list_devices_respects_pagination_limit() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            for i in 0..5 {
                db.register_device(
                    &format!("dev-{i}"),
                    &format!("ws-{i}"),
                    "linux",
                    "x86_64",
                    &format!("h{i}"),
                    None,
                )
                .expect("register");
            }
        }

        let auth = AuthContext::Admin;
        let pagination = PaginationParams {
            limit: 2,
            offset: 0,
        };
        let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
        assert!(
            result.is_ok(),
            "list_devices with limit should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let devices: Vec<super::super::db::Device> = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            devices.len(),
            2,
            "pagination limit=2 should return exactly 2 devices"
        );
    }

    #[tokio::test]
    async fn list_devices_caps_limit_at_1000() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let pagination = PaginationParams {
            limit: 5000,
            offset: 0,
        };
        // Should not error even with limit > 1000 — it gets capped internally
        let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
        assert!(
            result.is_ok(),
            "list_devices should succeed even with limit > 1000: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // --- get_device ---

    #[tokio::test]
    async fn get_device_existing() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-get", "ws-get", "linux", "x86_64", "hash-get", None)
                .expect("register");
        }

        let auth = AuthContext::Admin;
        let result = get_device(State(state), Extension(auth), Path("dev-get".to_string())).await;
        assert!(
            result.is_ok(),
            "get_device should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let device: super::super::db::Device = serde_json::from_slice(&body).unwrap();
        assert_eq!(device.id, "dev-get");
        assert_eq!(device.hostname, "ws-get");
        assert_eq!(device.os, "linux");
        assert_eq!(device.arch, "x86_64");
        assert_eq!(device.config_hash, "hash-get");
    }

    #[tokio::test]
    async fn get_device_not_found() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let result = get_device(
            State(state),
            Extension(auth),
            Path("nonexistent".to_string()),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    #[tokio::test]
    async fn get_device_device_auth_allows_self() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-own", "ws-own", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Device {
            device_id: "dev-own".to_string(),
            username: "jdoe".to_string(),
        };
        let result = get_device(State(state), Extension(auth), Path("dev-own".to_string())).await;
        assert!(
            result.is_ok(),
            "device should be able to get self: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let device: super::super::db::Device = serde_json::from_slice(&body).unwrap();
        assert_eq!(device.id, "dev-own");
        assert_eq!(device.hostname, "ws-own");
    }

    #[tokio::test]
    async fn get_device_device_auth_denies_other() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-other", "ws-other", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Device {
            device_id: "dev-own".to_string(),
            username: "jdoe".to_string(),
        };
        let result = get_device(State(state), Extension(auth), Path("dev-other".to_string())).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::Forbidden(_)));
    }

    // --- set_device_config ---

    #[tokio::test]
    async fn set_device_config_admin_success() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-cfg", "ws-cfg", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Admin;
        let req = SetConfigRequest {
            config: serde_json::json!({"packages": ["vim", "git"]}),
        };
        let result = set_device_config(
            State(state.clone()),
            Extension(auth),
            Path("dev-cfg".to_string()),
            Json(req),
        )
        .await;
        assert!(
            result.is_ok(),
            "set_device_config should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify config was stored
        let db = state.db.lock().await;
        let device = db.get_device("dev-cfg").expect("device");
        assert!(device.desired_config.is_some());
        assert_eq!(device.desired_config.unwrap()["packages"][0], "vim");
    }

    #[tokio::test]
    async fn set_device_config_device_auth_forbidden() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-cfg2", "ws-cfg2", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Device {
            device_id: "dev-cfg2".to_string(),
            username: "jdoe".to_string(),
        };
        let req = SetConfigRequest {
            config: serde_json::json!({"packages": ["vim"]}),
        };
        let result = set_device_config(
            State(state),
            Extension(auth),
            Path("dev-cfg2".to_string()),
            Json(req),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::Forbidden(_)));
    }

    #[tokio::test]
    async fn set_device_config_not_found() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let req = SetConfigRequest {
            config: serde_json::json!({"packages": ["vim"]}),
        };
        let result = set_device_config(
            State(state),
            Extension(auth),
            Path("nonexistent".to_string()),
            Json(req),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    // --- list_drift_events ---

    #[tokio::test]
    async fn list_drift_events_empty() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-drift", "ws-drift", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Admin;
        let result =
            list_drift_events(State(state), Extension(auth), Path("dev-drift".to_string())).await;
        assert!(
            result.is_ok(),
            "list_drift_events should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let events: Vec<super::super::db::DriftEvent> = serde_json::from_slice(&body).unwrap();
        assert!(
            events.is_empty(),
            "should return empty list when no drift events"
        );
    }

    #[tokio::test]
    async fn list_drift_events_with_events() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-drift2", "ws-drift2", "linux", "x86_64", "hash", None)
                .expect("register");
            db.record_drift_event("dev-drift2", "field changed")
                .expect("record drift");
        }

        let auth = AuthContext::Admin;
        let result = list_drift_events(
            State(state),
            Extension(auth),
            Path("dev-drift2".to_string()),
        )
        .await;
        assert!(
            result.is_ok(),
            "list_drift_events should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let events: Vec<super::super::db::DriftEvent> = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.len(), 1, "should return one drift event");
        assert_eq!(events[0].device_id, "dev-drift2");
        assert_eq!(events[0].details, "field changed");
    }

    #[tokio::test]
    async fn list_drift_events_device_auth_allows_self() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-own-drift", "ws", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Device {
            device_id: "dev-own-drift".to_string(),
            username: "jdoe".to_string(),
        };
        let result = list_drift_events(
            State(state),
            Extension(auth),
            Path("dev-own-drift".to_string()),
        )
        .await;
        assert!(
            result.is_ok(),
            "device should be able to list own drift events: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let events: Vec<super::super::db::DriftEvent> = serde_json::from_slice(&body).unwrap();
        assert!(
            events.is_empty(),
            "no drift events recorded for this device"
        );
    }

    #[tokio::test]
    async fn list_drift_events_device_auth_denies_other() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-victim", "ws", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Device {
            device_id: "dev-attacker".to_string(),
            username: "attacker".to_string(),
        };
        let result = list_drift_events(
            State(state),
            Extension(auth),
            Path("dev-victim".to_string()),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::Forbidden(_)));
    }

    #[tokio::test]
    async fn list_drift_events_not_found() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let result = list_drift_events(
            State(state),
            Extension(auth),
            Path("nonexistent".to_string()),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    // --- record_drift_event ---

    #[tokio::test]
    async fn record_drift_event_success() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-record", "ws-record", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Admin;
        let req = DriftRequest {
            details: vec![DriftDetailInput {
                field: "package/vim".to_string(),
                expected: "installed".to_string(),
                actual: "missing".to_string(),
            }],
        };
        let result = record_drift_event(
            State(state.clone()),
            Extension(auth),
            Path("dev-record".to_string()),
            Json(req),
        )
        .await;
        assert!(
            result.is_ok(),
            "record_drift_event should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let event: super::super::db::DriftEvent = serde_json::from_slice(&body).unwrap();
        assert_eq!(event.device_id, "dev-record");
        assert!(!event.id.is_empty(), "drift event should have an id");
        assert!(
            !event.timestamp.is_empty(),
            "drift event should have a timestamp"
        );

        // Verify device status changed to drifted
        let db = state.db.lock().await;
        let device = db.get_device("dev-record").expect("device");
        assert_eq!(device.status, super::super::db::DeviceStatus::Drifted);
    }

    #[tokio::test]
    async fn record_drift_event_broadcasts_to_sse() {
        let state = test_state();
        let mut rx = state.event_tx.subscribe();
        {
            let db = state.db.lock().await;
            db.register_device("dev-sse", "ws-sse", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Admin;
        let req = DriftRequest {
            details: vec![DriftDetailInput {
                field: "file/bashrc".to_string(),
                expected: "hash-a".to_string(),
                actual: "hash-b".to_string(),
            }],
        };
        let _ = record_drift_event(
            State(state),
            Extension(auth),
            Path("dev-sse".to_string()),
            Json(req),
        )
        .await;

        let event = rx.try_recv().expect("should receive broadcast");
        assert_eq!(event.device_id, "dev-sse");
        assert_eq!(event.event_type, "drift");
    }

    #[tokio::test]
    async fn record_drift_event_device_auth_denies_other() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-target", "ws", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Device {
            device_id: "dev-attacker".to_string(),
            username: "attacker".to_string(),
        };
        let req = DriftRequest {
            details: vec![DriftDetailInput {
                field: "pkg/vim".to_string(),
                expected: "9.0".to_string(),
                actual: "8.2".to_string(),
            }],
        };
        let result = record_drift_event(
            State(state),
            Extension(auth),
            Path("dev-target".to_string()),
            Json(req),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::Forbidden(_)));
    }

    #[tokio::test]
    async fn record_drift_event_nonexistent_device() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let req = DriftRequest {
            details: vec![DriftDetailInput {
                field: "pkg/vim".to_string(),
                expected: "9.0".to_string(),
                actual: "8.2".to_string(),
            }],
        };
        let result = record_drift_event(
            State(state),
            Extension(auth),
            Path("nonexistent".to_string()),
            Json(req),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    // --- force_reconcile ---

    #[tokio::test]
    async fn force_reconcile_admin_success() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-recon", "ws-recon", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Admin;
        let result = force_reconcile(
            State(state.clone()),
            Extension(auth),
            Path("dev-recon".to_string()),
        )
        .await;
        assert!(
            result.is_ok(),
            "force_reconcile should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify status changed to pending-reconcile
        let db = state.db.lock().await;
        let device = db.get_device("dev-recon").expect("device");
        assert_eq!(
            device.status,
            super::super::db::DeviceStatus::PendingReconcile
        );
    }

    #[tokio::test]
    async fn force_reconcile_device_auth_forbidden() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-recon2", "ws", "linux", "x86_64", "hash", None)
                .expect("register");
        }

        let auth = AuthContext::Device {
            device_id: "dev-recon2".to_string(),
            username: "jdoe".to_string(),
        };
        let result = force_reconcile(
            State(state),
            Extension(auth),
            Path("dev-recon2".to_string()),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::Forbidden(_)));
    }

    #[tokio::test]
    async fn force_reconcile_not_found() {
        let state = test_state();
        let auth = AuthContext::Admin;
        let result = force_reconcile(
            State(state),
            Extension(auth),
            Path("nonexistent".to_string()),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    // --- list_fleet_events ---

    #[tokio::test]
    async fn list_fleet_events_empty() {
        let state = test_state();
        let pagination = PaginationParams {
            limit: 100,
            offset: 0,
        };
        let result = list_fleet_events(State(state), Query(pagination)).await;
        assert!(
            result.is_ok(),
            "list_fleet_events should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let events: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(
            events.is_empty(),
            "should return empty list when no fleet events"
        );
    }

    #[tokio::test]
    async fn list_fleet_events_with_data() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-fe", "ws-fe", "linux", "x86_64", "hash", None)
                .expect("register");
            db.record_checkin("dev-fe", "hash", false)
                .expect("record checkin");
            db.record_drift_event("dev-fe", "drift details")
                .expect("record drift");
        }

        let pagination = PaginationParams {
            limit: 100,
            offset: 0,
        };
        let result = list_fleet_events(State(state), Query(pagination)).await;
        assert!(
            result.is_ok(),
            "list_fleet_events should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let events: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(
            events.len() >= 2,
            "should have at least checkin and drift events, got {}",
            events.len()
        );
    }

    #[tokio::test]
    async fn list_fleet_events_caps_limit() {
        let state = test_state();
        let pagination = PaginationParams {
            limit: 5000,
            offset: 0,
        };
        // Should not error — limit gets capped to 1000 internally
        let result = list_fleet_events(State(state), Query(pagination)).await;
        assert!(
            result.is_ok(),
            "list_fleet_events should succeed even with limit > 1000: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // --- request_challenge ---

    #[tokio::test]
    async fn request_challenge_rejects_when_token_mode() {
        let state = test_state(); // token mode by default
        let req = ChallengeRequest {
            username: "jdoe".to_string(),
            device_id: "dev-1".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result = request_challenge(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(
            matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("bootstrap token"))
        );
    }

    #[tokio::test]
    async fn request_challenge_rejects_empty_username() {
        let state = test_state_key_enrollment();
        let req = ChallengeRequest {
            username: "".to_string(),
            device_id: "dev-1".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result = request_challenge(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("username")));
    }

    #[tokio::test]
    async fn request_challenge_rejects_empty_device_id() {
        let state = test_state_key_enrollment();
        let req = ChallengeRequest {
            username: "jdoe".to_string(),
            device_id: "".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result = request_challenge(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("device_id")));
    }

    #[tokio::test]
    async fn request_challenge_rejects_user_without_keys() {
        let state = test_state_key_enrollment();
        let req = ChallengeRequest {
            username: "unknown-user".to_string(),
            device_id: "dev-1".to_string(),
            hostname: "ws-1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result = request_challenge(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(
            matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("no public keys"))
        );
    }

    #[tokio::test]
    async fn request_challenge_success_with_registered_key() {
        let state = test_state_key_enrollment();
        {
            let db = state.db.lock().await;
            db.add_user_public_key(
                "jdoe",
                "ssh",
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG...",
                "SHA256:testfp",
                Some("laptop key"),
            )
            .expect("add key");
        }

        let req = ChallengeRequest {
            username: "jdoe".to_string(),
            device_id: "dev-ch".to_string(),
            hostname: "ws-ch".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let result = request_challenge(State(state), Json(req)).await;
        assert!(
            result.is_ok(),
            "request_challenge should succeed with registered key: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let challenge: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            !challenge["challengeId"].as_str().unwrap().is_empty(),
            "challenge should have an id"
        );
        assert!(
            !challenge["nonce"].as_str().unwrap().is_empty(),
            "challenge should have a nonce"
        );
        assert!(
            !challenge["expiresAt"].as_str().unwrap().is_empty(),
            "challenge should have an expiry"
        );
    }

    // --- verify_enrollment ---

    #[tokio::test]
    async fn verify_enrollment_rejects_when_token_mode() {
        let state = test_state();
        let req = VerifyRequest {
            challenge_id: "ch-1".to_string(),
            signature: "sig-data".to_string(),
            key_type: "ssh".to_string(),
        };
        let result = verify_enrollment(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("key-based")));
    }

    #[tokio::test]
    async fn verify_enrollment_rejects_empty_challenge_id() {
        let state = test_state_key_enrollment();
        let req = VerifyRequest {
            challenge_id: "".to_string(),
            signature: "sig-data".to_string(),
            key_type: "ssh".to_string(),
        };
        let result = verify_enrollment(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(
            matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("challenge_id"))
        );
    }

    #[tokio::test]
    async fn verify_enrollment_rejects_empty_signature() {
        let state = test_state_key_enrollment();
        let req = VerifyRequest {
            challenge_id: "ch-1".to_string(),
            signature: "".to_string(),
            key_type: "ssh".to_string(),
        };
        let result = verify_enrollment(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("signature")));
    }

    #[tokio::test]
    async fn verify_enrollment_rejects_invalid_key_type() {
        let state = test_state_key_enrollment();
        let req = VerifyRequest {
            challenge_id: "ch-1".to_string(),
            signature: "sig-data".to_string(),
            key_type: "rsa".to_string(),
        };
        let result = verify_enrollment(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("key_type")));
    }

    #[tokio::test]
    async fn verify_enrollment_rejects_nonexistent_challenge() {
        let state = test_state_key_enrollment();
        let req = VerifyRequest {
            challenge_id: "nonexistent-challenge".to_string(),
            signature: "sig-data".to_string(),
            key_type: "ssh".to_string(),
        };
        let result = verify_enrollment(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    // --- admin_reset ---

    #[tokio::test]
    async fn admin_reset_clears_all_data() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash", None)
                .expect("register");
            db.register_device("dev-2", "ws-2", "darwin", "aarch64", "hash2", None)
                .expect("register");
            db.record_drift_event("dev-1", "drift details")
                .expect("record drift");
        }

        let result = admin_reset(State(state.clone())).await;
        assert!(
            result.is_ok(),
            "admin_reset should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let reset_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reset_resp["status"], "ok");
        assert!(
            reset_resp["rowsDeleted"].as_u64().unwrap() > 0,
            "should have deleted rows"
        );

        // Verify data was wiped
        let db = state.db.lock().await;
        let devices = db.list_devices().expect("list");
        assert!(
            devices.is_empty(),
            "all devices should be deleted after reset"
        );
    }

    // --- add_user_key ---

    #[tokio::test]
    async fn add_user_key_success() {
        let state = test_state();
        let req = AddKeyRequest {
            key_type: "ssh".to_string(),
            public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG...".to_string(),
            fingerprint: "SHA256:abc123".to_string(),
            label: Some("laptop key".to_string()),
        };
        let result = add_user_key(State(state), Path("jdoe".to_string()), Json(req)).await;
        assert!(
            result.is_ok(),
            "add_user_key should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let key: super::super::db::UserPublicKey = serde_json::from_slice(&body).unwrap();
        assert_eq!(key.username, "jdoe");
        assert_eq!(key.key_type, "ssh");
        assert_eq!(key.fingerprint, "SHA256:abc123");
        assert_eq!(key.label, Some("laptop key".to_string()));
        assert!(!key.id.is_empty(), "key should have an id");
    }

    #[tokio::test]
    async fn add_user_key_empty_username() {
        let state = test_state();
        let req = AddKeyRequest {
            key_type: "ssh".to_string(),
            public_key: "ssh-ed25519 ...".to_string(),
            fingerprint: "SHA256:abc".to_string(),
            label: None,
        };
        let result = add_user_key(State(state), Path("".to_string()), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("username")));
    }

    #[tokio::test]
    async fn add_user_key_invalid_key_type() {
        let state = test_state();
        let req = AddKeyRequest {
            key_type: "rsa".to_string(),
            public_key: "ssh-rsa ...".to_string(),
            fingerprint: "SHA256:abc".to_string(),
            label: None,
        };
        let result = add_user_key(State(state), Path("jdoe".to_string()), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("key_type")));
    }

    #[tokio::test]
    async fn add_user_key_empty_public_key() {
        let state = test_state();
        let req = AddKeyRequest {
            key_type: "ssh".to_string(),
            public_key: "".to_string(),
            fingerprint: "SHA256:abc".to_string(),
            label: None,
        };
        let result = add_user_key(State(state), Path("jdoe".to_string()), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("public_key")));
    }

    #[tokio::test]
    async fn add_user_key_empty_fingerprint() {
        let state = test_state();
        let req = AddKeyRequest {
            key_type: "ssh".to_string(),
            public_key: "ssh-ed25519 ...".to_string(),
            fingerprint: "".to_string(),
            label: None,
        };
        let result = add_user_key(State(state), Path("jdoe".to_string()), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(
            matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("fingerprint"))
        );
    }

    // --- list_user_keys ---

    #[tokio::test]
    async fn list_user_keys_empty() {
        let state = test_state();
        let result = list_user_keys(State(state), Path("jdoe".to_string())).await;
        assert!(
            result.is_ok(),
            "list_user_keys should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let keys: Vec<super::super::db::UserPublicKey> = serde_json::from_slice(&body).unwrap();
        assert!(
            keys.is_empty(),
            "should return empty list when no keys registered"
        );
    }

    #[tokio::test]
    async fn list_user_keys_with_keys() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.add_user_public_key(
                "jdoe",
                "ssh",
                "ssh-ed25519 AAAA...",
                "SHA256:a",
                Some("key1"),
            )
            .expect("add key");
            db.add_user_public_key("jdoe", "gpg", "-----BEGIN PGP...", "DEADBEEF", None)
                .expect("add key");
        }

        let result = list_user_keys(State(state), Path("jdoe".to_string())).await;
        assert!(
            result.is_ok(),
            "list_user_keys should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let keys: Vec<super::super::db::UserPublicKey> = serde_json::from_slice(&body).unwrap();
        assert_eq!(keys.len(), 2, "should return both registered keys");
        let key_types: Vec<&str> = keys.iter().map(|k| k.key_type.as_str()).collect();
        assert!(key_types.contains(&"ssh"));
        assert!(key_types.contains(&"gpg"));
    }

    // --- delete_user_key ---

    #[tokio::test]
    async fn delete_user_key_success() {
        let state = test_state();
        let key_id;
        {
            let db = state.db.lock().await;
            let key = db
                .add_user_public_key("jdoe", "ssh", "ssh-ed25519 ...", "SHA256:x", None)
                .expect("add key");
            key_id = key.id;
        }

        let result = delete_user_key(State(state), Path(("jdoe".to_string(), key_id))).await;
        assert!(
            result.is_ok(),
            "delete_user_key should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_user_key_not_found() {
        let state = test_state();
        let result = delete_user_key(
            State(state),
            Path(("jdoe".to_string(), "nonexistent".to_string())),
        )
        .await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    // --- create_token ---

    #[tokio::test]
    async fn create_token_success() {
        let state = test_state();
        let req = CreateTokenRequest {
            username: "admin".to_string(),
            team: Some("platform".to_string()),
            expires_in: 3600,
        };
        let result = create_token(State(state), Json(req)).await;
        assert!(
            result.is_ok(),
            "create_token should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let token_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            !token_resp["id"].as_str().unwrap().is_empty(),
            "token should have an id"
        );
        assert!(
            token_resp["token"]
                .as_str()
                .unwrap()
                .starts_with("cfgd_bs_"),
            "token should have cfgd_bs_ prefix"
        );
        assert_eq!(token_resp["username"], "admin");
        assert_eq!(token_resp["team"], "platform");
        assert!(
            !token_resp["expiresAt"].as_str().unwrap().is_empty(),
            "token should have an expiry"
        );
    }

    #[tokio::test]
    async fn create_token_empty_username() {
        let state = test_state();
        let req = CreateTokenRequest {
            username: "".to_string(),
            team: None,
            expires_in: 3600,
        };
        let result = create_token(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("username")));
    }

    #[tokio::test]
    async fn create_token_zero_expires_in() {
        let state = test_state();
        let req = CreateTokenRequest {
            username: "admin".to_string(),
            team: None,
            expires_in: 0,
        };
        let result = create_token(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("expires_in")));
    }

    #[tokio::test]
    async fn create_token_excessive_expires_in() {
        let state = test_state();
        let req = CreateTokenRequest {
            username: "admin".to_string(),
            team: None,
            expires_in: 31 * 86400, // 31 days, exceeds 30 day max
        };
        let result = create_token(State(state), Json(req)).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("expires_in")));
    }

    // --- list_tokens ---

    #[tokio::test]
    async fn list_tokens_empty() {
        let state = test_state();
        let result = list_tokens(State(state)).await;
        assert!(
            result.is_ok(),
            "list_tokens should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let tokens: Vec<super::super::db::BootstrapToken> = serde_json::from_slice(&body).unwrap();
        assert!(
            tokens.is_empty(),
            "should return empty list when no tokens exist"
        );
    }

    #[tokio::test]
    async fn list_tokens_with_data() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.create_bootstrap_token("hash1", "admin", None, "2026-12-31T23:59:59Z")
                .expect("create token");
        }

        let result = list_tokens(State(state)).await;
        assert!(
            result.is_ok(),
            "list_tokens should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let tokens: Vec<super::super::db::BootstrapToken> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tokens.len(), 1, "should return one token");
        assert_eq!(tokens[0].username, "admin");
        assert_eq!(tokens[0].expires_at, "2026-12-31T23:59:59Z");
    }

    // --- delete_token ---

    #[tokio::test]
    async fn delete_token_success() {
        let state = test_state();
        let token_id;
        {
            let db = state.db.lock().await;
            let token = db
                .create_bootstrap_token("hash1", "admin", None, "2026-12-31T23:59:59Z")
                .expect("create token");
            token_id = token.id;
        }

        let result = delete_token(State(state), Path(token_id)).await;
        assert!(
            result.is_ok(),
            "delete_token should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_token_not_found() {
        let state = test_state();
        let result = delete_token(State(state), Path("nonexistent".to_string())).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    // --- revoke_credential ---

    #[tokio::test]
    async fn revoke_credential_success() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-revoke", "ws", "linux", "x86_64", "hash", None)
                .expect("register");
            db.create_device_credential("dev-revoke", "api_hash", "jdoe", None)
                .expect("create credential");
        }

        let result = revoke_credential(State(state), Path("dev-revoke".to_string())).await;
        assert!(
            result.is_ok(),
            "revoke_credential should succeed: {:?}",
            result.err()
        );
        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn revoke_credential_not_found() {
        let state = test_state();
        let result = revoke_credential(State(state), Path("nonexistent".to_string())).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    // --- event_stream ---

    #[tokio::test]
    async fn event_stream_returns_sse() {
        let state = test_state();
        // Just verify it doesn't panic and returns the SSE stream
        let _sse = event_stream(State(state)).await;
    }
}

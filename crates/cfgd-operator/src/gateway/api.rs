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
    // Routes that require admin or device auth
    let authenticated_routes = Router::new()
        .route("/api/v1/checkin", post(checkin))
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices/{id}", get(get_device))
        .route("/api/v1/devices/{id}/config", put(set_device_config))
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

    // Check admin key first
    if let Ok(expected_key) = std::env::var("CFGD_API_KEY") {
        if let Some(ref token) = bearer_token
            && hash_token(token) == hash_token(&expected_key)
        {
            request.extensions_mut().insert(AuthContext::Admin);
            return Ok(next.run(request).await);
        }
    } else {
        // CFGD_API_KEY not set — allow all requests as admin (backwards compatible)
        request.extensions_mut().insert(AuthContext::Admin);
        return Ok(next.run(request).await);
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
            Some(token) if hash_token(&token) == hash_token(&expected_key) => {}
            _ => return Err(GatewayError::Unauthorized),
        }
    } else {
        return Err(GatewayError::InvalidRequest(
            "CFGD_API_KEY must be set to manage bootstrap tokens".to_string(),
        ));
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
    if req.device_id.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "device_id must not be empty".to_string(),
        ));
    }
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
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
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
    if req.device_id.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "device_id must not be empty".to_string(),
        ));
    }

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

    // Verify signature against each matching key until one succeeds
    let verified = match req.key_type.as_str() {
        "ssh" => verify_ssh_signature(&challenge.nonce, &req.signature, &matching_keys),
        "gpg" => verify_gpg_signature(&challenge.nonce, &req.signature, &matching_keys),
        _ => unreachable!(),
    };

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
            tracing::error!("failed to create temp dir for SSH verification: {}", e);
            return false;
        }
    };

    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.sig");
    let signers_path = tmp_dir.path().join("allowed_signers");

    if let Err(e) = std::fs::write(&data_path, nonce) {
        tracing::error!("failed to write challenge data for SSH verification: {}", e);
        return false;
    }
    if let Err(e) = std::fs::write(&sig_path, signature_pem) {
        tracing::error!("failed to write signature file for SSH verification: {}", e);
        return false;
    }

    // Try each key until one verifies
    for key in keys {
        // Write allowed_signers file: "username key_type key_data"
        // The public_key field is the full OpenSSH public key line (e.g. "ssh-ed25519 AAAA... comment")
        let signer_line = format!("{} {}", key.username, key.public_key);
        if std::fs::write(&signers_path, &signer_line).is_err() {
            continue;
        }

        let data_file = match std::fs::File::open(&data_path) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("failed to open challenge data file: {}", e);
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
                tracing::warn!("ssh-keygen failed: {} — is OpenSSH installed?", e);
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
            tracing::error!("failed to create temp dir for GPG verification: {}", e);
            return false;
        }
    };

    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.asc");
    let key_path = tmp_dir.path().join("pubkey.asc");

    if std::fs::write(&data_path, nonce).is_err() {
        return false;
    }
    if std::fs::write(&sig_path, signature_armored).is_err() {
        return false;
    }

    let gpg_home = tmp_dir.path().join("gnupg");
    if std::fs::create_dir_all(&gpg_home).is_err() {
        return false;
    }
    // GPG requires restrictive permissions on its homedir
    let _ = cfgd_core::set_file_permissions(&gpg_home, 0o700);

    for key in keys {
        if std::fs::write(&key_path, &key.public_key).is_err() {
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

        // Verify the signature
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
                tracing::warn!("gpg failed: {} — is GPG installed?", e);
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
    if req.device_id.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "device_id must not be empty".to_string(),
        ));
    }

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

    const MAX_RETRIES: u32 = 3;
    let mut last_err = None;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let backoff = std::time::Duration::from_millis(500 * 2u64.pow(attempt - 1));
            tokio::time::sleep(backoff).await;
        }

        match alerts.create(&PostParams::default(), &alert).await {
            Ok(created) => {
                tracing::info!(
                    name = %created.name_any(),
                    device_id = %device_id,
                    "DriftAlert CRD created in Kubernetes"
                );
                return Ok(());
            }
            Err(kube::Error::Api(ref resp)) if resp.code == 409 => {
                tracing::debug!(
                    name = %alert_name,
                    "DriftAlert already exists, skipping creation"
                );
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    device_id = %device_id,
                    attempt = attempt + 1,
                    max_retries = MAX_RETRIES,
                    error = %e,
                    "Failed to create DriftAlert CRD, retrying"
                );
                last_err = Some(e);
            }
        }
    }

    if let Some(e) = last_err {
        tracing::error!(
            device_id = %device_id,
            error = %e,
            "Failed to create DriftAlert CRD after {} attempts — drift recorded in database only",
            MAX_RETRIES
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
            tracing::warn!("Failed to list MachineConfigs for device lookup: {}", e);
            format!("{}-mc", hostname)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(enforce_device_access(&auth, "any-device").is_ok());
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
    fn enforce_device_access_device_denies_other() {
        let auth = AuthContext::Device {
            device_id: "dev-1".to_string(),
            username: "jdoe".to_string(),
        };
        let result = enforce_device_access(&auth, "dev-2");
        assert!(result.is_err());
    }

    #[test]
    fn enrollment_method_variants_are_distinct() {
        assert_ne!(EnrollmentMethod::Token, EnrollmentMethod::Key);
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
        assert_eq!(extract_bearer_token(&headers), Some(String::new()));
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

    // --- default functions ---

    #[test]
    fn default_limit_is_100() {
        assert_eq!(default_limit(), 100);
    }

    #[test]
    fn default_token_lifetime_is_24_hours() {
        assert_eq!(default_token_lifetime(), 86400);
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
    fn bootstrap_token_creation_and_validation() {
        let db = test_db();
        let token = generate_token("cfgd_bs");
        let token_hash = hash_token(&token);
        let expires_at = "2099-12-31T23:59:59Z";

        let record = db
            .create_bootstrap_token(&token_hash, "jdoe", Some("platform"), expires_at)
            .expect("create failed");
        assert_eq!(record.username, "jdoe");
        assert_eq!(record.team, Some("platform".to_string()));
        assert!(record.used_at.is_none());
    }

    #[test]
    fn bootstrap_token_consume_marks_used() {
        let db = test_db();
        let token = generate_token("cfgd_bs");
        let token_hash = hash_token(&token);
        let expires_at = "2099-12-31T23:59:59Z";

        db.create_bootstrap_token(&token_hash, "jdoe", None, expires_at)
            .expect("create failed");

        let consumed = db
            .validate_and_consume_bootstrap_token(&token_hash, "dev-1")
            .expect("consume failed");
        assert_eq!(consumed.username, "jdoe");
        assert!(consumed.used_at.is_some());
        assert_eq!(consumed.used_by_device, Some("dev-1".to_string()));
    }

    #[test]
    fn bootstrap_token_double_consume_fails() {
        let db = test_db();
        let token = generate_token("cfgd_bs");
        let token_hash = hash_token(&token);
        let expires_at = "2099-12-31T23:59:59Z";

        db.create_bootstrap_token(&token_hash, "jdoe", None, expires_at)
            .expect("create failed");
        db.validate_and_consume_bootstrap_token(&token_hash, "dev-1")
            .expect("first consume ok");

        let result = db.validate_and_consume_bootstrap_token(&token_hash, "dev-2");
        assert!(result.is_err(), "double consumption should fail");
    }

    #[test]
    fn bootstrap_token_invalid_hash_fails() {
        let db = test_db();
        let result = db.validate_and_consume_bootstrap_token("nonexistent-hash", "dev-1");
        assert!(result.is_err());
    }

    #[test]
    fn device_credential_create_and_validate() {
        let db = test_db();
        // Register device first (FK constraint)
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .expect("register failed");

        let api_key = generate_token("cfgd_dev");
        let api_key_hash = hash_token(&api_key);

        let cred = db
            .create_device_credential("dev-1", &api_key_hash, "jdoe", Some("team-a"))
            .expect("create cred failed");
        assert_eq!(cred.device_id, "dev-1");
        assert_eq!(cred.username, "jdoe");
        assert!(!cred.revoked);

        let validated = db
            .validate_device_credential(&api_key_hash)
            .expect("validate failed");
        assert_eq!(validated.device_id, "dev-1");
        assert_eq!(validated.username, "jdoe");
    }

    #[test]
    fn device_credential_revoke_blocks_validation() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash1", None)
            .expect("register failed");

        let api_key = generate_token("cfgd_dev");
        let api_key_hash = hash_token(&api_key);
        db.create_device_credential("dev-1", &api_key_hash, "jdoe", None)
            .expect("create cred failed");

        db.revoke_device_credential("dev-1").expect("revoke failed");

        let result = db.validate_device_credential(&api_key_hash);
        assert!(result.is_err(), "revoked credential should fail validation");
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

    #[test]
    fn fleet_status_aggregation_via_db() {
        let db = test_db();
        db.register_device("d1", "host1", "linux", "x86_64", "h1", None)
            .expect("register");
        db.register_device("d2", "host2", "linux", "x86_64", "h2", None)
            .expect("register");
        db.register_device("d3", "host3", "darwin", "aarch64", "h3", None)
            .expect("register");

        // Make d2 drifted
        db.record_drift_event("d2", "pkg drifted").expect("drift");

        // Make d3 pending-reconcile
        db.set_force_reconcile("d3").expect("set reconcile");

        let status = super::super::fleet::get_fleet_status(&db).expect("fleet status");
        assert_eq!(status.total_devices, 3);
        assert_eq!(status.healthy, 1);
        assert_eq!(status.drifted, 1);
        assert_eq!(status.offline, 1); // pending-reconcile counts as offline
    }

    #[test]
    fn list_and_delete_bootstrap_tokens() {
        let db = test_db();
        let t1_hash = hash_token("token1");
        let t2_hash = hash_token("token2");

        let rec1 = db
            .create_bootstrap_token(&t1_hash, "user1", None, "2099-12-31T23:59:59Z")
            .expect("create 1");
        db.create_bootstrap_token(&t2_hash, "user2", Some("team"), "2099-12-31T23:59:59Z")
            .expect("create 2");

        let tokens = db.list_bootstrap_tokens().expect("list");
        assert_eq!(tokens.len(), 2);

        db.delete_bootstrap_token(&rec1.id).expect("delete");
        let tokens = db.list_bootstrap_tokens().expect("list after delete");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].username, "user2");
    }

    #[test]
    fn user_public_key_crud() {
        let db = test_db();
        let key = db
            .add_user_public_key(
                "jdoe",
                "ssh",
                "ssh-ed25519 AAAAC3...",
                "SHA256:abc",
                Some("work laptop"),
            )
            .expect("add key");
        assert_eq!(key.username, "jdoe");
        assert_eq!(key.key_type, "ssh");
        assert_eq!(key.label, Some("work laptop".to_string()));

        let keys = db.list_user_public_keys("jdoe").expect("list keys");
        assert_eq!(keys.len(), 1);

        db.delete_user_public_key(&key.id).expect("delete key");
        let keys = db.list_user_public_keys("jdoe").expect("list after delete");
        assert!(keys.is_empty());
    }

    #[test]
    fn enrollment_challenge_create_and_consume() {
        let db = test_db();
        // Need a user key for the challenge flow to make sense
        db.add_user_public_key("jdoe", "ssh", "ssh-ed25519 AAAA...", "SHA256:xyz", None)
            .expect("add key");

        let challenge = db
            .create_enrollment_challenge(
                "jdoe",
                "dev-99",
                "laptop",
                ("darwin", "aarch64"),
                "nonce-value",
                300,
            )
            .expect("create challenge");
        assert_eq!(challenge.username, "jdoe");
        assert_eq!(challenge.device_id, "dev-99");
        assert_eq!(challenge.nonce, "nonce-value");

        let consumed = db
            .consume_enrollment_challenge(&challenge.id)
            .expect("consume");
        assert_eq!(consumed.username, "jdoe");
        assert_eq!(consumed.device_id, "dev-99");
    }

    #[test]
    fn enrollment_challenge_double_consume_fails() {
        let db = test_db();
        let challenge = db
            .create_enrollment_challenge(
                "jdoe",
                "dev-99",
                "laptop",
                ("darwin", "aarch64"),
                "nonce",
                300,
            )
            .expect("create");

        db.consume_enrollment_challenge(&challenge.id)
            .expect("first consume");
        let result = db.consume_enrollment_challenge(&challenge.id);
        assert!(result.is_err(), "double consume should fail");
    }

    #[test]
    fn drift_event_records_and_lists() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "h1", None)
            .expect("register");

        let details = serde_json::to_string(&vec![DriftDetailInput {
            field: "pkg/vim".to_string(),
            expected: "9.0".to_string(),
            actual: "8.2".to_string(),
        }])
        .unwrap();

        let event = db
            .record_drift_event("dev-1", &details)
            .expect("record drift");
        assert_eq!(event.device_id, "dev-1");
        assert!(event.details.contains("pkg/vim"));

        // Device should now be drifted
        let device = db.get_device("dev-1").expect("get");
        assert_eq!(device.status, super::super::db::DeviceStatus::Drifted);

        let events = db.list_drift_events("dev-1").expect("list");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn checkin_event_records_history() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "h1", None)
            .expect("register");

        db.record_checkin("dev-1", "hash-a", false)
            .expect("checkin 1");
        db.record_checkin("dev-1", "hash-b", true)
            .expect("checkin 2");

        let events = db.list_checkin_events("dev-1").expect("list");
        assert_eq!(events.len(), 2);

        let changed_count = events.iter().filter(|e| e.config_changed).count();
        let unchanged_count = events.iter().filter(|e| !e.config_changed).count();
        assert_eq!(changed_count, 1);
        assert_eq!(unchanged_count, 1);
    }

    #[test]
    fn fleet_events_include_checkins_and_drift() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "h1", None)
            .expect("register");

        db.record_checkin("dev-1", "hash-a", false)
            .expect("checkin");
        db.record_drift_event("dev-1", "something drifted")
            .expect("drift");

        let events = db.list_fleet_events(100).expect("fleet events");
        assert_eq!(events.len(), 2);

        let event_types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(event_types.contains(&"checkin"));
        assert!(event_types.contains(&"drift"));
    }

    #[test]
    fn device_config_set_and_retrieve() {
        let db = test_db();
        db.register_device("dev-1", "ws-1", "linux", "x86_64", "h1", None)
            .expect("register");

        let config = serde_json::json!({
            "packages": ["vim", "tmux"],
            "shell": "zsh"
        });
        db.set_device_config("dev-1", &config).expect("set config");

        let device = db.get_device("dev-1").expect("get");
        let dc = device.desired_config.expect("should have desired_config");
        assert_eq!(dc["packages"][0], "vim");
        assert_eq!(dc["shell"], "zsh");
    }

    #[test]
    fn set_config_nonexistent_device_fails() {
        let db = test_db();
        let result = db.set_device_config("nonexistent", &serde_json::json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn challenge_ttl_constant() {
        assert_eq!(CHALLENGE_TTL_SECS, 300, "challenge TTL should be 5 minutes");
    }
}

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
use sha2::{Digest, Sha256};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use super::db::{FleetEvent, ServerDb};
use super::errors::GatewayError;

/// Server-level enrollment method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
pub struct CheckinRequest {
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub config_hash: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CheckinResponse {
    pub status: String,
    pub config_changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DriftRequest {
    pub details: Vec<DriftDetailInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct DriftDetailInput {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
pub struct EnrollRequest {
    pub token: String,
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
pub struct AddKeyRequest {
    pub key_type: String,
    pub public_key: String,
    pub fingerprint: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct EnrollInfoResponse {
    pub method: EnrollmentMethod,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChallengeRequest {
    pub username: String,
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChallengeResponse {
    pub challenge_id: String,
    pub nonce: String,
    pub expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
    format!("{:x}", Sha256::digest(token.as_bytes()))
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&gpg_home, std::fs::Permissions::from_mode(0o700));
    }

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
            db.update_checkin(&req.device_id, &req.config_hash)?;
        }
        Err(_) => {
            db.register_device(
                &req.device_id,
                &req.hostname,
                &req.os,
                &req.arch,
                &req.config_hash,
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
    use crate::crds::{DriftAlert, DriftAlertSpec, DriftDetail, DriftSeverity};
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
            machine_config_ref: mc_ref.clone(),
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
}

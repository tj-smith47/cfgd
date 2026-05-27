use std::collections::HashMap;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};
use futures::stream::Stream;
use parking_lot::Mutex as PlMutex;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use super::db::{FleetEvent, ServerDb};
use super::errors::GatewayError;
use crate::metrics::Metrics;

/// In-memory store of active web-UI session IDs (hashed).
///
/// Session IDs are random 256-bit tokens set as the `cfgd_session` cookie
/// after successful `?token=` auth. We store only the SHA-256 hash + expiry,
/// so a disk snapshot or memory-scrape of this struct doesn't yield usable
/// cookies, and we never write the raw admin key (`CFGD_API_KEY`) into a
/// client cookie.
#[derive(Clone, Default)]
pub struct WebSessions {
    inner: Arc<PlMutex<HashMap<String, Instant>>>,
}

impl WebSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh session ID (raw, not hashed) valid for `ttl`.
    /// Also prunes expired entries so the map cannot grow unboundedly.
    pub fn insert(&self, session_id: &str, ttl: Duration) {
        let expires = Instant::now() + ttl;
        let hash = cfgd_core::sha256_hex(session_id.as_bytes());
        let mut map = self.inner.lock();
        let now = Instant::now();
        map.retain(|_, exp| *exp > now);
        map.insert(hash, expires);
    }

    /// Returns `true` if `session_id` is a known, non-expired session.
    pub fn validate(&self, session_id: &str) -> bool {
        let hash = cfgd_core::sha256_hex(session_id.as_bytes());
        let map = self.inner.lock();
        map.get(&hash)
            .is_some_and(|expires| *expires > Instant::now())
    }
}

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
    pub db: ServerDb,
    pub kube_client: Option<kube::Client>,
    pub event_tx: tokio::sync::broadcast::Sender<FleetEvent>,
    pub enrollment_method: EnrollmentMethod,
    pub metrics: Option<Metrics>,
    pub web_sessions: WebSessions,
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

    // Enrollment WRITE endpoints — no pre-auth, validated in handlers. These
    // do real work per request (gpg/ssh-keygen subprocess + tempdir + fs::write),
    // so an in-process IP-keyed token bucket caps unauthenticated bursts.
    // Defaults: 5 attempts up-front, 5/min refill — see `RateLimiter::per_minute`.
    //
    // `/api/v1/enroll/info` is INTENTIONALLY OUT of the rate-limited group:
    // it's a read-only discovery endpoint (returns the configured enrollment
    // method) and does no per-request work, so rate-limiting it serves no
    // attack-surface purpose and instead breaks legitimate clients (and the
    // E2E suite) that probe `/enroll/info` after a few enrollment attempts.
    let enroll_limiter = super::rate_limit::RateLimiter::per_minute(
        ENROLL_RATE_LIMIT_BURST,
        ENROLL_RATE_LIMIT_PER_MIN,
    );
    let enrollment_write_routes = Router::new()
        .route("/api/v1/enroll", post(enroll))
        .route("/api/v1/enroll/challenge", post(request_challenge))
        .route("/api/v1/enroll/verify", post(verify_enrollment))
        .route_layer(middleware::from_fn_with_state(
            enroll_limiter,
            super::rate_limit::rate_limit_middleware,
        ));
    let enrollment_info_route = Router::new().route("/api/v1/enroll/info", get(enroll_info));

    authenticated_routes
        .merge(admin_routes)
        .merge(enrollment_write_routes)
        .merge(enrollment_info_route)
}

/// Per-IP rate-limit budget for unauthenticated enrollment WRITE endpoints
/// (`/api/v1/enroll`, `/api/v1/enroll/challenge`, `/api/v1/enroll/verify`).
/// Tuned for legitimate operator flow (a handful of attempts during
/// enrollment) while making brute-force/oracle probes infeasible.
///
/// `/api/v1/enroll/info` is read-only and is NOT subject to this limit.
pub(crate) const ENROLL_RATE_LIMIT_BURST: u32 = 5;
pub(crate) const ENROLL_RATE_LIMIT_PER_MIN: u32 = 5;
// Length bounds for device-supplied identifiers. These are enforced on
// every enrollment / checkin entry point — they defend against log
// injection (unbounded strings in structured logs), URL traversal (when a
// device_id lands in a path), and DB-pressure DoS.
const DEVICE_ID_MAX_LEN: usize = 128;
const HOSTNAME_MAX_LEN: usize = 253;
const USERNAME_MAX_LEN: usize = 64;

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

/// Validate an admin-supplied username.
///
/// The username is embedded verbatim in the SSH `allowed_signers` file (one signer
/// per line) and passed as `-I` to `ssh-keygen -Y verify`, and appears in GPG log
/// lines. Rejecting control characters (newline / CR / NUL / tab) is load-bearing:
/// a newline would inject an additional signer line in `allowed_signers`.
fn validate_username(s: &str) -> Result<(), GatewayError> {
    if s.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "username must not be empty".to_string(),
        ));
    }
    if s.len() > USERNAME_MAX_LEN {
        return Err(GatewayError::InvalidRequest(format!(
            "username exceeds {USERNAME_MAX_LEN} chars"
        )));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(GatewayError::InvalidRequest(
            "username may only contain [A-Za-z0-9._-]".to_string(),
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
        // CFGD_API_KEY not set — reject admin operations (require explicit key).
        // Log-once at debug so device-mode deployments don't spam logs at
        // RUST_LOG=debug; operators who want a single signal still get it on
        // the first request after startup.
        static ADMIN_KEY_MISSING_LOGGED: Once = Once::new();
        ADMIN_KEY_MISSING_LOGGED.call_once(|| {
            tracing::debug!("CFGD_API_KEY not set — admin API access is disabled");
        });
    }

    // Check per-device API key — scope the lock tightly so it's released before
    // downstream handlers run (otherwise every device request serializes through one lock)
    if let Some(ref token) = bearer_token {
        let token_hash = hash_token(token);
        let cred = state.db.validate_device_credential(&token_hash).await.ok();
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
/// Reset all gateway data (admin-only). Wipes devices, events, tokens, credentials.
async fn admin_reset(State(state): State<SharedState>) -> Result<impl IntoResponse, GatewayError> {
    let deleted = state.db.reset_data().await?;
    Ok(Json(serde_json::json!({
        "status": "ok",
        "rowsDeleted": deleted
    })))
}

// --- Submodule declarations + glob re-exports ---
//
// Handlers and helpers live in focused submodules; we glob-import them so
// `router()` references unqualified handler names exactly as they appeared
// before the carve. This also flattens them into `super::*` for `tests.rs`.
mod device;
mod drift;
mod enroll;
mod fleet;
mod tokens;
mod user_keys;

use device::*;
use drift::*;
use enroll::*;
use fleet::*;
use tokens::*;
use user_keys::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_drift;
#[cfg(test)]
mod tests_router;

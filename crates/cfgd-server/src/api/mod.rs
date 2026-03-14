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

use crate::db::{FleetEvent, ServerDb};
use crate::errors::ServerError;

/// Shared application state: database + optional Kubernetes client + event broadcast.
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<tokio::sync::Mutex<ServerDb>>,
    pub kube_client: Option<kube::Client>,
    pub event_tx: tokio::sync::broadcast::Sender<FleetEvent>,
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
        .route_layer(middleware::from_fn(admin_auth_middleware));

    // Enrollment endpoint — no pre-auth, validates bootstrap token in handler
    let enrollment_routes = Router::new().route("/api/v1/enroll", post(enroll));

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
) -> Result<axum::response::Response, ServerError> {
    let bearer_token = extract_bearer_token(&headers);

    // Check admin key first
    if let Ok(expected_key) = std::env::var("CFGD_API_KEY") {
        if let Some(ref token) = bearer_token
            && token == &expected_key
        {
            request.extensions_mut().insert(AuthContext::Admin);
            return Ok(next.run(request).await);
        }
    } else {
        // CFGD_API_KEY not set — allow all requests as admin (backwards compatible)
        request.extensions_mut().insert(AuthContext::Admin);
        return Ok(next.run(request).await);
    }

    // Check per-device API key
    if let Some(ref token) = bearer_token {
        let token_hash = hash_token(token);
        let db = state.db.lock().await;
        if let Ok(cred) = db.validate_device_credential(&token_hash) {
            request.extensions_mut().insert(AuthContext::Device {
                device_id: cred.device_id,
                username: cred.username,
            });
            return Ok(next.run(request).await);
        }
    }

    Err(ServerError::Unauthorized)
}

/// Admin-only auth middleware — requires CFGD_API_KEY.
async fn admin_auth_middleware(
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, ServerError> {
    if let Ok(expected_key) = std::env::var("CFGD_API_KEY") {
        match extract_bearer_token(&headers) {
            Some(token) if token == expected_key => {}
            _ => return Err(ServerError::Unauthorized),
        }
    } else {
        return Err(ServerError::InvalidRequest(
            "CFGD_API_KEY must be set to manage bootstrap tokens".to_string(),
        ));
    }
    Ok(next.run(request).await)
}

/// Enforce that a device-authenticated request can only access its own resources.
fn enforce_device_access(auth: &AuthContext, requested_device_id: &str) -> Result<(), ServerError> {
    match auth {
        AuthContext::Admin => Ok(()),
        AuthContext::Device { device_id, .. } => {
            if device_id == requested_device_id {
                Ok(())
            } else {
                Err(ServerError::Forbidden(format!(
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
) -> Result<impl IntoResponse, ServerError> {
    if req.device_id.is_empty() {
        return Err(ServerError::InvalidRequest(
            "device_id must not be empty".to_string(),
        ));
    }
    if req.token.is_empty() {
        return Err(ServerError::InvalidRequest(
            "token must not be empty".to_string(),
        ));
    }

    let db = state.db.lock().await;

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

    // Broadcast enrollment event
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

// --- Admin Token Management ---

async fn create_token(
    State(state): State<SharedState>,
    Json(req): Json<CreateTokenRequest>,
) -> Result<impl IntoResponse, ServerError> {
    if req.username.is_empty() {
        return Err(ServerError::InvalidRequest(
            "username must not be empty".to_string(),
        ));
    }
    if req.expires_in == 0 || req.expires_in > 30 * 86400 {
        return Err(ServerError::InvalidRequest(
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

async fn list_tokens(State(state): State<SharedState>) -> Result<impl IntoResponse, ServerError> {
    let db = state.db.lock().await;
    let tokens = db.list_bootstrap_tokens()?;
    Ok(Json(tokens))
}

async fn delete_token(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ServerError> {
    let db = state.db.lock().await;
    db.delete_bootstrap_token(&id)?;

    tracing::info!(token_id = %id, "bootstrap token deleted");

    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_credential(
    State(state): State<SharedState>,
    Path(device_id): Path<String>,
) -> Result<impl IntoResponse, ServerError> {
    let db = state.db.lock().await;
    db.revoke_device_credential(&device_id)?;

    tracing::info!(device_id = %device_id, "device credential revoked");

    Ok(StatusCode::NO_CONTENT)
}

// --- Device endpoints ---

async fn checkin(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(req): Json<CheckinRequest>,
) -> Result<impl IntoResponse, ServerError> {
    if req.device_id.is_empty() {
        return Err(ServerError::InvalidRequest(
            "device_id must not be empty".to_string(),
        ));
    }

    // Device auth: can only check in as self
    enforce_device_access(&auth, &req.device_id)?;

    let db = state.db.lock().await;

    let existing = db.get_device(&req.device_id);
    let config_changed = match &existing {
        Ok(device) => device.config_hash != req.config_hash,
        Err(ServerError::NotFound(_)) => false,
        Err(_) => false,
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

    let desired_config = if config_changed {
        db.get_device(&req.device_id)
            .ok()
            .and_then(|d| d.desired_config)
    } else {
        None
    };

    // Record checkin event for history
    let _ = db.record_checkin(&req.device_id, &req.config_hash, config_changed);

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
) -> Result<impl IntoResponse, ServerError> {
    // Device auth: can only list self
    if let AuthContext::Device { ref device_id, .. } = auth {
        let db = state.db.lock().await;
        let device = db.get_device(device_id)?;
        return Ok(Json(vec![device]));
    }
    let db = state.db.lock().await;
    let devices = db.list_devices_paginated(pagination.limit, pagination.offset)?;
    Ok(Json(devices))
}

async fn get_device(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ServerError> {
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
) -> Result<impl IntoResponse, ServerError> {
    // Only admin can push config to devices
    if !matches!(auth, AuthContext::Admin) {
        return Err(ServerError::Forbidden(
            "only admin can push config to devices".to_string(),
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
) -> Result<impl IntoResponse, ServerError> {
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
) -> Result<impl IntoResponse, ServerError> {
    enforce_device_access(&auth, &id)?;

    let details_str = serde_json::to_string(&req.details)
        .map_err(|e| ServerError::Internal(format!("failed to serialize drift details: {e}")))?;

    let event = {
        let db = state.db.lock().await;
        db.record_drift_event(&id, &details_str)?
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
        create_drift_alert_crd(client, &id, &req.details, &event.timestamp).await?;
    }

    Ok((StatusCode::CREATED, Json(event)))
}

async fn force_reconcile(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ServerError> {
    // Only admin can force reconcile
    if !matches!(auth, AuthContext::Admin) {
        return Err(ServerError::Forbidden(
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
) -> Result<impl IntoResponse, ServerError> {
    let db = state.db.lock().await;
    let events = db.list_fleet_events_paginated(pagination.limit, pagination.offset)?;
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
    details: &[DriftDetailInput],
    timestamp: &str,
) -> Result<(), ServerError> {
    use cfgd_operator::crds::{DriftAlert, DriftAlertSpec, DriftDetail, DriftSeverity};
    use kube::ResourceExt;
    use kube::api::{Api, PostParams};

    let alerts: Api<DriftAlert> = Api::default_namespaced(client.clone());

    let mc_ref = find_machine_config_for_device(client, device_id).await;

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

    let alert = DriftAlert::new(
        &alert_name,
        DriftAlertSpec {
            device_id: device_id.to_string(),
            machine_config_ref: mc_ref,
            drift_details,
            severity: DriftSeverity::Medium,
        },
    );

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

/// Find the MachineConfig CRD name that corresponds to a device.
async fn find_machine_config_for_device(client: &kube::Client, device_id: &str) -> String {
    use cfgd_operator::crds::MachineConfig;
    use kube::ResourceExt;
    use kube::api::{Api, ListParams};

    let machines: Api<MachineConfig> = Api::all(client.clone());
    match machines.list(&ListParams::default()).await {
        Ok(list) => {
            for mc in &list.items {
                if mc.spec.hostname == device_id {
                    return mc.name_any();
                }
            }
            if list.items.len() == 1 {
                return list.items[0].name_any();
            }
            format!("{}-mc", device_id)
        }
        Err(e) => {
            tracing::warn!("Failed to list MachineConfigs for device lookup: {}", e);
            format!("{}-mc", device_id)
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
}

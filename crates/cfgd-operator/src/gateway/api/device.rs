//! Device-facing endpoints: check-in, list, get, set-config.

use super::*;
pub(super) async fn checkin(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(req): Json<CheckinRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    validate_device_id(&req.device_id)?;
    validate_hostname(&req.hostname)?;

    // Device auth: can only check in as self
    enforce_device_access(&auth, &req.device_id)?;

    let db = state.db.clone();
    let device_id = req.device_id.clone();
    let hostname = req.hostname.clone();
    let os = req.os.clone();
    let arch = req.arch.clone();
    let config_hash = req.config_hash.clone();
    let compliance = req.compliance_summary.clone();

    let (config_changed, desired_config) = db
        .with_write_tx(move |tx| {
            let existing = crate::gateway::db::get_device_tx(tx, &device_id);
            let (config_changed, desired) = match &existing {
                Ok(device) => (
                    device.config_hash != config_hash,
                    if device.config_hash != config_hash {
                        device.desired_config.clone()
                    } else {
                        None
                    },
                ),
                Err(GatewayError::NotFound(_)) => (false, None),
                Err(_) => (false, None),
            };

            match &existing {
                Ok(_) => {
                    crate::gateway::db::update_checkin_tx(
                        tx,
                        &device_id,
                        &config_hash,
                        compliance.as_ref(),
                    )?;
                }
                Err(_) => {
                    crate::gateway::db::register_device_tx(
                        tx,
                        &device_id,
                        &hostname,
                        &os,
                        &arch,
                        &config_hash,
                        compliance.as_ref(),
                    )?;
                }
            }

            crate::gateway::db::record_checkin_tx(tx, &device_id, &config_hash, config_changed)?;
            Ok((config_changed, desired))
        })
        .await?;

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

pub(super) async fn list_devices(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(pagination): Query<PaginationParams>,
) -> Result<impl IntoResponse, GatewayError> {
    // Device auth: can only list self
    if let AuthContext::Device { ref device_id, .. } = auth {
        let device = state.db.get_device(device_id).await?;
        return Ok(Json(vec![device]));
    }
    let limit = pagination.limit.min(1000);
    let devices = state
        .db
        .list_devices_paginated(limit, pagination.offset)
        .await?;
    Ok(Json(devices))
}

pub(super) async fn get_device(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    enforce_device_access(&auth, &id)?;
    let device = state.db.get_device(&id).await?;
    Ok(Json(device))
}

pub(super) async fn set_device_config(
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
    // Authoritative config-size policy. The route's DefaultBodyLimit sits
    // above this (see MAX_REQUEST_BODY_BYTES) so an over-policy config reaches
    // this check and gets the specific 400 below, rather than a generic 413.
    let json_str = serde_json::to_string(&req.config)
        .map_err(|e| GatewayError::InvalidRequest(e.to_string()))?;
    if json_str.len() > super::MAX_CONFIG_BYTES {
        return Err(GatewayError::InvalidRequest(
            "config exceeds 10MB size limit".to_string(),
        ));
    }

    state.db.set_device_config(&id, &req.config).await?;

    tracing::info!(device_id = %id, "desired config updated");

    Ok(StatusCode::NO_CONTENT)
}

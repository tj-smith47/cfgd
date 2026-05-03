//! Fleet-level event endpoints: drift events per device, fleet-wide event log,
//! force-reconcile, and the SSE event stream.

use super::*;
pub(super) async fn list_drift_events(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    enforce_device_access(&auth, &id)?;
    let events = state.db.list_drift_events(&id).await?;
    Ok(Json(events))
}

pub(super) async fn record_drift_event(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<DriftRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    enforce_device_access(&auth, &id)?;

    let details_str = serde_json::to_string(&req.details)
        .map_err(|e| GatewayError::Internal(format!("failed to serialize drift details: {e}")))?;

    let db = state.db.clone();
    let id_c = id.clone();
    let details_c = details_str.clone();
    let (event, device_hostname) = db
        .with_write_tx(move |tx| {
            let device = crate::gateway::db::get_device_tx(tx, &id_c)?;
            let evt = crate::gateway::db::record_drift_event_tx(tx, &id_c, &details_c)?;
            Ok((evt, Some(device.hostname)))
        })
        .await?;

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

pub(super) async fn force_reconcile(
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
    state.db.set_force_reconcile(&id).await?;

    tracing::info!(device_id = %id, "force reconcile requested");

    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn list_fleet_events(
    State(state): State<SharedState>,
    Query(pagination): Query<PaginationParams>,
) -> Result<impl IntoResponse, GatewayError> {
    let limit = pagination.limit.min(1000);
    let events = state
        .db
        .list_fleet_events_paginated(limit, pagination.offset)
        .await?;
    Ok(Json(events))
}

/// SSE endpoint — streams fleet events in real-time.
pub(super) async fn event_stream(
    State(state): State<SharedState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(data) => Some(Ok(Event::default()
                .event(event.event_type.clone())
                .data(data))),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    device_id = %event.device_id,
                    event_type = %event.event_type,
                    "failed to serialize fleet event for SSE; dropping",
                );
                None
            }
        },
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
            tracing::warn!(skipped = n, "SSE subscriber lagged; dropped events");
            None
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

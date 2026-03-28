use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::errors::OperatorError;

#[derive(Clone)]
pub struct HealthState(Arc<AtomicBool>);

impl Default for HealthState {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl HealthState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_ready(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn healthz_handler(
    axum::extract::State(_state): axum::extract::State<HealthState>,
) -> (axum::http::StatusCode, &'static str) {
    (axum::http::StatusCode::OK, "ok")
}

async fn readyz_handler(
    axum::extract::State(state): axum::extract::State<HealthState>,
) -> (axum::http::StatusCode, &'static str) {
    if state.0.load(Ordering::SeqCst) {
        (axum::http::StatusCode::OK, "ready")
    } else {
        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

pub async fn run_probe_server(port: u16, state: HealthState) -> Result<(), OperatorError> {
    let app = axum::Router::new()
        .route("/healthz", axum::routing::get(healthz_handler))
        .route("/readyz", axum::routing::get(readyz_handler))
        .with_state(state);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| OperatorError::Health(format!("bind {addr}: {e}")))?;

    tracing::info!(%addr, "health probe server listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| OperatorError::Health(format!("serve: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let state = HealthState::new();
        let resp = healthz_handler(axum::extract::State(state)).await;
        assert_eq!(resp.0, axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn readyz_returns_503_when_not_ready() {
        let state = HealthState::new();
        let resp = readyz_handler(axum::extract::State(state)).await;
        assert_eq!(resp.0, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readyz_returns_200_when_ready() {
        let state = HealthState::new();
        state.set_ready();
        let resp = readyz_handler(axum::extract::State(state)).await;
        assert_eq!(resp.0, axum::http::StatusCode::OK);
    }
}

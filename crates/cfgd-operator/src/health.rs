use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use prometheus_client::metrics::gauge::Gauge;

use crate::errors::OperatorError;

/// The two signals the probe server reports.
///
/// Readiness says every listener the pod's Services route to is accepting:
/// the admission webhook when the pod has webhook certificates, and the device
/// gateway when it is enabled. Leadership says this pod runs the controllers.
/// A standby answers admission as well as the leader does, so the webhook
/// alone never waits on the lease; the gateway starts only once this pod runs
/// the controllers, so with leader election on, a standby's gateway (and with
/// it readiness) waits on the lease.
#[derive(Clone)]
pub struct HealthState(Arc<Signals>);

struct Signals {
    webhook: Listener,
    gateway: Listener,
    leader: AtomicBool,
    leader_gauge: Gauge,
}

struct Listener {
    expected: bool,
    accepting: AtomicBool,
}

impl Listener {
    fn new(expected: bool) -> Self {
        Self {
            expected,
            accepting: AtomicBool::new(false),
        }
    }

    fn is_up(&self) -> bool {
        !self.expected || self.accepting.load(Ordering::SeqCst)
    }
}

impl HealthState {
    /// `leader_gauge` mirrors leadership into the metrics registry;
    /// `webhook` and `gateway` name the listeners readiness waits for.
    pub fn new(leader_gauge: Gauge, webhook: bool, gateway: bool) -> Self {
        Self(Arc::new(Signals {
            webhook: Listener::new(webhook),
            gateway: Listener::new(gateway),
            leader: AtomicBool::new(false),
            leader_gauge,
        }))
    }

    /// The admission webhook has loaded its certificates and is accepting.
    pub fn set_webhook_serving(&self) {
        self.0.webhook.accepting.store(true, Ordering::SeqCst);
    }

    /// The device gateway's listener is bound and accepting.
    pub fn set_gateway_serving(&self) {
        self.0.gateway.accepting.store(true, Ordering::SeqCst);
    }

    /// This pod holds the leader lease (or runs without leader election).
    /// A lost lease ends the process, so leadership is never cleared.
    pub fn set_leader(&self) {
        self.0.leader.store(true, Ordering::SeqCst);
        self.0.leader_gauge.set(1);
    }

    pub fn is_ready(&self) -> bool {
        self.0.webhook.is_up() && self.0.gateway.is_up()
    }

    pub fn is_leader(&self) -> bool {
        self.0.leader.load(Ordering::SeqCst)
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
    if state.is_ready() {
        (axum::http::StatusCode::OK, "ready")
    } else {
        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

async fn leaderz_handler(
    axum::extract::State(state): axum::extract::State<HealthState>,
) -> (axum::http::StatusCode, &'static str) {
    if state.is_leader() {
        (axum::http::StatusCode::OK, "leader")
    } else {
        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "standby")
    }
}

fn probe_router(state: HealthState) -> axum::Router {
    axum::Router::new()
        .route("/healthz", axum::routing::get(healthz_handler))
        .route("/readyz", axum::routing::get(readyz_handler))
        .route("/leaderz", axum::routing::get(leaderz_handler))
        .with_state(state)
}

pub async fn run_probe_server(port: u16, state: HealthState) -> Result<(), OperatorError> {
    // GET-only probes; 8 KiB is a generous safety net against abusive clients.
    const HEALTH_MAX_BODY_BYTES: usize = 8 * 1024;

    let app =
        probe_router(state).layer(axum::extract::DefaultBodyLimit::max(HEALTH_MAX_BODY_BYTES));

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
    use axum::http::StatusCode;

    use tower::ServiceExt;

    async fn get(state: &HealthState, path: &str) -> StatusCode {
        let request = axum::http::Request::get(path)
            .body(axum::body::Body::empty())
            .expect("probe request must build");
        probe_router(state.clone())
            .oneshot(request)
            .await
            .expect("probe router is infallible")
            .status()
    }

    /// `(GET /readyz, GET /leaderz)` through the router the probe server serves.
    async fn probes(state: &HealthState) -> (StatusCode, StatusCode) {
        (get(state, "/readyz").await, get(state, "/leaderz").await)
    }

    fn state(webhook: bool, gateway: bool) -> (HealthState, Gauge) {
        let gauge = Gauge::default();
        (HealthState::new(gauge.clone(), webhook, gateway), gauge)
    }

    const DOWN: StatusCode = StatusCode::SERVICE_UNAVAILABLE;

    #[tokio::test]
    async fn healthz_returns_ok() {
        assert_eq!(get(&state(true, true).0, "/healthz").await, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_starting_pod_is_neither_ready_nor_leader() {
        let (state, _) = state(true, false);
        assert_eq!(probes(&state).await, (DOWN, DOWN));
    }

    /// A standby serving admission is an endpoint of the webhook Service, so a
    /// roll never leaves the webhook with no backend while the old pod still
    /// holds the lease.
    #[tokio::test]
    async fn a_serving_standby_is_ready_and_reports_standby() {
        let (state, gauge) = state(true, false);
        state.set_webhook_serving();
        assert_eq!(probes(&state).await, (StatusCode::OK, DOWN));
        assert_eq!(gauge.get(), 0);
    }

    #[tokio::test]
    async fn a_serving_leader_is_ready_and_reports_leader() {
        let (state, gauge) = state(true, false);
        state.set_webhook_serving();
        state.set_leader();
        assert_eq!(probes(&state).await, (StatusCode::OK, StatusCode::OK));
        assert_eq!(gauge.get(), 1, "leadership must reach the metrics gauge");
    }

    /// The leader lease alone does not make a pod ready: until the webhook
    /// serves, admission sent to it would fail.
    #[tokio::test]
    async fn a_leader_whose_webhook_is_not_serving_is_not_ready() {
        let (state, _) = state(true, false);
        state.set_leader();
        assert_eq!(probes(&state).await, (DOWN, StatusCode::OK));
    }

    /// A pod with the gateway enabled is ready only once the gateway accepts,
    /// whatever the webhook and the lease say: until then a gateway Service
    /// would send requests to a port nothing listens on.
    #[tokio::test]
    async fn readiness_waits_for_the_gateway_listener() {
        let (state, _) = state(true, true);
        state.set_webhook_serving();
        state.set_leader();
        assert_eq!(probes(&state).await, (DOWN, StatusCode::OK));
        state.set_gateway_serving();
        assert_eq!(probes(&state).await, (StatusCode::OK, StatusCode::OK));
    }

    /// A gateway pod with no webhook certificates turns ready on the gateway
    /// alone.
    #[tokio::test]
    async fn a_gateway_without_a_webhook_is_ready_once_the_gateway_accepts() {
        let (state, _) = state(false, true);
        assert_eq!(probes(&state).await, (DOWN, DOWN));
        state.set_gateway_serving();
        assert_eq!(probes(&state).await, (StatusCode::OK, DOWN));
    }

    /// A pod that serves neither listener carries only the controllers, which
    /// no Service routes to, so it is ready from the start.
    #[tokio::test]
    async fn a_pod_with_no_listener_is_ready_at_once() {
        let (state, _) = state(false, false);
        assert_eq!(probes(&state).await, (StatusCode::OK, DOWN));
    }
}

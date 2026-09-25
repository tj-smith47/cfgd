use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use prometheus_client::metrics::gauge::Gauge;

use crate::errors::OperatorError;

/// The two signals the probe server reports.
///
/// Readiness says the pod can take the traffic its Services send it: the
/// admission webhook is serving, and on a pod whose device gateway runs only
/// under the leader lease, the lease is held. Leadership says this pod runs the
/// controllers. A standby answers admission as well as the leader does, so
/// readiness waits on the lease only where a Service would otherwise route
/// gateway traffic to a pod with no gateway listening.
#[derive(Clone, Default)]
pub struct HealthState(Arc<Signals>);

#[derive(Default)]
struct Signals {
    serving: AtomicBool,
    leader: AtomicBool,
    ready_needs_leader: bool,
    leader_gauge: Gauge,
}

impl HealthState {
    /// `leader_gauge` mirrors leadership into the metrics registry;
    /// `ready_needs_leader` makes readiness wait on the lease as well.
    pub fn new(leader_gauge: Gauge, ready_needs_leader: bool) -> Self {
        Self(Arc::new(Signals {
            ready_needs_leader,
            leader_gauge,
            ..Signals::default()
        }))
    }

    /// Everything this pod serves to its Services is up.
    pub fn set_serving(&self) {
        self.0.serving.store(true, Ordering::SeqCst);
    }

    /// This pod holds the leader lease (or runs without leader election).
    /// A lost lease ends the process, so leadership is never cleared.
    pub fn set_leader(&self) {
        self.0.leader.store(true, Ordering::SeqCst);
        self.0.leader_gauge.set(1);
    }

    pub fn is_ready(&self) -> bool {
        self.0.serving.load(Ordering::SeqCst) && (!self.0.ready_needs_leader || self.is_leader())
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

    #[tokio::test]
    async fn healthz_returns_ok() {
        assert_eq!(
            get(&HealthState::default(), "/healthz").await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_starting_pod_is_neither_ready_nor_leader() {
        let state = HealthState::default();
        assert_eq!(
            probes(&state).await,
            (
                StatusCode::SERVICE_UNAVAILABLE,
                StatusCode::SERVICE_UNAVAILABLE
            )
        );
    }

    /// A standby serving admission is an endpoint of the webhook Service, so a
    /// roll never leaves the webhook with no backend while the old pod still
    /// holds the lease.
    #[tokio::test]
    async fn a_serving_standby_is_ready_and_reports_standby() {
        let state = HealthState::default();
        state.set_serving();
        assert_eq!(
            probes(&state).await,
            (StatusCode::OK, StatusCode::SERVICE_UNAVAILABLE)
        );
    }

    #[tokio::test]
    async fn a_serving_leader_is_ready_and_reports_leader() {
        let gauge = Gauge::default();
        let state = HealthState::new(gauge.clone(), false);
        state.set_serving();
        state.set_leader();
        assert_eq!(probes(&state).await, (StatusCode::OK, StatusCode::OK));
        assert_eq!(gauge.get(), 1, "leadership must reach the metrics gauge");
    }

    /// The leader lease alone does not make a pod ready: until the webhook
    /// serves, admission sent to it would fail.
    #[tokio::test]
    async fn a_leader_whose_webhook_is_not_serving_is_not_ready() {
        let state = HealthState::default();
        state.set_leader();
        assert_eq!(
            probes(&state).await,
            (StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK)
        );
    }

    /// Where the device gateway runs only under the lease, a standby has no
    /// gateway listening, so readiness waits on leadership.
    #[tokio::test]
    async fn readiness_waits_on_the_lease_where_the_gateway_needs_it() {
        let gauge = Gauge::default();
        let state = HealthState::new(gauge.clone(), true);
        state.set_serving();
        assert_eq!(
            probes(&state).await,
            (
                StatusCode::SERVICE_UNAVAILABLE,
                StatusCode::SERVICE_UNAVAILABLE
            )
        );
        assert_eq!(gauge.get(), 0);
        state.set_leader();
        assert_eq!(probes(&state).await, (StatusCode::OK, StatusCode::OK));
    }
}

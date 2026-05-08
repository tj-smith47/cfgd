//! Router-builder fixture for the webhook tests.
//!
//! Tests drive the same `Router` the production `run_webhook_server`
//! constructs, including its `DefaultBodyLimit` middleware. The bottom
//! kube `Client` is a stub that returns 200 `{}` to every call — fine for
//! tests of the validate/mutate handlers that don't touch the apiserver.
//!
//! Tests of `enforce_module_policy` (which calls `Api::list`) construct a
//! richer `MockKubeHarness` separately.
#![cfg(test)]

use axum::Router;
use hyper::body::Bytes;
use kube::Client;
use prometheus_client::registry::Registry;
use tower::service_fn;

use super::{WebhookState, build_webhook_router};
use crate::metrics::Metrics;

fn stub_kube_client() -> Client {
    let svc = service_fn(|_req: hyper::Request<kube::client::Body>| async {
        Ok::<_, std::convert::Infallible>(
            hyper::Response::builder()
                .status(200)
                .body(kube::client::Body::from(Bytes::from_static(b"{}")))
                .expect("test response must build"),
        )
    });
    Client::new(svc, "default")
}

/// Build the webhook Router for tests, returning the live `Metrics` so
/// post-request assertions on counters/histograms work.
pub(super) fn test_webhook_router() -> (Router, Metrics) {
    test_webhook_router_with_client(stub_kube_client())
}

/// Build the webhook Router using a caller-provided `kube::Client` —
/// for tests that drive `enforce_module_policy` / `collect_policy_modules`
/// through the shared `MockKubeHarness`.
pub(super) fn test_webhook_router_with_client(client: Client) -> (Router, Metrics) {
    let mut registry = Registry::default();
    let metrics = Metrics::new(&mut registry);
    let state = WebhookState {
        metrics: metrics.clone(),
        client,
    };
    (build_webhook_router(state), metrics)
}

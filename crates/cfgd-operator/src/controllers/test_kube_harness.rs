//! Mock kube-rs Client harness for reconcile-fn tests.
//!
//! A test enqueues a sequence of expected request/response pairs, the
//! harness installs a `tower_test::mock` Service inside a `kube::Client`,
//! and a driver task replays the queue in order. The reconcile fn under
//! test is awaited normally; when it returns, the test calls
//! [`MockKubeHarness::finish`] which joins the driver and surfaces every
//! captured request body for assertions.
//!
//! Pattern source: kube-rs upstream `kube-client/src/client/mod.rs::tests::test_mock`.
//! Each call site here mirrors that pattern; the wrapper makes multi-call
//! reconciles ergonomic to write.
#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use kube::Client;
use kube::client::Body;
use prometheus_client::registry::Registry;
use serde::Serialize;
use tokio::task::JoinHandle;
use tower_test::mock;

use crate::controllers::ControllerContext;
use crate::metrics::Metrics;

/// One expected HTTP call in the reconcile's request sequence.
///
/// Constructed via [`ExpectedCall::get`], [`ExpectedCall::list`],
/// [`ExpectedCall::patch`], [`ExpectedCall::patch_status`],
/// [`ExpectedCall::delete`], or [`ExpectedCall::post`], then customized
/// with `.returning_*` / `.with_status` / `.expecting_query`.
pub(super) struct ExpectedCall {
    method: Method,
    /// Exact match on the URI path component (no query string, no scheme).
    path: String,
    /// Optional substring that must appear in the URI's query string.
    /// Useful for `fieldManager=cfgd-operator/status` assertions without
    /// pinning the full encoded query.
    query_contains: Option<String>,
    response_status: StatusCode,
    response_body: Vec<u8>,
}

impl ExpectedCall {
    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    pub fn list(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    pub fn patch(path: impl Into<String>) -> Self {
        Self::new(Method::PATCH, path)
    }

    pub fn patch_status(path: impl Into<String>) -> Self {
        // Same HTTP method, but pin the /status subpath in the path itself.
        let p: String = path.into();
        debug_assert!(
            p.ends_with("/status"),
            "patch_status path must end with /status, got: {p}"
        );
        Self::new(Method::PATCH, p)
    }

    pub fn delete(path: impl Into<String>) -> Self {
        Self::new(Method::DELETE, path)
    }

    pub fn post(path: impl Into<String>) -> Self {
        Self::new(Method::POST, path)
    }

    fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query_contains: None,
            response_status: StatusCode::OK,
            response_body: b"{}".to_vec(),
        }
    }

    /// Pin a substring that must appear in the request's query string.
    pub fn with_query_contains(mut self, fragment: impl Into<String>) -> Self {
        self.query_contains = Some(fragment.into());
        self
    }

    /// Reply with a JSON-serialized object as the response body.
    pub fn returning_json<T: Serialize>(mut self, value: &T) -> Self {
        self.response_body =
            serde_json::to_vec(value).expect("test fixture must serialize cleanly");
        self
    }

    /// Reply with raw bytes (for non-JSON responses or pre-serialized payloads).
    pub fn returning_raw(mut self, bytes: Vec<u8>) -> Self {
        self.response_body = bytes;
        self
    }

    /// Override the response status (default `200 OK`).
    pub fn with_status(mut self, code: u16) -> Self {
        self.response_status =
            StatusCode::from_u16(code).expect("test fixture must use valid status");
        self
    }

    /// Reply with a Kubernetes 404 status response. The kube Client converts
    /// this into `kube::Error::Api(ErrorResponse{ code: 404, .. })` which the
    /// reconciler matches on.
    pub fn returning_404(mut self, name: &str) -> Self {
        let status = serde_json::json!({
            "kind": "Status",
            "apiVersion": "v1",
            "metadata": {},
            "status": "Failure",
            "message": format!("{} not found", name),
            "reason": "NotFound",
            "details": { "name": name },
            "code": 404,
        });
        self.response_status = StatusCode::NOT_FOUND;
        self.response_body = serde_json::to_vec(&status).expect("status must serialize");
        self
    }

    /// Reply with a Kubernetes 5xx status response.
    pub fn returning_server_error(mut self, code: u16, message: &str) -> Self {
        let status = serde_json::json!({
            "kind": "Status",
            "apiVersion": "v1",
            "metadata": {},
            "status": "Failure",
            "message": message,
            "code": code,
        });
        self.response_status =
            StatusCode::from_u16(code).expect("test fixture must use valid status");
        self.response_body = serde_json::to_vec(&status).expect("status must serialize");
        self
    }
}

/// One captured request from the driver loop, exposed to the test for
/// post-reconcile assertions.
pub(super) struct CapturedRequest {
    pub method: Method,
    pub path: String,
    pub body: Vec<u8>,
}

impl CapturedRequest {
    /// Parse the captured body as JSON. Panics if the body is not valid JSON.
    pub fn body_json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "captured {} {} body is not valid JSON: {} — body was: {}",
                self.method,
                self.path,
                e,
                String::from_utf8_lossy(&self.body)
            )
        })
    }
}

/// Report returned from [`MockKubeHarness::finish`].
pub(super) struct HarnessReport {
    pub captured: Vec<CapturedRequest>,
}

impl HarnessReport {
    /// Find the first captured request whose `path` ends with `path_suffix`
    /// AND matches the given `method`. Useful for asserting that a specific
    /// call (e.g., the events POST) happened, without pinning its absolute
    /// position in the sequence.
    pub fn find(&self, method: Method, path_suffix: &str) -> Option<&CapturedRequest> {
        self.captured
            .iter()
            .find(|r| r.method == method && r.path.ends_with(path_suffix))
    }
}

/// Build a Kubernetes `events.k8s.io/v1` POST expectation for `namespace`,
/// returning a minimal accepted Event. Use this for every reconcile branch
/// that calls `recorder.publish(...)`.
pub(super) fn expect_event_post(namespace: &str) -> ExpectedCall {
    ExpectedCall::post(format!(
        "/apis/events.k8s.io/v1/namespaces/{namespace}/events"
    ))
    .with_status(201)
    .returning_json(&serde_json::json!({
        "apiVersion": "events.k8s.io/v1",
        "kind": "Event",
        "metadata": { "name": "test-event", "namespace": namespace },
    }))
}

/// The test harness. Holds the driver `JoinHandle` and the live `Client` /
/// `ControllerContext` references the test passes into the reconcile fn.
pub(super) struct MockKubeHarness {
    driver: JoinHandle<HarnessReport>,
}

impl MockKubeHarness {
    /// Construct the harness, spawn the driver task, and return a fully-wired
    /// `Arc<ControllerContext>` ready to pass into a `reconcile_*` fn.
    ///
    /// The `Registry` is returned alongside so the test can inspect emitted
    /// metrics after the reconcile completes.
    pub fn new(expected: Vec<ExpectedCall>) -> (Arc<ControllerContext>, Registry, Self) {
        let (mock_service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();

        let driver = tokio::spawn(async move {
            let mut captured = Vec::with_capacity(expected.len());

            for expected_call in expected.into_iter() {
                let (request, send) = match handle.next_request().await {
                    Some(r) => r,
                    None => panic!(
                        "expected {} {} but the kube Client was dropped before the request landed",
                        expected_call.method, expected_call.path,
                    ),
                };

                let actual_method = request.method().clone();
                let actual_path = request.uri().path().to_string();
                let actual_query = request.uri().query().unwrap_or("").to_string();

                assert_eq!(
                    actual_method,
                    expected_call.method,
                    "method mismatch on call #{}: expected {} but got {} for path {}",
                    captured.len() + 1,
                    expected_call.method,
                    actual_method,
                    actual_path,
                );
                assert_eq!(
                    actual_path,
                    expected_call.path,
                    "path mismatch on call #{}: expected {} but got {} (method {})",
                    captured.len() + 1,
                    expected_call.path,
                    actual_path,
                    actual_method,
                );
                if let Some(fragment) = &expected_call.query_contains {
                    assert!(
                        actual_query.contains(fragment),
                        "query missing fragment on call #{}: expected `{}` to appear in `{}` (path {})",
                        captured.len() + 1,
                        fragment,
                        actual_query,
                        actual_path,
                    );
                }

                let body_bytes = match request.into_body().collect().await {
                    Ok(collected) => collected.to_bytes().to_vec(),
                    Err(e) => panic!(
                        "failed to read body for {} {}: {}",
                        actual_method, actual_path, e
                    ),
                };

                let _ = actual_query;
                captured.push(CapturedRequest {
                    method: actual_method,
                    path: actual_path,
                    body: body_bytes,
                });

                let response = Response::builder()
                    .status(expected_call.response_status)
                    .header("content-type", "application/json")
                    .body(Body::from(expected_call.response_body))
                    .expect("test response must build");
                send.send_response(response);
            }

            HarnessReport { captured }
        });

        let client = Client::new(mock_service, "default");
        let reporter = kube::runtime::events::Reporter {
            controller: "cfgd-operator-test".into(),
            instance: None,
        };
        let recorder = kube::runtime::events::Recorder::new(client.clone(), reporter);
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);
        let ctx = Arc::new(ControllerContext {
            client,
            recorder,
            metrics,
        });

        (ctx, registry, Self { driver })
    }

    /// Await the driver, asserting all expected calls were consumed in order.
    /// Panics if the driver hasn't finished within `5s` (likely the reconcile
    /// made fewer kube calls than expected).
    pub async fn finish(self) -> HarnessReport {
        match tokio::time::timeout(Duration::from_secs(5), self.driver).await {
            Ok(Ok(report)) => report,
            Ok(Err(join_err)) => panic!("harness driver task panicked: {join_err}"),
            Err(_) => panic!(
                "harness driver did not complete within 5s — \
                 the reconcile likely made fewer kube calls than expected, \
                 or made a call out of order"
            ),
        }
    }
}

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

use futures::FutureExt;
use http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use kube::Client;
use kube::client::Body;
use kube::runtime::reflector::{self, Lookup, Store};
use kube::runtime::watcher;
use prometheus_client::registry::Registry;
use serde::Serialize;
use tokio::task::JoinHandle;
use tower_test::mock;

use crate::controllers::{ControllerContext, ControllerStores};
use crate::metrics::Metrics;

/// Build a populated, ready [`Store`] from a fixed set of objects.
///
/// Mirrors what a live reflector does on its initial list: `Init`, one
/// `InitApply` per object, then `InitDone` — which is also what flips the
/// store to ready. The `Writer` is dropped on return; a store that has already
/// been marked ready stays ready and keeps its contents.
pub(crate) fn seeded_store<K>(objects: Vec<K>) -> Store<K>
where
    K: Lookup + Clone + 'static,
    K::DynamicType: Eq + std::hash::Hash + Clone + Default,
{
    let (store, mut writer) = reflector::store::<K>();
    writer.apply_watcher_event(&watcher::Event::Init);
    for object in objects {
        writer.apply_watcher_event(&watcher::Event::InitApply(object));
    }
    writer.apply_watcher_event(&watcher::Event::InitDone);
    store
}

/// A [`Store`] that has never completed an initial list — the shape a reconcile
/// sees while the operator is still starting up.
///
/// The `Writer` comes back with it and the test must hold it for the length of
/// the assertion: dropping it resolves `wait_until_ready` with `WriterDropped`,
/// which is a different failure from the one under test (a cache that is simply
/// not populated yet).
pub(crate) fn unready_store<K>() -> (Store<K>, reflector::store::Writer<K>)
where
    K: Lookup + Clone + 'static,
    K::DynamicType: Eq + std::hash::Hash + Clone + Default,
{
    reflector::store::<K>()
}

/// Every cache empty and ready — the default for a reconcile whose branch
/// reads no cross-resource state.
pub(crate) fn empty_stores() -> ControllerStores {
    ControllerStores {
        machine_configs: seeded_store(vec![]),
        config_policies: seeded_store(vec![]),
        cluster_config_policies: seeded_store(vec![]),
        modules: seeded_store(vec![]),
        drift_alerts: seeded_store(vec![]),
        namespaces: seeded_store(vec![]),
    }
}

/// One expected HTTP call in the reconcile's request sequence.
///
/// Constructed via [`ExpectedCall::get`], [`ExpectedCall::list`],
/// [`ExpectedCall::patch`], [`ExpectedCall::patch_status`],
/// [`ExpectedCall::delete`], or [`ExpectedCall::post`], then customized
/// with `.returning_*` / `.with_status` / `.expecting_query`.
pub(crate) struct ExpectedCall {
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
pub(crate) struct CapturedRequest {
    pub method: Method,
    pub path: String,
    /// The raw query string, so a test can assert a parameter is ABSENT.
    /// `ExpectedCall::with_query_contains` only proves presence, which cannot
    /// state a claim like "this apply must not be forced".
    pub query: String,
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
pub(crate) struct HarnessReport {
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
pub(crate) fn expect_event_post(namespace: &str) -> ExpectedCall {
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

/// What the driver task hands back: the expected calls it matched, plus any
/// request that arrived after the queue was exhausted.
struct DriverOutcome {
    captured: Vec<CapturedRequest>,
    unexpected: Vec<String>,
}

/// The test harness. Holds the driver `JoinHandle` and the live `Client` /
/// `ControllerContext` references the test passes into the reconcile fn.
pub(crate) struct MockKubeHarness {
    driver: JoinHandle<DriverOutcome>,
    stop: tokio::sync::oneshot::Sender<()>,
}

impl MockKubeHarness {
    /// Construct the harness, spawn the driver task, and return a fully-wired
    /// `Arc<ControllerContext>` ready to pass into a `reconcile_*` fn.
    ///
    /// The `Registry` is returned alongside so the test can inspect emitted
    /// metrics after the reconcile completes.
    pub fn new(expected: Vec<ExpectedCall>) -> (Arc<ControllerContext>, Registry, Self) {
        Self::with_stores(expected, empty_stores())
    }

    /// Same as [`MockKubeHarness::new`], but with caches the test has seeded.
    ///
    /// Every cross-resource read a reconcile makes now comes from these, so a
    /// call that still reaches the mock service is a LIST the controller was
    /// supposed to have stopped making — the queue is the assertion.
    pub fn with_stores(
        expected: Vec<ExpectedCall>,
        stores: ControllerStores,
    ) -> (Arc<ControllerContext>, Registry, Self) {
        let (mock_service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
        let (stop, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

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

                captured.push(CapturedRequest {
                    method: actual_method,
                    path: actual_path,
                    query: actual_query,
                    body: body_bytes,
                });

                let response = Response::builder()
                    .status(expected_call.response_status)
                    .header("content-type", "application/json")
                    .body(Body::from(expected_call.response_body))
                    .expect("test response must build");
                send.send_response(response);
            }

            // The handle deliberately outlives the expected queue. Dropping it
            // here closes the channel, so a further request fails as a
            // transport error — and every caller that only `warn!`s a failed
            // patch swallows that, leaving `captured` empty. A test asserting
            // "this reconcile made no calls" would then be satisfied by a call
            // it never saw. Answering with the same error while RECORDING the
            // request keeps the reconcile's behaviour identical and makes the
            // extra call visible.
            let mut unexpected: Vec<String> = Vec::new();
            let mut record_unexpected = |request: Request<Body>, send: mock::SendResponse<_>| {
                unexpected.push(format!("{} {}", request.method(), request.uri().path()));
                send.send_error("unexpected request after the harness queue was consumed");
            };
            loop {
                tokio::select! {
                    // Biased so a request already in the channel is taken
                    // before the stop signal that raced it.
                    biased;
                    request = handle.next_request() => match request {
                        Some((request, send)) => record_unexpected(request, send),
                        None => break,
                    },
                    _ = &mut stop_rx => break,
                }
            }
            while let Some(Some((request, send))) = handle.next_request().now_or_never() {
                record_unexpected(request, send);
            }

            DriverOutcome {
                captured,
                unexpected,
            }
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
            stores,
        });

        (ctx, registry, Self { driver, stop })
    }

    /// Await the driver, asserting all expected calls were consumed in order
    /// and that the reconcile made no further call afterwards.
    ///
    /// Panics if the driver hasn't finished within `5s` (likely the reconcile
    /// made fewer kube calls than expected).
    pub async fn finish(self) -> HarnessReport {
        // Releases the driver from its post-queue watch. Failure means the
        // driver is already gone, which the join below reports properly.
        let _ = self.stop.send(());
        let outcome = match tokio::time::timeout(Duration::from_secs(5), self.driver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(join_err)) => panic!("harness driver task panicked: {join_err}"),
            Err(_) => panic!(
                "harness driver did not complete within 5s — \
                 the reconcile likely made fewer kube calls than expected, \
                 or made a call out of order"
            ),
        };
        assert!(
            outcome.unexpected.is_empty(),
            "the reconcile made {} request(s) beyond its expected queue: {}",
            outcome.unexpected.len(),
            outcome.unexpected.join(", "),
        );
        HarnessReport {
            captured: outcome.captured,
        }
    }
}

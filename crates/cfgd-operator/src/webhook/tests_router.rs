//! Router-level tests for the webhook server.
//!
//! Each test drives the production `Router` (built via
//! `build_webhook_router`) end-to-end via `tower::ServiceExt::oneshot`,
//! exercising body parsing, the `Json<AdmissionReview>` extractor,
//! `DefaultBodyLimit` middleware, and the response-body shape that the
//! kube-apiserver consumes.
#![cfg(test)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kube::core::DynamicObject;
use kube::core::admission::AdmissionReview;
use tower::ServiceExt;

use super::test_router::test_webhook_router;
use crate::metrics::WebhookLabels;

fn admission_review_body(kind: &str, resource: &str, spec: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "test-uid",
            "kind": {"group": "cfgd.io", "version": "v1alpha1", "kind": kind},
            "resource": {"group": "cfgd.io", "version": "v1alpha1", "resource": resource},
            "operation": "CREATE",
            "userInfo": {"username": "test-user"},
            "object": {
                "apiVersion": "cfgd.io/v1alpha1",
                "kind": kind,
                "metadata": {"name": "test", "namespace": "default"},
                "spec": spec,
            }
        }
    }))
    .expect("admission review json")
}

async fn parse_response(response: axum::response::Response) -> AdmissionReview<DynamicObject> {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response is an AdmissionReview")
}

fn post(path: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("request must build")
}

// -----------------------------------------------------------------------
// Healthz liveness probe
// -----------------------------------------------------------------------

#[tokio::test]
async fn healthz_returns_200_ok() {
    let (router, _metrics) = test_webhook_router();

    let request = Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.as_ref(), b"ok");
}

// -----------------------------------------------------------------------
// validate-machineconfig
// -----------------------------------------------------------------------

#[tokio::test]
async fn validate_machineconfig_allows_valid_spec() {
    let (router, metrics) = test_webhook_router();

    let body = admission_review_body(
        "MachineConfig",
        "machineconfigs",
        serde_json::json!({"hostname": "test", "profile": "default"}),
    );

    let response = router
        .oneshot(post("/validate-machineconfig", body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(resp.allowed, "valid spec must be allowed");

    let allowed = metrics
        .webhook_requests_total
        .get_or_create(&WebhookLabels {
            operation: "validate_machineconfig".to_string(),
            result: "allowed".to_string(),
        })
        .get();
    assert_eq!(allowed, 1, "metrics must record allowed");
}

#[tokio::test]
async fn validate_machineconfig_denies_empty_hostname() {
    let (router, metrics) = test_webhook_router();
    let body = admission_review_body(
        "MachineConfig",
        "machineconfigs",
        serde_json::json!({"hostname": "", "profile": "default"}),
    );

    let response = router
        .oneshot(post("/validate-machineconfig", body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(!resp.allowed, "empty hostname must be denied");
    assert!(
        resp.result.message.contains("hostname"),
        "denial must reference hostname: {}",
        resp.result.message
    );

    let denied = metrics
        .webhook_requests_total
        .get_or_create(&WebhookLabels {
            operation: "validate_machineconfig".to_string(),
            result: "denied".to_string(),
        })
        .get();
    assert_eq!(denied, 1);
}

#[tokio::test]
async fn validate_machineconfig_denies_path_traversal_in_file_path() {
    let (router, _metrics) = test_webhook_router();
    let body = admission_review_body(
        "MachineConfig",
        "machineconfigs",
        serde_json::json!({
            "hostname": "test",
            "profile": "default",
            "files": [{
                "path": "../etc/passwd",
                "content": "x",
                "mode": "0644",
            }],
        }),
    );

    let response = router
        .oneshot(post("/validate-machineconfig", body))
        .await
        .unwrap();
    let review = parse_response(response).await;
    let resp = review.response.unwrap();
    assert!(!resp.allowed);
    assert!(
        resp.result.message.contains("traversal") || resp.result.message.contains(".."),
        "must call out traversal: {}",
        resp.result.message
    );
}

// -----------------------------------------------------------------------
// validate-configpolicy
// -----------------------------------------------------------------------

#[tokio::test]
async fn validate_configpolicy_allows_valid_spec() {
    let (router, _metrics) = test_webhook_router();

    let body = admission_review_body(
        "ConfigPolicy",
        "configpolicies",
        serde_json::json!({"requiredModules": [], "packages": []}),
    );

    let response = router
        .oneshot(post("/validate-configpolicy", body))
        .await
        .unwrap();
    let review = parse_response(response).await;
    assert!(review.response.unwrap().allowed);
}

#[tokio::test]
async fn validate_configpolicy_denies_invalid_semver_requirement() {
    let (router, _metrics) = test_webhook_router();

    let body = admission_review_body(
        "ConfigPolicy",
        "configpolicies",
        serde_json::json!({
            "packages": [{"name": "kubectl", "version": "not a valid semver req"}],
        }),
    );

    let response = router
        .oneshot(post("/validate-configpolicy", body))
        .await
        .unwrap();
    let review = parse_response(response).await;
    let resp = review.response.unwrap();
    assert!(!resp.allowed);
    assert!(
        resp.result.message.contains("semver") || resp.result.message.contains("version"),
        "{}",
        resp.result.message
    );
}

// -----------------------------------------------------------------------
// validate-clusterconfigpolicy
// -----------------------------------------------------------------------

#[tokio::test]
async fn validate_clusterconfigpolicy_allows_valid_spec() {
    let (router, _metrics) = test_webhook_router();
    let body = admission_review_body(
        "ClusterConfigPolicy",
        "clusterconfigpolicies",
        serde_json::json!({"requiredModules": [], "packages": []}),
    );

    let response = router
        .oneshot(post("/validate-clusterconfigpolicy", body))
        .await
        .unwrap();
    let review = parse_response(response).await;
    assert!(review.response.unwrap().allowed);
}

// -----------------------------------------------------------------------
// validate-driftalert
// -----------------------------------------------------------------------

#[tokio::test]
async fn validate_driftalert_allows_minimal_valid_spec() {
    let (router, _metrics) = test_webhook_router();
    let body = admission_review_body(
        "DriftAlert",
        "driftalerts",
        serde_json::json!({
            "deviceId": "device-1",
            "machineConfigRef": {"name": "mc-1"},
            "driftDetails": [],
            "severity": "Medium",
        }),
    );

    let response = router
        .oneshot(post("/validate-driftalert", body))
        .await
        .unwrap();
    let review = parse_response(response).await;
    assert!(review.response.unwrap().allowed);
}

// -----------------------------------------------------------------------
// validate-machineconfig — bad request body
// -----------------------------------------------------------------------

#[tokio::test]
async fn validate_machineconfig_with_garbage_body_returns_invalid_review() {
    let (router, metrics) = test_webhook_router();

    let response = router
        .oneshot(post("/validate-machineconfig", b"not json".to_vec()))
        .await
        .unwrap();

    // Json extractor rejects malformed body → 4xx.
    assert!(
        response.status().is_client_error(),
        "garbage JSON must yield 4xx, got {}",
        response.status()
    );
    // No metrics fired (the handler never ran).
    let total = metrics
        .webhook_requests_total
        .get_or_create(&WebhookLabels {
            operation: "validate_machineconfig".to_string(),
            result: "allowed".to_string(),
        })
        .get();
    assert_eq!(total, 0);
}

#[tokio::test]
async fn validate_machineconfig_delete_operation_without_object_is_allowed() {
    let (router, metrics) = test_webhook_router();
    // DELETE has no `object` field — `validate_object_spec` lets it through
    // since there's nothing to validate on a delete. Exercises the
    // "no object" branch.
    let body = serde_json::to_vec(&serde_json::json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "delete-uid",
            "kind": {"group": "cfgd.io", "version": "v1alpha1", "kind": "MachineConfig"},
            "resource": {"group": "cfgd.io", "version": "v1alpha1", "resource": "machineconfigs"},
            "operation": "DELETE",
            "userInfo": {"username": "test"},
        }
    }))
    .unwrap();

    let response = router
        .oneshot(post("/validate-machineconfig", body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(resp.allowed, "DELETE without object must be allowed");

    let allowed = metrics
        .webhook_requests_total
        .get_or_create(&WebhookLabels {
            operation: "validate_machineconfig".to_string(),
            result: "allowed".to_string(),
        })
        .get();
    assert_eq!(allowed, 1);
}

// -----------------------------------------------------------------------
// validate-module — happy path (no kube list needed when no policies)
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// mutate-pods
// -----------------------------------------------------------------------

fn pod_admission_review(pod_object: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "pod-uid",
            "kind": {"group": "", "version": "v1", "kind": "Pod"},
            "resource": {"group": "", "version": "v1", "resource": "pods"},
            "operation": "CREATE",
            "namespace": "test-ns",
            "userInfo": {"username": "test"},
            "object": pod_object,
        }
    }))
    .unwrap()
}

#[tokio::test]
async fn mutate_pods_with_no_annotations_or_policy_passes_through_unchanged() {
    let (router, metrics) = test_webhook_router();

    let pod = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "no-mods", "namespace": "test-ns"},
        "spec": {
            "containers": [{"name": "main", "image": "alpine:latest"}],
        }
    });

    let response = router
        .oneshot(post("/mutate-pods", pod_admission_review(pod)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(resp.allowed, "pod with no modules must be allowed");
    assert!(resp.patch.is_none(), "no-modules pod must produce no patch");

    let allowed = metrics
        .webhook_requests_total
        .get_or_create(&WebhookLabels {
            operation: "mutate_pods".to_string(),
            result: "allowed".to_string(),
        })
        .get();
    assert_eq!(allowed, 1);
}

#[tokio::test]
async fn mutate_pods_with_delete_operation_skips_object_processing() {
    let (router, _metrics) = test_webhook_router();
    // DELETE has no object; the handler returns AdmissionResponse::from(&req)
    // immediately (allowed).
    let body = serde_json::to_vec(&serde_json::json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "delete-pod",
            "kind": {"group": "", "version": "v1", "kind": "Pod"},
            "resource": {"group": "", "version": "v1", "resource": "pods"},
            "operation": "DELETE",
            "namespace": "test-ns",
            "userInfo": {"username": "test"},
        }
    }))
    .unwrap();

    let response = router.oneshot(post("/mutate-pods", body)).await.unwrap();
    let review = parse_response(response).await;
    assert!(review.response.unwrap().allowed);
}

#[tokio::test]
async fn mutate_pods_with_garbage_body_records_error_metric() {
    let (router, _metrics) = test_webhook_router();
    let response = router
        .oneshot(post("/mutate-pods", b"\x00garbage\xff".to_vec()))
        .await
        .unwrap();
    // Json extractor rejects it → 4xx.
    assert!(response.status().is_client_error());
}

#[tokio::test]
async fn mutate_pods_with_default_namespace_when_request_namespace_missing() {
    let (router, _metrics) = test_webhook_router();
    // No `namespace` in request — handler uses "default" fallback.
    let body = serde_json::to_vec(&serde_json::json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "no-ns-pod",
            "kind": {"group": "", "version": "v1", "kind": "Pod"},
            "resource": {"group": "", "version": "v1", "resource": "pods"},
            "operation": "CREATE",
            "userInfo": {"username": "test"},
            "object": {
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {"name": "no-ns"},
                "spec": {"containers": [{"name": "main", "image": "alpine"}]},
            }
        }
    }))
    .unwrap();

    let response = router.oneshot(post("/mutate-pods", body)).await.unwrap();
    let review = parse_response(response).await;
    assert!(review.response.unwrap().allowed);
}

#[tokio::test]
async fn validate_module_returns_response_with_uid_matching_request() {
    // Hits handle_validate_module, including the conversion +
    // validate_object_spec path. The stub kube client returns `{}` for
    // LIST CCPs; whichever branch enforce_module_policy takes, the
    // response must echo the request's UID per AdmissionResponse contract.
    let (router, _metrics) = test_webhook_router();

    let body = admission_review_body(
        "Module",
        "modules",
        serde_json::json!({"packages": [{"name": "kubectl"}]}),
    );

    let response = router
        .oneshot(post("/validate-module", body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert_eq!(
        resp.uid, "test-uid",
        "AdmissionResponse must echo the request UID"
    );
}

// -----------------------------------------------------------------------
// try_into Err arms — AdmissionReview without `request` field
//
// All 6 admission handlers contain the same `try_into` arm:
//     match AdmissionReview::<DynamicObject>::try_into(review) {
//         Ok(r) => r,
//         Err(e) => { record "error" metric; return invalid review }
//     }
// The garbage-body test above exercises body parsing (4xx before the
// handler), not the typed-conversion Err arm. To reach it we post a
// well-formed AdmissionReview JSON with NO `request` field — that
// deserializes (request: Option<AdmissionRequest> = None) but the
// TryFrom<AdmissionReview<T>> for AdmissionRequest<T> impl in kube-rs
// returns MissingRequest. The handler then takes the Err branch.
// -----------------------------------------------------------------------

fn review_without_request() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
    }))
    .expect("review-without-request json")
}

fn assert_error_metric(metrics: &crate::metrics::Metrics, operation: &str) {
    let count = metrics
        .webhook_requests_total
        .get_or_create(&WebhookLabels {
            operation: operation.to_string(),
            result: "error".to_string(),
        })
        .get();
    assert_eq!(
        count, 1,
        "operation {operation:?} must record exactly one error metric"
    );
}

async fn assert_invalid_review_body(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::OK);
    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    // AdmissionResponse::invalid sets allowed=false and includes the failure
    // message in `result.message`. We pin both so the contract is locked in.
    assert!(
        !resp.allowed,
        "TryInto-Err arm must produce a not-allowed review"
    );
    let status = resp.result;
    assert!(
        status.message.contains("bad admission request"),
        "TryInto-Err arm must surface 'bad admission request' in result.message, got: {status:?}"
    );
}

#[tokio::test]
async fn validate_machineconfig_records_error_metric_when_request_field_missing() {
    let (router, metrics) = test_webhook_router();
    let response = router
        .oneshot(post("/validate-machineconfig", review_without_request()))
        .await
        .unwrap();
    assert_invalid_review_body(response).await;
    assert_error_metric(&metrics, "validate_machineconfig");
}

#[tokio::test]
async fn validate_configpolicy_records_error_metric_when_request_field_missing() {
    let (router, metrics) = test_webhook_router();
    let response = router
        .oneshot(post("/validate-configpolicy", review_without_request()))
        .await
        .unwrap();
    assert_invalid_review_body(response).await;
    assert_error_metric(&metrics, "validate_configpolicy");
}

#[tokio::test]
async fn validate_clusterconfigpolicy_records_error_metric_when_request_field_missing() {
    let (router, metrics) = test_webhook_router();
    let response = router
        .oneshot(post(
            "/validate-clusterconfigpolicy",
            review_without_request(),
        ))
        .await
        .unwrap();
    assert_invalid_review_body(response).await;
    assert_error_metric(&metrics, "validate_clusterconfigpolicy");
}

#[tokio::test]
async fn validate_driftalert_records_error_metric_when_request_field_missing() {
    let (router, metrics) = test_webhook_router();
    let response = router
        .oneshot(post("/validate-driftalert", review_without_request()))
        .await
        .unwrap();
    assert_invalid_review_body(response).await;
    assert_error_metric(&metrics, "validate_driftalert");
}

#[tokio::test]
async fn validate_module_records_error_metric_when_request_field_missing() {
    let (router, metrics) = test_webhook_router();
    let response = router
        .oneshot(post("/validate-module", review_without_request()))
        .await
        .unwrap();
    assert_invalid_review_body(response).await;
    assert_error_metric(&metrics, "validate_module");
}

#[tokio::test]
async fn mutate_pods_records_error_metric_when_request_field_missing() {
    let (router, metrics) = test_webhook_router();
    let response = router
        .oneshot(post("/mutate-pods", review_without_request()))
        .await
        .unwrap();
    assert_invalid_review_body(response).await;
    assert_error_metric(&metrics, "mutate_pods");
}

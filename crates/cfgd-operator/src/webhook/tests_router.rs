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

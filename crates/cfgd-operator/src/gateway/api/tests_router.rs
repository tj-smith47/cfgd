//! Router-level tests for the gateway API endpoints.
//!
//! Each test drives the production `Router` (built via `api::router`) end-to-end
//! via `tower::ServiceExt::oneshot`, exercising auth middleware, body parsing,
//! per-route handlers, and the `GatewayError -> IntoResponse` mapping.
#![cfg(test)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serial_test::serial;
use tower::ServiceExt;

use super::*;
use crate::gateway::test_state::test_state;

const TEST_ADMIN_KEY: &str = "test-admin-secret";

fn router_with_state(state: SharedState) -> Router {
    super::router(state.clone()).with_state(state)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build GET")
}

fn get_with_bearer(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("build GET")
}

fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .expect("build POST")
}

fn post_json_with_bearer(uri: &str, token: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .expect("build POST")
}

fn put_bytes_with_bearer(uri: &str, token: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body))
        .expect("build PUT")
}

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

// -----------------------------------------------------------------------
// auth_middleware — 401 when no bearer token, no API key set
// -----------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn list_devices_returns_401_when_no_bearer_token_and_no_api_key() {
    // Safety: serial_test ensures no other test concurrently mutates this env var.
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router.oneshot(get("/api/v1/devices")).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn list_devices_returns_401_with_invalid_bearer_when_no_api_key() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router
        .oneshot(get_with_bearer("/api/v1/devices", "wrong-key"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// -----------------------------------------------------------------------
// auth_middleware — admin path
// -----------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn list_devices_returns_200_with_admin_key() {
    unsafe {
        std::env::set_var("CFGD_API_KEY", TEST_ADMIN_KEY);
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router
        .oneshot(get_with_bearer("/api/v1/devices", TEST_ADMIN_KEY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_bytes(response).await;
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let devices = json
        .as_array()
        .expect("response is a JSON array of devices");
    assert_eq!(devices.len(), 0, "no devices in fresh state");

    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
}

#[tokio::test]
#[serial]
async fn admin_endpoint_returns_401_when_api_key_not_set() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router.oneshot(get("/api/v1/admin/tokens")).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn admin_endpoint_returns_401_with_wrong_admin_key() {
    unsafe {
        std::env::set_var("CFGD_API_KEY", TEST_ADMIN_KEY);
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router
        .oneshot(get_with_bearer("/api/v1/admin/tokens", "not-the-right-key"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
}

#[tokio::test]
#[serial]
async fn admin_endpoint_returns_200_with_correct_admin_key() {
    unsafe {
        std::env::set_var("CFGD_API_KEY", TEST_ADMIN_KEY);
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router
        .oneshot(get_with_bearer("/api/v1/admin/tokens", TEST_ADMIN_KEY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
}

// -----------------------------------------------------------------------
// Enrollment endpoints — no pre-auth required
// -----------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn enroll_info_returns_method_without_authentication() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router.oneshot(get("/api/v1/enroll/info")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_bytes(response).await;
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Response shape: {"method":"token","required_fields":[...]}
    let method = json.get("method").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        ["token", "deviceFlow", "tokenAndDeviceFlow"].contains(&method),
        "unexpected enrollment method in response: {method}"
    );
}

#[tokio::test]
#[serial]
async fn enroll_with_invalid_payload_returns_400() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router
        .oneshot(post_json(
            "/api/v1/enroll",
            serde_json::json!({"unrelated": "field"}),
        ))
        .await
        .unwrap();
    // The handler validates required fields and returns InvalidRequest (4xx).
    assert!(
        response.status().is_client_error(),
        "invalid enrollment must yield 4xx, got {}",
        response.status()
    );
}

#[tokio::test]
#[serial]
async fn enroll_with_empty_device_id_returns_400() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router
        .oneshot(post_json(
            "/api/v1/enroll",
            serde_json::json!({
                "device_id": "",
                "hostname": "ws-1",
                "platform": "linux",
                "arch": "x86_64",
                "method": "token",
                "token": "any",
            }),
        ))
        .await
        .unwrap();
    assert!(
        response.status().is_client_error(),
        "empty device_id must yield 4xx, got {}",
        response.status()
    );
}

// -----------------------------------------------------------------------
// 404 routing
// -----------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn unknown_route_returns_404() {
    unsafe {
        std::env::set_var("CFGD_API_KEY", TEST_ADMIN_KEY);
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router
        .oneshot(get_with_bearer("/api/v1/does-not-exist", TEST_ADMIN_KEY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
}

// -----------------------------------------------------------------------
// Enroll — token mode happy path + error branches
// -----------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn enroll_with_unconfigured_token_returns_4xx() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    // Default state uses Token enrollment.
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router
        .oneshot(post_json(
            "/api/v1/enroll",
            serde_json::json!({
                "deviceId": "dev-1",
                "hostname": "host-1",
                "os": "linux",
                "arch": "x86_64",
                "token": "not-a-real-bootstrap-token",
            }),
        ))
        .await
        .unwrap();
    assert!(
        response.status().is_client_error(),
        "unrecognized bootstrap token must yield 4xx, got {}",
        response.status()
    );
}

#[tokio::test]
#[serial]
async fn enroll_with_empty_token_returns_400() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();
    let router = router_with_state(state);

    let response = router
        .oneshot(post_json(
            "/api/v1/enroll",
            serde_json::json!({
                "deviceId": "dev-1",
                "hostname": "host-1",
                "os": "linux",
                "arch": "x86_64",
                "token": "",
            }),
        ))
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let body = String::from_utf8(body_bytes(response).await).unwrap_or_default();
    assert!(
        body.contains("token") || body.contains("empty"),
        "error body should mention empty token: {body}"
    );
}

#[tokio::test]
#[serial]
async fn enroll_with_valid_bootstrap_token_returns_201_and_api_key() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();

    // Provision a bootstrap token for "alice".
    let bootstrap = "boot-token-xyz";
    let bootstrap_hash = hash_token(bootstrap);
    let expires_at = "2099-01-01T00:00:00Z";
    state
        .db
        .create_bootstrap_token(&bootstrap_hash, "alice", None, expires_at)
        .await
        .expect("insert bootstrap");

    let router = router_with_state(state);

    let response = router
        .oneshot(post_json(
            "/api/v1/enroll",
            serde_json::json!({
                "deviceId": "alice-laptop",
                "hostname": "alice-laptop.test",
                "os": "linux",
                "arch": "x86_64",
                "token": bootstrap,
            }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(response).await).expect("json body");
    assert_eq!(body["status"], "enrolled");
    assert_eq!(body["deviceId"], "alice-laptop");
    assert_eq!(body["username"], "alice");
    assert!(
        body["apiKey"]
            .as_str()
            .unwrap_or("")
            .starts_with("cfgd_dev_"),
        "expected device API key prefix: {}",
        body["apiKey"]
    );
}

// -----------------------------------------------------------------------
// Device-token auth path (per-device credential)
// -----------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn checkin_with_device_token_succeeds_after_enrollment() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();

    // Provision a device + credential directly via the DB so we don't need
    // to round-trip through the enrollment handler.
    state
        .db
        .register_device("dev-checkin", "host-1", "linux", "x86_64", "abc", None)
        .await
        .expect("register device");
    let token = "dev-bearer-token-xyz";
    let token_hash = hash_token(token);
    state
        .db
        .create_device_credential("dev-checkin", &token_hash, "user1", None)
        .await
        .expect("insert credential");

    let router = router_with_state(state);

    // Checkin payload — server validates auth then records the checkin.
    let response = router
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            token,
            serde_json::json!({
                "deviceId": "dev-checkin",
                "hostname": "host-1",
                "os": "linux",
                "arch": "x86_64",
                "configHash": "abc",
            }),
        ))
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "checkin failed: {} body={:?}",
        response.status(),
        String::from_utf8_lossy(&body_bytes(response).await)
    );
}

#[tokio::test]
#[serial]
async fn device_token_cannot_access_resources_of_other_device() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (state, _tmp) = test_state();

    state
        .db
        .register_device("dev-a", "host-a", "linux", "x86_64", "abc", None)
        .await
        .expect("register dev-a");
    state
        .db
        .register_device("dev-b", "host-b", "linux", "x86_64", "abc", None)
        .await
        .expect("register dev-b");

    let token = "dev-a-token";
    let token_hash = hash_token(token);
    state
        .db
        .create_device_credential("dev-a", &token_hash, "user-a", None)
        .await
        .expect("insert credential");

    let router = router_with_state(state);

    // dev-a tries to fetch dev-b's record → 403.
    let response = router
        .oneshot(get_with_bearer("/api/v1/devices/dev-b", token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// -----------------------------------------------------------------------
// set_device_config — config-size policy (specific 400) vs body backstop (413)
//
// The wrapping HTTP body `{"config": <config>}` is always LARGER than the
// config it carries. The route's `DefaultBodyLimit` must sit above the
// handler's `MAX_CONFIG_BYTES` policy so an over-policy config still reaches
// the handler and gets the actionable 400 — instead of a generic 413 that
// never mentions the policy.
// -----------------------------------------------------------------------

/// Build a `{"config": "<filler>"}` request body whose embedded config JSON
/// string serializes to just over `MAX_CONFIG_BYTES`, so the handler's
/// size check trips. Returns `(body_bytes, embedded_config_len)`.
fn oversize_config_body() -> Vec<u8> {
    // The config value is a JSON string; its serialized length is the filler
    // length + 2 quote chars. Make that exceed MAX_CONFIG_BYTES by ~1 MiB.
    let filler = "x".repeat(MAX_CONFIG_BYTES + 1024 * 1024);
    serde_json::to_vec(&serde_json::json!({ "config": filler })).expect("serialize oversize body")
}

#[tokio::test]
#[serial]
async fn set_device_config_over_policy_returns_specific_400_through_router() {
    unsafe {
        std::env::set_var("CFGD_API_KEY", TEST_ADMIN_KEY);
    }
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-big", "host-big", "linux", "x86_64", "abc", None)
        .await
        .expect("register device");
    let router = router_with_state(state);

    let body = oversize_config_body();
    let response = router
        .oneshot(put_bytes_with_bearer(
            "/api/v1/devices/dev-big/config",
            TEST_ADMIN_KEY,
            body,
        ))
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "over-policy config must reach the handler and yield the specific 400, not a generic 413"
    );
    let body_str = String::from_utf8(body_bytes(response).await).unwrap_or_default();
    assert!(
        body_str.contains("config exceeds 10MB size limit"),
        "expected actionable policy message, got: {body_str}"
    );

    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
}

#[tokio::test]
#[serial]
async fn set_device_config_over_body_backstop_returns_413_through_router() {
    unsafe {
        std::env::set_var("CFGD_API_KEY", TEST_ADMIN_KEY);
    }
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-huge", "host-huge", "linux", "x86_64", "abc", None)
        .await
        .expect("register device");
    let router = router_with_state(state);

    // A body strictly above the route backstop is rejected before the handler.
    let filler = "x".repeat(MAX_REQUEST_BODY_BYTES + 1024);
    let body = serde_json::to_vec(&serde_json::json!({ "config": filler })).expect("serialize");
    let response = router
        .oneshot(put_bytes_with_bearer(
            "/api/v1/devices/dev-huge/config",
            TEST_ADMIN_KEY,
            body,
        ))
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "body above the route backstop must yield 413 (DoS guard)"
    );

    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
}

#[tokio::test]
#[serial]
async fn set_device_config_under_policy_succeeds_through_router() {
    unsafe {
        std::env::set_var("CFGD_API_KEY", TEST_ADMIN_KEY);
    }
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-ok", "host-ok", "linux", "x86_64", "abc", None)
        .await
        .expect("register device");
    let router = router_with_state(state);

    // ~9 MiB config — under the policy, should be accepted (204).
    let filler = "x".repeat(9 * 1024 * 1024);
    let body = serde_json::to_vec(&serde_json::json!({ "config": filler })).expect("serialize");
    let response = router
        .oneshot(put_bytes_with_bearer(
            "/api/v1/devices/dev-ok/config",
            TEST_ADMIN_KEY,
            body,
        ))
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "under-policy config must be accepted"
    );

    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
}

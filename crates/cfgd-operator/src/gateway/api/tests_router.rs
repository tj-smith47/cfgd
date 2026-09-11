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
use crate::controllers::test_kube_harness::{ExpectedCall, MockKubeHarness};
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

    // Provision a device + credential directly via the DB to avoid needing
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

// -----------------------------------------------------------------------
// The check-in's Kubernetes half: the status it writes and the cluster
// schedules it answers with
// -----------------------------------------------------------------------

/// An `ObjectList` body carrying one MachineConfig for `hostname`, as the
/// gateway's cluster-wide lookup reads it.
fn machine_config_list(namespace: &str, name: &str, hostname: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "MachineConfigList",
        "metadata": { "resourceVersion": "1" },
        "items": [{
            "apiVersion": "cfgd.io/v1alpha1",
            "kind": "MachineConfig",
            "metadata": { "name": name, "namespace": namespace },
            "spec": { "hostname": hostname, "profile": "base" },
        }],
    })
}

/// An `ObjectList` body carrying the given BackupPolicy objects.
fn backup_policy_list(items: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "BackupPolicyList",
        "metadata": { "resourceVersion": "1" },
        "items": items,
    })
}

/// One BackupPolicy whose status already carries the rows the controller wrote.
fn backup_policy(
    namespace: &str,
    name: &str,
    created: &str,
    units: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "BackupPolicy",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "creationTimestamp": created,
        },
        "spec": { "selector": {}, "units": [] },
        "status": { "units": units, "machinesMatched": 1 },
    })
}

/// A device with a credential, and the bearer token that authenticates it.
async fn enrolled_device(state: &SharedState, device_id: &str, hostname: &str) -> String {
    state
        .db
        .register_device(device_id, hostname, "linux", "x86_64", "abc", None)
        .await
        .expect("register device");
    let token = format!("bearer-{device_id}");
    state
        .db
        .create_device_credential(device_id, &hash_token(&token), "user1", None)
        .await
        .expect("insert credential");
    token
}

fn checkin_body(device_id: &str, hostname: &str) -> serde_json::Value {
    serde_json::json!({
        "deviceId": device_id,
        "hostname": hostname,
        "os": "linux",
        "arch": "x86_64",
        "configHash": "abc",
        "packageVersions": { "brew/git": "2.45.1" },
        "backupScheduleOwners": { "dotfiles": "cluster", "notes": "local" },
    })
}

/// A check-in body carrying the two maps this device observed.
fn checkin_body_reporting(
    device_id: &str,
    hostname: &str,
    package_versions: serde_json::Value,
    backup_schedule_owners: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "deviceId": device_id,
        "hostname": hostname,
        "os": "linux",
        "arch": "x86_64",
        "configHash": "abc",
        "packageVersions": package_versions,
        "backupScheduleOwners": backup_schedule_owners,
    })
}

/// The MachineConfig status path a `host-1` check-in resolves to.
const MACHINE_STATUS_PATH: &str =
    "/apis/cfgd.io/v1alpha1/namespaces/fleet/machineconfigs/workstation-1-mc/status";

/// One status apply, its field manager named. A map's manager is the ONLY
/// writer of its field, so a fixture states which manager it expects rather
/// than accepting whichever apply came first.
fn expect_status_apply(field_manager: &str) -> ExpectedCall {
    ExpectedCall::patch_status(MACHINE_STATUS_PATH).with_query_contains(format!(
        "fieldManager={}",
        field_manager.replace('/', "%2F")
    ))
}

/// The calls a check-in reporting BOTH maps makes against a cluster that holds
/// a MachineConfig for `host-1`, with no policy to project: one apply per map,
/// each under its own manager.
fn checkin_kube_calls() -> Vec<ExpectedCall> {
    vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
            .returning_json(&machine_config_list("fleet", "workstation-1-mc", "host-1")),
        expect_status_apply("cfgd-operator/gateway/packages"),
        expect_status_apply("cfgd-operator/gateway/backups"),
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/namespaces/fleet/backuppolicies")
            .returning_json(&backup_policy_list(vec![])),
    ]
}

/// Every status apply the check-in made, as `(fieldManager, body)`.
fn status_applies(
    report: &crate::controllers::test_kube_harness::HarnessReport,
) -> Vec<(String, serde_json::Value)> {
    report
        .captured
        .iter()
        .filter(|r| {
            r.method == http::Method::PATCH
                && r.path.ends_with("/machineconfigs/workstation-1-mc/status")
        })
        .map(|r| {
            assert!(
                r.query.contains("force=true"),
                "each manager is the sole writer of its one field, so its apply is forced ({})",
                r.query
            );
            let manager = r
                .query
                .split('&')
                .find_map(|p| p.strip_prefix("fieldManager="))
                .expect("an apply always names its field manager")
                .replace("%2F", "/");
            (manager, r.body_json())
        })
        .collect()
}

/// The body applied under `field_manager`, or a panic naming what was applied
/// instead.
fn applied_under(
    report: &crate::controllers::test_kube_harness::HarnessReport,
    field_manager: &str,
) -> serde_json::Value {
    let applies = status_applies(report);
    applies
        .iter()
        .find(|(manager, _)| manager == field_manager)
        .map(|(_, body)| body.clone())
        .unwrap_or_else(|| {
            panic!(
                "no apply under {field_manager}; the check-in applied {:?}",
                applies.iter().map(|(m, _)| m).collect::<Vec<_>>()
            )
        })
}

/// The `status` object of an apply body, for a claim about which fields it
/// NAMES. A serde index answers `null` both for an absent key and for one
/// present with a null value, and the two are different applies: the second
/// claims the field for this manager and clears it.
fn status_object(body: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
    body["status"]
        .as_object()
        .unwrap_or_else(|| panic!("an apply body carries a status object: {body}"))
}

#[tokio::test]
#[serial]
async fn checkin_patches_the_devices_machine_config_status() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(checkin_kube_calls());
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body("dev-1", "host-1"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let report = harness.finish().await;
    let packages = applied_under(&report, "cfgd-operator/gateway/packages");
    assert_eq!(packages["status"]["packageVersions"]["brew/git"], "2.45.1");
    let backups = applied_under(&report, "cfgd-operator/gateway/backups");
    assert_eq!(
        backups["status"]["backupScheduleOwners"]["notes"],
        serde_json::json!("local")
    );
    // An apply body is a whole object: the API server reads the type and the
    // name from it, and a fragment would be rejected.
    for body in [&packages, &backups] {
        assert_eq!(body["apiVersion"], "cfgd.io/v1alpha1");
        assert_eq!(body["kind"], "MachineConfig");
        assert_eq!(body["metadata"]["name"], "workstation-1-mc");
    }
}

/// A manager owns exactly one field. A server-side apply removes the fields its
/// own manager stops naming, so two maps under one manager would make a
/// check-in that observed only one delete the other; two managers make the
/// unobserved map unreportable rather than blanked.
#[tokio::test]
#[serial]
async fn each_reported_map_is_applied_under_its_own_field_manager() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(checkin_kube_calls());
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body_reporting(
                "dev-1",
                "host-1",
                serde_json::json!({ "brew/git": "2.45.1" }),
                serde_json::json!({ "dotfiles": "cluster" }),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let report = harness.finish().await;
    let applies = status_applies(&report);
    assert_eq!(
        applies.len(),
        2,
        "one apply per observed map, never one carrying both"
    );
    // ABSENT, not present-and-null: a body naming the other map with a null
    // still names it, and a server-side apply reads a named field as one this
    // manager now owns and means to clear.
    let packages = applied_under(&report, "cfgd-operator/gateway/packages");
    assert!(
        status_object(&packages)
            .get("backupScheduleOwners")
            .is_none(),
        "the packages manager never names the map it does not own: {packages}"
    );
    let backups = applied_under(&report, "cfgd-operator/gateway/backups");
    assert!(
        status_object(&backups).get("packageVersions").is_none(),
        "the backups manager never names the map it does not own: {backups}"
    );
}

/// A device whose managers could not be queried reports the owners alone. Only
/// the backups manager writes, and the versions the cluster holds are left
/// where they are, because the manager that owns them named nothing this time.
#[tokio::test]
#[serial]
async fn a_device_reporting_one_map_leaves_the_others_field_manager_silent() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
            .returning_json(&machine_config_list("fleet", "workstation-1-mc", "host-1")),
        expect_status_apply("cfgd-operator/gateway/backups"),
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/namespaces/fleet/backuppolicies")
            .returning_json(&backup_policy_list(vec![])),
    ]);
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            serde_json::json!({
                "deviceId": "dev-1",
                "hostname": "host-1",
                "os": "linux",
                "arch": "x86_64",
                "configHash": "abc",
                "backupScheduleOwners": { "dotfiles": "cluster" },
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let report = harness.finish().await;
    let applies = status_applies(&report);
    assert_eq!(
        applies.len(),
        1,
        "a map the device did not observe produces no apply for its manager"
    );
    assert_eq!(applies[0].0, "cfgd-operator/gateway/backups");
    assert!(
        status_object(&applies[0].1)
            .get("packageVersions")
            .is_none(),
        "the body names nothing the packages manager owns: {}",
        applies[0].1
    );
}

/// The device dropped a `spec.backups[]` unit, so its next check-in reports the
/// units it still declares. The apply carries the WHOLE map, which is what
/// retires the row: a merge patch would leave the retired unit standing forever
/// and the BackupPolicy controller would keep emitting a row for a unit the
/// machine no longer has.
#[tokio::test]
#[serial]
async fn a_device_that_stops_reporting_a_unit_clears_its_row() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(checkin_kube_calls());
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body_reporting(
                "dev-1",
                "host-1",
                serde_json::json!({ "brew/git": "2.45.1" }),
                serde_json::json!({ "dotfiles": "cluster" }),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let report = harness.finish().await;
    let owners =
        applied_under(&report, "cfgd-operator/gateway/backups")["status"]["backupScheduleOwners"]
            .clone();
    assert_eq!(
        owners,
        serde_json::json!({ "dotfiles": "cluster" }),
        "the apply states the whole map, so the unit that stopped being reported is gone"
    );
}

/// "I looked and hold none" is a report, not silence: the map is applied empty
/// so the last key the machine reported is retired too.
#[tokio::test]
#[serial]
async fn a_device_that_reports_no_units_clears_the_map() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(checkin_kube_calls());
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body_reporting(
                "dev-1",
                "host-1",
                serde_json::json!({}),
                serde_json::json!({}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let report = harness.finish().await;
    assert_eq!(
        applied_under(&report, "cfgd-operator/gateway/packages")["status"]["packageVersions"],
        serde_json::json!({})
    );
    assert_eq!(
        applied_under(&report, "cfgd-operator/gateway/backups")["status"]["backupScheduleOwners"],
        serde_json::json!({})
    );
}

/// A device that observed NEITHER map — an older agent — writes no status at
/// all. An apply prunes what it omits, so a body that claims nothing must not
/// be sent at all, or every check-in from an older agent would blank the two
/// facts only a device can report.
#[tokio::test]
#[serial]
async fn a_device_that_reports_neither_map_writes_no_status() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
            .returning_json(&machine_config_list("fleet", "workstation-1-mc", "host-1")),
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/namespaces/fleet/backuppolicies")
            .returning_json(&backup_policy_list(vec![])),
    ]);
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            serde_json::json!({
                "deviceId": "dev-1",
                "hostname": "host-1",
                "os": "linux",
                "arch": "x86_64",
                "configHash": "abc",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let report = harness.finish().await;
    assert!(
        status_applies(&report).is_empty(),
        "a check-in that observed nothing writes nothing, under either manager"
    );
}

/// A MachineConfig carrying no namespace is addressable by nothing: the
/// check-in skips both the status write and the policy list rather than
/// composing a request path with an empty namespace in it.
#[tokio::test]
#[serial]
async fn a_machine_config_with_no_namespace_is_never_addressed() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let namespace_less = serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "MachineConfigList",
        "metadata": { "resourceVersion": "1" },
        "items": [{
            "apiVersion": "cfgd.io/v1alpha1",
            "kind": "MachineConfig",
            "metadata": { "name": "workstation-1-mc" },
            "spec": { "hostname": "host-1", "profile": "base" },
        }],
    });
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs").returning_json(&namespace_less),
    ]);
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body("dev-1", "host-1"),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the device's check-in never depends on the cluster naming its machine"
    );
    harness.finish().await;
}

/// The cluster refusing the status costs the fleet its view of the device, and
/// the device nothing: its own reconcile does not depend on the write landing.
#[tokio::test]
#[serial]
async fn checkin_succeeds_when_the_status_patch_is_refused() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
            .returning_json(&machine_config_list("fleet", "workstation-1-mc", "host-1")),
        expect_status_apply("cfgd-operator/gateway/packages")
            .returning_server_error(403, "machineconfigs.cfgd.io is forbidden"),
        expect_status_apply("cfgd-operator/gateway/backups")
            .returning_server_error(403, "machineconfigs.cfgd.io is forbidden"),
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/namespaces/fleet/backuppolicies")
            .returning_json(&backup_policy_list(vec![])),
    ]);
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body("dev-1", "host-1"),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a refused status write never fails the check-in"
    );
    harness.finish().await;
}

#[tokio::test]
#[serial]
async fn checkin_answers_with_the_cluster_owned_projection_and_never_a_local_pin() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let policy = backup_policy(
        "fleet",
        "nightly",
        "2026-01-01T00:00:00Z",
        vec![
            serde_json::json!({
                "name": "dotfiles",
                "hostname": "host-1",
                "owner": "cluster",
                "schedule": "daily",
                "retention": 7,
            }),
            // Carrying a schedule on purpose: the gateway reads the OWNER to
            // decide, never the shape of the row the controller happened to
            // write, so a row that would otherwise project is the only fixture
            // that pins the reading.
            serde_json::json!({
                "name": "notes",
                "hostname": "host-1",
                "owner": "local",
                "schedule": "0 5 * * *",
                "message": "the machine pins this unit's schedule",
            }),
            // The owner word is the enum's own PascalCase serialization, which
            // is what a status written by the controller actually carries; the
            // parser is case-insensitive, so this row projects like the
            // lowercase one above.
            serde_json::json!({
                "name": "photos",
                "hostname": "host-1",
                "owner": "Cluster",
                "schedule": "0 4 * * 0",
                "retention": 2,
            }),
            // A word no layer spells is an answer the policy could not be read
            // for, never a cadence to push at the machine.
            serde_json::json!({
                "name": "archives",
                "hostname": "host-1",
                "owner": "tenant",
                "schedule": "0 6 * * *",
                "retention": 9,
            }),
        ],
    );
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
            .returning_json(&machine_config_list("fleet", "workstation-1-mc", "host-1")),
        expect_status_apply("cfgd-operator/gateway/packages"),
        expect_status_apply("cfgd-operator/gateway/backups"),
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/namespaces/fleet/backuppolicies")
            .returning_json(&backup_policy_list(vec![policy])),
    ]);
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body("dev-1", "host-1"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(response).await).expect("json body");
    assert_eq!(body["backupSchedules"]["dotfiles"]["schedule"], "daily");
    assert_eq!(body["backupSchedules"]["dotfiles"]["retention"], 7);
    assert_eq!(body["backupSchedules"]["photos"]["schedule"], "0 4 * * 0");
    assert!(
        body["backupSchedules"].get("notes").is_none(),
        "a unit the machine pinned is never projected back at it: {}",
        body["backupSchedules"]
    );
    assert!(
        body["backupSchedules"].get("archives").is_none(),
        "an owner word no layer spells projects nothing: {}",
        body["backupSchedules"]
    );
    harness.finish().await;
}

/// A check-in is the one request path a whole fleet drives, so it answers from
/// the cache the controllers already keep rather than listing the namespace's
/// policies again: the harness expects no list, and finding one would fail it.
#[tokio::test]
#[serial]
async fn checkin_answers_from_the_published_cache_without_listing_policies() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let policy: crate::crds::BackupPolicy = serde_json::from_value(backup_policy(
        "fleet",
        "nightly",
        "2026-01-01T00:00:00Z",
        vec![serde_json::json!({
            "name": "dotfiles",
            "hostname": "host-1",
            "owner": "cluster",
            "schedule": "daily",
            "retention": 7,
        })],
    ))
    .expect("a BackupPolicy the controller could have cached");
    let (store, mut writer) = kube::runtime::reflector::store::<crate::crds::BackupPolicy>();
    writer.apply_watcher_event(&kube::runtime::watcher::Event::Init);
    writer.apply_watcher_event(&kube::runtime::watcher::Event::InitApply(policy));
    writer.apply_watcher_event(&kube::runtime::watcher::Event::InitDone);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
            .returning_json(&machine_config_list("fleet", "workstation-1-mc", "host-1")),
        expect_status_apply("cfgd-operator/gateway/packages"),
        expect_status_apply("cfgd-operator/gateway/backups"),
    ]);
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    state.backup_policies.publish(store);
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body("dev-1", "host-1"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(response).await).expect("json body");
    assert_eq!(body["backupSchedules"]["dotfiles"]["schedule"], "daily");
    assert_eq!(body["backupSchedules"]["dotfiles"]["retention"], 7);
    harness.finish().await;
}

#[tokio::test]
#[serial]
async fn checkin_prefers_the_older_policy_when_two_name_one_unit() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let unit = |schedule: &str, retention: u32| {
        serde_json::json!({
            "name": "dotfiles",
            "hostname": "host-1",
            "owner": "cluster",
            "schedule": schedule,
            "retention": retention,
        })
    };
    // Listed newest first, so a gateway answering in list order would send the
    // younger policy's cadence.
    let policies = vec![
        backup_policy(
            "fleet",
            "hourly",
            "2026-06-01T00:00:00Z",
            vec![unit("hourly", 3)],
        ),
        backup_policy(
            "fleet",
            "nightly",
            "2026-01-01T00:00:00Z",
            vec![unit("daily", 7)],
        ),
    ];
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
            .returning_json(&machine_config_list("fleet", "workstation-1-mc", "host-1")),
        expect_status_apply("cfgd-operator/gateway/packages"),
        expect_status_apply("cfgd-operator/gateway/backups"),
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/namespaces/fleet/backuppolicies")
            .returning_json(&backup_policy_list(policies)),
    ]);
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body("dev-1", "host-1"),
        ))
        .await
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(response).await).expect("json body");
    assert_eq!(body["backupSchedules"]["dotfiles"]["schedule"], "daily");
    assert_eq!(body["backupSchedules"]["dotfiles"]["retention"], 7);
    harness.finish().await;
}

/// A read that succeeded and found no policy scheduling this machine is an
/// ANSWER: the field is present and empty, and the device retires the cadences
/// it last held. Without it a machine dropped from every policy would keep
/// running the cluster's old cadence forever.
#[tokio::test]
#[serial]
async fn a_cluster_that_schedules_nothing_answers_with_an_empty_projection() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(checkin_kube_calls());
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body("dev-1", "host-1"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(response).await).expect("json body");
    assert_eq!(
        body["backupSchedules"],
        serde_json::json!({}),
        "a read that found nothing is an answer, not silence: {body}"
    );
    harness.finish().await;
}

/// The gateway could not list MachineConfigs, so it does not know which machine
/// this is, let alone what schedules it. It answers with NO projection at all:
/// the device replaces its whole recorded set from an answer, and an outage
/// must not retire the cadences the cluster still owns.
#[tokio::test]
#[serial]
async fn a_machine_config_list_that_fails_answers_with_no_projection() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
            .returning_server_error(503, "the apiserver is unavailable"),
    ]);
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body("dev-1", "host-1"),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the device's check-in never depends on the cluster being readable"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(response).await).expect("json body");
    assert!(
        body.get("backupSchedules").is_none(),
        "a gateway that could not read the cluster says nothing about it: {body}"
    );
    harness.finish().await;
}

/// The same rule one list down: the machine resolved, its status was written,
/// and only the policy list failed. The gateway still cannot say what the
/// cluster owns, so it answers with no projection.
#[tokio::test]
#[serial]
async fn a_backup_policy_list_that_fails_answers_with_no_projection() {
    unsafe {
        std::env::remove_var("CFGD_API_KEY");
    }
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
            .returning_json(&machine_config_list("fleet", "workstation-1-mc", "host-1")),
        expect_status_apply("cfgd-operator/gateway/packages"),
        expect_status_apply("cfgd-operator/gateway/backups"),
        ExpectedCall::list("/apis/cfgd.io/v1alpha1/namespaces/fleet/backuppolicies")
            .returning_server_error(403, "backuppolicies.cfgd.io is forbidden"),
    ]);
    let (state, _tmp) = crate::gateway::test_state::test_state_with_kube(ctx.client.clone());
    let token = enrolled_device(&state, "dev-1", "host-1").await;

    let response = router_with_state(state)
        .oneshot(post_json_with_bearer(
            "/api/v1/checkin",
            &token,
            checkin_body("dev-1", "host-1"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(response).await).expect("json body");
    assert!(
        body.get("backupSchedules").is_none(),
        "a policy list that failed says nothing about what the cluster owns: {body}"
    );
    harness.finish().await;
}

//! Shared test helpers for gateway integration tests.
//!
//! `GatewayTestApp` wraps the gateway Axum router with an in-memory SQLite
//! DB so tests can `oneshot` requests without a TCP listener.
#![cfg(test)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::gateway::api::SharedState;
use crate::gateway::db::ServerDb;
use crate::gateway::test_state::test_state;

/// Ergonomic wrapper around the gateway `Router` + DB for integration tests.
///
/// Avoids boilerplate in every test: creates a fresh state, wires up the
/// router, and provides convenience methods for common HTTP verbs.
pub(crate) struct GatewayTestApp {
    state: SharedState,
    _tmp: tempfile::TempDir,
}

impl GatewayTestApp {
    /// Create a new test app with a fresh in-memory SQLite DB.
    pub fn new() -> Self {
        let (state, tmp) = test_state();
        Self { state, _tmp: tmp }
    }

    /// Access the underlying DB for state setup or assertions.
    pub fn db(&self) -> &ServerDb {
        &self.state.db
    }

    /// Access the shared state (for direct AppState manipulation in tests).
    #[allow(dead_code)]
    pub fn state(&self) -> &SharedState {
        &self.state
    }

    /// Build the router wired to this app's state.
    fn router(&self) -> Router {
        crate::gateway::api::router(self.state.clone()).with_state(self.state.clone())
    }

    /// Send a GET request and return the response.
    pub async fn get(&self, uri: &str) -> TestResponse {
        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("build GET");
        self.send(req).await
    }

    /// Send a GET request with a Bearer token.
    pub async fn get_with_bearer(&self, uri: &str, token: &str) -> TestResponse {
        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .expect("build GET");
        self.send(req).await
    }

    /// Send a POST request with a JSON body.
    pub async fn post(&self, uri: &str, body: serde_json::Value) -> TestResponse {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .expect("build POST");
        self.send(req).await
    }

    /// Send a POST request with a JSON body and Bearer token.
    #[allow(dead_code)]
    pub async fn post_with_bearer(
        &self,
        uri: &str,
        token: &str,
        body: serde_json::Value,
    ) -> TestResponse {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .expect("build POST");
        self.send(req).await
    }

    /// Send a DELETE request with a Bearer token.
    #[allow(dead_code)]
    pub async fn delete_with_bearer(&self, uri: &str, token: &str) -> TestResponse {
        let req = Request::builder()
            .method("DELETE")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .expect("build DELETE");
        self.send(req).await
    }

    /// Send an arbitrary request through the router.
    async fn send(&self, request: Request<Body>) -> TestResponse {
        let response = self.router().oneshot(request).await.expect("oneshot");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes()
            .to_vec();
        TestResponse { status, body }
    }
}

/// Parsed response from `GatewayTestApp` convenience methods.
pub(crate) struct TestResponse {
    status: StatusCode,
    body: Vec<u8>,
}

impl TestResponse {
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Parse the response body as JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("response body is valid JSON")
    }

    /// Raw body bytes.
    #[allow(dead_code)]
    pub fn body_bytes(&self) -> &[u8] {
        &self.body
    }
}

// -----------------------------------------------------------------------
// Tests exercising GatewayTestApp itself
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::test_helpers::EnvVarGuard;
    use serial_test::serial;

    const TEST_ADMIN_KEY: &str = "gateway-test-app-admin-key";

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn gateway_test_app_enrollment_happy_path() {
        let _g = EnvVarGuard::unset("CFGD_API_KEY");
        let app = GatewayTestApp::new();

        // Provision a bootstrap token for enrollment.
        let bootstrap_token = "boot-token-gta-test";
        let token_hash = cfgd_core::sha256_hex(bootstrap_token.as_bytes());
        app.db()
            .create_bootstrap_token(&token_hash, "testuser", None, "2099-01-01T00:00:00Z")
            .await
            .expect("insert bootstrap token");

        // Enroll a device.
        let resp = app
            .post(
                "/api/v1/enroll",
                serde_json::json!({
                    "deviceId": "gta-device-1",
                    "hostname": "gta-host.test",
                    "os": "linux",
                    "arch": "x86_64",
                    "token": bootstrap_token,
                }),
            )
            .await;

        assert_eq!(resp.status(), StatusCode::CREATED);
        let json = resp.json();
        assert_eq!(json["status"], "enrolled");
        assert_eq!(json["deviceId"], "gta-device-1");
        assert_eq!(json["username"], "testuser");
        assert!(
            json["apiKey"]
                .as_str()
                .unwrap_or("")
                .starts_with("cfgd_dev_"),
            "device API key should have cfgd_dev_ prefix"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn gateway_test_app_device_listing_after_enrollment() {
        let _g = EnvVarGuard::set("CFGD_API_KEY", TEST_ADMIN_KEY);
        let app = GatewayTestApp::new();

        // Provision a separate bootstrap token per device (tokens are single-use).
        let devices = [
            ("dev-list-1", "host-1.test", "boot-token-list-1"),
            ("dev-list-2", "host-2.test", "boot-token-list-2"),
        ];
        for &(_, _, token) in &devices {
            let token_hash = cfgd_core::sha256_hex(token.as_bytes());
            app.db()
                .create_bootstrap_token(&token_hash, "listuser", None, "2099-01-01T00:00:00Z")
                .await
                .expect("insert bootstrap token");
        }

        // Enroll two devices.
        for &(id, host, token) in &devices {
            let resp = app
                .post(
                    "/api/v1/enroll",
                    serde_json::json!({
                        "deviceId": id,
                        "hostname": host,
                        "os": "linux",
                        "arch": "x86_64",
                        "token": token,
                    }),
                )
                .await;
            assert_eq!(
                resp.status(),
                StatusCode::CREATED,
                "enrollment for {id} failed"
            );
        }

        // List devices via the admin API.
        let resp = app.get_with_bearer("/api/v1/devices", TEST_ADMIN_KEY).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let json = resp.json();
        let devices = json.as_array().expect("devices response is a JSON array");
        assert_eq!(devices.len(), 2, "expected 2 enrolled devices");

        // Verify DB directly too.
        let db_devices = app.db().list_devices().await.expect("list_devices");
        assert_eq!(db_devices.len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn gateway_test_app_unauthenticated_device_list_returns_401() {
        let _g = EnvVarGuard::unset("CFGD_API_KEY");
        let app = GatewayTestApp::new();

        let resp = app.get("/api/v1/devices").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

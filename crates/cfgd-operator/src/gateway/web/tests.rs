use super::*;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::middleware;
use axum::routing::get;
use tower::ServiceExt;

use crate::gateway::test_state::test_state;

// --- status_to_class ---

#[test]
fn status_to_class_mapping() {
    let cases: &[(&str, &str)] = &[
        ("healthy", "healthy"),
        ("drifted", "drifted"),
        ("pending-reconcile", "pending-reconcile"),
        ("offline", "offline"),
        ("something-else", "offline"),
    ];
    for (input, expected) in cases {
        assert_eq!(status_to_class(input), *expected, "failed for {input:?}");
    }
}

// --- dashboard ---

#[tokio::test]
async fn dashboard_empty_device_list() {
    let (state, _tmp) = test_state();
    let result = dashboard(State(state)).await;
    assert!(result.is_ok());
    let html = result.unwrap().0;
    assert!(html.contains("cfgd Fleet Dashboard"));
    assert!(html.contains("Configuration management control plane"));
    assert!(html.contains("No devices registered yet"));
    assert!(html.contains("Total Devices"));
    // All stats should be 0
    assert!(html.contains(r#"<div class="value">0</div>"#));
    // Nav links present
    assert!(
        html.contains(r#"<a href="/">Devices</a>"#)
            || html.contains(r#"<a href="/" class="active">Devices</a>"#)
    );
    assert!(html.contains(r#"<a href="/events">Events</a>"#));
}

#[tokio::test]
async fn dashboard_with_devices_shows_device_rows() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "workstation-1", "linux", "x86_64", "abc123", None)
        .await
        .expect("register device");
    state
        .db
        .register_device("dev-2", "laptop-2", "darwin", "aarch64", "def456", None)
        .await
        .expect("register device");
    let result = dashboard(State(state)).await;
    assert!(result.is_ok());
    let html = result.unwrap().0;
    // Device names appear in table
    assert!(html.contains("workstation-1"));
    assert!(html.contains("laptop-2"));
    // Device IDs appear
    assert!(html.contains("dev-1"));
    assert!(html.contains("dev-2"));
    // OS/arch values appear
    assert!(html.contains("linux"));
    assert!(html.contains("darwin"));
    assert!(html.contains("x86_64"));
    assert!(html.contains("aarch64"));
    // Config hashes appear
    assert!(html.contains("abc123"));
    assert!(html.contains("def456"));
    // Should have a table, not the "no devices" message
    assert!(html.contains("<table"));
    assert!(!html.contains("No devices registered yet"));
    // Stats: 2 total, 2 healthy (register_device sets Healthy)
    assert!(html.contains(r#"<div class="value">2</div>"#));
}

#[tokio::test]
async fn dashboard_stat_cards_reflect_device_statuses() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("d1", "host1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    state
        .db
        .register_device("d2", "host2", "linux", "x86_64", "h2", None)
        .await
        .expect("register");
    // Cause drift on d2
    state
        .db
        .record_drift_event("d2", "field changed")
        .await
        .expect("drift");
    let result = dashboard(State(state)).await;
    let html = result.unwrap().0;
    // Total = 2, Healthy = 1, Drifted = 1, Offline = 0
    // The stat cards are: total, healthy, drifted, offline in that order
    // We check the stat card structure
    assert!(html.contains(r#"<div class="stat-card total">"#));
    assert!(html.contains(r#"<div class="stat-card healthy">"#));
    assert!(html.contains(r#"<div class="stat-card drifted">"#));
    assert!(html.contains(r#"<div class="stat-card offline">"#));
    // Check the status badge on the drifted device
    assert!(html.contains(r#"<span class="status drifted">drifted</span>"#));
    // And the healthy device
    assert!(html.contains(r#"<span class="status healthy">healthy</span>"#));
}

#[tokio::test]
async fn dashboard_escapes_html_in_device_fields() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device(
            "dev-<xss>",
            "host<script>",
            "linux&os",
            "x86\"64",
            "hash'val",
            None,
        )
        .await
        .expect("register");
    let result = dashboard(State(state)).await;
    let html = result.unwrap().0;
    // XSS characters should be escaped
    assert!(html.contains("dev-&lt;xss&gt;"));
    assert!(html.contains("host&lt;script&gt;"));
    assert!(html.contains("linux&amp;os"));
    assert!(html.contains("x86&quot;64"));
    assert!(html.contains("hash&apos;val"));
    // Raw dangerous characters should NOT appear
    assert!(!html.contains("<xss>"));
    assert!(!html.contains("<script>"));
}

// --- device_detail ---

#[tokio::test]
async fn device_detail_existing_device() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device(
            "dev-42",
            "my-workstation",
            "linux",
            "x86_64",
            "abc123",
            None,
        )
        .await
        .expect("register");
    let result = device_detail(State(state), Path("dev-42".to_string())).await;
    assert!(result.is_ok());
    let html = result.unwrap().0;
    // Device details in the page
    assert!(html.contains("my-workstation"));
    assert!(html.contains("dev-42"));
    assert!(html.contains("linux"));
    assert!(html.contains("x86_64"));
    assert!(html.contains("abc123"));
    // Breadcrumb navigation
    assert!(html.contains("Fleet Dashboard"));
    // No drift/checkin events by default
    assert!(html.contains("No drift events recorded"));
    assert!(html.contains("No check-in events recorded"));
    // Push config section
    assert!(html.contains("Push Configuration"));
    // Actions section
    assert!(html.contains("Force Reconcile"));
}

#[tokio::test]
async fn device_detail_not_found_returns_error() {
    let (state, _tmp) = test_state();
    let result = device_detail(State(state), Path("nonexistent".to_string())).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, GatewayError::NotFound(_)));
}

#[tokio::test]
async fn device_detail_with_drift_events() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    state
        .db
        .record_drift_event(
            "dev-1",
            r#"[{"field":"packages","expected":"vim","actual":"missing"}]"#,
        )
        .await
        .expect("drift");
    let result = device_detail(State(state), Path("dev-1".to_string())).await;
    let html = result.unwrap().0;
    // Should not say "no drift events"
    assert!(!html.contains("No drift events recorded"));
    // Should contain parsed drift detail fields
    assert!(html.contains("packages"));
    assert!(html.contains("vim"));
    assert!(html.contains("missing"));
    // Drift events count
    assert!(html.contains("Drift Events (1)"));
}

#[tokio::test]
async fn device_detail_with_checkin_events() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    state
        .db
        .record_checkin("dev-1", "hash-abc", false)
        .await
        .expect("checkin");
    state
        .db
        .record_checkin("dev-1", "hash-def", true)
        .await
        .expect("checkin changed");
    let result = device_detail(State(state), Path("dev-1".to_string())).await;
    let html = result.unwrap().0;
    // Should not say "no check-in events"
    assert!(!html.contains("No check-in events recorded"));
    // Config hashes appear
    assert!(html.contains("hash-abc"));
    assert!(html.contains("hash-def"));
    // Changed badge for the config-changed checkin
    assert!(html.contains(r#"<span class="status drifted">changed</span>"#));
    // OK badge for the unchanged checkin
    assert!(html.contains(r#"<span class="status healthy">ok</span>"#));
    // Checkin count
    assert!(html.contains("Check-in History (2)"));
}

#[tokio::test]
async fn device_detail_with_desired_config() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    let config = serde_json::json!({"packages": ["vim", "git"]});
    state
        .db
        .set_device_config("dev-1", &config)
        .await
        .expect("set config");
    let result = device_detail(State(state), Path("dev-1".to_string())).await;
    let html = result.unwrap().0;
    // Desired config section should show the formatted JSON
    assert!(html.contains("Desired Configuration"));
    assert!(html.contains("vim"));
    assert!(html.contains("git"));
    // Should NOT contain the "no desired config" message
    assert!(!html.contains("No desired config set"));
}

#[tokio::test]
async fn device_detail_without_desired_config() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    let result = device_detail(State(state), Path("dev-1".to_string())).await;
    let html = result.unwrap().0;
    assert!(html.contains("No desired config set"));
}

#[tokio::test]
async fn device_detail_escapes_html_in_fields() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device(
            "dev-<id>",
            "host<name>",
            "os&type",
            "arch\"val",
            "hash'v",
            None,
        )
        .await
        .expect("register");
    let result = device_detail(State(state), Path("dev-<id>".to_string())).await;
    let html = result.unwrap().0;
    assert!(html.contains("dev-&lt;id&gt;"));
    assert!(html.contains("host&lt;name&gt;"));
    assert!(html.contains("os&amp;type"));
    assert!(!html.contains("host<name>"));
}

// --- fleet_events ---

#[tokio::test]
async fn fleet_events_empty() {
    let (state, _tmp) = test_state();
    let result = fleet_events(State(state)).await;
    assert!(result.is_ok());
    let html = result.unwrap().0;
    assert!(html.contains("cfgd Fleet Events"));
    assert!(html.contains("No events recorded yet"));
    assert!(html.contains("Events</a>"));
    assert!(html.contains("Devices</a>"));
}

#[tokio::test]
async fn fleet_events_with_checkin_events() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    state
        .db
        .record_checkin("dev-1", "hash-abc", false)
        .await
        .expect("checkin");
    let result = fleet_events(State(state)).await;
    let html = result.unwrap().0;
    // Should have a table, not the empty message
    assert!(!html.contains("No events recorded yet"));
    assert!(html.contains("<table"));
    // Device link
    assert!(html.contains(r#"<a href="/devices/dev-1">dev-1</a>"#));
    // Checkin event type
    assert!(html.contains("checkin"));
    // Config hash in summary
    assert!(html.contains("hash-abc"));
}

#[tokio::test]
async fn fleet_events_with_drift_events() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    state
        .db
        .record_drift_event(
            "dev-1",
            r#"[{"field":"sysctl","expected":"1","actual":"0"}]"#,
        )
        .await
        .expect("drift");
    let result = fleet_events(State(state)).await;
    let html = result.unwrap().0;
    assert!(html.contains("drift"));
    assert!(html.contains("sysctl"));
    // Drift events get the "drifted" status class
    assert!(html.contains(r#"class="status drifted"#));
    // Device ID link
    assert!(html.contains("dev-1"));
}

#[tokio::test]
async fn fleet_events_with_config_changed_events() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    state
        .db
        .record_checkin("dev-1", "new-hash", true)
        .await
        .expect("changed checkin");
    let result = fleet_events(State(state)).await;
    let html = result.unwrap().0;
    assert!(html.contains("config-changed"));
    // Config-changed events also get the "drifted" type class
    assert!(html.contains(r#"class="status drifted"#));
}

#[tokio::test]
async fn fleet_events_device_filter_dropdown_populated() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-a", "host-a", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    state
        .db
        .register_device("dev-b", "host-b", "linux", "x86_64", "h2", None)
        .await
        .expect("register");
    state
        .db
        .record_checkin("dev-a", "h1", false)
        .await
        .expect("checkin");
    state
        .db
        .record_checkin("dev-b", "h2", false)
        .await
        .expect("checkin");
    let result = fleet_events(State(state)).await;
    let html = result.unwrap().0;
    // Device filter dropdown should contain both device IDs as options
    assert!(html.contains(r#"<option value="dev-a">dev-a</option>"#));
    assert!(html.contains(r#"<option value="dev-b">dev-b</option>"#));
}

#[tokio::test]
async fn fleet_events_escapes_html_in_device_ids() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-<x>", "host", "linux", "x86_64", "h", None)
        .await
        .expect("register");
    state
        .db
        .record_checkin("dev-<x>", "h", false)
        .await
        .expect("checkin");
    let result = fleet_events(State(state)).await;
    let html = result.unwrap().0;
    assert!(html.contains("dev-&lt;x&gt;"));
    assert!(!html.contains("dev-<x>"));
}

// --- fleet_events SSE badge ---

#[tokio::test]
async fn fleet_events_contains_sse_badge() {
    let (state, _tmp) = test_state();
    let result = fleet_events(State(state)).await;
    let html = result.unwrap().0;
    assert!(html.contains("sse-badge"));
    assert!(html.contains("EventSource"));
    assert!(html.contains("/api/v1/events/stream"));
}

// --- web_auth_middleware ---

/// Build a minimal router with the auth middleware bound to `state` for testing.
fn auth_test_app(state: SharedState) -> axum::Router {
    axum::Router::new()
        .route("/test", get(|| async { "ok" }))
        .route_layer(middleware::from_fn_with_state(state, web_auth_middleware))
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_allows_when_no_api_key_set() {
    // Ensure CFGD_API_KEY is not set
    unsafe { std::env::remove_var("CFGD_API_KEY") };

    let (state, _tmp) = test_state();
    let app = auth_test_app(state);
    let resp = app
        .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_rejects_without_credentials_when_key_set() {
    unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

    let (state, _tmp) = test_state();
    let app = auth_test_app(state);
    let resp = app
        .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    unsafe { std::env::remove_var("CFGD_API_KEY") };
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_accepts_valid_bearer_token() {
    unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

    let (state, _tmp) = test_state();
    let app = auth_test_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/test")
                .header("Authorization", "Bearer test-secret-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    unsafe { std::env::remove_var("CFGD_API_KEY") };
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_rejects_wrong_bearer_token() {
    unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

    let (state, _tmp) = test_state();
    let app = auth_test_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/test")
                .header("Authorization", "Bearer wrong-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    unsafe { std::env::remove_var("CFGD_API_KEY") };
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_accepts_valid_session_cookie() {
    unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

    let (state, _tmp) = test_state();
    state.web_sessions.insert("sess-registered", SESSION_TTL);
    let app = auth_test_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/test")
                .header(header::COOKIE, "cfgd_session=sess-registered")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    unsafe { std::env::remove_var("CFGD_API_KEY") };
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_rejects_unknown_session_cookie() {
    unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

    let (state, _tmp) = test_state();
    let app = auth_test_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/test")
                .header(header::COOKIE, "cfgd_session=not-a-real-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    unsafe { std::env::remove_var("CFGD_API_KEY") };
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_rejects_raw_api_key_as_session_cookie() {
    // Regression: we must NOT accept the raw CFGD_API_KEY as a cfgd_session value.
    unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

    let (state, _tmp) = test_state();
    let app = auth_test_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/test")
                .header(header::COOKIE, "cfgd_session=test-secret-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    unsafe { std::env::remove_var("CFGD_API_KEY") };
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_accepts_cookie_among_multiple() {
    unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

    let (state, _tmp) = test_state();
    state.web_sessions.insert("sess-abc", SESSION_TTL);
    let app = auth_test_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/test")
                .header(
                    header::COOKIE,
                    "other=value; cfgd_session=sess-abc; another=thing",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    unsafe { std::env::remove_var("CFGD_API_KEY") };
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_token_query_param_redirects_and_sets_cookie() {
    unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

    let (state, _tmp) = test_state();
    let app = auth_test_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/test?token=test-secret-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Should redirect (303 See Other)
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    // Location header should strip the token query param
    let location = resp
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(location, "/test");
    // Set-Cookie header should contain the session cookie with required flags,
    // and the cookie value must NOT be the admin API key.
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(set_cookie.starts_with("cfgd_session=cfgd_ws_"));
    assert!(!set_cookie.contains("test-secret-key"));
    assert!(set_cookie.contains("Secure"));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Max-Age=86400"));

    unsafe { std::env::remove_var("CFGD_API_KEY") };
}

#[tokio::test]
#[serial_test::serial]
async fn auth_middleware_wrong_token_query_param_rejected() {
    unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

    let (state, _tmp) = test_state();
    let app = auth_test_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/test?token=wrong-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    unsafe { std::env::remove_var("CFGD_API_KEY") };
}

// --- COMMON_STYLES ---

#[test]
fn common_styles_contains_expected_classes() {
    assert!(COMMON_STYLES.contains(".status.healthy"));
    assert!(COMMON_STYLES.contains(".status.drifted"));
    assert!(COMMON_STYLES.contains(".status.offline"));
    assert!(COMMON_STYLES.contains(".status.pending-reconcile"));
    assert!(COMMON_STYLES.contains("table"));
    assert!(COMMON_STYLES.contains(".container"));
    assert!(COMMON_STYLES.contains(".nav"));
    assert!(COMMON_STYLES.contains(".empty"));
}

// --- router ---

#[tokio::test]
#[serial_test::serial]
async fn router_wires_routes() {
    let (state, _tmp) = test_state();
    let app = router(state.clone()).with_state(state);

    // Ensure CFGD_API_KEY is not set so auth middleware lets us through
    unsafe { std::env::remove_var("CFGD_API_KEY") };

    // Dashboard route
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Events route
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

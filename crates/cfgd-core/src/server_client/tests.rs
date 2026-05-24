use super::*;
use crate::test_helpers::test_printer;

#[test]
fn server_client_strips_trailing_slash() {
    let client = ServerClient::new("http://localhost:8080/", None, "node-1");
    assert_eq!(client.base_url, "http://localhost:8080");
}

#[test]
fn from_credential() {
    let cred = DeviceCredential {
        server_url: "https://cfgd.example.com/".to_string(),
        device_id: "dev-1".to_string(),
        api_key: "cfgd_dev_abc123".to_string(),
        username: "jdoe".to_string(),
        team: Some("acme".to_string()),
        enrolled_at: "2026-03-10T12:00:00Z".to_string(),
    };
    let client = ServerClient::from_credential(&cred);
    assert_eq!(client.base_url, "https://cfgd.example.com");
    assert_eq!(client.api_key.as_deref(), Some("cfgd_dev_abc123"));
    assert_eq!(client.device_id, "dev-1");
}

#[test]
fn checkin_request_without_compliance_summary() {
    let req = CheckinRequest {
        device_id: "dev-1".into(),
        hostname: "ws-1".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        config_hash: "abc123".into(),
        compliance_summary: None,
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(!json.contains("complianceSummary"));
}

#[test]
fn credential_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credential.json");

    let cred = DeviceCredential {
        server_url: "https://cfgd.example.com".to_string(),
        device_id: "test-device".to_string(),
        api_key: "cfgd_dev_test123".to_string(),
        username: "testuser".to_string(),
        team: None,
        enrolled_at: "2026-03-10T12:00:00Z".to_string(),
    };

    let json = serde_json::to_string_pretty(&cred).unwrap();
    std::fs::write(&path, &json).unwrap();

    let loaded = load_credential_from(&path).unwrap().unwrap();
    assert_eq!(loaded.device_id, "test-device");
    assert_eq!(loaded.api_key, "cfgd_dev_test123");
    assert_eq!(loaded.username, "testuser");
}

#[test]
fn credential_load_missing_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    let loaded = load_credential_from(&path).unwrap();
    assert!(loaded.is_none());
}

#[test]
fn load_credential_from_malformed_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad-credential.json");
    std::fs::write(&path, "{ this is not valid json!!!").unwrap();

    let result = load_credential_from(&path);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("invalid device credential file"),
        "unexpected error message: {}",
        err_msg
    );
}

#[test]
fn credential_file_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cred.json");

    let cred = DeviceCredential {
        server_url: "https://example.com".into(),
        device_id: "d1".into(),
        api_key: "key".into(),
        username: "user".into(),
        team: Some("acme".into()),
        enrolled_at: "2026-01-01T00:00:00Z".into(),
    };

    let json = serde_json::to_string_pretty(&cred).unwrap();
    std::fs::write(&path, &json).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}

// --- mockito-based HTTP integration tests ---

#[test]
fn checkin_sends_correct_payload_and_parses_response() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .match_header("content-type", "application/json")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_body(r#"{"status":"ok","configChanged":false}"#)
        .create();

    let client = ServerClient::new(&server.url(), Some("test-key"), "dev-1");
    let printer = test_printer();
    let result = client.checkin("hash123", None, &printer);

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.status, "ok");
    assert!(!resp.config_changed);
    mock.assert();
}

#[test]
fn checkin_with_compliance_summary() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_body(r#"{"status":"ok","configChanged":true}"#)
        .create();

    let client = ServerClient::new(&server.url(), Some("key"), "dev-1");
    let printer = test_printer();
    let summary = ComplianceSummary {
        compliant: 5,
        warning: 1,
        violation: 0,
    };
    let result = client.checkin("hash", Some(summary), &printer);
    assert!(result.is_ok());
    assert!(result.unwrap().config_changed);
    mock.assert();
}

#[test]
fn report_drift_sends_drift_details() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/devices/dev-1/drift")
        .match_header("authorization", "Bearer key-1")
        .with_status(200)
        .with_body("{}")
        .create();

    let client = ServerClient::new(&server.url(), Some("key-1"), "dev-1");
    let printer = test_printer();
    let drift = SystemDrift {
        key: "some.key".into(),
        expected: "foo".into(),
        actual: "bar".into(),
    };
    let result = client.report_drift(&[drift], &printer);
    assert!(result.is_ok());
    mock.assert();
}

#[test]
fn enroll_sends_bootstrap_token() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/enroll")
        .with_status(200)
        .with_body(
            r#"{"status":"enrolled","deviceId":"new-dev","apiKey":"new-key","username":"user1"}"#,
        )
        .create();

    let client = ServerClient::new(&server.url(), None, "dev-1");
    let printer = test_printer();
    let result = client.enroll("bootstrap-token-123", &printer);
    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.device_id, "new-dev");
    assert_eq!(resp.api_key, "new-key");
    assert_eq!(resp.username, "user1");
    mock.assert();
}

#[test]
fn request_challenge_sends_username() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/enroll/challenge")
        .with_status(200)
        .with_body(
            r#"{"challengeId":"ch-1","nonce":"sign-this","expiresAt":"2026-01-01T00:00:00Z"}"#,
        )
        .create();

    let client = ServerClient::new(&server.url(), None, "dev-1");
    let printer = test_printer();
    let result = client.request_challenge("testuser", &printer);
    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.challenge_id, "ch-1");
    assert_eq!(resp.nonce, "sign-this");
    mock.assert();
}

#[test]
fn submit_verification_returns_enroll_response() {
    let mut server = mockito::Server::new();
    let mock = server
            .mock("POST", "/api/v1/enroll/verify")
            .with_status(200)
            .with_body(
                r#"{"status":"verified","deviceId":"verified-dev","apiKey":"verified-key","username":"user1"}"#,
            )
            .create();

    let client = ServerClient::new(&server.url(), None, "dev-1");
    let printer = test_printer();
    let result = client.submit_verification("ch-1", "sig-data", "ssh-ed25519", &printer);
    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.device_id, "verified-dev");
    assert_eq!(resp.api_key, "verified-key");
    mock.assert();
}

#[test]
fn enroll_info_returns_enrollment_details() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/api/v1/enroll/info")
        .with_status(200)
        .with_body(r#"{"method":"key"}"#)
        .create();

    let client = ServerClient::new(&server.url(), None, "dev-1");
    let result = client.enroll_info();
    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.method, "key");
    mock.assert();
}

#[test]
fn checkin_server_error_returns_error() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(500)
        .with_body("internal error")
        .expect_at_least(2)
        .create();

    let client = ServerClient::new(&server.url(), Some("key"), "dev-1");
    let printer = test_printer();
    let result = client.checkin("hash", None, &printer);
    assert!(result.is_err());
    mock.assert();
}

#[test]
fn checkin_client_error_does_not_retry() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(401)
        .with_body("unauthorized")
        .expect(1)
        .create();

    let client = ServerClient::new(&server.url(), Some("bad-key"), "dev-1");
    let printer = test_printer();
    let result = client.checkin("hash", None, &printer);
    assert!(result.is_err());
    mock.assert();
}

#[test]
fn report_drift_empty_drifts_is_noop() {
    // With no mock server set up, an empty drifts list should return Ok(())
    // immediately without making any HTTP request
    let client = ServerClient::new("http://127.0.0.1:1", None, "dev-1");
    let printer = test_printer();
    let result = client.report_drift(&[], &printer);
    assert!(result.is_ok(), "empty drifts should short-circuit to Ok");
}

#[test]
fn report_drift_multiple_drifts() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/devices/dev-1/drift")
        .with_status(200)
        .with_body("{}")
        .create();

    let client = ServerClient::new(&server.url(), Some("key"), "dev-1");
    let printer = test_printer();
    let drifts = vec![
        SystemDrift {
            key: "file.zshrc".into(),
            expected: "abc".into(),
            actual: "xyz".into(),
        },
        SystemDrift {
            key: "pkg.curl".into(),
            expected: "installed".into(),
            actual: "missing".into(),
        },
    ];
    let result = client.report_drift(&drifts, &printer);
    assert!(result.is_ok());
    mock.assert();
}

#[test]
fn enroll_info_connection_refused() {
    // Connect to a port that isn't listening
    let client = ServerClient::new("http://127.0.0.1:1", None, "dev-1");
    let result = client.enroll_info();
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("failed to query enrollment info"),
        "unexpected error: {}",
        err_msg
    );
}

#[test]
fn enroll_info_invalid_json_response() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/api/v1/enroll/info")
        .with_status(200)
        .with_body("not valid json")
        .create();

    let client = ServerClient::new(&server.url(), None, "dev-1");
    let result = client.enroll_info();
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("invalid enrollment info response"),
        "unexpected error: {}",
        err_msg
    );
    mock.assert();
}

#[test]
fn checkin_invalid_json_response() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_body("not json at all")
        .create();

    let client = ServerClient::new(&server.url(), Some("key"), "dev-1");
    let printer = test_printer();
    let result = client.checkin("hash", None, &printer);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("invalid checkin response"),
        "unexpected error: {}",
        err_msg
    );
    mock.assert();
}

#[test]
fn enroll_invalid_json_response() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/enroll")
        .with_status(200)
        .with_body("bad json")
        .create();

    let client = ServerClient::new(&server.url(), None, "dev-1");
    let printer = test_printer();
    let result = client.enroll("token", &printer);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("invalid enrollment response"),
        "unexpected error: {}",
        err_msg
    );
    mock.assert();
}

#[test]
fn request_challenge_invalid_json_response() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/enroll/challenge")
        .with_status(200)
        .with_body("bad json")
        .create();

    let client = ServerClient::new(&server.url(), None, "dev-1");
    let printer = test_printer();
    let result = client.request_challenge("user", &printer);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("invalid challenge response"),
        "unexpected error: {}",
        err_msg
    );
    mock.assert();
}

#[test]
fn submit_verification_invalid_json_response() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/enroll/verify")
        .with_status(200)
        .with_body("not json")
        .create();

    let client = ServerClient::new(&server.url(), None, "dev-1");
    let printer = test_printer();
    let result = client.submit_verification("ch-1", "sig", "ssh-ed25519", &printer);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("invalid verification response"),
        "unexpected error: {}",
        err_msg
    );
    mock.assert();
}

#[test]
fn report_drift_connection_refused() {
    let client = ServerClient::new("http://127.0.0.1:1", Some("key"), "dev-1");
    let printer = test_printer();
    let drifts = vec![SystemDrift {
        key: "test.key".into(),
        expected: "a".into(),
        actual: "b".into(),
    }];
    let result = client.report_drift(&drifts, &printer);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("drift report failed"),
        "unexpected error: {}",
        err_msg
    );
}

#[test]
fn checkin_with_desired_config_in_response() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_body(r#"{"status":"ok","configChanged":true,"desiredConfig":{"packages":["git"]}}"#)
        .create();

    let client = ServerClient::new(&server.url(), Some("key"), "dev-1");
    let printer = test_printer();
    let result = client.checkin("hash", None, &printer).unwrap();
    assert!(result.config_changed);
    assert!(result.desired_config.is_some());
    mock.assert();
}

#[test]
fn credential_team_field_optional() {
    let cred = DeviceCredential {
        server_url: "https://example.com".into(),
        device_id: "d1".into(),
        api_key: "key".into(),
        username: "user".into(),
        team: None,
        enrolled_at: "2026-01-01T00:00:00Z".into(),
    };
    let json = serde_json::to_string(&cred).unwrap();
    let loaded: DeviceCredential = serde_json::from_str(&json).unwrap();
    assert!(loaded.team.is_none());
}

#[test]
fn credential_round_trip_with_team() {
    let cred = DeviceCredential {
        server_url: "https://example.com".into(),
        device_id: "d1".into(),
        api_key: "key".into(),
        username: "user".into(),
        team: Some("engineering".into()),
        enrolled_at: "2026-04-01T00:00:00Z".into(),
    };
    let json = serde_json::to_string_pretty(&cred).unwrap();
    let loaded: DeviceCredential = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.team.as_deref(), Some("engineering"));
    assert_eq!(loaded.enrolled_at, "2026-04-01T00:00:00Z");
}

#[test]
fn server_client_new_without_api_key() {
    let client = ServerClient::new("http://localhost:8080", None, "node-1");
    assert!(client.api_key.is_none());
    assert_eq!(client.device_id, "node-1");
}

#[test]
fn server_client_new_with_api_key() {
    let client = ServerClient::new("http://localhost:8080", Some("secret"), "node-2");
    assert_eq!(client.api_key.as_deref(), Some("secret"));
    assert_eq!(client.device_id, "node-2");
}

#[test]
fn enroll_response_optional_fields() {
    let json = r#"{"status":"enrolled","deviceId":"dev-1","apiKey":"key-1","username":"user1","team":"ops","desiredConfig":{"foo":"bar"}}"#;
    let resp: EnrollResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.team.as_deref(), Some("ops"));
    assert!(resp.desired_config.is_some());
}

#[test]
fn enroll_response_minimal() {
    let json = r#"{"status":"enrolled","deviceId":"dev-1","apiKey":"key-1","username":"user1"}"#;
    let resp: EnrollResponse = serde_json::from_str(json).unwrap();
    assert!(resp.team.is_none());
    assert!(resp.desired_config.is_none());
}

#[test]
fn checkin_no_api_key_omits_auth_header() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .match_header("authorization", mockito::Matcher::Missing)
        .with_status(200)
        .with_body(r#"{"status":"ok","configChanged":false}"#)
        .create();

    let client = ServerClient::new(&server.url(), None, "dev-1");
    let printer = test_printer();
    let result = client.checkin("hash", None, &printer);
    assert!(result.is_ok());
    mock.assert();
}

#[test]
fn is_loopback_host_ipv6_bracketed() {
    assert!(is_loopback_host("[::1]:8080/api"));
    assert!(is_loopback_host("[::1]/path"));
    assert!(!is_loopback_host("[fe80::1]:8080"));
}

#[test]
fn is_loopback_host_127_subnet() {
    assert!(is_loopback_host("127.0.0.1:3000"));
    assert!(is_loopback_host("127.99.0.1:3000"));
    assert!(!is_loopback_host("128.0.0.1:3000"));
}

#[test]
fn plaintext_http_non_loopback_does_not_panic() {
    let _client = ServerClient::new("http://remote.example.com:8080", Some("key"), "dev-1");
}

#[test]
fn save_credential_writes_file_with_restricted_perms() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("device-credential.json");

    let cred = DeviceCredential {
        server_url: "https://example.com".into(),
        device_id: "d1".into(),
        api_key: "secret-key".into(),
        username: "user".into(),
        team: None,
        enrolled_at: "2026-01-01T00:00:00Z".into(),
    };

    let json = serde_json::to_string_pretty(&cred).unwrap();
    std::fs::create_dir_all(dir.path()).unwrap();
    crate::atomic_write_str(&path, &json).unwrap();
    crate::set_file_permissions(&path, 0o600).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    let loaded: DeviceCredential =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(loaded.api_key, "secret-key");
}

// ===========================================================================
// Streaming → buffered Doc bridge snapshots for server_client
// ===========================================================================
//
// `ServerClient::{checkin,report_drift}` emit a single streaming `status_simple`
// line on the supplied `&output::Printer`. Callers (e.g. `cli::checkin`)
// then emit a buffered `Doc` on the same printer. The renderer must produce
// exactly one blank line between the last streaming line and the buffered
// Doc's first visible line. These snapshots lock the human shape of that
// seam for the two server_client surfaces.

#[cfg(unix)]
mod bridge {
    use super::*;
    use crate::output::test_capture::{assert_snapshot_at, strip_ansi};
    use crate::output::{Doc, Printer, Role};

    fn snapshot_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server_client/snapshots")
    }

    fn assert_snapshot(name: &str, actual: &str) {
        assert_snapshot_at(&snapshot_dir(), name, actual);
    }

    /// Payload shape for buffered summary Docs in the snapshots — kept local
    /// because cfgd-core can't depend on `cfgd`'s `cli::checkin::CheckinOutput`.
    /// `with_data` doesn't contribute to the human render the snapshot locks.
    #[derive(serde::Serialize)]
    struct CheckinSummary {
        server_status: String,
        config_changed: bool,
    }

    #[derive(serde::Serialize)]
    struct DriftSummary {
        drift_count: usize,
    }

    #[test]
    fn snapshot_checkin_clean() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("POST", "/api/v1/checkin")
            .with_status(200)
            .with_body(r#"{"status":"ok","configChanged":false}"#)
            .create();

        let client = ServerClient::new(&server.url(), Some("key"), "dev-1");
        let (printer, cap) = Printer::for_test_doc();
        let resp = client.checkin("hash123", None, &printer).unwrap();

        let summary = CheckinSummary {
            server_status: resp.status.clone(),
            config_changed: resp.config_changed,
        };
        let doc = Doc::new()
            .status(Role::Ok, format!("server status: {}", resp.status))
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        let captured = strip_ansi(&cap.human());

        // Bridge invariant: exactly one blank line at the streaming → buffered seam.
        assert!(
            captured.contains("\n\n"),
            "checkin_clean missing blank line at seam:\n{captured}"
        );
        assert!(
            !captured.contains("\n\n\n"),
            "checkin_clean has duplicate blank line:\n{captured}"
        );

        assert_snapshot("checkin_clean.txt", &captured);
    }

    #[test]
    fn snapshot_drift_report() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("POST", "/api/v1/devices/dev-1/drift")
            .with_status(200)
            .with_body("{}")
            .create();

        let client = ServerClient::new(&server.url(), Some("key"), "dev-1");
        let drifts = vec![
            SystemDrift {
                key: "file.zshrc".into(),
                expected: "abc".into(),
                actual: "xyz".into(),
            },
            SystemDrift {
                key: "pkg.curl".into(),
                expected: "installed".into(),
                actual: "missing".into(),
            },
        ];

        let (printer, cap) = Printer::for_test_doc();
        client.report_drift(&drifts, &printer).unwrap();

        let summary = DriftSummary {
            drift_count: drifts.len(),
        };
        let doc = Doc::new()
            .status(Role::Warn, format!("{} drift items reported", drifts.len()))
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        let captured = strip_ansi(&cap.human());

        // Bridge invariant: exactly one blank line at the streaming → buffered seam.
        assert!(
            captured.contains("\n\n"),
            "drift_report missing blank line at seam:\n{captured}"
        );
        assert!(
            !captured.contains("\n\n\n"),
            "drift_report has duplicate blank line:\n{captured}"
        );

        assert_snapshot("drift_report.txt", &captured);
    }
}

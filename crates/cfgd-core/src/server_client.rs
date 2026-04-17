use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::compliance::ComplianceSummary;
use crate::errors::{CfgdError, Result};
use crate::output::Printer;
use crate::providers::SystemDrift;

/// Client for communicating with the device gateway.
pub struct ServerClient {
    base_url: String,
    api_key: Option<String>,
    device_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckinRequest {
    device_id: String,
    hostname: String,
    os: String,
    arch: String,
    config_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    compliance_summary: Option<ComplianceSummary>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResponse {
    pub status: String,
    pub config_changed: bool,
    #[serde(default)]
    pub desired_config: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DriftReport {
    details: Vec<DriftDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DriftDetail {
    field: String,
    expected: String,
    actual: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnrollRequest {
    token: String,
    device_id: String,
    hostname: String,
    os: String,
    arch: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollResponse {
    pub status: String,
    pub device_id: String,
    pub api_key: String,
    pub username: String,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub desired_config: Option<serde_json::Value>,
}

// --- Key-based enrollment types ---

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeRequest {
    username: String,
    device_id: String,
    hostname: String,
    os: String,
    arch: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeResponse {
    pub challenge_id: String,
    pub nonce: String,
    pub expires_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyRequest {
    challenge_id: String,
    signature: String,
    key_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollInfoResponse {
    pub method: String,
}

/// Stored device credential — saved locally after enrollment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCredential {
    pub server_url: String,
    pub device_id: String,
    pub api_key: String,
    pub username: String,
    #[serde(default)]
    pub team: Option<String>,
    pub enrolled_at: String,
}

/// Returns true when `authority` (the part after `http://` — may include port
/// and path) points at localhost, `127.0.0.1/8`, or `::1`. Used to suppress
/// the plaintext-scheme warning for loopback dev setups.
fn is_loopback_host(authority: &str) -> bool {
    // Strip any path suffix so we compare just the host[:port].
    let host_port = authority.split('/').next().unwrap_or(authority);
    // Handle bracketed IPv6 host + port: `[::1]:8080`.
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        host_port.rsplit_once(':').map_or(host_port, |(h, _)| h)
    };
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.starts_with("127.")
}

impl ServerClient {
    pub fn new(base_url: &str, api_key: Option<&str>, device_id: &str) -> Self {
        // Plaintext HTTP to a non-loopback host leaks the device API key to
        // everything on the network path. Tests hit `http://localhost:...` and
        // `http://127.0.0.1:...` constantly, so only warn on genuinely remote
        // plaintext URLs.
        if let Some(rest) = base_url.strip_prefix("http://")
            && !is_loopback_host(rest)
        {
            tracing::warn!(
                base_url = %base_url,
                "device gateway URL uses plaintext http:// — the device API key travels in the Authorization header and will be visible to anything on the network path. Use https:// in production."
            );
        }
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(String::from),
            device_id: device_id.to_string(),
        }
    }

    fn agent(&self) -> ureq::Agent {
        crate::http::http_agent(crate::http::HTTP_API_TIMEOUT)
    }

    fn build_request(&self, method: &str, path: &str) -> ureq::Request {
        let url = format!("{}{}", self.base_url, path);
        let agent = self.agent();
        let mut req = match method {
            "POST" => agent.post(&url),
            _ => agent.get(&url),
        };

        if let Some(ref key) = self.api_key {
            req = req.set("Authorization", &format!("Bearer {}", key));
        }

        req
    }

    /// Send a POST request with exponential backoff on network failures.
    fn post_with_retry(&self, path: &str, body_json: &str) -> std::result::Result<String, String> {
        let retry = crate::retry::BackoffConfig::DEFAULT_TRANSIENT;
        let mut last_err = String::new();
        for attempt in 0..retry.max_attempts {
            let delay = retry.delay_for_attempt(attempt);
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }

            match self
                .build_request("POST", path)
                .set("Content-Type", "application/json")
                .send_string(body_json)
            {
                Ok(resp) => match resp.into_string() {
                    Ok(body) => return Ok(body),
                    Err(e) => {
                        last_err = format!("failed to read response: {}", e);
                    }
                },
                Err(ureq::Error::Transport(e)) => {
                    last_err = format!("network error: {}", e);
                    tracing::debug!(
                        attempt = attempt + 1,
                        max = retry.max_attempts,
                        error = %e,
                        "Request failed, retrying"
                    );
                }
                Err(ureq::Error::Status(code, _)) if code >= 500 => {
                    last_err = format!("server error (HTTP {})", code);
                    tracing::debug!(
                        attempt = attempt + 1,
                        max = retry.max_attempts,
                        code,
                        "Server error, retrying"
                    );
                }
                Err(e) => {
                    return Err(format!("request error: {}", e));
                }
            }
        }
        Err(format!(
            "failed after {} attempts: {}",
            retry.max_attempts, last_err
        ))
    }

    /// Check in with the device gateway, reporting current config hash and optional compliance summary.
    pub fn checkin(
        &self,
        config_hash: &str,
        compliance_summary: Option<ComplianceSummary>,
        printer: &Printer,
    ) -> Result<CheckinResponse> {
        let hostname = crate::hostname_string();

        let body = CheckinRequest {
            device_id: self.device_id.clone(),
            hostname,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            config_hash: config_hash.to_string(),
            compliance_summary,
        };

        let body_json = serde_json::to_string(&body).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize checkin request: {}",
                e
            )))
        })?;

        printer.info("Checking in with device gateway");

        let response_body = self
            .post_with_retry("/api/v1/checkin", &body_json)
            .map_err(|e| {
                CfgdError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("device gateway checkin failed: {}", e),
                ))
            })?;

        serde_json::from_str(&response_body).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid checkin response: {}", e),
            ))
        })
    }

    /// Report drift events to the device gateway.
    pub fn report_drift(&self, drifts: &[SystemDrift], printer: &Printer) -> Result<()> {
        if drifts.is_empty() {
            return Ok(());
        }

        let details: Vec<DriftDetail> = drifts
            .iter()
            .map(|d| DriftDetail {
                field: d.key.clone(),
                expected: d.expected.clone(),
                actual: d.actual.clone(),
            })
            .collect();

        let body = DriftReport { details };
        let body_json = serde_json::to_string(&body).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize drift report: {}",
                e
            )))
        })?;

        let path = format!("/api/v1/devices/{}/drift", self.device_id);
        printer.info(&format!(
            "Reporting {} drift events to device gateway",
            drifts.len()
        ));

        self.post_with_retry(&path, &body_json).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("device gateway drift report failed: {}", e),
            ))
        })?;

        Ok(())
    }

    /// Enroll this device with the device gateway using a bootstrap token.
    /// Returns the enrollment response including the permanent device API key.
    pub fn enroll(&self, bootstrap_token: &str, printer: &Printer) -> Result<EnrollResponse> {
        let hostname = crate::hostname_string();

        let body = EnrollRequest {
            token: bootstrap_token.to_string(),
            device_id: self.device_id.clone(),
            hostname,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        };

        let body_json = serde_json::to_string(&body).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize enrollment request: {}",
                e
            )))
        })?;

        printer.info("Enrolling device with device gateway");

        let response_body = self
            .post_with_retry("/api/v1/enroll", &body_json)
            .map_err(|e| {
                CfgdError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("device gateway enrollment failed: {}", e),
                ))
            })?;

        serde_json::from_str(&response_body).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid enrollment response: {}", e),
            ))
        })
    }

    /// Query the server's enrollment method (token or key).
    pub fn enroll_info(&self) -> Result<EnrollInfoResponse> {
        let url = format!("{}/api/v1/enroll/info", self.base_url);
        let resp = ureq::get(&url).call().map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("failed to query enrollment info: {}", e),
            ))
        })?;
        let body = resp.into_string().map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to read enrollment info response: {}",
                e
            )))
        })?;
        serde_json::from_str(&body).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid enrollment info response: {}", e),
            ))
        })
    }

    /// Request an enrollment challenge from the server (key-based enrollment).
    pub fn request_challenge(
        &self,
        username: &str,
        printer: &Printer,
    ) -> Result<ChallengeResponse> {
        let hostname = crate::hostname_string();

        let body = ChallengeRequest {
            username: username.to_string(),
            device_id: self.device_id.clone(),
            hostname,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        };

        let body_json = serde_json::to_string(&body).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize challenge request: {}",
                e
            )))
        })?;

        printer.info("Requesting enrollment challenge");

        let response_body = self
            .post_with_retry("/api/v1/enroll/challenge", &body_json)
            .map_err(|e| {
                CfgdError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("enrollment challenge request failed: {}", e),
                ))
            })?;

        serde_json::from_str(&response_body).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid challenge response: {}", e),
            ))
        })
    }

    /// Submit a signed challenge for verification (key-based enrollment).
    pub fn submit_verification(
        &self,
        challenge_id: &str,
        signature: &str,
        key_type: &str,
        printer: &Printer,
    ) -> Result<EnrollResponse> {
        let body = VerifyRequest {
            challenge_id: challenge_id.to_string(),
            signature: signature.to_string(),
            key_type: key_type.to_string(),
        };

        let body_json = serde_json::to_string(&body).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize verification request: {}",
                e
            )))
        })?;

        printer.info("Submitting signed challenge for verification");

        let response_body = self
            .post_with_retry("/api/v1/enroll/verify", &body_json)
            .map_err(|e| {
                CfgdError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("enrollment verification failed: {}", e),
                ))
            })?;

        serde_json::from_str(&response_body).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid verification response: {}", e),
            ))
        })
    }

    /// Create a client from a stored device credential.
    pub fn from_credential(cred: &DeviceCredential) -> Self {
        Self {
            base_url: cred.server_url.trim_end_matches('/').to_string(),
            api_key: Some(cred.api_key.clone()),
            device_id: cred.device_id.clone(),
        }
    }
}

// --- Credential Storage ---

/// Path to the device credential file: `~/.local/share/cfgd/device-credential.json`
pub fn credential_path() -> Result<PathBuf> {
    let dir = crate::state::default_state_dir()?;
    Ok(dir.join("device-credential.json"))
}

/// Save a device credential to disk after enrollment.
pub fn save_credential(cred: &DeviceCredential) -> Result<PathBuf> {
    let path = credential_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to create credential directory: {}", e),
            ))
        })?;
        // Restrict parent directory to owner-only access — even if the credential file
        // briefly has permissive permissions during atomic_write, the directory ACL
        // prevents other users from accessing it.
        crate::set_file_permissions(parent, 0o700)?;
    }
    let json = serde_json::to_string_pretty(cred).map_err(|e| {
        CfgdError::Io(std::io::Error::other(format!(
            "failed to serialize credential: {}",
            e
        )))
    })?;
    crate::atomic_write_str(&path, &json)?;

    // Restrict file permissions (no-op on Windows)
    crate::set_file_permissions(&path, 0o600)?;

    Ok(path)
}

/// Load a previously stored device credential.
pub fn load_credential() -> Result<Option<DeviceCredential>> {
    load_credential_from(&credential_path()?)
}

/// Load a device credential from a specific path.
pub fn load_credential_from(path: &Path) -> Result<Option<DeviceCredential>> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(path)?;
    let cred: DeviceCredential = serde_json::from_str(&contents).map_err(|e| {
        CfgdError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid device credential file: {}", e),
        ))
    })?;
    Ok(Some(cred))
}

#[cfg(test)]
mod tests {
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
            .with_body(
                r#"{"status":"ok","configChanged":true,"desiredConfig":{"packages":["git"]}}"#,
            )
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
        let json =
            r#"{"status":"enrolled","deviceId":"dev-1","apiKey":"key-1","username":"user1"}"#;
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
}

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::compliance::ComplianceSummary;
use crate::errors::{CfgdError, Result};
use crate::output::{Printer, Role};
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

        printer.status_simple(Role::Info, "Checking in with device gateway");

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
        printer.status_simple(
            Role::Info,
            format!("Reporting {} drift events to device gateway", drifts.len()),
        );

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

        printer.status_simple(Role::Info, "Enrolling device with device gateway");

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

        printer.status_simple(Role::Info, "Requesting enrollment challenge");

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

        printer.status_simple(Role::Info, "Submitting signed challenge for verification");

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

/// Filename of the enrolled device credential. Lives in the state dir and is a
/// sensitive migration sidecar artifact (its parent dir is kept 0700).
pub const DEVICE_CREDENTIAL_FILENAME: &str = "device-credential.json";

/// Path to the device credential file: `~/.local/state/cfgd/device-credential.json`
pub fn credential_path() -> Result<PathBuf> {
    let dir = crate::state::default_state_dir()?;
    Ok(dir.join(DEVICE_CREDENTIAL_FILENAME))
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
mod tests;

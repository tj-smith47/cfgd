use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{CfgdError, Result};
use crate::output::Printer;
use crate::providers::SystemDrift;

/// Client for communicating with cfgd-server.
pub struct ServerClient {
    base_url: String,
    api_key: Option<String>,
    device_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct CheckinRequest {
    device_id: String,
    hostname: String,
    os: String,
    arch: String,
    config_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CheckinResponse {
    pub status: String,
    pub config_changed: bool,
    #[serde(default)]
    pub desired_config: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct DriftReport {
    details: Vec<DriftDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct DriftDetail {
    field: String,
    expected: String,
    actual: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct EnrollRequest {
    token: String,
    device_id: String,
    hostname: String,
    os: String,
    arch: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

/// Stored device credential — saved locally after enrollment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DeviceCredential {
    pub server_url: String,
    pub device_id: String,
    pub api_key: String,
    pub username: String,
    #[serde(default)]
    pub team: Option<String>,
    pub enrolled_at: String,
}

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 500;

impl ServerClient {
    pub fn new(base_url: &str, api_key: Option<&str>, device_id: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(String::from),
            device_id: device_id.to_string(),
        }
    }

    fn build_request(&self, method: &str, path: &str) -> ureq::Request {
        let url = format!("{}{}", self.base_url, path);
        let mut req = match method {
            "POST" => ureq::post(&url),
            _ => ureq::get(&url),
        };

        if let Some(ref key) = self.api_key {
            req = req.set("Authorization", &format!("Bearer {}", key));
        }

        req
    }

    /// Send a POST request with exponential backoff on network failures.
    fn post_with_retry(&self, path: &str, body_json: &str) -> std::result::Result<String, String> {
        let mut last_err = String::new();
        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                let backoff =
                    std::time::Duration::from_millis(INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1));
                std::thread::sleep(backoff);
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
                        max = MAX_RETRIES,
                        error = %e,
                        "Request failed, retrying"
                    );
                }
                Err(e) => {
                    // Non-transport errors (4xx, 5xx) are not retryable
                    return Err(format!("server error: {}", e));
                }
            }
        }
        Err(format!(
            "failed after {} attempts: {}",
            MAX_RETRIES, last_err
        ))
    }

    /// Check in with cfgd-server, reporting current config hash.
    pub fn checkin(&self, config_hash: &str, printer: &Printer) -> Result<CheckinResponse> {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let body = CheckinRequest {
            device_id: self.device_id.clone(),
            hostname,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            config_hash: config_hash.to_string(),
        };

        let body_json = serde_json::to_string(&body).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize checkin request: {}",
                e
            )))
        })?;

        printer.info("Checking in with cfgd-server");

        let response_body = self
            .post_with_retry("/api/v1/checkin", &body_json)
            .map_err(|e| {
                CfgdError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("cfgd-server checkin failed: {}", e),
                ))
            })?;

        serde_json::from_str(&response_body).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid checkin response: {}", e),
            ))
        })
    }

    /// Report drift events to cfgd-server.
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
            "Reporting {} drift events to cfgd-server",
            drifts.len()
        ));

        self.post_with_retry(&path, &body_json).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("cfgd-server drift report failed: {}", e),
            ))
        })?;

        Ok(())
    }

    /// Enroll this device with cfgd-server using a bootstrap token.
    /// Returns the enrollment response including the permanent device API key.
    pub fn enroll(&self, bootstrap_token: &str, printer: &Printer) -> Result<EnrollResponse> {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

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

        printer.info("Enrolling device with cfgd-server");

        let response_body = self
            .post_with_retry("/api/v1/enroll", &body_json)
            .map_err(|e| {
                CfgdError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("cfgd-server enrollment failed: {}", e),
                ))
            })?;

        serde_json::from_str(&response_body).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid enrollment response: {}", e),
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
    }
    let json = serde_json::to_string_pretty(cred).map_err(|e| {
        CfgdError::Io(std::io::Error::other(format!(
            "failed to serialize credential: {}",
            e
        )))
    })?;
    std::fs::write(&path, &json)?;

    // Restrict file permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

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

    #[test]
    fn server_client_new() {
        let client = ServerClient::new("http://localhost:8080", Some("test-key"), "node-1");
        assert_eq!(client.base_url, "http://localhost:8080");
        assert_eq!(client.api_key.as_deref(), Some("test-key"));
        assert_eq!(client.device_id, "node-1");
    }

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
    fn drift_report_serialization() {
        let report = DriftReport {
            details: vec![DriftDetail {
                field: "sysctl.net.ipv4.ip_forward".to_string(),
                expected: "1".to_string(),
                actual: "0".to_string(),
            }],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("net.ipv4.ip_forward"));
        assert!(json.contains("\"expected\":\"1\""));
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
    fn enroll_request_serialization() {
        let req = EnrollRequest {
            token: "cfgd_bs_abc123".to_string(),
            device_id: "dev-1".to_string(),
            hostname: "macbook".to_string(),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("cfgd_bs_abc123"));
        assert!(json.contains("dev-1"));
    }
}

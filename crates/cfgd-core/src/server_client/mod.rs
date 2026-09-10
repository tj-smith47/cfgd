use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::compliance::ComplianceSummary;
use crate::errors::{CfgdError, Result};
use crate::output::{Printer, Role};
use crate::providers::SystemDrift;

/// Ceiling on a server-advised wait. The gateway's own quota hands a token back
/// in seconds, so anything past a minute is either a misconfigured proxy or a
/// server asking a device to stand still for longer than the operator who ran
/// the command would wait; honouring it verbatim would turn one refused
/// enrollment into a hang.
const ADVISED_WAIT_CEILING: std::time::Duration = std::time::Duration::from_secs(60);

/// What a reader does about an exhausted ladder whose LAST refusal was the
/// gateway's quota. It is derived from that last refusal rather than remembered,
/// because a 429 followed by two server errors is a server problem: telling the
/// reader to enrol from another address would send them after a limit that is no
/// longer refusing them.
const RATE_LIMIT_NEXT_STEP: &str = "; the gateway limits enrollment attempts per source address, so retry in a minute or enrol from another address";

/// The wait a 429 asked for: the `Retry-After` header in its delta-seconds form,
/// falling back to the gateway's own `retry_after_secs` body field, clamped to
/// [`ADVISED_WAIT_CEILING`].
///
/// `None` means "not stated", which leaves the caller on its own ladder. The
/// header's other legal form is an HTTP-date, and a device's clock is exactly
/// what cannot be trusted to subtract one — so a value that is not a plain
/// count of seconds reads as unstated rather than as zero.
fn advised_wait(header: Option<&str>, body: &str) -> Option<std::time::Duration> {
    let secs = header
        .and_then(|v| v.trim().parse::<u64>().ok())
        .or_else(|| {
            serde_json::from_str::<serde_json::Value>(body.trim())
                .ok()
                .and_then(|v| v.get("retry_after_secs")?.as_u64())
        })?;
    Some(std::time::Duration::from_secs(secs).min(ADVISED_WAIT_CEILING))
}

/// The server's own words for a refusal, for the error a hard 4xx returns: the
/// gateway answers JSON carrying an `error` field, and anything else (a proxy's
/// HTML page) is carried verbatim up to a line's worth, because a status code
/// alone tells the reader nothing about which field the gateway rejected.
fn refusal_detail(body: &str) -> String {
    let trimmed = body.trim();
    if let Some(message) = serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|value| value.get("error")?.as_str().map(str::to_string))
    {
        return message;
    }
    trimmed.chars().take(200).collect()
}

/// Client for communicating with the device gateway.
pub struct ServerClient {
    base_url: String,
    api_key: Option<String>,
    device_id: String,
}

/// What the machine reports about itself on a check-in, beyond the config hash
/// and the compliance summary.
///
/// The device is the only thing that can answer either question, and the
/// check-in is its only channel to the cluster: `MachineConfig.status`
/// carries both maps, and a `ConfigPolicy` version pin and a `BackupPolicy`
/// schedule projection are decided from them.
///
/// Each map is an `Option` because "I looked and found none" and "I could not
/// look" are different facts and the gateway acts on them differently: an
/// observed map is applied whole, so a key the machine stopped reporting is
/// retired, while an unobserved one is left alone with whatever the cluster
/// already holds. `None` is also what an older device's body deserializes as.
#[derive(Debug, Default, Clone)]
pub struct CheckinFacts {
    /// Installed versions of the packages this machine DECLARES, keyed
    /// `<manager>/<package>` by [`crate::state::package_resource_id`].
    pub package_versions: Option<BTreeMap<String, String>>,
    /// Which layer owns each declared backup unit's schedule, as
    /// [`crate::config::ScheduleOwner::label`] spells it.
    pub backup_schedule_owners: Option<BTreeMap<String, String>>,
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
    /// Omitted when the device did not observe it, which is the body an older
    /// device sends and a gateway that predates the field parses. An observed
    /// map is sent whole, empty included: that is what lets the gateway retire
    /// a key this machine no longer reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_versions: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backup_schedule_owners: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResponse {
    pub status: String,
    pub config_changed: bool,
    #[serde(default)]
    pub desired_config: Option<serde_json::Value>,
    /// The cadences a cluster `BackupPolicy` owns for this machine, keyed by
    /// unit name.
    ///
    /// Absent from an older gateway's answer, and from one that could not read
    /// the cluster: either way this machine learned nothing and keeps the set
    /// it recorded. Present and empty is an answer, and retires them.
    #[serde(default)]
    pub backup_schedules: Option<crate::backup::ScheduleProjections>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DriftReport {
    details: Vec<DriftDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DriftDetail {
    /// The drifted setting's identity, qualified by the configurator that
    /// reported it (`sysctl.net.ipv4.ip_forward`), composed through
    /// [`crate::reconciler::system_resource_key`].
    ///
    /// Never the bare key: two configurators may declare the same one, and the
    /// DriftAlert CRD merges `driftDetails` by this field. Never the
    /// `system:`-prefixed spelling either — that outer wrapper belongs to
    /// cfgd's own resource-id grammar, not to a fleet wire field.
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
    // Strip any path suffix to compare just the host[:port].
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

    fn build_request(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let url = format!("{}{}", self.base_url, path);
        // Status-as-error off for THIS request only, leaving the shared agent's
        // setting alone: `ureq::Error::StatusCode` carries nothing but the code,
        // and a 429 answers with the wait it wants honoured in its own header and
        // body. Reading those needs the response, not an error.
        let mut req = self
            .agent()
            .post(&url)
            .config()
            .http_status_as_error(false)
            .build();

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", &format!("Bearer {}", key));
        }

        req
    }

    /// Send a POST request with exponential backoff, narrating the wait.
    ///
    /// The one place every gateway round-trip blocks, and the only one that
    /// knows an attempt failed and a backoff is being slept off — so the
    /// narration lives here rather than at five call sites that cannot see
    /// either. `label` is the caller's words for what it is asking for.
    ///
    /// Narrated SILENTLY: the caller prints a permanent line naming this
    /// request before it calls, and the command around it settles the verdict
    /// once the answer is in, so a settled line here would be a third
    /// statement of one round-trip. `label` is the caller's words, and the
    /// bar carries only the WAITING half of them, so the standing line and
    /// the live one beneath it do not read as one sentence printed twice.
    fn post_with_retry(
        &self,
        path: &str,
        body_json: &str,
        printer: &Printer,
        label: &str,
    ) -> std::result::Result<String, String> {
        printer.narrate_silent(format!("{label}: waiting for response"), |sp| {
            self.post_attempts(path, body_json, label, sp)
        })
    }

    /// The retry ladder itself, split out so `post_with_retry` reads as the
    /// one narrated wait it is: every arm below either returns or falls
    /// through to the next attempt, and the wrapper's finish is the same on
    /// all of them.
    fn post_attempts(
        &self,
        path: &str,
        body_json: &str,
        label: &str,
        sp: &mut crate::output::Spinner<'_>,
    ) -> std::result::Result<String, String> {
        // Which ladder the NEXT wait is measured on: only the arm that saw the
        // failure knows whether the server was busy or was rationing.
        let mut policy = crate::retry::BackoffConfig::DEFAULT_TRANSIENT;
        let mut wait = std::time::Duration::ZERO;
        // Whether that wait is the gateway's quota rather than a busy server,
        // which is the difference between a blip and eighteen seconds of
        // standing still — the reader of the spinner is owed that distinction.
        let mut rationed = false;
        let mut last_err = String::new();
        let mut attempt = 0;
        while attempt < policy.max_attempts {
            if !wait.is_zero() {
                // Named only from the second attempt on: the opening label
                // already covers the first, and "attempt 1 of 3" on a request
                // that will succeed reads as a problem.
                sp.set_message(if rationed {
                    format!(
                        "{label}: rate limited, waiting {:?} for the gateway's quota, attempt {} of {}",
                        wait,
                        attempt + 1,
                        policy.max_attempts
                    )
                } else {
                    format!(
                        "{label}: retrying, attempt {} of {}",
                        attempt + 1,
                        policy.max_attempts
                    )
                });
                std::thread::sleep(wait);
            }
            let next_delay = |p: &crate::retry::BackoffConfig| p.delay_for_attempt(attempt + 1);

            match self
                .build_request(path)
                .header("Content-Type", "application/json")
                .send(body_json)
            {
                Ok(mut resp) => {
                    let status = resp.status().as_u16();
                    // Read off the response before the body is consumed.
                    let advised = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    let body = resp.body_mut().read_to_string();

                    if status < 400 {
                        match body {
                            Ok(body) => return Ok(body),
                            Err(e) => {
                                rationed = false;
                                policy = crate::retry::BackoffConfig::DEFAULT_TRANSIENT;
                                wait = next_delay(&policy);
                                last_err = format!("failed to read response: {}", e);
                            }
                        }
                    } else if status == 429 {
                        // 429 is the one 4xx that says "later" rather than
                        // "never": the gateway rations its enrollment routes per
                        // source IP, so a device sharing an egress address with
                        // the rest of its fleet could otherwise never enrol at
                        // all. The server's own advised wait outranks the ladder
                        // — it knows when the next token lands.
                        rationed = true;
                        policy = crate::retry::BackoffConfig::rate_limited();
                        wait = advised_wait(advised.as_deref(), body.as_deref().unwrap_or(""))
                            .unwrap_or_else(|| next_delay(&policy));
                        last_err = "rate limited (HTTP 429)".to_string();
                        tracing::debug!(
                            attempt = attempt + 1,
                            max = policy.max_attempts,
                            wait_secs = wait.as_secs(),
                            "Rate limited, retrying"
                        );
                    } else if status >= 500 {
                        // 5xx responses are retried as transient server errors.
                        rationed = false;
                        policy = crate::retry::BackoffConfig::DEFAULT_TRANSIENT;
                        wait = next_delay(&policy);
                        last_err = format!("server error (HTTP {})", status);
                        tracing::debug!(
                            attempt = attempt + 1,
                            max = policy.max_attempts,
                            status,
                            "Server error, retrying"
                        );
                    } else {
                        // Every other 4xx is a hard request error — do not retry.
                        let detail = refusal_detail(body.as_deref().unwrap_or(""));
                        return Err(if detail.is_empty() {
                            format!("request error: HTTP {}", status)
                        } else {
                            format!("request error: HTTP {}: {}", status, detail)
                        });
                    }
                }
                // Everything reaching the error arm is a transport-layer failure
                // (Io, Timeout, HostNotFound, ConnectionFailed, Protocol, …):
                // this request reads no status as an error.
                Err(e) => {
                    rationed = false;
                    policy = crate::retry::BackoffConfig::DEFAULT_TRANSIENT;
                    wait = next_delay(&policy);
                    last_err = format!("network error: {}", e);
                    tracing::debug!(
                        attempt = attempt + 1,
                        max = policy.max_attempts,
                        error = %e,
                        "Request failed, retrying"
                    );
                }
            }
            attempt += 1;
        }
        Err(format!(
            "failed after {} attempts: {}{}",
            policy.max_attempts,
            last_err,
            if rationed { RATE_LIMIT_NEXT_STEP } else { "" }
        ))
    }

    /// Check in with the device gateway, reporting current config hash and optional compliance summary.
    pub fn checkin(
        &self,
        config_hash: &str,
        compliance_summary: Option<ComplianceSummary>,
        facts: CheckinFacts,
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
            package_versions: facts.package_versions,
            backup_schedule_owners: facts.backup_schedule_owners,
        };

        let body_json = serde_json::to_string(&body).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize checkin request: {}",
                e
            )))
        })?;

        // One binding for the permanent line and the bar beneath it: the
        // two name the same request and must never drift apart.
        let label = "Checking in with device gateway";
        printer.status_simple(Role::Info, label);

        let response_body = self
            .post_with_retry("/api/v1/checkin", &body_json, printer, label)
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

    /// Report drifted system settings to the device gateway.
    ///
    /// The gateway's whole drift picture is what this method sends, and every
    /// element is a [`SystemDrift`] — one system configurator's setting whose
    /// live value diverged from the declared one. Packages, files, env vars and
    /// aliases are never carried here, so the narration names the class rather
    /// than letting a fleet reader take a settings report for a machine verdict.
    ///
    /// Each drift arrives paired with the configurator that reported it, as
    /// [`crate::compliance::system_drifts`] yields them: the wire field is the
    /// qualified identity, so two configurators sharing a key stay two rows.
    pub fn report_drift(&self, drifts: &[(&str, &SystemDrift)], printer: &Printer) -> Result<()> {
        if drifts.is_empty() {
            return Ok(());
        }

        let details: Vec<DriftDetail> = drifts
            .iter()
            .map(|(configurator, d)| DriftDetail {
                field: crate::reconciler::system_resource_key(configurator, &d.key),
                expected: d.expected.clone(),
                actual: d.actual.clone(),
            })
            .collect();

        let body = DriftReport { details };
        let body_json = serde_json::to_string(&body).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                // fleet-drift-ok: an internal serialization failure, not a claim
                "failed to serialize drift report: {}",
                e
            )))
        })?;

        let path = format!("/api/v1/devices/{}/drift", self.device_id);
        // One binding for the permanent line and the bar beneath it: the two
        // name the same request and must never drift apart.
        let label = format!(
            "Reporting {} to device gateway",
            crate::pluralize(drifts.len(), "drifted system setting")
        );
        printer.status_simple(Role::Info, label.clone());

        self.post_with_retry(&path, &body_json, printer, &label)
            .map_err(|e| {
                CfgdError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("device gateway system settings drift report failed: {}", e),
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

        // One binding for the permanent line and the bar beneath it: the
        // two name the same request and must never drift apart.
        let label = "Enrolling device with device gateway";
        printer.status_simple(Role::Info, label);

        let response_body = self
            .post_with_retry("/api/v1/enroll", &body_json, printer, label)
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
        let mut resp = ureq::get(&url).call().map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("failed to query enrollment info: {}", e),
            ))
        })?;
        let body = resp.body_mut().read_to_string().map_err(|e| {
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

        // One binding for the permanent line and the bar beneath it: the
        // two name the same request and must never drift apart.
        let label = "Requesting enrollment challenge";
        printer.status_simple(Role::Info, label);

        let response_body = self
            .post_with_retry("/api/v1/enroll/challenge", &body_json, printer, label)
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

        // One binding for the permanent line and the bar beneath it: the
        // two name the same request and must never drift apart.
        let label = "Submitting signed challenge for verification";
        printer.status_simple(Role::Info, label);

        let response_body = self
            .post_with_retry("/api/v1/enroll/verify", &body_json, printer, label)
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

/// Whether a stored credential authenticates a check-in to `server_url`.
///
/// The ONE comparison behind that question, so `cfgd checkin` and the daemon's
/// periodic check-in cannot disagree about which gateway a credential is for.
/// A trailing slash is not part of a gateway's identity.
pub fn credential_matches(server_url: &str, cred: &DeviceCredential) -> bool {
    cred.server_url.trim_end_matches('/') == server_url.trim_end_matches('/')
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

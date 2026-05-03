use super::*;

// --- Server Check-in ---

#[derive(Debug, Serialize)]
pub(crate) struct CheckinPayload {
    pub(crate) device_id: String,
    pub(crate) hostname: String,
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) config_hash: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CheckinServerResponse {
    #[serde(rename = "status")]
    pub(crate) _status: String,
    pub(crate) config_changed: bool,
    #[serde(rename = "config")]
    pub(crate) _config: Option<serde_json::Value>,
}

/// Generate a stable device ID from the hostname using SHA256.
pub(crate) fn generate_device_id() -> std::result::Result<String, String> {
    let host = hostname::get()
        .map_err(|e| format!("failed to get hostname: {}", e))?
        .to_string_lossy()
        .to_string();
    Ok(crate::sha256_hex(host.as_bytes()))
}

/// Compute a SHA256 hash of the resolved profile serialized to YAML.
pub(crate) fn compute_config_hash(
    resolved: &ResolvedProfile,
) -> std::result::Result<String, String> {
    let yaml = serde_yaml::to_string(&resolved.merged.packages)
        .map_err(|e| format!("failed to serialize profile for hashing: {}", e))?;
    Ok(crate::sha256_hex(yaml.as_bytes()))
}

/// Perform a server check-in. Returns true if the server indicates config has changed.
/// On any error, logs a warning and returns false (best-effort).
pub(crate) fn server_checkin(server_url: &str, resolved: &ResolvedProfile) -> bool {
    let device_id = match generate_device_id() {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "server check-in failed");
            return false;
        }
    };

    let host = match hostname::get() {
        Ok(h) => h.to_string_lossy().to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "server check-in: failed to get hostname");
            return false;
        }
    };

    let config_hash = match compute_config_hash(resolved) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "server check-in failed");
            return false;
        }
    };

    let payload = CheckinPayload {
        device_id,
        hostname: host,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        config_hash,
    };

    let url = format!("{}/api/v1/checkin", server_url.trim_end_matches('/'));

    let body = match serde_json::to_string(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "server check-in: failed to serialize payload");
            return false;
        }
    };

    tracing::info!(
        url = %url,
        device_id = %payload.device_id,
        "checking in with server"
    );

    match ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string(&body)
    {
        Ok(response) => {
            let status = response.status();
            match response.into_string() {
                Ok(resp_body) => match serde_json::from_str::<CheckinServerResponse>(&resp_body) {
                    Ok(resp) => {
                        tracing::info!(
                            config_changed = resp.config_changed,
                            "server check-in successful"
                        );
                        resp.config_changed
                    }
                    Err(e) => {
                        tracing::warn!(
                            status = status,
                            error = %e,
                            "server check-in: failed to parse response"
                        );
                        false
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "server check-in: failed to read response body");
                    false
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "server check-in failed");
            false
        }
    }
}

/// Find the server URL from the config's origin, if origin type is `server`.
pub(crate) fn find_server_url(config: &CfgdConfig) -> Option<String> {
    config
        .spec
        .origin
        .iter()
        .find(|o| matches!(o.origin_type, OriginType::Server))
        .map(|o| o.url.clone())
}

/// Perform a server check-in if configured. Returns true if config changed on server.
pub(crate) fn try_server_checkin(config: &CfgdConfig, resolved: &ResolvedProfile) -> bool {
    match find_server_url(config) {
        Some(url) => server_checkin(&url, resolved),
        None => false,
    }
}

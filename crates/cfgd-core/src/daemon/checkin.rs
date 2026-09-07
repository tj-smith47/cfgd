use super::*;

// --- Server Check-in ---

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckinPayload {
    pub(crate) device_id: String,
    pub(crate) hostname: String,
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) config_hash: String,
    /// Omitted when empty, so a machine with nothing to report sends the body
    /// a gateway that predates the field already parses.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) backup_schedule_owners: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckinServerResponse {
    #[serde(rename = "status")]
    pub(crate) _status: String,
    pub(crate) config_changed: bool,
    #[serde(rename = "config")]
    pub(crate) _config: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) backup_schedules: crate::backup::ScheduleProjections,
}

/// What a check-in produced: whether the gateway reports the config changed,
/// and the cluster-owned cadences it answered with.
///
/// The two travel together because one round-trip produces both, and a caller
/// that took only the bool left the projection on the floor — the whole reason
/// the response carries it.
#[derive(Debug, Default)]
pub(crate) struct CheckinOutcome {
    pub(crate) config_changed: bool,
    pub(crate) backup_schedules: crate::backup::ScheduleProjections,
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

/// Perform a server check-in, reporting which layer owns each backup unit's
/// schedule and taking back the cadences the cluster owns.
///
/// On any error, logs a warning and returns the empty outcome (best-effort):
/// the machine's own reconcile never depends on the cluster answering.
pub(crate) fn server_checkin(server_url: &str, resolved: &ResolvedProfile) -> CheckinOutcome {
    let device_id = match generate_device_id() {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "daemon: server check-in failed to derive a device id");
            return CheckinOutcome::default();
        }
    };

    let host = match hostname::get() {
        Ok(h) => h.to_string_lossy().to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "daemon: server check-in failed to get hostname");
            return CheckinOutcome::default();
        }
    };

    let config_hash = match compute_config_hash(resolved) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "daemon: server check-in failed to hash the profile");
            return CheckinOutcome::default();
        }
    };

    let payload = CheckinPayload {
        device_id,
        hostname: host,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        config_hash,
        backup_schedule_owners: crate::backup::declared_schedule_owners(&resolved.merged.backups),
    };

    let url = format!("{}/api/v1/checkin", server_url.trim_end_matches('/'));

    let body = match serde_json::to_string(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "daemon: server check-in failed to serialize payload");
            return CheckinOutcome::default();
        }
    };

    tracing::debug!(url = %url, device_id = %payload.device_id, "daemon: check-in request");
    tracing::info!("daemon: checking in with {url}");

    match ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(body.as_str())
    {
        Ok(mut response) => {
            let status = response.status().as_u16();
            match response.body_mut().read_to_string() {
                Ok(resp_body) => match serde_json::from_str::<CheckinServerResponse>(&resp_body) {
                    Ok(resp) => {
                        tracing::debug!(
                            config_changed = resp.config_changed,
                            cluster_scheduled_units = resp.backup_schedules.len(),
                            "daemon: check-in response"
                        );
                        tracing::info!(
                            "daemon: server check-in succeeded — config {}",
                            if resp.config_changed {
                                "changed"
                            } else {
                                "unchanged"
                            }
                        );
                        CheckinOutcome {
                            config_changed: resp.config_changed,
                            backup_schedules: resp.backup_schedules,
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            status = status,
                            error = %e,
                            "daemon: server check-in failed to parse response"
                        );
                        CheckinOutcome::default()
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "daemon: server check-in failed to read response body");
                    CheckinOutcome::default()
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "daemon: server check-in request failed");
            CheckinOutcome::default()
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

/// Perform a server check-in if configured. A machine with no server origin
/// reports nothing and takes nothing back.
pub(crate) fn try_server_checkin(
    config: &CfgdConfig,
    resolved: &ResolvedProfile,
) -> CheckinOutcome {
    match find_server_url(config) {
        Some(url) => server_checkin(&url, resolved),
        None => CheckinOutcome::default(),
    }
}

/// Persist the cadences a check-in answered with into the state store under
/// `state_dir`, so the daemon's timers and `cfgd backup list` read one answer.
///
/// Best-effort by the same rule the check-in itself is: an unwritable state
/// store costs the machine the cluster's cadence, never its own reconcile.
pub(crate) fn record_cluster_schedules_in(
    state_dir: Option<&std::path::Path>,
    projections: &crate::backup::ScheduleProjections,
) {
    let store = match state_dir {
        Some(dir) => crate::state::StateStore::open_in_dir(dir),
        None => crate::state::StateStore::open_default(),
    };
    match store {
        Ok(store) => crate::backup::record_cluster_schedules(&store, projections),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "daemon: state store unavailable — the cluster-owned backup schedules were not recorded"
            );
        }
    }
}

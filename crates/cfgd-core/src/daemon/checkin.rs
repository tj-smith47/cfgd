use super::*;

// --- Server Check-in ---

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinPayload {
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub config_hash: String,
    /// Omitted when this check-in observed no such map, which is the body a
    /// gateway that predates the field already parses. An observed map is sent
    /// whole, empty included, so the gateway can retire a key the machine
    /// stopped reporting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_versions: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_schedule_owners: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinServerResponse {
    /// The gateway's own word for the outcome, as its `CheckinResponse`
    /// serializes it.
    pub status: String,
    pub config_changed: bool,
    /// The configuration the gateway pushed for this machine, under the key the
    /// gateway's own `CheckinResponse` serializes it as. Saved as the pending
    /// server config, which the next reconcile consumes.
    #[serde(default)]
    pub desired_config: Option<serde_json::Value>,
    #[serde(default)]
    pub backup_schedules: crate::backup::ScheduleProjections,
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
    /// The projection the gateway ANSWERED with, `None` for a check-in that
    /// never got an answer.
    ///
    /// The distinction is the whole difference between a fleet cadence being
    /// replaced and being lost: recording is a whole-set replace, so folding a
    /// failed round-trip into an empty map would let one unreachable gateway
    /// delete every cadence the cluster owns for this machine.
    pub(crate) backup_schedules: Option<crate::backup::ScheduleProjections>,
}

/// Compute a SHA256 hash of the resolved profile serialized to YAML.
pub(crate) fn compute_config_hash(
    resolved: &ResolvedProfile,
) -> std::result::Result<String, String> {
    let yaml = serde_yaml::to_string(&resolved.merged.packages)
        .map_err(|e| format!("failed to serialize profile for hashing: {}", e))?;
    Ok(crate::sha256_hex(yaml.as_bytes()))
}

/// Perform a server check-in as the enrolled device, reporting what this tick
/// observed about the machine and taking back the cadences the cluster owns.
///
/// The credential is required rather than optional: `/api/v1/checkin` sits
/// behind the gateway's auth middleware, and it enforces that the bearer names
/// the same device the body does — so an unauthenticated post, or one carrying
/// a device id derived from the hostname instead of the enrolment's, is a
/// request the gateway can only refuse.
///
/// On any error, logs a warning and returns an outcome that ANSWERED nothing
/// (best-effort): the machine's own reconcile never depends on the cluster
/// answering, and nothing the cluster owns is retired by a round-trip that did
/// not happen.
pub(crate) fn server_checkin(
    server_url: &str,
    resolved: &ResolvedProfile,
    facts: crate::server_client::CheckinFacts,
    credential: &crate::server_client::DeviceCredential,
) -> CheckinOutcome {
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
        device_id: credential.device_id.clone(),
        hostname: host,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        config_hash,
        package_versions: facts.package_versions,
        backup_schedule_owners: facts.backup_schedule_owners,
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
        .header("Authorization", &format!("Bearer {}", credential.api_key))
        .send(body.as_str())
    {
        Ok(mut response) => {
            let status = response.status().as_u16();
            match response.body_mut().read_to_string() {
                Ok(resp_body) => match serde_json::from_str::<CheckinServerResponse>(&resp_body) {
                    Ok(resp) => {
                        tracing::debug!(
                            server_status = %resp.status,
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
                        if let Some(ref pushed) = resp.desired_config
                            && let Err(e) = crate::state::save_pending_server_config(pushed)
                        {
                            tracing::warn!(
                                error = %e,
                                "daemon: the configuration the gateway pushed was not saved"
                            );
                        }
                        CheckinOutcome {
                            config_changed: resp.config_changed,
                            backup_schedules: Some(resp.backup_schedules),
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
///
/// A configured origin the machine holds no enrolment for is a logged skip
/// rather than an anonymous post: the gateway would refuse it, and a request
/// that cannot be authenticated is one the operator has to be told about.
pub(crate) fn try_server_checkin(
    config: &CfgdConfig,
    resolved: &ResolvedProfile,
    facts: crate::server_client::CheckinFacts,
) -> CheckinOutcome {
    let Some(url) = find_server_url(config) else {
        return CheckinOutcome::default();
    };
    let credential = crate::server_client::load_credential()
        .ok()
        .flatten()
        .filter(|cred| crate::server_client::credential_matches(&url, cred));
    match credential {
        Some(cred) => server_checkin(&url, resolved, facts, &cred),
        None => {
            tracing::warn!(
                url = %url,
                "daemon: no device credential for this gateway — skipping the check-in; run `cfgd enroll` on this machine"
            );
            CheckinOutcome::default()
        }
    }
}

/// Persist the cadences a check-in answered with into the state store under
/// `state_dir`, so the daemon's timers and `cfgd backup list` read one answer.
///
/// Best-effort by the same rule the check-in itself is: an unwritable state
/// store costs the machine the cluster's cadence, never its own reconcile.
/// `true` when the recorded set is not the one that was already there, which is
/// the daemon's cue to re-resolve its backup timers: the arm happened before
/// this answer arrived, and a healthy daemon resolves the set once per process.
pub(crate) fn record_cluster_schedules_in(
    state_dir: Option<&std::path::Path>,
    projections: &crate::backup::ScheduleProjections,
) -> bool {
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
            false
        }
    }
}

use super::*;

// --- Server Check-in ---

/// What a check-in produced: whether the gateway reports the config changed,
/// and the cluster-owned cadences it answered with.
///
/// The two travel together because one round-trip produces both, and a caller
/// that took only the bool left the projection on the floor — the whole reason
/// the response carries it.
#[derive(Debug, Default)]
pub struct CheckinOutcome {
    pub config_changed: bool,
    /// The projection the gateway ANSWERED with, `None` for a check-in that
    /// never got an answer.
    ///
    /// The distinction is the whole difference between a fleet cadence being
    /// replaced and being lost: recording is a whole-set replace, so folding a
    /// failed round-trip into an empty map would let one unreachable gateway
    /// delete every cadence the cluster owns for this machine.
    pub backup_schedules: Option<crate::backup::ScheduleProjections>,
}

/// Perform a server check-in as the enrolled device, reporting what
/// [`crate::server_client::CheckinFacts::collect`] composed and taking back the
/// cadences the cluster owns.
///
/// The credential is required: `/api/v1/checkin` sits behind the gateway's auth
/// middleware, which enforces that the bearer names the same device the body
/// does. The request goes through [`crate::server_client::ServerClient`], the
/// one client `cfgd checkin` uses too, so both senders put the same body on the
/// wire and retry a busy gateway on the same ladder.
///
/// On any error, logs a warning and returns an outcome that ANSWERED nothing
/// (best-effort): the machine's own reconcile never depends on the cluster
/// answering, and nothing the cluster owns is retired by a round-trip that did
/// not happen.
pub(crate) fn server_checkin(
    credential: &crate::server_client::DeviceCredential,
    facts: crate::server_client::CheckinFacts<'_>,
    printer: &Printer,
) -> CheckinOutcome {
    tracing::debug!(url = %credential.server_url, device_id = %credential.device_id, "daemon: check-in request");
    tracing::info!("daemon: checking in with {}", credential.server_url);

    // The daemon's own account of the round-trip is the journal lines here;
    // the client's progress line is for a terminal nobody watches.
    let quiet = printer.at_verbosity(crate::output::Verbosity::Quiet);
    match crate::server_client::ServerClient::from_credential(credential).checkin(facts, &quiet) {
        Ok(resp) => {
            tracing::debug!(
                server_status = %resp.status,
                config_changed = resp.config_changed,
                cluster_scheduled_units = resp
                    .backup_schedules
                    .as_ref()
                    .map_or(0, std::collections::BTreeMap::len),
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
                backup_schedules: resp.backup_schedules,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "daemon: server check-in failed");
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
///
/// `facts` is called only once a gateway and a credential for it are known, so
/// a machine that reports to no gateway pays for none of the observation. It
/// returns `None` when the facts could not be composed, having logged why, and
/// the check-in is skipped.
pub fn try_server_checkin<'a>(
    config: &CfgdConfig,
    printer: &Printer,
    facts: impl FnOnce() -> Option<crate::server_client::CheckinFacts<'a>>,
) -> CheckinOutcome {
    let Some(url) = find_server_url(config) else {
        return CheckinOutcome::default();
    };
    let credential = crate::server_client::load_credential()
        .ok()
        .flatten()
        .filter(|cred| crate::server_client::credential_matches(&url, cred));
    let Some(cred) = credential else {
        tracing::warn!(
            url = %url,
            "daemon: no device credential for this gateway — skipping the check-in; run `cfgd enroll` on this machine"
        );
        return CheckinOutcome::default();
    };
    match facts() {
        Some(facts) => server_checkin(&cred, facts, printer),
        None => CheckinOutcome::default(),
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

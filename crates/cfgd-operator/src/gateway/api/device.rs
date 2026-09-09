//! Device-facing endpoints: check-in, list, get, set-config.
//!
//! The check-in is the device's ONE channel to the cluster, so it carries the
//! two facts only the device can answer — the versions it holds for the
//! packages it declares, and which layer owns each backup unit's schedule —
//! onto its `MachineConfig.status`, and answers with the cadences a cluster
//! `BackupPolicy` owns for it. Both halves are best-effort against Kubernetes:
//! the device's own reconcile never depends on the cluster accepting a status
//! or having a policy to project.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;

use cfgd_core::backup::BackupScheduleProjection;

use super::*;
pub(super) async fn checkin(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(req): Json<CheckinRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    validate_device_id(&req.device_id)?;
    validate_hostname(&req.hostname)?;

    // Device auth: can only check in as self
    enforce_device_access(&auth, &req.device_id)?;

    let db = state.db.clone();
    let device_id = req.device_id.clone();
    let hostname = req.hostname.clone();
    let os = req.os.clone();
    let arch = req.arch.clone();
    let config_hash = req.config_hash.clone();
    let compliance = req.compliance_summary.clone();

    let (config_changed, desired_config) = db
        .with_write_tx(move |tx| {
            let existing = crate::gateway::db::get_device_tx(tx, &device_id);
            let (config_changed, desired) = match &existing {
                Ok(device) => (
                    device.config_hash != config_hash,
                    if device.config_hash != config_hash {
                        device.desired_config.clone()
                    } else {
                        None
                    },
                ),
                Err(GatewayError::NotFound(_)) => (false, None),
                Err(_) => (false, None),
            };

            match &existing {
                Ok(_) => {
                    crate::gateway::db::update_checkin_tx(
                        tx,
                        &device_id,
                        &config_hash,
                        compliance.as_ref(),
                    )?;
                }
                Err(_) => {
                    crate::gateway::db::register_device_tx(
                        tx,
                        &device_id,
                        &hostname,
                        &os,
                        &arch,
                        &config_hash,
                        compliance.as_ref(),
                    )?;
                }
            }

            crate::gateway::db::record_checkin_tx(tx, &device_id, &config_hash, config_changed)?;
            Ok((config_changed, desired))
        })
        .await?;

    // Broadcast to SSE subscribers
    let event_type = if config_changed {
        "config-changed"
    } else {
        "checkin"
    };
    let _ = state.event_tx.send(FleetEvent {
        timestamp: cfgd_core::utc_now_iso8601(),
        device_id: req.device_id.clone(),
        event_type: event_type.to_string(),
        summary: req.config_hash.clone(),
    });

    let auth_label = match &auth {
        AuthContext::Admin => "admin".to_string(),
        AuthContext::Device { username, .. } => format!("device({})", username),
    };
    tracing::info!(
        device_id = %req.device_id,
        hostname = %req.hostname,
        auth = %auth_label,
        config_changed,
        "device checked in"
    );

    // Kubernetes, after the database transaction and never inside it: a
    // cluster that is unreachable, or absent entirely (a standalone gateway),
    // costs the fleet its view of this device, never the device its check-in.
    // A projection is what the cluster OWNS, so a gateway that could not read
    // the cluster answers with none rather than with an empty one: the device
    // executes an answer as a whole-set replace, and "I could not look" must
    // not retire every cadence the fleet set.
    let backup_schedules = match &state.kube_client {
        Some(client) => match find_machine_config_ref(client, &req.hostname).await {
            Ok(Some((namespace, name))) => {
                report_device_status(client, &namespace, &name, &req).await;
                policy_owned_schedules(&state, client, &namespace, &req.hostname).await
            }
            // No MachineConfig names this hostname: there is nothing to write a
            // status onto and no namespace to look for policies in, which is a
            // read that succeeded and found nothing scheduled.
            Ok(None) => Some(BTreeMap::new()),
            Err(_) => None,
        },
        // A standalone gateway holds no client, so it knows nothing about a
        // cluster's policies rather than knowing there are none.
        None => None,
    };

    Ok((
        StatusCode::OK,
        Json(CheckinResponse {
            status: "ok".to_string(),
            config_changed,
            desired_config,
            backup_schedules,
        }),
    ))
}

/// Apply the device-reported halves of `MachineConfig.status` onto the object
/// the hostname resolved to, one map per write.
///
/// A failure is logged and nothing more: the check-in's outcome is the
/// device's, and it does not depend on the cluster accepting a status. It is
/// logged at `error` because the visible symptom of a refused write is a status
/// that silently stops moving, which a reader has no other way to notice.
///
/// Each map goes out under its OWN field manager, and a map the device did not
/// observe produces no write at all. Each map is one leaf in the schema
/// (`x-kubernetes-map-type: atomic`), so the forced apply writes the applied
/// value alone and a key the device stopped reporting is gone because the whole
/// map was replaced. One manager owning both maps would drop the fields it
/// stopped naming one level up and delete the whole map the device could not
/// observe this time. `packageVersions` and
/// `backupScheduleOwners` are facts the controller cannot see for itself, so an
/// older agent, or one whose managers could not be queried, must not blank what
/// the cluster still holds.
///
/// The applies are forced. Each manager is the sole writer of its one field, so
/// a conflict can only be an ownership entry left by an older release, whose
/// whole-status merge patch claimed `packageVersions` under the controller's
/// manager; yielding to it would strand the device's status forever.
async fn report_device_status(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    req: &CheckinRequest,
) {
    use crate::controllers::{FIELD_MANAGER_GATEWAY_BACKUPS, FIELD_MANAGER_GATEWAY_PACKAGES};

    if let Some(ref versions) = req.package_versions {
        apply_status_map(
            client,
            namespace,
            name,
            &req.device_id,
            FIELD_MANAGER_GATEWAY_PACKAGES,
            DeviceReportedStatus {
                package_versions: Some(versions),
                backup_schedule_owners: None,
            },
        )
        .await;
    }
    if let Some(ref owners) = req.backup_schedule_owners {
        apply_status_map(
            client,
            namespace,
            name,
            &req.device_id,
            FIELD_MANAGER_GATEWAY_BACKUPS,
            DeviceReportedStatus {
                package_versions: None,
                backup_schedule_owners: Some(owners),
            },
        )
        .await;
    }
}

/// The status an apply carries: the one map its field manager owns, and nothing
/// else. A field left `None` is absent from the body, which is what keeps a
/// manager from claiming the other manager's map.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceReportedStatus<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    package_versions: Option<&'a BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_schedule_owners: Option<&'a BTreeMap<String, String>>,
}

/// An apply body is a whole object, not a fragment: the API server reads the
/// type and the name from it. The maps are borrowed straight into the encoder,
/// so a check-in does not clone its own report to send it.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MachineConfigStatusApply<'a> {
    api_version: std::borrow::Cow<'a, str>,
    kind: std::borrow::Cow<'a, str>,
    metadata: ApplyMetadata<'a>,
    status: DeviceReportedStatus<'a>,
}

#[derive(Debug, serde::Serialize)]
struct ApplyMetadata<'a> {
    name: &'a str,
}

/// One map, one manager, one forced apply.
async fn apply_status_map(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    device_id: &str,
    field_manager: &str,
    status: DeviceReportedStatus<'_>,
) {
    use crate::crds::MachineConfig;
    use kube::api::{Api, Patch, PatchParams};

    let machines: Api<MachineConfig> = Api::namespaced(client.clone(), namespace);
    let body = MachineConfigStatusApply {
        api_version: <MachineConfig as kube::Resource>::api_version(&()),
        kind: <MachineConfig as kube::Resource>::kind(&()),
        metadata: ApplyMetadata { name },
        status,
    };
    if let Err(e) = machines
        .patch_status(
            name,
            &PatchParams::apply(field_manager).force(),
            &Patch::Apply(&body),
        )
        .await
    {
        tracing::error!(
            machine_config = %name,
            namespace = %namespace,
            device_id = %device_id,
            field_manager = %field_manager,
            error = %e,
            "device-reported MachineConfig status was not written; the check-in still succeeded"
        );
    }
}

/// The cadences a cluster `BackupPolicy` owns for `hostname`, keyed by unit
/// name.
///
/// Read from the controllers' own watch cache where one stands, and from a
/// namespaced list where none does: the cache is empty in a standalone gateway
/// and while a controller run is still completing its first list, and an
/// unpopulated cache answers exactly as a cluster that schedules nothing would.
///
/// Only a row the controller wrote as `cluster` projects: an owner word read
/// any other way — `local`, or one no layer spells — is the machine's own
/// refusal or an answer the policy could not be read for, and sending either
/// would be the silent override the whole surface exists to prevent. The word
/// is read through `ScheduleOwner`'s case-insensitive parser, the same reading
/// the controller gives the device's own pin.
///
/// Two policies naming one unit for one machine is a cluster-side conflict the
/// gateway cannot resolve on merit, so it resolves it stably: the policy with
/// the older `creationTimestamp` wins and the collision is logged. A stable
/// answer is what keeps a machine from alternating between two cadences on
/// successive check-ins.
async fn policy_owned_schedules(
    state: &SharedState,
    client: &kube::Client,
    namespace: &str,
    hostname: &str,
) -> Option<BTreeMap<String, BackupScheduleProjection>> {
    use crate::crds::BackupPolicy;
    use kube::api::{Api, ListParams};

    let policies = match cached_policies(state, namespace) {
        Some(cached) => cached,
        None => {
            let api: Api<BackupPolicy> = Api::namespaced(client.clone(), namespace);
            match api.list(&ListParams::default()).await {
                Ok(list) => list.items.into_iter().map(Arc::new).collect(),
                Err(e) => {
                    tracing::warn!(
                        namespace = %namespace,
                        error = %e,
                        "failed to list BackupPolicies for a device check-in; the device is told nothing rather than that the cluster owns nothing"
                    );
                    return None;
                }
            }
        }
    };

    Some(project_owned_schedules(&policies, namespace, hostname))
}

/// The namespace's policies out of the published watch cache, or `None` while
/// no cache stands ready to answer.
fn cached_policies(
    state: &SharedState,
    namespace: &str,
) -> Option<Vec<Arc<crate::crds::BackupPolicy>>> {
    use futures::FutureExt;

    let store = state.backup_policies.get()?;
    // A cache that has not completed its initial list holds nothing and says
    // so exactly as an empty cluster would, so a check-in landing in that
    // window lists rather than telling the device the cluster owns nothing.
    store.wait_until_ready().now_or_never()?.ok()?;
    Some(store.state_filter(|policy| policy.metadata.namespace.as_deref() == Some(namespace)))
}

/// Fold the policies scheduling `hostname` into one cadence per unit.
fn project_owned_schedules(
    policies: &[Arc<crate::crds::BackupPolicy>],
    namespace: &str,
    hostname: &str,
) -> BTreeMap<String, BackupScheduleProjection> {
    use crate::crds::ScheduleOwner;
    use kube::ResourceExt;

    let mut ordered: Vec<&Arc<crate::crds::BackupPolicy>> = policies.iter().collect();
    // Name breaks a timestamp tie, so two policies created in the same second
    // still resolve to one winner on every check-in.
    ordered.sort_by_cached_key(|p| (p.creation_timestamp(), p.name_any()));

    let mut projected: BTreeMap<String, BackupScheduleProjection> = BTreeMap::new();
    let mut claimed_by: BTreeMap<String, String> = BTreeMap::new();
    for policy in ordered {
        let Some(status) = policy.status.as_ref() else {
            continue;
        };
        let policy_name = policy.name_any();
        for row in &status.units {
            if row.hostname != hostname
                || !matches!(
                    row.owner.parse::<ScheduleOwner>(),
                    Ok(ScheduleOwner::Cluster)
                )
            {
                continue;
            }
            // A cluster-owned row always carries the schedule the policy set;
            // one that does not describes nothing this machine could run.
            let Some(schedule) = row.schedule.clone() else {
                continue;
            };
            match projected.entry(row.name.clone()) {
                Entry::Vacant(slot) => {
                    slot.insert(BackupScheduleProjection {
                        schedule,
                        retention: row.retention,
                    });
                    claimed_by.insert(row.name.clone(), policy_name.clone());
                }
                Entry::Occupied(_) => {
                    tracing::warn!(
                        namespace = %namespace,
                        hostname = %hostname,
                        unit = %row.name,
                        applied = %claimed_by.get(&row.name).map_or("", String::as_str),
                        ignored = %policy_name,
                        "two BackupPolicies schedule one backup unit for one machine; the older policy wins"
                    );
                }
            }
        }
    }
    projected
}

pub(super) async fn list_devices(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(pagination): Query<PaginationParams>,
) -> Result<impl IntoResponse, GatewayError> {
    // Device auth: can only list self
    if let AuthContext::Device { ref device_id, .. } = auth {
        let device = state.db.get_device(device_id).await?;
        return Ok(Json(vec![device]));
    }
    let limit = pagination.limit.min(1000);
    let devices = state
        .db
        .list_devices_paginated(limit, pagination.offset)
        .await?;
    Ok(Json(devices))
}

pub(super) async fn get_device(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, GatewayError> {
    enforce_device_access(&auth, &id)?;
    let device = state.db.get_device(&id).await?;
    Ok(Json(device))
}

pub(super) async fn set_device_config(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(req): Json<SetConfigRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    // Only admin can push config to devices
    if !matches!(auth, AuthContext::Admin) {
        return Err(GatewayError::Forbidden(
            "only admin can push config to devices".to_string(),
        ));
    }
    // Authoritative config-size policy. The route's DefaultBodyLimit sits
    // above this (see MAX_REQUEST_BODY_BYTES) so an over-policy config reaches
    // this check and gets the specific 400 below, rather than a generic 413.
    let json_str = serde_json::to_string(&req.config)
        .map_err(|e| GatewayError::InvalidRequest(e.to_string()))?;
    if json_str.len() > super::MAX_CONFIG_BYTES {
        return Err(GatewayError::InvalidRequest(
            "config exceeds 10MB size limit".to_string(),
        ));
    }

    state.db.set_device_config(&id, &req.config).await?;

    tracing::info!(device_id = %id, "desired config updated");

    Ok(StatusCode::NO_CONTENT)
}

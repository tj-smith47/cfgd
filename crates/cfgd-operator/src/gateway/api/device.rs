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
    let backup_schedules = match &state.kube_client {
        Some(client) => {
            match find_machine_config_ref(client, &req.hostname).await {
                Some((namespace, name)) => {
                    report_device_status(client, &namespace, &name, &req).await;
                    policy_owned_schedules(client, &namespace, &req.hostname).await
                }
                // No MachineConfig names this hostname: there is nothing to
                // write a status onto and no namespace to look for policies in.
                None => BTreeMap::new(),
            }
        }
        None => BTreeMap::new(),
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
/// the hostname resolved to.
///
/// A failure is a `warn` and nothing more: the check-in's outcome is the
/// device's, and it does not depend on the cluster accepting a status.
///
/// Server-side apply under the gateway's own field manager, so the two maps are
/// OWNED here: a key the device stopped reporting — a package it uninstalled, a
/// backup unit it no longer declares — is pruned by the same write that carries
/// the rest, and the fields the controller computes are left alone because this
/// manager never claimed them. The apply is not forced: a conflict means
/// something else took these fields, which is a fact to report rather than
/// overwrite.
///
/// A map the device did not OBSERVE is absent from the request, and absent from
/// the body: `packageVersions` and `backupScheduleOwners` are facts the
/// controller cannot see for itself, so an older agent, or one whose managers
/// could not be queried, must not blank what the cluster still holds. Because an
/// apply prunes what it omits, a check-in that observed neither map writes
/// nothing at all, and one that observed only some writes only those.
async fn report_device_status(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    req: &CheckinRequest,
) {
    use crate::controllers::FIELD_MANAGER_GATEWAY;
    use crate::crds::MachineConfig;
    use kube::api::{Api, Patch, PatchParams};

    let mut status = serde_json::Map::new();
    if let Some(ref versions) = req.package_versions {
        status.insert("packageVersions".to_string(), serde_json::json!(versions));
    }
    if let Some(ref owners) = req.backup_schedule_owners {
        status.insert(
            "backupScheduleOwners".to_string(),
            serde_json::json!(owners),
        );
    }
    if status.is_empty() {
        return;
    }

    let machines: Api<MachineConfig> = Api::namespaced(client.clone(), namespace);
    // An apply body is a whole object, not a fragment: the API server reads the
    // type and the name from it.
    let patch = serde_json::json!({
        "apiVersion": <MachineConfig as kube::Resource>::api_version(&()),
        "kind": <MachineConfig as kube::Resource>::kind(&()),
        "metadata": { "name": name },
        "status": serde_json::Value::Object(status),
    });
    if let Err(e) = machines
        .patch_status(
            name,
            &PatchParams::apply(FIELD_MANAGER_GATEWAY),
            &Patch::Apply(patch),
        )
        .await
    {
        tracing::warn!(
            machine_config = %name,
            namespace = %namespace,
            device_id = %req.device_id,
            error = %e,
            "device-reported MachineConfig status was not written; the check-in still succeeded"
        );
    }
}

/// The cadences a cluster `BackupPolicy` owns for `hostname`, keyed by unit
/// name.
///
/// A live list, like [`find_machine_config_ref`]. Only a row the controller
/// wrote as `cluster` projects: an owner word read any other way — `local`, or
/// one no layer spells — is the machine's own refusal or an answer the policy
/// could not read, and sending either would be the silent override the whole
/// surface exists to prevent. The word is read through `ScheduleOwner`'s
/// case-insensitive parser, the same reading the controller gives the device's
/// own pin.
///
/// Two policies naming one unit for one machine is a cluster-side conflict the
/// gateway cannot resolve on merit, so it resolves it stably: the policy with
/// the older `creationTimestamp` wins and the collision is logged. A stable
/// answer is what keeps a machine from alternating between two cadences on
/// successive check-ins.
async fn policy_owned_schedules(
    client: &kube::Client,
    namespace: &str,
    hostname: &str,
) -> BTreeMap<String, BackupScheduleProjection> {
    use crate::crds::{BackupPolicy, ScheduleOwner};
    use kube::ResourceExt;
    use kube::api::{Api, ListParams};

    let policies: Api<BackupPolicy> = Api::namespaced(client.clone(), namespace);
    let list = match policies.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(
                namespace = %namespace,
                error = %e,
                "failed to list BackupPolicies for a device check-in; no cluster schedule was sent"
            );
            return BTreeMap::new();
        }
    };

    let mut ordered: Vec<&BackupPolicy> = list.items.iter().collect();
    // Name breaks a timestamp tie, so two policies created in the same second
    // still resolve to one winner on every check-in.
    ordered.sort_by_key(|p| (p.creation_timestamp(), p.name_any()));

    let mut projected: BTreeMap<String, BackupScheduleProjection> = BTreeMap::new();
    let mut claimed_by: BTreeMap<String, String> = BTreeMap::new();
    for policy in ordered {
        let Some(status) = policy.status.as_ref() else {
            continue;
        };
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
                    claimed_by.insert(row.name.clone(), policy.name_any());
                }
                Entry::Occupied(_) => {
                    tracing::warn!(
                        namespace = %namespace,
                        hostname = %hostname,
                        unit = %row.name,
                        applied = %claimed_by.get(&row.name).map_or("", String::as_str),
                        ignored = %policy.name_any(),
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

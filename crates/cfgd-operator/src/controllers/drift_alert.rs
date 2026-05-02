use std::sync::Arc;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Client, Resource, ResourceExt};
use tracing::{info, warn};

use crate::crds::{DriftAlert, DriftAlertStatus, MachineConfig};
use crate::errors::OperatorError;
use crate::metrics::DriftLabels;

use super::{
    ControllerContext, FIELD_MANAGER_OPERATOR, FIELD_MANAGER_STATUS, build_drift_alert_conditions,
    emit_event, namespaced_api, record_reconcile_metrics, record_reconcile_success,
};
pub(super) async fn reconcile_drift_alert(
    obj: Arc<DriftAlert>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    let mc_name = &obj.spec.machine_config_ref.name;
    let mc_namespace = obj
        .spec
        .machine_config_ref
        .namespace
        .as_deref()
        .unwrap_or(&namespace);

    info!(
        name = %name,
        machine_config = %mc_name,
        device_id = %obj.spec.device_id,
        severity = ?obj.spec.severity,
        details_count = obj.spec.drift_details.len(),
        "reconciling DriftAlert"
    );

    let machines: Api<MachineConfig> = namespaced_api(&ctx.client, mc_namespace)?;

    match machines.get(mc_name).await {
        Ok(mc) => {
            // Require valid UID for owner reference (H7 fix)
            let mc_uid = mc.metadata.uid.clone().ok_or_else(|| {
                OperatorError::Reconciliation(format!(
                    "MachineConfig {} has no UID — cannot set owner reference",
                    mc.name_any()
                ))
            })?;

            let owner_ref = OwnerReference {
                api_version: cfgd_core::API_VERSION.to_string(),
                kind: "MachineConfig".to_string(),
                name: mc.name_any(),
                uid: mc_uid,
                controller: Some(true),
                block_owner_deletion: Some(true),
            };

            let existing_owners = obj.metadata.owner_references.as_deref().unwrap_or(&[]);
            let has_owner_ref = existing_owners.iter().any(|r| {
                r.kind == "MachineConfig" && r.name == owner_ref.name && r.uid == owner_ref.uid
            });

            if !has_owner_ref {
                let mut updated_owners: Vec<OwnerReference> = existing_owners.to_vec();
                updated_owners.push(owner_ref);
                let patch = serde_json::json!({
                    "metadata": {
                        "ownerReferences": updated_owners
                    }
                });
                let da_api: Api<DriftAlert> = namespaced_api(&ctx.client, &namespace)?;
                da_api
                    .patch(
                        &name,
                        &PatchParams::apply(FIELD_MANAGER_OPERATOR),
                        &Patch::Merge(patch),
                    )
                    .await
                    .map_err(|e| {
                        OperatorError::Reconciliation(format!(
                            "failed to set owner reference on DriftAlert {name}: {e}"
                        ))
                    })?;
                info!(name = %name, machine_config = %mc.name_any(), "set owner reference on DriftAlert");
            }

            // Check if a DriftDetected condition already exists
            let has_drift_condition = mc
                .status
                .as_ref()
                .map(|s| {
                    s.conditions
                        .iter()
                        .any(|c| c.condition_type == "DriftDetected" && c.status == "True")
                })
                .unwrap_or(false);

            // If MC has no drift condition and no drift details, this alert is resolved — delete it
            if !has_drift_condition && obj.spec.drift_details.is_empty() {
                let alerts: Api<DriftAlert> = namespaced_api(&ctx.client, &namespace)?;

                let now = cfgd_core::utc_now_iso8601();
                let da_status = serde_json::json!({
                    "status": DriftAlertStatus {
                        detected_at: obj.status.as_ref().and_then(|s| s.detected_at.clone()),
                        resolved_at: Some(now.clone()),
                        conditions: build_drift_alert_conditions(
                            &obj.spec.severity,
                            true,
                            &obj.spec.device_id,
                            obj.spec.drift_details.len(),
                            &now,
                            obj.meta().generation,
                        ),
                    }
                });
                if let Err(e) = alerts
                    .patch_status(
                        &name,
                        &PatchParams::apply(FIELD_MANAGER_STATUS),
                        &Patch::Merge(da_status),
                    )
                    .await
                {
                    warn!(name = %name, error = %e, "failed to set Resolved condition on DriftAlert");
                }

                emit_event(
                    &ctx.recorder,
                    &obj.object_ref(&()),
                    EventType::Normal,
                    "DriftResolved",
                    format!("DriftAlert {} resolved", name),
                    "DriftCheck",
                )
                .await;

                if let Err(e) = alerts.delete(&name, &Default::default()).await {
                    warn!(name = %name, error = %e, "failed to delete resolved DriftAlert");
                }

                record_reconcile_success(&ctx, "drift_alert", start);

                return Ok(Action::requeue(std::time::Duration::from_secs(60)));
            }

            if !has_drift_condition {
                ctx.metrics
                    .drift_events_total
                    .get_or_create(&DriftLabels {
                        severity: format!("{:?}", obj.spec.severity),
                        namespace: namespace.clone(),
                    })
                    .inc();

                let now = cfgd_core::utc_now_iso8601();
                let mc_generation = mc.meta().generation;
                let mc_status = serde_json::json!({
                    "status": {
                        "conditions": [
                            {
                                "type": "DriftDetected",
                                "status": "True",
                                "reason": "DriftActive",
                                "message": format!(
                                    "Drift detected on device {} — {} detail(s)",
                                    obj.spec.device_id,
                                    obj.spec.drift_details.len()
                                ),
                                "lastTransitionTime": now.clone(),
                                "observedGeneration": mc_generation,
                            }
                        ]
                    }
                });

                machines
                    .patch_status(
                        mc_name,
                        &PatchParams::apply(FIELD_MANAGER_STATUS),
                        &Patch::Merge(mc_status),
                    )
                    .await
                    .map_err(|e| {
                        OperatorError::Reconciliation(format!(
                            "failed to update drift status for MachineConfig {mc_name}: {e}"
                        ))
                    })?;

                info!(
                    machine_config = %mc_name,
                    "machineConfig drift condition set"
                );

                emit_event(
                    &ctx.recorder,
                    &mc.object_ref(&()),
                    EventType::Warning,
                    "DriftDetected",
                    format!(
                        "Drift detected from device {} — {} details",
                        obj.spec.device_id,
                        obj.spec.drift_details.len()
                    ),
                    "DriftCheck",
                )
                .await;

                // Patch DriftAlert status with Resolved=False condition
                let da_api: Api<DriftAlert> = namespaced_api(&ctx.client, &namespace)?;
                let da_status = serde_json::json!({
                    "status": DriftAlertStatus {
                        detected_at: Some(now.clone()),
                        resolved_at: None,
                        conditions: build_drift_alert_conditions(
                            &obj.spec.severity,
                            false,
                            &obj.spec.device_id,
                            obj.spec.drift_details.len(),
                            &now,
                            obj.meta().generation,
                        ),
                    }
                });
                da_api
                    .patch_status(
                        &name,
                        &PatchParams::apply(FIELD_MANAGER_STATUS),
                        &Patch::Merge(da_status),
                    )
                    .await
                    .map_err(|e| {
                        OperatorError::Reconciliation(format!(
                            "failed to update DriftAlert status for {name}: {e}"
                        ))
                    })?;
            }
        }
        Err(kube::Error::Api(resp)) if resp.code == 404 => {
            warn!(
                machine_config = %mc_name,
                "driftAlert references non-existent MachineConfig"
            );
            // Record as error, not success (H6 fix)
            record_reconcile_metrics(&ctx, "drift_alert", "error", start);
            return Ok(Action::requeue(std::time::Duration::from_secs(60)));
        }
        Err(e) => {
            return Err(OperatorError::Reconciliation(format!(
                "failed to get MachineConfig {mc_name}: {e}"
            )));
        }
    }

    record_reconcile_success(&ctx, "drift_alert", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

/// Check whether any active DriftAlerts exist for a MachineConfig.
/// Matches by spec.machineConfigRef.name since labels may not be set.
pub(super) async fn has_active_drift_alerts(
    client: &Client,
    namespace: &str,
    mc_name: &str,
) -> bool {
    let alerts: Api<DriftAlert> = if namespace.is_empty() {
        Api::all(client.clone())
    } else {
        Api::namespaced(client.clone(), namespace)
    };

    // List all DriftAlerts and filter by machineConfigRef.name since labels are not guaranteed
    match alerts.list(&ListParams::default()).await {
        Ok(list) => list
            .items
            .iter()
            .any(|da| da.spec.machine_config_ref.name == mc_name),
        Err(e) => {
            warn!(error = %e, mc_name = %mc_name, "failed to list DriftAlerts for drift check");
            false
        }
    }
}

pub(super) async fn cleanup_drift_alerts(client: &Client, namespace: &str, mc_name: &str) {
    let alerts: Api<DriftAlert> = if namespace.is_empty() {
        Api::all(client.clone())
    } else {
        Api::namespaced(client.clone(), namespace)
    };

    // List all and filter by machineConfigRef.name (labels may not be set)
    let list = match alerts.list(&ListParams::default()).await {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, "failed to list DriftAlerts for cleanup");
            return;
        }
    };

    for alert in list {
        if alert.spec.machine_config_ref.name != mc_name {
            continue;
        }
        let alert_name = alert.name_any();
        if let Err(e) = alerts.delete(&alert_name, &Default::default()).await {
            warn!(
                name = %alert_name,
                error = %e,
                "failed to delete resolved DriftAlert"
            );
        } else {
            info!(name = %alert_name, "deleted resolved DriftAlert");
        }
    }
}

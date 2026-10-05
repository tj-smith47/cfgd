use std::sync::Arc;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Resource, ResourceExt};
use tracing::{info, warn};

use crate::crds::{DriftAlert, DriftAlertStatus, MachineConfig};
use crate::errors::OperatorError;
use crate::metrics::DriftLabels;

use super::{
    ControllerContext, ControllerStores, FIELD_MANAGER_OPERATOR, FIELD_MANAGER_STATUS,
    build_condition, build_drift_alert_conditions, emit_event, namespaced_api,
    record_reconcile_metrics, record_reconcile_success, upsert_condition,
};
pub(super) async fn reconcile_drift_alert(
    obj: Arc<DriftAlert>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    let (mc_namespace, mc_name) = alert_target(&obj);

    info!(
        name = %name,
        machine_config = %mc_name,
        device_id = %obj.spec.device_id,
        severity = ?obj.spec.severity,
        details_count = obj.spec.drift_details.len(),
        "reconciling DriftAlert"
    );

    let machines: Api<MachineConfig> = namespaced_api(&ctx.client, mc_namespace)?;

    match ctx.stores.machine_config(mc_namespace, mc_name).await? {
        Some(mc) => {
            // An owner reference without a UID is silently dropped by the
            // API server, so a missing UID must fail loudly instead.
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

            let has_drift_condition = reports_drift(&mc);

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

            // The machine is read from the cache, which can still predate the
            // DriftDetected patch below when this alert's own status patch
            // re-runs it; the alert's recorded detection is what says the drift
            // was already reported.
            let already_reported = obj
                .status
                .as_ref()
                .is_some_and(|s| s.detected_at.is_some() && s.resolved_at.is_none());

            if !has_drift_condition && !already_reported {
                ctx.metrics
                    .drift_events_total
                    .get_or_create(&DriftLabels {
                        severity: format!("{:?}", obj.spec.severity),
                        namespace: namespace.clone(),
                    })
                    .inc();

                let now = cfgd_core::utc_now_iso8601();
                let mc_existing_conditions = mc
                    .status
                    .as_ref()
                    .map(|s| s.conditions.as_slice())
                    .unwrap_or(&[]);
                let (drift_status, drift_reason, drift_message) =
                    super::machine_config::drift_detected(true, mc_name);
                let drift_condition = build_condition(
                    mc_existing_conditions,
                    "DriftDetected",
                    drift_status,
                    drift_reason,
                    &drift_message,
                    &now,
                    mc.meta().generation,
                );
                let mc_status = serde_json::json!({
                    "status": {
                        "conditions": upsert_condition(mc_existing_conditions, drift_condition)
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
                            "failed to update the DriftDetected condition of MachineConfig {mc_name}: {e}"
                        ))
                    })?;

                info!(
                    machine_config = %mc_name,
                    // fleet-drift-ok: a journal line naming the condition it patched
                    "machineConfig drift condition set"
                );

                emit_event(
                    &ctx.recorder,
                    &mc.object_ref(&()),
                    EventType::Warning,
                    "DriftDetected",
                    format!(
                        "Device {} reported {}",
                        obj.spec.device_id,
                        cfgd_core::pluralize(
                            obj.spec.drift_details.len(),
                            "drifted system setting"
                        )
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
        None => {
            warn!(
                machine_config = %mc_name,
                "driftAlert references non-existent MachineConfig"
            );
            // A dangling MachineConfig reference is a reconcile error, not a
            // successful no-op — success here would hide it from the metrics.
            record_reconcile_metrics(&ctx, "drift_alert", "error", start);
            return Ok(Action::requeue(std::time::Duration::from_secs(60)));
        }
    }

    record_reconcile_success(&ctx, "drift_alert", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

/// The `(namespace, name)` of the MachineConfig an alert reports on. A
/// reference with no namespace names a machine in the alert's own namespace.
///
/// Every reader matching an alert to its machine resolves the target here, so
/// the machine controller, this controller and the watch between them agree on
/// which alerts belong to which machine.
pub(super) fn alert_target(alert: &DriftAlert) -> (&str, &str) {
    let target = &alert.spec.machine_config_ref;
    let namespace = target
        .namespace
        .as_deref()
        .or(alert.metadata.namespace.as_deref())
        .unwrap_or_default();
    (namespace, target.name.as_str())
}

/// Whether `mc` carries a True DriftDetected condition: the one fact about
/// its machine's status an alert's verdict reads.
pub(super) fn reports_drift(mc: &MachineConfig) -> bool {
    mc.status.as_ref().is_some_and(|s| {
        s.conditions
            .iter()
            .any(|c| c.condition_type == "DriftDetected" && c.status == "True")
    })
}

/// Check whether any active DriftAlerts exist for a MachineConfig.
/// Matches by the alert's resolved target since labels may not be set.
///
/// Read from the DriftAlert watch cache the DriftAlert controller already
/// maintains: a LIST here would be one per MachineConfig per reconcile sweep,
/// quadratic across a fleet. Every namespace is read, because an
/// alert may live outside the namespace of the machine it names.
///
/// The caller must treat this single snapshot as the only view of alert state:
/// a follow-up read to act on "no alerts" races with alert creation and can
/// delete a freshly created alert before its first reconcile.
pub(super) async fn has_active_drift_alerts(
    stores: &ControllerStores,
    namespace: &str,
    mc_name: &str,
) -> Result<bool, OperatorError> {
    Ok(stores
        .drift_alerts_in("")
        .await?
        .iter()
        .any(|da| alert_target(da) == (namespace, mc_name)))
}

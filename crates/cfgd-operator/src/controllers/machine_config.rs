use std::sync::Arc;

use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Resource, ResourceExt};
use tracing::info;

use crate::crds::{MachineConfig, MachineConfigSpec, MachineConfigStatus};
use crate::errors::OperatorError;
use crate::metrics::DriftLabels;

use super::drift_alert::{cleanup_drift_alerts, has_active_drift_alerts};
use super::module::resolve_module_refs;
use super::{
    ControllerContext, FIELD_MANAGER_OPERATOR, FIELD_MANAGER_STATUS, MACHINE_CONFIG_FINALIZER,
    build_condition, emit_event, find_condition_status, namespaced_api, record_reconcile_success,
};

pub(super) async fn reconcile_machine_config(
    obj: Arc<MachineConfig>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();

    let machines_api: Api<MachineConfig> = namespaced_api(&ctx.client, &namespace)?;

    let finalizers = obj.metadata.finalizers.as_deref().unwrap_or(&[]);
    let has_finalizer = finalizers.iter().any(|f| f == MACHINE_CONFIG_FINALIZER);

    if obj.metadata.deletion_timestamp.is_some() && has_finalizer {
        info!(name = %name, "machineConfig being deleted, running cleanup");
        let updated: Vec<&str> = finalizers
            .iter()
            .filter(|f| f.as_str() != MACHINE_CONFIG_FINALIZER)
            .map(|f| f.as_str())
            .collect();
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": updated
            }
        });
        machines_api
            .patch(
                &name,
                &PatchParams::apply(FIELD_MANAGER_OPERATOR),
                &Patch::Merge(patch),
            )
            .await
            .map_err(|e| {
                OperatorError::Reconciliation(format!(
                    "failed to remove finalizer from {name}: {e}"
                ))
            })?;
        return Ok(Action::await_change());
    }

    if obj.metadata.deletion_timestamp.is_none() && !has_finalizer {
        let mut updated: Vec<String> = finalizers.to_vec();
        updated.push(MACHINE_CONFIG_FINALIZER.to_string());
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": updated
            }
        });
        machines_api
            .patch(
                &name,
                &PatchParams::apply(FIELD_MANAGER_OPERATOR),
                &Patch::Merge(patch),
            )
            .await
            .map_err(|e| {
                OperatorError::Reconciliation(format!("failed to add finalizer to {name}: {e}"))
            })?;
        info!(name = %name, "added finalizer to MachineConfig");
    }

    if let Err(e) = validate_spec(&obj.spec) {
        let error_msg = e.to_string();
        emit_event(
            &ctx.recorder,
            &obj.object_ref(&()),
            EventType::Warning,
            "ReconcileError",
            format!("Reconciliation failed for {}: {}", name, error_msg),
            "Reconcile",
        )
        .await;
        return Err(e);
    }

    info!(
        name = %name,
        namespace = %namespace,
        hostname = %obj.spec.hostname,
        profile = %obj.spec.profile,
        packages = obj.spec.packages.len(),
        files = obj.spec.files.len(),
        "reconciling MachineConfig"
    );

    let current_generation = obj.meta().generation;
    let existing_status = obj.status.as_ref();
    let observed_generation = existing_status.and_then(|s| s.observed_generation);
    let existing_conditions = existing_status
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);

    // Check if any DriftAlerts exist for this MachineConfig
    let has_drift = has_active_drift_alerts(&ctx.client, &namespace, &name).await;

    // Skip if we've already observed this generation, no drift, and condition already reflects that
    let generation_unchanged =
        current_generation.is_some() && current_generation == observed_generation;
    let had_drift = existing_conditions
        .iter()
        .any(|c| c.condition_type == "DriftDetected" && c.status == "True");
    if generation_unchanged && !has_drift && !had_drift {
        info!(name = %name, "already reconciled this generation, skipping");
        return Ok(Action::requeue(std::time::Duration::from_secs(60)));
    }

    // If not drifted, clean up any stale DriftAlerts for this MachineConfig
    if !has_drift {
        cleanup_drift_alerts(&ctx.client, &namespace, &name).await;
    }

    // Resolve moduleRefs against Module CRDs (cluster-scoped)
    let (modules_resolved_status, modules_resolved_reason, modules_resolved_message) =
        resolve_module_refs(&ctx.client, &obj.spec.module_refs).await;

    let now = cfgd_core::utc_now_iso8601();

    let (drift_status, drift_reason, drift_message) = if has_drift {
        (
            "True",
            "DriftActive",
            format!("MachineConfig {} has detected drift on device", name),
        )
    } else {
        (
            "False",
            "NoDrift",
            format!("No drift detected for MachineConfig {}", name),
        )
    };

    // Preserve existing package_versions from status (C1 fix)
    let existing_package_versions = existing_status
        .map(|s| s.package_versions.clone())
        .unwrap_or_default();

    let status = serde_json::json!({
        "status": MachineConfigStatus {
            last_reconciled: Some(now.clone()),
            observed_generation: current_generation,
            conditions: vec![
                build_condition(
                    existing_conditions,
                    "Reconciled", "True", "ReconcileSuccess",
                    &format!("MachineConfig {} reconciled successfully", name),
                    &now, current_generation,
                ),
                build_condition(
                    existing_conditions,
                    "DriftDetected", drift_status, drift_reason,
                    &drift_message,
                    &now, current_generation,
                ),
                build_condition(
                    existing_conditions,
                    "ModulesResolved", modules_resolved_status, modules_resolved_reason,
                    &modules_resolved_message,
                    &now, current_generation,
                ),
                // Compliant is set by policy controllers — preserve existing value
                build_condition(
                    existing_conditions,
                    "Compliant",
                    find_condition_status(existing_conditions, "Compliant")
                        .as_deref()
                        .unwrap_or("Unknown"),
                    find_condition_status(existing_conditions, "Compliant")
                        .as_deref()
                        .map(|s| if s == "True" { "PolicyCompliant" } else if s == "False" { "PolicyViolation" } else { "NotEvaluated" })
                        .unwrap_or("NotEvaluated"),
                    find_condition_status(existing_conditions, "Compliant")
                        .as_deref()
                        .map(|_| "Policy compliance evaluated by ConfigPolicy controller")
                        .unwrap_or("Awaiting policy evaluation"),
                    &now, current_generation,
                ),
            ],
            package_versions: existing_package_versions,
        }
    });

    if let Err(e) = machines_api
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER_STATUS),
            &Patch::Merge(status),
        )
        .await
    {
        let error_msg = format!("failed to update status for {name}: {e}");
        emit_event(
            &ctx.recorder,
            &obj.object_ref(&()),
            EventType::Warning,
            "ReconcileError",
            format!("Reconciliation failed for {}: {}", name, error_msg),
            "Reconcile",
        )
        .await;
        return Err(OperatorError::Reconciliation(error_msg));
    }

    info!(name = %name, "status updated with last_reconciled timestamp");

    emit_event(
        &ctx.recorder,
        &obj.object_ref(&()),
        EventType::Normal,
        "Reconciled",
        format!("MachineConfig {} reconciled successfully", name),
        "Reconcile",
    )
    .await;

    if has_drift {
        emit_event(
            &ctx.recorder,
            &obj.object_ref(&()),
            EventType::Warning,
            "DriftDetected",
            format!("Drift detected on device for MachineConfig {}", name),
            "DriftCheck",
        )
        .await;

        ctx.metrics
            .drift_events_total
            .get_or_create(&DriftLabels {
                severity: "warning".to_string(),
                namespace: namespace.clone(),
            })
            .inc();
    }

    record_reconcile_success(&ctx, "machine_config", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

pub(super) fn validate_spec(spec: &MachineConfigSpec) -> Result<(), OperatorError> {
    spec.validate()
        .map_err(|errors| OperatorError::InvalidSpec(errors.join("; ")))
}

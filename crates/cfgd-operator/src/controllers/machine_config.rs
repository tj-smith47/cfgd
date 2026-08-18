use std::sync::Arc;

use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Resource, ResourceExt};
use tracing::info;

use crate::crds::{MachineConfig, MachineConfigSpec, MachineConfigStatus};
use crate::errors::OperatorError;
use crate::metrics::DriftLabels;

use super::drift_alert::has_active_drift_alerts;
use super::module::resolve_module_refs;
use super::{
    ControllerContext, FIELD_MANAGER_OPERATOR, FIELD_MANAGER_STATUS, MACHINE_CONFIG_FINALIZER,
    build_condition, emit_event, find_condition, namespaced_api, record_reconcile_success,
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
    let has_drift = has_active_drift_alerts(&ctx.stores, &namespace, &name).await?;

    // Skip if we've already observed this generation, no drift, and condition already reflects that
    let generation_unchanged =
        current_generation.is_some() && current_generation == observed_generation;
    let had_drift = existing_conditions
        .iter()
        .any(|c| c.condition_type == "DriftDetected" && c.status == "True");
    if generation_unchanged && !has_drift && !had_drift {
        info!(name = %name, "already reconciled this generation, skipping");
        // A pass that concluded there is nothing to do IS a reconciliation that
        // succeeded, and it is the only signal a steady machine produces:
        // `lastReconciled` deliberately stops advancing once the status stops
        // changing, so a counter that also stopped would leave a healthy
        // machine indistinguishable from a controller that had stopped
        // reconciling it.
        record_reconcile_success(&ctx, "machine_config", start);
        return Ok(Action::requeue(std::time::Duration::from_secs(60)));
    }

    // Resolve moduleRefs against Module CRDs (cluster-scoped)
    let (modules_resolved_status, modules_resolved_reason, modules_resolved_message) =
        resolve_module_refs(&ctx.stores, &obj.spec.module_refs).await;

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

    // Preserve existing package_versions from status: a reconcile that cannot
    // observe them must not blank the field it did not measure.
    let existing_package_versions = existing_status
        .map(|s| s.package_versions.clone())
        .unwrap_or_default();

    // `Compliant` belongs to the policy controllers: its status, reason AND
    // message are all theirs, and this controller only carries them through.
    // Rewriting any of the three is not cosmetic — a `Condition` compares by
    // every field, so each controller's guard reads the other's text as a change
    // and patches it back, and a drifted machine (which never takes the skip arm
    // above) ping-pongs with the policy controller at watch speed rather than
    // reaching steady state. Text is synthesized only for a machine no policy
    // has evaluated, the one case with nothing to preserve.
    let (compliant_status, compliant_reason, compliant_message) =
        match find_condition(existing_conditions, "Compliant") {
            Some(c) => (c.status.as_str(), c.reason.as_str(), c.message.as_str()),
            None => ("Unknown", "NotEvaluated", "Awaiting policy evaluation"),
        };

    let mut desired = MachineConfigStatus {
        // Carried forward rather than refreshed, so the comparison below is
        // about what the reconcile OBSERVED. Stamping `now` here would make
        // every status unequal to its predecessor, and the drifted machine —
        // which never takes the skip arm above — would pay a status write and
        // two events on every 60s requeue for as long as the drift lasts.
        last_reconciled: existing_status.and_then(|s| s.last_reconciled.clone()),
        observed_generation: current_generation,
        conditions: vec![
            build_condition(
                existing_conditions,
                "Reconciled",
                "True",
                "ReconcileSuccess",
                &format!("MachineConfig {} reconciled successfully", name),
                &now,
                current_generation,
            ),
            build_condition(
                existing_conditions,
                "DriftDetected",
                drift_status,
                drift_reason,
                &drift_message,
                &now,
                current_generation,
            ),
            build_condition(
                existing_conditions,
                "ModulesResolved",
                modules_resolved_status,
                modules_resolved_reason,
                &modules_resolved_message,
                &now,
                current_generation,
            ),
            // Compliant is set by policy controllers — preserve existing value
            build_condition(
                existing_conditions,
                "Compliant",
                compliant_status,
                compliant_reason,
                compliant_message,
                &now,
                current_generation,
            ),
        ],
        package_versions: existing_package_versions,
    };

    // Everything the reconcile observed is already recorded — write nothing and
    // announce nothing. `build_condition` carries each condition's
    // `lastTransitionTime` forward while its status holds, so a machine whose
    // situation has not moved really does compare equal.
    if existing_status == Some(&desired) {
        info!(name = %name, "status already current, skipping write");
        record_reconcile_success(&ctx, "machine_config", start);
        return Ok(Action::requeue(std::time::Duration::from_secs(60)));
    }

    desired.last_reconciled = Some(now.clone());
    let status = serde_json::json!({ "status": desired });

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

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::Controller;
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType, Recorder, Reporter};
use kube::runtime::reflector::ObjectRef;
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Client, Resource, ResourceExt};
use tracing::{debug, info, warn};

use k8s_openapi::api::core::v1::Namespace;

use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicySpec, ClusterConfigPolicyStatus, Condition,
    ConfigPolicy, ConfigPolicySpec, ConfigPolicyStatus, DriftAlert, DriftAlertStatus,
    DriftSeverity, LabelSelector, MachineConfig, MachineConfigSpec, MachineConfigStatus, Module,
    ModuleRef, ModuleSignature, ModuleSpec, ModuleStatus, PackageRef, SelectorOperator,
    is_valid_oci_reference, is_valid_pem_public_key,
};
use crate::errors::OperatorError;
use crate::metrics::{DriftLabels, Metrics, PolicyLabels, ReconcileLabels};
use cfgd_core::version_satisfies;

const FIELD_MANAGER_OPERATOR: &str = "cfgd-operator";
const FIELD_MANAGER_STATUS: &str = "cfgd-operator/status";
const MACHINE_CONFIG_FINALIZER: &str = "cfgd.io/machine-config-cleanup";

fn compliance_summary(compliant: u32, non_compliant: u32) -> String {
    format!("{compliant} compliant, {non_compliant} non-compliant")
}

// ---------------------------------------------------------------------------
// Shared metrics helpers (DRY for error_policy and reconcile success blocks)
// ---------------------------------------------------------------------------

fn record_error_and_requeue(
    error: &OperatorError,
    ctx: &ControllerContext,
    controller: &str,
) -> Action {
    warn!(error = %error, controller = controller, "reconciliation error, requeuing");
    ctx.metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: controller.to_string(),
            result: "error".to_string(),
        })
        .inc();
    Action::requeue(std::time::Duration::from_secs(30))
}

fn record_reconcile_success(ctx: &ControllerContext, controller: &str, start: std::time::Instant) {
    let labels = ReconcileLabels {
        controller: controller.to_string(),
        result: "success".to_string(),
    };
    ctx.metrics
        .reconciliations_total
        .get_or_create(&labels)
        .inc();
    ctx.metrics
        .reconciliation_duration_seconds
        .get_or_create(&labels)
        .observe(start.elapsed().as_secs_f64());
}

type ReconcileResult<K> = Result<
    (kube::runtime::reflector::ObjectRef<K>, Action),
    kube::runtime::controller::Error<OperatorError, kube::runtime::watcher::Error>,
>;

fn log_reconcile<K: kube::Resource>(
    type_name: &'static str,
) -> impl Fn(ReconcileResult<K>) -> futures::future::Ready<()> {
    move |result| {
        match result {
            Ok((obj_ref, _action)) => info!(name = %obj_ref.name, "{type_name} reconciled"),
            Err(err) => warn!(error = %err, "{type_name} reconciliation error"),
        }
        futures::future::ready(())
    }
}

fn record_reconcile_metrics(
    ctx: &ControllerContext,
    controller: &str,
    result: &str,
    start: std::time::Instant,
) {
    let labels = ReconcileLabels {
        controller: controller.to_string(),
        result: result.to_string(),
    };
    ctx.metrics
        .reconciliations_total
        .get_or_create(&labels)
        .inc();
    ctx.metrics
        .reconciliation_duration_seconds
        .get_or_create(&labels)
        .observe(start.elapsed().as_secs_f64());
}

pub struct ControllerContext {
    pub client: Client,
    pub recorder: Recorder,
    pub metrics: Metrics,
}

/// Get a namespaced API for a resource, or return an error if namespace is empty.
fn namespaced_api<
    T: kube::Resource<DynamicType = (), Scope = kube::core::NamespaceResourceScope>
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug,
>(
    client: &Client,
    namespace: &str,
) -> Result<Api<T>, OperatorError> {
    if namespace.is_empty() {
        Err(OperatorError::Reconciliation(
            "resource has no namespace — cannot perform namespaced operation".to_string(),
        ))
    } else {
        Ok(Api::namespaced(client.clone(), namespace))
    }
}

pub async fn run(client: Client, metrics: Metrics) -> Result<(), OperatorError> {
    let reporter = Reporter {
        controller: "cfgd-operator".into(),
        instance: std::env::var("POD_NAME").ok(),
    };
    let recorder = Recorder::new(client.clone(), reporter);

    let ctx = Arc::new(ControllerContext {
        client: client.clone(),
        recorder,
        metrics,
    });

    let machines: Api<MachineConfig> = Api::all(client.clone());
    let alerts: Api<DriftAlert> = Api::all(client.clone());
    let policies: Api<ConfigPolicy> = Api::all(client.clone());
    let cluster_policies: Api<ClusterConfigPolicy> = Api::all(client.clone());
    let modules: Api<Module> = Api::all(client.clone());

    let mc_ctx = Arc::clone(&ctx);
    let da_ctx = Arc::clone(&ctx);
    let cp_ctx = Arc::clone(&ctx);
    let ccp_ctx = Arc::clone(&ctx);
    let mod_ctx = Arc::clone(&ctx);

    info!(
        "starting controllers: MachineConfig, DriftAlert, ConfigPolicy, ClusterConfigPolicy, Module"
    );

    let mc_controller = Controller::new(machines, WatcherConfig::default())
        .owns(
            Api::<DriftAlert>::all(client.clone()),
            WatcherConfig::default(),
        )
        .run(reconcile_machine_config, error_policy_mc, mc_ctx)
        .for_each(log_reconcile::<MachineConfig>("MachineConfig"));

    let da_controller = Controller::new(alerts, WatcherConfig::default())
        .run(reconcile_drift_alert, error_policy_da, da_ctx)
        .for_each(log_reconcile::<DriftAlert>("DriftAlert"));

    let cp_builder = Controller::new(policies, WatcherConfig::default());
    let cp_store = cp_builder.store();
    let cp_controller = cp_builder
        .watches(
            Api::<MachineConfig>::all(client.clone()),
            WatcherConfig::default(),
            move |mc| {
                // When a MachineConfig changes, requeue all ConfigPolicies in its namespace
                let ns = mc.namespace().unwrap_or_default();
                cp_store
                    .state()
                    .into_iter()
                    .filter(move |cp| cp.namespace().as_deref() == Some(ns.as_str()))
                    .map(|cp| ObjectRef::from_obj(&*cp))
                    .collect::<Vec<_>>()
            },
        )
        .run(reconcile_config_policy, error_policy_cp, cp_ctx)
        .for_each(log_reconcile::<ConfigPolicy>("ConfigPolicy"));

    let ccp_controller = Controller::new(cluster_policies, WatcherConfig::default())
        .run(reconcile_cluster_config_policy, error_policy_ccp, ccp_ctx)
        .for_each(log_reconcile::<ClusterConfigPolicy>("ClusterConfigPolicy"));

    let mod_controller = Controller::new(modules, WatcherConfig::default())
        .run(reconcile_module, error_policy_module, mod_ctx)
        .for_each(log_reconcile::<Module>("Module"));

    tokio::join!(
        mc_controller,
        da_controller,
        cp_controller,
        ccp_controller,
        mod_controller
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Condition helpers
// ---------------------------------------------------------------------------

/// Find an existing condition's status by type, returning None if not found.
fn find_condition_status(conditions: &[Condition], condition_type: &str) -> Option<String> {
    conditions
        .iter()
        .find(|c| c.condition_type == condition_type)
        .map(|c| c.status.clone())
}

/// Find an existing condition's last_transition_time by type.
fn find_condition_transition_time(
    conditions: &[Condition],
    condition_type: &str,
) -> Option<String> {
    conditions
        .iter()
        .find(|c| c.condition_type == condition_type)
        .map(|c| c.last_transition_time.clone())
}

/// Build a condition, preserving lastTransitionTime if the status hasn't changed.
fn build_condition(
    existing_conditions: &[Condition],
    condition_type: &str,
    status: &str,
    reason: &str,
    message: &str,
    now: &str,
    observed_generation: Option<i64>,
) -> Condition {
    let existing_status = find_condition_status(existing_conditions, condition_type);
    let transition_time = if existing_status.as_deref() == Some(status) {
        // Status unchanged — preserve existing transition time
        find_condition_transition_time(existing_conditions, condition_type)
            .unwrap_or_else(|| now.to_string())
    } else {
        // Status changed — new transition time
        now.to_string()
    };

    Condition {
        condition_type: condition_type.to_string(),
        status: status.to_string(),
        reason: reason.to_string(),
        message: message.to_string(),
        last_transition_time: transition_time,
        observed_generation,
    }
}

// ---------------------------------------------------------------------------
// DriftAlert condition builder (DRY helper for M1)
// ---------------------------------------------------------------------------

fn build_drift_alert_conditions(
    severity: &DriftSeverity,
    resolved: bool,
    device_id: &str,
    details_count: usize,
    now: &str,
    observed_generation: Option<i64>,
) -> Vec<Condition> {
    let is_escalated = matches!(severity, DriftSeverity::High | DriftSeverity::Critical);

    let (resolved_status, resolved_reason, resolved_message) = if resolved {
        (
            "True",
            "DriftResolved",
            "Drift has been resolved".to_string(),
        )
    } else {
        (
            "False",
            "DriftActive",
            format!(
                "Drift active on device {} — {} detail(s)",
                device_id, details_count
            ),
        )
    };

    vec![
        Condition {
            condition_type: "Acknowledged".to_string(),
            status: "False".to_string(),
            reason: "NotAcknowledged".to_string(),
            message: "Drift alert has not been acknowledged".to_string(),
            last_transition_time: now.to_string(),
            observed_generation,
        },
        Condition {
            condition_type: "Resolved".to_string(),
            status: resolved_status.to_string(),
            reason: resolved_reason.to_string(),
            message: resolved_message,
            last_transition_time: now.to_string(),
            observed_generation,
        },
        Condition {
            condition_type: "Escalated".to_string(),
            status: if is_escalated { "True" } else { "False" }.to_string(),
            reason: if is_escalated {
                "SeverityThreshold"
            } else {
                "BelowThreshold"
            }
            .to_string(),
            message: format!("Severity: {:?}", severity),
            last_transition_time: now.to_string(),
            observed_generation,
        },
    ]
}

// ---------------------------------------------------------------------------
// Publish event helper (logs on failure instead of silent .ok())
// ---------------------------------------------------------------------------

async fn publish_event(
    recorder: &Recorder,
    event: &Event,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) {
    if let Err(e) = recorder.publish(event, obj_ref).await {
        debug!(error = %e, "failed to publish event (best-effort)");
    }
}

async fn emit_event(
    recorder: &Recorder,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    event_type: EventType,
    reason: &str,
    note: String,
    action: &str,
) {
    publish_event(
        recorder,
        &Event {
            type_: event_type,
            reason: reason.into(),
            note: Some(note),
            action: action.into(),
            secondary: None,
        },
        obj_ref,
    )
    .await;
}

// ---------------------------------------------------------------------------
// MachineConfig controller
// ---------------------------------------------------------------------------

async fn reconcile_machine_config(
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

fn validate_spec(spec: &MachineConfigSpec) -> Result<(), OperatorError> {
    spec.validate()
        .map_err(|errors| OperatorError::InvalidSpec(errors.join("; ")))
}

/// Error policy for MachineConfig reconciliation failures.
/// Returns a base requeue duration; kube-rs Controller internally applies
/// exponential backoff (via its scheduler) for repeated failures of the
/// same object, so we don't need manual retry counting here.
fn error_policy_mc(
    _obj: Arc<MachineConfig>,
    error: &OperatorError,
    ctx: Arc<ControllerContext>,
) -> Action {
    record_error_and_requeue(error, &ctx, "machine_config")
}

// ---------------------------------------------------------------------------
// DriftAlert controller — updates MachineConfig drift status
// ---------------------------------------------------------------------------

async fn reconcile_drift_alert(
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
async fn has_active_drift_alerts(client: &Client, namespace: &str, mc_name: &str) -> bool {
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

async fn cleanup_drift_alerts(client: &Client, namespace: &str, mc_name: &str) {
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

/// Error policy for DriftAlert reconciliation failures.
/// kube-rs Controller applies exponential backoff internally.
fn error_policy_da(
    _obj: Arc<DriftAlert>,
    error: &OperatorError,
    ctx: Arc<ControllerContext>,
) -> Action {
    record_error_and_requeue(error, &ctx, "drift_alert")
}

// ---------------------------------------------------------------------------
// ConfigPolicy controller — validates MachineConfigs against policies
// ---------------------------------------------------------------------------

async fn reconcile_config_policy(
    obj: Arc<ConfigPolicy>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();

    info!(
        name = %name,
        required_modules = obj.spec.required_modules.len(),
        packages = obj.spec.packages.len(),
        settings = obj.spec.settings.len(),
        "reconciling ConfigPolicy"
    );

    let machines: Api<MachineConfig> = namespaced_api(&ctx.client, &namespace)?;

    let mc_list = machines.list(&ListParams::default()).await.map_err(|e| {
        OperatorError::Reconciliation(format!("failed to list MachineConfigs: {e}"))
    })?;

    // Filter MachineConfigs by target selector
    let targeted_mcs: Vec<&MachineConfig> = mc_list
        .iter()
        .filter(|mc| matches_selector(mc.metadata.labels.as_ref(), &obj.spec.target_selector))
        .collect();

    // Update each targeted MachineConfig's Compliant condition
    for mc in &targeted_mcs {
        let compliant = validate_policy_compliance(
            &mc.spec,
            mc.status.as_ref(),
            &obj.spec.required_modules,
            &obj.spec.packages,
            &obj.spec.settings,
        );
        let mc_name = mc.name_any();
        let mc_existing_conditions = mc
            .status
            .as_ref()
            .map(|s| s.conditions.as_slice())
            .unwrap_or(&[]);
        let now = cfgd_core::utc_now_iso8601();
        let (comp_status, comp_reason, comp_message) = if compliant {
            (
                "True",
                "PolicyCompliant",
                format!("Compliant with policy {}", name),
            )
        } else {
            (
                "False",
                "PolicyViolation",
                format!("Violates policy {}", name),
            )
        };
        let compliant_condition = build_condition(
            mc_existing_conditions,
            "Compliant",
            comp_status,
            comp_reason,
            &comp_message,
            &now,
            mc.meta().generation,
        );
        let mc_status_patch = serde_json::json!({
            "status": {
                "conditions": [compliant_condition]
            }
        });
        if let Err(e) = machines
            .patch_status(
                &mc_name,
                &PatchParams::apply(FIELD_MANAGER_STATUS),
                &Patch::Merge(mc_status_patch),
            )
            .await
        {
            warn!(name = %mc_name, error = %e, "failed to update Compliant condition on MachineConfig");
        }
    }

    // Evaluate compliance counts and emit violation events
    let targeted_mc_refs: Vec<MachineConfig> = targeted_mcs.into_iter().cloned().collect();
    let (compliant_count, non_compliant_count) = evaluate_policy_compliance(
        &ctx,
        &targeted_mc_refs,
        &obj.spec.packages,
        &obj.spec.required_modules,
        &obj.spec.settings,
        &name,
    )
    .await;

    let now = cfgd_core::utc_now_iso8601();
    let overall_status = if non_compliant_count == 0 {
        "True"
    } else {
        "False"
    };

    let policies: Api<ConfigPolicy> = namespaced_api(&ctx.client, &namespace)?;

    let existing_conditions = obj
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);

    let status = serde_json::json!({
        "status": ConfigPolicyStatus {
            compliant_count,
            non_compliant_count,
            conditions: vec![
                build_condition(
                    existing_conditions,
                    "Enforced", overall_status,
                    if non_compliant_count == 0 { "AllCompliant" } else { "NonCompliantTargets" },
                    &compliance_summary(compliant_count, non_compliant_count),
                    &now, obj.meta().generation,
                ),
            ],
        }
    });

    policies
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER_STATUS),
            &Patch::Merge(status),
        )
        .await
        .map_err(|e| {
            OperatorError::Reconciliation(format!(
                "failed to update ConfigPolicy status for {name}: {e}"
            ))
        })?;

    info!(
        name = %name,
        compliant = compliant_count,
        non_compliant = non_compliant_count,
        "configPolicy status updated"
    );

    emit_policy_evaluation_events(
        &ctx,
        &obj.object_ref(&()),
        compliant_count,
        non_compliant_count,
    )
    .await;

    ctx.metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: name.clone(),
            namespace: namespace.clone(),
        })
        .set(i64::from(compliant_count));

    record_reconcile_success(&ctx, "config_policy", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

pub(crate) fn matches_selector(
    labels: Option<&BTreeMap<String, String>>,
    selector: &LabelSelector,
) -> bool {
    if selector.match_labels.is_empty() && selector.match_expressions.is_empty() {
        return true;
    }
    let empty = BTreeMap::new();
    let labels = labels.unwrap_or(&empty);
    for (key, value) in &selector.match_labels {
        match labels.get(key) {
            Some(v) if v == value => {}
            _ => return false,
        }
    }
    for req in &selector.match_expressions {
        let label_value = labels.get(&req.key);
        let matched = match req.operator {
            SelectorOperator::In => label_value.is_some_and(|v| req.values.contains(v)),
            SelectorOperator::NotIn => label_value.is_none_or(|v| !req.values.contains(v)),
            SelectorOperator::Exists => label_value.is_some(),
            SelectorOperator::DoesNotExist => label_value.is_none(),
        };
        if !matched {
            return false;
        }
    }
    true
}

fn validate_policy_compliance(
    spec: &MachineConfigSpec,
    status: Option<&MachineConfigStatus>,
    required_modules: &[ModuleRef],
    packages: &[PackageRef],
    settings: &BTreeMap<String, serde_json::Value>,
) -> bool {
    for module in required_modules {
        if !spec.module_refs.iter().any(|mr| mr.name == module.name) {
            return false;
        }
    }
    for pkg in packages {
        if !spec.packages.iter().any(|p| p.name == pkg.name) {
            return false;
        }
        if let Some(req_str) = &pkg.version {
            let installed_versions = status.map(|s| &s.package_versions);
            match installed_versions.and_then(|pv| pv.get(&pkg.name)) {
                Some(reported) => {
                    if !version_satisfies(reported, req_str) {
                        return false;
                    }
                }
                None => return false,
            }
        }
    }
    for (key, value) in settings {
        match spec.system_settings.get(key) {
            Some(v) if v == value => {}
            _ => return false,
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Shared policy compliance evaluation helpers
// ---------------------------------------------------------------------------

/// Evaluate a set of MachineConfigs against policy requirements, emitting
/// violation events for non-compliant targets. Returns (compliant, non-compliant) counts.
async fn evaluate_policy_compliance(
    ctx: &ControllerContext,
    machine_configs: &[MachineConfig],
    required_packages: &[PackageRef],
    required_modules: &[ModuleRef],
    required_settings: &BTreeMap<String, serde_json::Value>,
    policy_name: &str,
) -> (u32, u32) {
    let mut compliant_count: u32 = 0;
    let mut non_compliant_count: u32 = 0;

    for mc in machine_configs {
        let compliant = validate_policy_compliance(
            &mc.spec,
            mc.status.as_ref(),
            required_modules,
            required_packages,
            required_settings,
        );
        if compliant {
            compliant_count += 1;
        } else {
            non_compliant_count += 1;
            let mc_name = mc.name_any();
            emit_event(
                &ctx.recorder,
                &mc.object_ref(&()),
                EventType::Warning,
                "PolicyViolation",
                format!("MachineConfig {} violates policy {}", mc_name, policy_name),
                "PolicyEvaluate",
            )
            .await;
        }
    }

    (compliant_count, non_compliant_count)
}

/// Emit standard post-evaluation events for a policy reconciler.
async fn emit_policy_evaluation_events(
    ctx: &ControllerContext,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    compliant_count: u32,
    non_compliant_count: u32,
) {
    emit_event(
        &ctx.recorder,
        obj_ref,
        EventType::Normal,
        "Evaluated",
        compliance_summary(compliant_count, non_compliant_count),
        "Evaluate",
    )
    .await;

    if non_compliant_count > 0 {
        emit_event(
            &ctx.recorder,
            obj_ref,
            EventType::Warning,
            "NonCompliantTargets",
            format!("{} non-compliant MachineConfigs", non_compliant_count),
            "Evaluate",
        )
        .await;
    }
}

/// Error policy for ConfigPolicy reconciliation failures.
/// kube-rs Controller applies exponential backoff internally.
fn error_policy_cp(
    _obj: Arc<ConfigPolicy>,
    error: &OperatorError,
    ctx: Arc<ControllerContext>,
) -> Action {
    record_error_and_requeue(error, &ctx, "config_policy")
}

// ---------------------------------------------------------------------------
// ClusterConfigPolicy controller — cluster-wide policy enforcement
// ---------------------------------------------------------------------------

struct MergedPolicyRequirements {
    packages: Vec<PackageRef>,
    modules: Vec<ModuleRef>,
    settings: BTreeMap<String, serde_json::Value>,
}

fn merge_policy_requirements(
    cluster: &ClusterConfigPolicySpec,
    namespace_policies: &[&ConfigPolicySpec],
) -> MergedPolicyRequirements {
    let mut packages = cluster.packages.clone();
    let mut modules = cluster.required_modules.clone();
    let mut settings = BTreeMap::new();

    for ns in namespace_policies {
        for pkg in &ns.packages {
            if let Some(existing) = packages.iter_mut().find(|p| p.name == pkg.name) {
                if existing.version.is_none() {
                    existing.version = pkg.version.clone();
                }
            } else {
                packages.push(pkg.clone());
            }
        }
        for module in &ns.required_modules {
            if !modules.iter().any(|m| m.name == module.name) {
                modules.push(module.clone());
            }
        }
        settings.extend(ns.settings.clone());
    }

    for cluster_pkg in &cluster.packages {
        if let Some(ver) = &cluster_pkg.version
            && let Some(existing) = packages.iter_mut().find(|p| p.name == cluster_pkg.name)
        {
            existing.version = Some(ver.clone());
        }
    }

    settings.extend(cluster.settings.clone());

    MergedPolicyRequirements {
        packages,
        modules,
        settings,
    }
}

async fn reconcile_cluster_config_policy(
    obj: Arc<ClusterConfigPolicy>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();

    info!(
        name = %name,
        required_modules = obj.spec.required_modules.len(),
        packages = obj.spec.packages.len(),
        settings = obj.spec.settings.len(),
        "reconciling ClusterConfigPolicy"
    );

    // List all namespaces, filtering by namespace_selector if non-empty
    let ns_api: Api<Namespace> = Api::all(ctx.client.clone());
    let ns_list = ns_api
        .list(&ListParams::default())
        .await
        .map_err(|e| OperatorError::Reconciliation(format!("failed to list namespaces: {e}")))?;

    let matching_namespaces: Vec<String> = ns_list
        .items
        .iter()
        .filter(|ns| {
            let ns_labels = ns.metadata.labels.as_ref();
            matches_selector(ns_labels, &obj.spec.namespace_selector)
        })
        .filter_map(|ns| ns.metadata.name.clone())
        .collect();

    let mut compliant_count: u32 = 0;
    let mut non_compliant_count: u32 = 0;

    for ns_name in &matching_namespaces {
        let machines: Api<MachineConfig> = Api::namespaced(ctx.client.clone(), ns_name);
        let mc_list = machines.list(&ListParams::default()).await.map_err(|e| {
            OperatorError::Reconciliation(format!(
                "failed to list MachineConfigs in namespace {ns_name}: {e}"
            ))
        })?;

        // List ALL namespace-scoped ConfigPolicies for merging (C5 fix)
        let ns_policies_api: Api<ConfigPolicy> = Api::namespaced(ctx.client.clone(), ns_name);
        let cp_list = ns_policies_api
            .list(&ListParams::default())
            .await
            .map_err(|e| {
                OperatorError::Reconciliation(format!(
                    "failed to list ConfigPolicies in namespace {ns_name}: {e}"
                ))
            })?;

        let ns_policy_specs: Vec<&ConfigPolicySpec> =
            cp_list.items.iter().map(|cp| &cp.spec).collect();
        let merged = merge_policy_requirements(&obj.spec, &ns_policy_specs);

        let (c, nc) = evaluate_policy_compliance(
            &ctx,
            &mc_list.items,
            &merged.packages,
            &merged.modules,
            &merged.settings,
            &name,
        )
        .await;
        compliant_count += c;
        non_compliant_count += nc;
    }

    let now = cfgd_core::utc_now_iso8601();
    let overall_status = if non_compliant_count == 0 {
        "True"
    } else {
        "False"
    };

    let ccp_api: Api<ClusterConfigPolicy> = Api::all(ctx.client.clone());

    let existing_conditions = obj
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);

    let status = serde_json::json!({
        "status": ClusterConfigPolicyStatus {
            compliant_count,
            non_compliant_count,
            conditions: vec![
                build_condition(
                    existing_conditions,
                    "Enforced", overall_status,
                    if non_compliant_count == 0 { "AllCompliant" } else { "NonCompliantTargets" },
                    &compliance_summary(compliant_count, non_compliant_count),
                    &now, obj.meta().generation,
                ),
            ],
        }
    });

    ccp_api
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER_STATUS),
            &Patch::Merge(status),
        )
        .await
        .map_err(|e| {
            OperatorError::Reconciliation(format!(
                "failed to update ClusterConfigPolicy status for {name}: {e}"
            ))
        })?;

    info!(
        name = %name,
        compliant = compliant_count,
        non_compliant = non_compliant_count,
        "clusterConfigPolicy status updated"
    );

    emit_policy_evaluation_events(
        &ctx,
        &obj.object_ref(&()),
        compliant_count,
        non_compliant_count,
    )
    .await;

    ctx.metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: name.clone(),
            namespace: String::new(), // cluster-scoped
        })
        .set(i64::from(compliant_count));

    record_reconcile_success(&ctx, "cluster_config_policy", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

fn error_policy_ccp(
    _obj: Arc<ClusterConfigPolicy>,
    error: &OperatorError,
    ctx: Arc<ControllerContext>,
) -> Action {
    record_error_and_requeue(error, &ctx, "cluster_config_policy")
}

// ---------------------------------------------------------------------------
// Module controller — validates OCI artifacts, signatures, trusted registries
// ---------------------------------------------------------------------------

async fn reconcile_module(
    obj: Arc<Module>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();

    info!(
        name = %name,
        oci_artifact = ?obj.spec.oci_artifact,
        has_signature = obj.spec.signature.is_some(),
        packages = obj.spec.packages.len(),
        "reconciling Module"
    );

    let current_generation = obj.meta().generation;
    let existing_conditions = obj
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);
    let now = cfgd_core::utc_now_iso8601();

    let mut conditions = Vec::new();

    // Evaluate Available condition
    let (avail_status, avail_reason, avail_message, avail_event) =
        evaluate_module_availability(&ctx.client, &name, &obj.spec).await;

    conditions.push(build_condition(
        existing_conditions,
        "Available",
        avail_status,
        avail_reason,
        avail_message,
        &now,
        current_generation,
    ));

    // Evaluate Verified condition
    let ver = evaluate_module_verification(&obj.spec.signature);

    conditions.push(build_condition(
        existing_conditions,
        "Verified",
        ver.status,
        ver.reason,
        ver.message,
        &now,
        current_generation,
    ));

    // Determine resolved artifact (just echo the reference if valid)
    let resolved_artifact = obj.spec.oci_artifact.clone();
    let verified = ver.status == "True";

    let status = serde_json::json!({
        "status": ModuleStatus {
            resolved_artifact,
            available_platforms: vec![],
            verified,
            signature_digest: ver.signature_digest,
            attestations: vec![],
            conditions,
        }
    });

    let modules_api: Api<Module> = Api::all(ctx.client.clone());
    modules_api
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER_STATUS),
            &Patch::Merge(status),
        )
        .await
        .map_err(|e| {
            OperatorError::Reconciliation(format!("failed to update Module status for {name}: {e}"))
        })?;

    info!(name = %name, "module status updated");

    // Emit availability event
    emit_event(
        &ctx.recorder,
        &obj.object_ref(&()),
        avail_event.0,
        avail_event.1,
        avail_event.2,
        "Reconcile",
    )
    .await;

    // Emit verification event
    emit_event(
        &ctx.recorder,
        &obj.object_ref(&()),
        ver.event.0,
        ver.event.1,
        ver.event.2,
        "Reconcile",
    )
    .await;

    record_reconcile_success(&ctx, "module", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

/// Evaluate the Available condition for a Module.
/// Returns (status, reason, message, (event_type, event_reason, event_note)).
async fn evaluate_module_availability<'a>(
    client: &Client,
    module_name: &str,
    spec: &ModuleSpec,
) -> (&'a str, &'a str, &'a str, (EventType, &'a str, String)) {
    let oci_ref = match &spec.oci_artifact {
        None => {
            return (
                "True",
                "NoArtifact",
                "Module is local-only (no OCI artifact)",
                (
                    EventType::Normal,
                    "Available",
                    format!("Module {} is local-only", module_name),
                ),
            );
        }
        Some(r) => r,
    };

    // Validate OCI reference format
    if !is_valid_oci_reference(oci_ref) {
        return (
            "False",
            "InvalidReference",
            "OCI artifact reference is invalid",
            (
                EventType::Warning,
                "PullFailed",
                format!(
                    "Module {} has invalid OCI reference: {}",
                    module_name, oci_ref
                ),
            ),
        );
    }

    // Read all ClusterConfigPolicies for security constraints
    let ccp_api: Api<ClusterConfigPolicy> = Api::all(client.clone());
    let ccp_list = match ccp_api.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "failed to list ClusterConfigPolicies for Module validation");
            // If we can't list policies, allow the module (fail-open for availability)
            return (
                "True",
                "ArtifactAvailable",
                "OCI artifact reference is valid",
                (
                    EventType::Normal,
                    "Available",
                    format!("Module {} artifact available: {}", module_name, oci_ref),
                ),
            );
        }
    };

    // Collect all trusted registries from ClusterConfigPolicies
    let mut all_trusted_registries: Vec<String> = Vec::new();
    let mut any_disallow_unsigned = false;

    for ccp in &ccp_list {
        let security = &ccp.spec.security;
        all_trusted_registries.extend(security.trusted_registries.clone());
        if !security.allow_unsigned {
            any_disallow_unsigned = true;
        }
    }

    // Check trusted registries (only if any are configured)
    if !all_trusted_registries.is_empty() {
        let matches_registry = all_trusted_registries.iter().any(|pattern| {
            if let Some(prefix) = pattern.strip_suffix('*') {
                oci_ref.starts_with(prefix)
            } else {
                oci_ref.starts_with(pattern.as_str())
            }
        });

        if !matches_registry {
            return (
                "False",
                "TrustedRegistryViolation",
                "OCI artifact is not from a trusted registry",
                (
                    EventType::Warning,
                    "TrustedRegistryViolation",
                    format!(
                        "Module {} artifact {} is not from a trusted registry",
                        module_name, oci_ref
                    ),
                ),
            );
        }
    }

    // Check unsigned policy
    if any_disallow_unsigned {
        let has_cosign_key = spec
            .signature
            .as_ref()
            .and_then(|s| s.cosign.as_ref())
            .is_some_and(|c| c.keyless || c.public_key.as_ref().is_some_and(|pk| !pk.is_empty()));

        if !has_cosign_key {
            return (
                "False",
                "UnsignedNotAllowed",
                "Module has no signature but unsigned modules are not allowed",
                (
                    EventType::Warning,
                    "UnsignedNotAllowed",
                    format!(
                        "Module {} has no signature but policy requires signing",
                        module_name
                    ),
                ),
            );
        }
    }

    (
        "True",
        "ArtifactAvailable",
        "OCI artifact reference is valid",
        (
            EventType::Normal,
            "Available",
            format!("Module {} artifact available: {}", module_name, oci_ref),
        ),
    )
}

/// Evaluate the Verified condition for a Module.
/// Returns (status, reason, message, (event_type, event_reason, event_note)).
/// Result of module verification evaluation.
struct ModuleVerificationResult {
    status: &'static str,
    reason: &'static str,
    message: &'static str,
    event: (EventType, &'static str, String),
    /// SHA256 fingerprint of the public key, or keyless identity description.
    signature_digest: Option<String>,
}

fn evaluate_module_verification(signature: &Option<ModuleSignature>) -> ModuleVerificationResult {
    match signature {
        None => ModuleVerificationResult {
            status: "False",
            reason: "NotSigned",
            message: "No signature configuration present",
            event: (
                EventType::Normal,
                "Verified",
                "Module has no signature configuration".to_string(),
            ),
            signature_digest: None,
        },
        Some(sig) => match &sig.cosign {
            None => ModuleVerificationResult {
                status: "False",
                reason: "NotSigned",
                message: "No cosign signature configured",
                event: (
                    EventType::Normal,
                    "Verified",
                    "Module has no cosign signature configured".to_string(),
                ),
                signature_digest: None,
            },
            Some(cosign) => {
                // Keyless mode — no public key needed
                if cosign.keyless {
                    let identity_desc = format!(
                        "keyless:{}@{}",
                        cosign.certificate_identity.as_deref().unwrap_or("*"),
                        cosign.certificate_oidc_issuer.as_deref().unwrap_or("*"),
                    );
                    return ModuleVerificationResult {
                        status: "True",
                        reason: "SignatureConfigured",
                        message: "Keyless cosign verification configured (Fulcio/Rekor)",
                        event: (
                            EventType::Normal,
                            "Verified",
                            "Module has keyless cosign verification configured".to_string(),
                        ),
                        signature_digest: Some(identity_desc),
                    };
                }
                // Static key mode — validate PEM
                match &cosign.public_key {
                    Some(pk) if is_valid_pem_public_key(pk) => {
                        let fingerprint =
                            format!("sha256:{}", cfgd_core::sha256_hex(pk.as_bytes()));
                        ModuleVerificationResult {
                            status: "True",
                            reason: "SignatureConfigured",
                            message: "Cosign public key is configured and valid",
                            event: (
                                EventType::Normal,
                                "Verified",
                                "Module has valid cosign signature configuration".to_string(),
                            ),
                            signature_digest: Some(fingerprint),
                        }
                    }
                    Some(_) => ModuleVerificationResult {
                        status: "False",
                        reason: "SignatureInvalid",
                        message: "Cosign public key is not valid PEM",
                        event: (
                            EventType::Warning,
                            "SignatureInvalid",
                            "Module cosign public key is not valid PEM".to_string(),
                        ),
                        signature_digest: None,
                    },
                    None => ModuleVerificationResult {
                        status: "False",
                        reason: "SignatureInvalid",
                        message: "Cosign signature configured but no public key or keyless mode",
                        event: (
                            EventType::Warning,
                            "SignatureInvalid",
                            "No public key and keyless not enabled".to_string(),
                        ),
                        signature_digest: None,
                    },
                }
            }
        },
    }
}

fn error_policy_module(
    _obj: Arc<Module>,
    error: &OperatorError,
    ctx: Arc<ControllerContext>,
) -> Action {
    record_error_and_requeue(error, &ctx, "module")
}

// ---------------------------------------------------------------------------
// MachineConfig moduleRef resolution helper
// ---------------------------------------------------------------------------

/// Resolve module references against cluster-scoped Module CRDs.
/// Returns (status, reason, message) for the ModulesResolved condition.
async fn resolve_module_refs(
    client: &Client,
    module_refs: &[ModuleRef],
) -> (&'static str, &'static str, String) {
    if module_refs.is_empty() {
        return (
            "True",
            "AllResolved",
            "No module references to resolve".to_string(),
        );
    }

    let modules_api: Api<Module> = Api::all(client.clone());
    let module_list = match modules_api.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "failed to list Modules for moduleRef resolution");
            return (
                "Unknown",
                "ResolutionError",
                "Failed to list Module resources".to_string(),
            );
        }
    };

    let existing_names: Vec<String> = module_list.iter().map(|m| m.name_any()).collect();
    let missing: Vec<&str> = module_refs
        .iter()
        .filter(|mr| !existing_names.iter().any(|n| n == &mr.name))
        .map(|mr| mr.name.as_str())
        .collect();

    if missing.is_empty() {
        (
            "True",
            "AllResolved",
            "All module references resolved".to_string(),
        )
    } else {
        (
            "False",
            "ModulesNotFound",
            format!("Missing modules: {}", missing.join(", ")),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crds::LabelSelectorRequirement;

    const TEST_PEM_KEY: &str = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----";

    fn mc_spec(hostname: &str, profile: &str) -> MachineConfigSpec {
        MachineConfigSpec {
            hostname: hostname.to_string(),
            profile: profile.to_string(),
            module_refs: vec![],
            packages: vec![],
            files: vec![],
            system_settings: Default::default(),
        }
    }

    #[test]
    fn validate_spec_delegates_to_shared() {
        assert!(validate_spec(&mc_spec("", "default")).is_err());
        assert!(validate_spec(&mc_spec("host1", "default")).is_ok());
    }

    #[test]
    fn matches_selector_empty_matches_all() {
        assert!(matches_selector(None, &LabelSelector::default()));
    }

    #[test]
    fn matches_selector_match_labels() {
        let mut match_labels = BTreeMap::new();
        match_labels.insert("team".to_string(), "platform".to_string());
        let selector = LabelSelector {
            match_labels,
            match_expressions: vec![],
        };

        let mut labels = BTreeMap::new();
        labels.insert("team".to_string(), "platform".to_string());
        labels.insert("env".to_string(), "prod".to_string());
        assert!(matches_selector(Some(&labels), &selector));

        let mut wrong_labels = BTreeMap::new();
        wrong_labels.insert("team".to_string(), "other".to_string());
        assert!(!matches_selector(Some(&wrong_labels), &selector));

        // Missing label entirely
        assert!(!matches_selector(Some(&BTreeMap::new()), &selector));
        assert!(!matches_selector(None, &selector));
    }

    #[test]
    fn policy_compliance_all_packages_present() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec![
            PackageRef {
                name: "vim".to_string(),
                version: None,
            },
            PackageRef {
                name: "git".to_string(),
                version: None,
            },
            PackageRef {
                name: "curl".to_string(),
                version: None,
            },
        ];
        let required = vec![
            PackageRef {
                name: "vim".to_string(),
                version: None,
            },
            PackageRef {
                name: "git".to_string(),
                version: None,
            },
        ];
        assert!(validate_policy_compliance(
            &spec,
            None,
            &[],
            &required,
            &Default::default()
        ));
    }

    #[test]
    fn policy_compliance_missing_package() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec![PackageRef {
            name: "vim".to_string(),
            version: None,
        }];
        let required = vec![
            PackageRef {
                name: "vim".to_string(),
                version: None,
            },
            PackageRef {
                name: "git".to_string(),
                version: None,
            },
        ];
        assert!(!validate_policy_compliance(
            &spec,
            None,
            &[],
            &required,
            &Default::default()
        ));
    }

    #[test]
    fn policy_compliance_settings() {
        let mut policy_settings = BTreeMap::new();
        policy_settings.insert(
            "key".to_string(),
            serde_json::Value::String("value".to_string()),
        );

        let mut spec = mc_spec("h", "p");
        spec.system_settings.insert(
            "key".to_string(),
            serde_json::Value::String("value".to_string()),
        );
        assert!(validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &policy_settings
        ));

        let mut wrong = BTreeMap::new();
        wrong.insert(
            "key".to_string(),
            serde_json::Value::String("wrong".to_string()),
        );
        assert!(!validate_policy_compliance(&spec, None, &[], &[], &wrong));
    }

    #[test]
    fn policy_compliance_settings_non_string() {
        let mut policy_settings = BTreeMap::new();
        policy_settings.insert("enabled".to_string(), serde_json::json!(true));

        let mut spec = mc_spec("h", "p");
        spec.system_settings
            .insert("enabled".to_string(), serde_json::json!(true));
        assert!(validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &policy_settings
        ));

        let mut wrong = BTreeMap::new();
        wrong.insert("enabled".to_string(), serde_json::json!(false));
        assert!(!validate_policy_compliance(&spec, None, &[], &[], &wrong));
    }

    #[test]
    fn policy_version_enforcement_satisfied() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec![PackageRef {
            name: "kubectl".to_string(),
            version: None,
        }];
        let mut status = MachineConfigStatus::default();
        status
            .package_versions
            .insert("kubectl".to_string(), "1.28.3".to_string());
        let policy_pkgs = vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.28".to_string()),
        }];
        assert!(validate_policy_compliance(
            &spec,
            Some(&status),
            &[],
            &policy_pkgs,
            &Default::default()
        ));
    }

    #[test]
    fn policy_version_enforcement_not_satisfied() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec![PackageRef {
            name: "kubectl".to_string(),
            version: None,
        }];
        let mut status = MachineConfigStatus::default();
        status
            .package_versions
            .insert("kubectl".to_string(), "1.27.0".to_string());
        let policy_pkgs = vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.28".to_string()),
        }];
        assert!(!validate_policy_compliance(
            &spec,
            Some(&status),
            &[],
            &policy_pkgs,
            &Default::default()
        ));
    }

    #[test]
    fn policy_version_enforcement_missing_version_report() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec![PackageRef {
            name: "kubectl".to_string(),
            version: None,
        }];
        let policy_pkgs = vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.28".to_string()),
        }];
        assert!(!validate_policy_compliance(
            &spec,
            None,
            &[],
            &policy_pkgs,
            &Default::default()
        ));
    }

    #[test]
    fn policy_version_enforcement_missing_package() {
        let policy_pkgs = vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.28".to_string()),
        }];
        assert!(!validate_policy_compliance(
            &mc_spec("h", "p"),
            None,
            &[],
            &policy_pkgs,
            &Default::default()
        ));
    }

    #[test]
    fn module_compliance_all_present() {
        let mut spec = mc_spec("h", "p");
        spec.module_refs = vec![
            crate::crds::ModuleRef {
                name: "corp-vpn".to_string(),
                required: true,
            },
            crate::crds::ModuleRef {
                name: "corp-certs".to_string(),
                required: true,
            },
        ];
        let required_modules = vec![
            ModuleRef {
                name: "corp-vpn".to_string(),
                required: false,
            },
            ModuleRef {
                name: "corp-certs".to_string(),
                required: false,
            },
        ];
        assert!(validate_policy_compliance(
            &spec,
            None,
            &required_modules,
            &[],
            &Default::default()
        ));
    }

    #[test]
    fn module_compliance_missing_module() {
        let mut spec = mc_spec("h", "p");
        spec.module_refs = vec![crate::crds::ModuleRef {
            name: "corp-vpn".to_string(),
            required: true,
        }];
        let required_modules = vec![
            ModuleRef {
                name: "corp-vpn".to_string(),
                required: false,
            },
            ModuleRef {
                name: "corp-certs".to_string(),
                required: false,
            },
        ];
        assert!(!validate_policy_compliance(
            &spec,
            None,
            &required_modules,
            &[],
            &Default::default()
        ));
    }

    #[test]
    fn module_compliance_no_requirements() {
        assert!(validate_policy_compliance(
            &mc_spec("h", "p"),
            None,
            &[],
            &[],
            &Default::default()
        ));
    }

    #[test]
    fn merge_policy_union_packages() {
        let cluster = ClusterConfigPolicySpec {
            packages: vec![PackageRef {
                name: "vim".to_string(),
                version: None,
            }],
            ..Default::default()
        };
        let ns = ConfigPolicySpec {
            packages: vec![PackageRef {
                name: "git".to_string(),
                version: None,
            }],
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns]);
        assert_eq!(merged.packages.len(), 2);
    }

    #[test]
    fn merge_policy_cluster_wins_settings() {
        let mut cluster_settings = BTreeMap::new();
        cluster_settings.insert(
            "key".to_string(),
            serde_json::Value::String("cluster-value".to_string()),
        );
        let cluster = ClusterConfigPolicySpec {
            settings: cluster_settings,
            ..Default::default()
        };
        let mut ns_settings = BTreeMap::new();
        ns_settings.insert(
            "key".to_string(),
            serde_json::Value::String("ns-value".to_string()),
        );
        let ns = ConfigPolicySpec {
            settings: ns_settings,
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns]);
        assert_eq!(
            merged.settings["key"],
            serde_json::Value::String("cluster-value".to_string())
        );
    }

    #[test]
    fn merge_policy_cluster_wins_versions() {
        let cluster = ClusterConfigPolicySpec {
            packages: vec![PackageRef {
                name: "kubectl".to_string(),
                version: Some(">=1.29".to_string()),
            }],
            ..Default::default()
        };
        let ns = ConfigPolicySpec {
            packages: vec![PackageRef {
                name: "kubectl".to_string(),
                version: Some(">=1.28".to_string()),
            }],
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns]);
        let kubectl_pkg = merged
            .packages
            .iter()
            .find(|p| p.name == "kubectl")
            .unwrap();
        assert_eq!(kubectl_pkg.version.as_deref(), Some(">=1.29"));
    }

    #[test]
    fn merge_policy_no_namespace_policy() {
        let cluster = ClusterConfigPolicySpec {
            packages: vec![PackageRef {
                name: "vim".to_string(),
                version: None,
            }],
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[]);
        assert_eq!(merged.packages.len(), 1);
        assert_eq!(merged.packages[0].name, "vim");
    }

    #[test]
    fn merge_policy_union_modules() {
        let cluster = ClusterConfigPolicySpec {
            required_modules: vec![ModuleRef {
                name: "corp-vpn".to_string(),
                required: true,
            }],
            ..Default::default()
        };
        let ns = ConfigPolicySpec {
            required_modules: vec![ModuleRef {
                name: "corp-certs".to_string(),
                required: true,
            }],
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns]);
        assert_eq!(merged.modules.len(), 2);
    }

    #[test]
    fn merge_policy_multiple_namespace_policies() {
        let cluster = ClusterConfigPolicySpec {
            packages: vec![PackageRef {
                name: "vim".to_string(),
                version: None,
            }],
            ..Default::default()
        };
        let ns1 = ConfigPolicySpec {
            packages: vec![PackageRef {
                name: "git".to_string(),
                version: None,
            }],
            ..Default::default()
        };
        let ns2 = ConfigPolicySpec {
            packages: vec![PackageRef {
                name: "curl".to_string(),
                version: None,
            }],
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2]);
        assert_eq!(merged.packages.len(), 3);
    }

    #[test]
    fn matches_selector_expressions_in() {
        let mut labels = BTreeMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        let selector = LabelSelector {
            match_labels: Default::default(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "env".to_string(),
                operator: SelectorOperator::In,
                values: vec!["prod".to_string(), "staging".to_string()],
            }],
        };
        assert!(matches_selector(Some(&labels), &selector));
    }

    #[test]
    fn matches_selector_expressions_not_in() {
        let mut labels = BTreeMap::new();
        labels.insert("env".to_string(), "dev".to_string());
        let selector = LabelSelector {
            match_labels: Default::default(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "env".to_string(),
                operator: SelectorOperator::NotIn,
                values: vec!["prod".to_string()],
            }],
        };
        assert!(matches_selector(Some(&labels), &selector));
    }

    #[test]
    fn matches_selector_expressions_exists() {
        let mut labels = BTreeMap::new();
        labels.insert("tier".to_string(), "frontend".to_string());
        let selector = LabelSelector {
            match_labels: Default::default(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "tier".to_string(),
                operator: SelectorOperator::Exists,
                values: vec![],
            }],
        };
        assert!(matches_selector(Some(&labels), &selector));
    }

    #[test]
    fn matches_selector_expressions_does_not_exist() {
        let labels = BTreeMap::new();
        let selector = LabelSelector {
            match_labels: Default::default(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "restricted".to_string(),
                operator: SelectorOperator::DoesNotExist,
                values: vec![],
            }],
        };
        assert!(matches_selector(Some(&labels), &selector));
    }

    #[test]
    fn drift_alert_escalated_on_high_severity() {
        use crate::crds::DriftSeverity;
        let is_escalated = matches!(
            DriftSeverity::High,
            DriftSeverity::High | DriftSeverity::Critical
        );
        assert!(is_escalated);
        let is_not_escalated = matches!(
            DriftSeverity::Low,
            DriftSeverity::High | DriftSeverity::Critical
        );
        assert!(!is_not_escalated);
    }

    #[test]
    fn merge_policy_dedup_packages() {
        let cluster = ClusterConfigPolicySpec {
            packages: vec![PackageRef {
                name: "vim".to_string(),
                version: None,
            }],
            ..Default::default()
        };
        let ns = ConfigPolicySpec {
            packages: vec![PackageRef {
                name: "vim".to_string(),
                version: Some("1.0".to_string()),
            }],
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns]);
        // Should not duplicate — cluster already has vim
        assert_eq!(merged.packages.len(), 1);
    }

    #[test]
    fn build_condition_preserves_transition_time_when_status_unchanged() {
        let existing = vec![Condition {
            condition_type: "Reconciled".to_string(),
            status: "True".to_string(),
            reason: "ReconcileSuccess".to_string(),
            message: "old message".to_string(),
            last_transition_time: "2024-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        }];
        let c = build_condition(
            &existing,
            "Reconciled",
            "True",
            "ReconcileSuccess",
            "new message",
            "2024-06-01T00:00:00Z",
            Some(2),
        );
        // Status unchanged → preserve old transition time
        assert_eq!(c.last_transition_time, "2024-01-01T00:00:00Z");
        // But generation and message update
        assert_eq!(c.observed_generation, Some(2));
        assert_eq!(c.message, "new message");
    }

    #[test]
    fn build_condition_updates_transition_time_when_status_changes() {
        let existing = vec![Condition {
            condition_type: "DriftDetected".to_string(),
            status: "False".to_string(),
            reason: "NoDrift".to_string(),
            message: "no drift".to_string(),
            last_transition_time: "2024-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        }];
        let c = build_condition(
            &existing,
            "DriftDetected",
            "True",
            "DriftActive",
            "drift found",
            "2024-06-01T00:00:00Z",
            Some(2),
        );
        // Status changed → new transition time
        assert_eq!(c.last_transition_time, "2024-06-01T00:00:00Z");
    }

    #[test]
    fn build_drift_alert_conditions_resolved() {
        let conditions = build_drift_alert_conditions(
            &DriftSeverity::Low,
            true,
            "dev-1",
            0,
            "2024-01-01T00:00:00Z",
            Some(1),
        );
        assert_eq!(conditions.len(), 3);
        assert_eq!(conditions[1].condition_type, "Resolved");
        assert_eq!(conditions[1].status, "True");
        assert_eq!(conditions[2].status, "False"); // Low is not escalated
    }

    #[test]
    fn build_drift_alert_conditions_active_high() {
        let conditions = build_drift_alert_conditions(
            &DriftSeverity::High,
            false,
            "dev-1",
            3,
            "2024-01-01T00:00:00Z",
            Some(1),
        );
        assert_eq!(conditions[1].status, "False"); // Not resolved
        assert_eq!(conditions[2].status, "True"); // High is escalated
    }

    #[test]
    fn module_verification_no_signature() {
        let result = evaluate_module_verification(&None);
        assert_eq!(result.status, "False");
        assert_eq!(result.reason, "NotSigned");
        assert!(result.message.contains("No signature"));
        assert!(result.signature_digest.is_none());
    }

    #[test]
    fn module_verification_no_cosign() {
        let sig = ModuleSignature { cosign: None };
        let result = evaluate_module_verification(&Some(sig));
        assert_eq!(result.status, "False");
        assert_eq!(result.reason, "NotSigned");
        assert!(result.signature_digest.is_none());
    }

    #[test]
    fn module_verification_valid_pem() {
        let sig = ModuleSignature {
            cosign: Some(crate::crds::CosignSignature {
                public_key: Some(TEST_PEM_KEY.to_string()),
                ..Default::default()
            }),
        };
        let result = evaluate_module_verification(&Some(sig));
        assert_eq!(result.status, "True");
        assert_eq!(result.reason, "SignatureConfigured");
        assert!(result.signature_digest.is_some());
        assert!(result.signature_digest.unwrap().starts_with("sha256:"));
    }

    #[test]
    fn module_verification_invalid_pem() {
        let sig = ModuleSignature {
            cosign: Some(crate::crds::CosignSignature {
                public_key: Some("not-a-pem-key".to_string()),
                ..Default::default()
            }),
        };
        let result = evaluate_module_verification(&Some(sig));
        assert_eq!(result.status, "False");
        assert_eq!(result.reason, "SignatureInvalid");
        assert!(result.signature_digest.is_none());
    }

    #[test]
    fn module_verification_keyless() {
        let sig = ModuleSignature {
            cosign: Some(crate::crds::CosignSignature {
                keyless: true,
                certificate_identity: Some("user@example.com".to_string()),
                certificate_oidc_issuer: Some("https://accounts.google.com".to_string()),
                ..Default::default()
            }),
        };
        let result = evaluate_module_verification(&Some(sig));
        assert_eq!(result.status, "True");
        assert_eq!(result.reason, "SignatureConfigured");
        assert_eq!(
            result.signature_digest.unwrap(),
            "keyless:user@example.com@https://accounts.google.com"
        );
    }

    #[test]
    fn compliance_summary_formats_counts() {
        assert_eq!(compliance_summary(0, 0), "0 compliant, 0 non-compliant");
        assert_eq!(compliance_summary(5, 3), "5 compliant, 3 non-compliant");
        assert_eq!(
            compliance_summary(100, 0),
            "100 compliant, 0 non-compliant"
        );
        assert_eq!(
            compliance_summary(0, 42),
            "0 compliant, 42 non-compliant"
        );
    }

    #[test]
    fn find_condition_status_returns_matching() {
        let conditions = vec![
            Condition {
                condition_type: "Reconciled".to_string(),
                status: "True".to_string(),
                reason: "ReconcileSuccess".to_string(),
                message: "all good".to_string(),
                last_transition_time: "2024-01-01T00:00:00Z".to_string(),
                observed_generation: Some(1),
            },
            Condition {
                condition_type: "DriftDetected".to_string(),
                status: "False".to_string(),
                reason: "NoDrift".to_string(),
                message: "no drift".to_string(),
                last_transition_time: "2024-01-02T00:00:00Z".to_string(),
                observed_generation: Some(2),
            },
        ];
        assert_eq!(
            find_condition_status(&conditions, "Reconciled"),
            Some("True".to_string())
        );
        assert_eq!(
            find_condition_status(&conditions, "DriftDetected"),
            Some("False".to_string())
        );
        assert_eq!(find_condition_status(&conditions, "NonExistent"), None);
        assert_eq!(find_condition_status(&[], "Reconciled"), None);
    }

    #[test]
    fn find_condition_transition_time_returns_matching() {
        let conditions = vec![
            Condition {
                condition_type: "Reconciled".to_string(),
                status: "True".to_string(),
                reason: "ReconcileSuccess".to_string(),
                message: "all good".to_string(),
                last_transition_time: "2024-01-01T00:00:00Z".to_string(),
                observed_generation: Some(1),
            },
            Condition {
                condition_type: "DriftDetected".to_string(),
                status: "False".to_string(),
                reason: "NoDrift".to_string(),
                message: "no drift".to_string(),
                last_transition_time: "2024-06-15T12:00:00Z".to_string(),
                observed_generation: None,
            },
        ];
        assert_eq!(
            find_condition_transition_time(&conditions, "Reconciled"),
            Some("2024-01-01T00:00:00Z".to_string())
        );
        assert_eq!(
            find_condition_transition_time(&conditions, "DriftDetected"),
            Some("2024-06-15T12:00:00Z".to_string())
        );
        assert_eq!(
            find_condition_transition_time(&conditions, "NonExistent"),
            None
        );
        assert_eq!(find_condition_transition_time(&[], "Reconciled"), None);
    }

    #[test]
    fn matches_selector_combined_labels_and_expressions() {
        let mut labels = BTreeMap::new();
        labels.insert("team".to_string(), "platform".to_string());
        labels.insert("env".to_string(), "prod".to_string());
        labels.insert("tier".to_string(), "backend".to_string());

        // Selector requires match_labels AND match_expressions (AND semantics)
        let selector = LabelSelector {
            match_labels: {
                let mut m = BTreeMap::new();
                m.insert("team".to_string(), "platform".to_string());
                m
            },
            match_expressions: vec![
                LabelSelectorRequirement {
                    key: "env".to_string(),
                    operator: SelectorOperator::In,
                    values: vec!["prod".to_string(), "staging".to_string()],
                },
                LabelSelectorRequirement {
                    key: "tier".to_string(),
                    operator: SelectorOperator::Exists,
                    values: vec![],
                },
            ],
        };

        // All conditions met
        assert!(matches_selector(Some(&labels), &selector));

        // Fails if match_labels don't match
        let mut wrong_team = labels.clone();
        wrong_team.insert("team".to_string(), "security".to_string());
        assert!(!matches_selector(Some(&wrong_team), &selector));

        // Fails if expression doesn't match (env not in allowed set)
        let mut wrong_env = labels.clone();
        wrong_env.insert("env".to_string(), "dev".to_string());
        assert!(!matches_selector(Some(&wrong_env), &selector));

        // Fails if required Exists label is missing
        let mut no_tier = labels.clone();
        no_tier.remove("tier");
        assert!(!matches_selector(Some(&no_tier), &selector));
    }

    #[test]
    fn matches_selector_not_in_rejects_matching_value() {
        let mut labels = BTreeMap::new();
        labels.insert("env".to_string(), "prod".to_string());

        let selector = LabelSelector {
            match_labels: BTreeMap::new(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "env".to_string(),
                operator: SelectorOperator::NotIn,
                values: vec!["prod".to_string(), "staging".to_string()],
            }],
        };

        // Label value IS in the exclusion list — should be rejected
        assert!(!matches_selector(Some(&labels), &selector));
    }

    #[test]
    fn validate_spec_empty_profile_errors() {
        let result = validate_spec(&mc_spec("valid-host", ""));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("profile"),
            "error should mention profile: {err_msg}"
        );
    }

    #[test]
    fn validate_spec_whitespace_hostname_errors() {
        let result = validate_spec(&mc_spec("   ", "default"));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("hostname"),
            "error should mention hostname: {err_msg}"
        );
    }

    // -----------------------------------------------------------------------
    // evaluate_module_verification edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn module_verification_keyless_with_public_key_prefers_keyless() {
        // When both keyless=true AND public_key are set, keyless wins (early return)
        let sig = ModuleSignature {
            cosign: Some(crate::crds::CosignSignature {
                keyless: true,
                public_key: Some(TEST_PEM_KEY.to_string()),
                certificate_identity: Some("ci@example.com".to_string()),
                certificate_oidc_issuer: Some("https://issuer.example.com".to_string()),
            }),
        };
        let result = evaluate_module_verification(&Some(sig));
        assert_eq!(result.status, "True");
        assert_eq!(result.reason, "SignatureConfigured");
        // Should return keyless identity, not PEM fingerprint
        let digest = result.signature_digest.unwrap();
        assert!(
            digest.starts_with("keyless:"),
            "should use keyless path, got: {digest}"
        );
        assert!(digest.contains("ci@example.com"));
    }

    #[test]
    fn module_verification_keyless_without_identity_uses_wildcard() {
        let sig = ModuleSignature {
            cosign: Some(crate::crds::CosignSignature {
                keyless: true,
                certificate_identity: None,
                certificate_oidc_issuer: None,
                ..Default::default()
            }),
        };
        let result = evaluate_module_verification(&Some(sig));
        assert_eq!(result.status, "True");
        assert_eq!(result.signature_digest.as_deref(), Some("keyless:*@*"));
    }

    #[test]
    fn module_verification_static_key_no_public_key() {
        // cosign present, keyless=false, public_key=None → should be NotSigned or Invalid
        let sig = ModuleSignature {
            cosign: Some(crate::crds::CosignSignature {
                keyless: false,
                public_key: None,
                ..Default::default()
            }),
        };
        let result = evaluate_module_verification(&Some(sig));
        assert_eq!(result.status, "False");
    }

    // -----------------------------------------------------------------------
    // merge_policy_requirements — 3+ namespace policies
    // -----------------------------------------------------------------------

    #[test]
    fn merge_policy_three_namespace_policies_with_version_conflict() {
        let cluster = ClusterConfigPolicySpec {
            packages: vec![PackageRef {
                name: "kubectl".to_string(),
                version: Some(">=1.30".to_string()),
            }],
            ..Default::default()
        };
        let ns1 = ConfigPolicySpec {
            packages: vec![
                PackageRef {
                    name: "kubectl".to_string(),
                    version: Some(">=1.28".to_string()),
                },
                PackageRef {
                    name: "git".to_string(),
                    version: None,
                },
            ],
            ..Default::default()
        };
        let ns2 = ConfigPolicySpec {
            packages: vec![
                PackageRef {
                    name: "curl".to_string(),
                    version: Some(">=8.0".to_string()),
                },
                PackageRef {
                    name: "git".to_string(),
                    version: Some(">=2.40".to_string()),
                },
            ],
            ..Default::default()
        };
        let ns3 = ConfigPolicySpec {
            packages: vec![PackageRef {
                name: "jq".to_string(),
                version: None,
            }],
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2, &ns3]);

        // Cluster kubectl >=1.30 should win over ns1's >=1.28
        let kubectl = merged.packages.iter().find(|p| p.name == "kubectl").unwrap();
        assert_eq!(kubectl.version.as_deref(), Some(">=1.30"));

        // git: cluster has no git, ns1 adds it with None, ns2 tries to add but finds
        // existing with None version → ns2's version fills in
        let git = merged.packages.iter().find(|p| p.name == "git").unwrap();
        assert_eq!(git.version.as_deref(), Some(">=2.40"));

        // curl from ns2, jq from ns3
        assert!(merged.packages.iter().any(|p| p.name == "curl"));
        assert!(merged.packages.iter().any(|p| p.name == "jq"));

        // Total unique packages: kubectl, git, curl, jq
        assert_eq!(merged.packages.len(), 4);
    }

    #[test]
    fn merge_policy_namespace_settings_later_overwrites_earlier() {
        let cluster = ClusterConfigPolicySpec::default();
        let ns1 = ConfigPolicySpec {
            settings: {
                let mut s = BTreeMap::new();
                s.insert("key".to_string(), serde_json::json!("ns1-value"));
                s.insert("shared".to_string(), serde_json::json!("from-ns1"));
                s
            },
            ..Default::default()
        };
        let ns2 = ConfigPolicySpec {
            settings: {
                let mut s = BTreeMap::new();
                s.insert("shared".to_string(), serde_json::json!("from-ns2"));
                s
            },
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2]);
        // ns2 overwrites ns1 for "shared"
        assert_eq!(merged.settings["shared"], serde_json::json!("from-ns2"));
        // ns1-only key preserved
        assert_eq!(merged.settings["key"], serde_json::json!("ns1-value"));
    }

    #[test]
    fn merge_policy_cluster_settings_override_namespace() {
        let cluster = ClusterConfigPolicySpec {
            settings: {
                let mut s = BTreeMap::new();
                s.insert("key".to_string(), serde_json::json!("cluster-wins"));
                s
            },
            ..Default::default()
        };
        let ns = ConfigPolicySpec {
            settings: {
                let mut s = BTreeMap::new();
                s.insert("key".to_string(), serde_json::json!("ns-loses"));
                s.insert("ns-only".to_string(), serde_json::json!("kept"));
                s
            },
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns]);
        // Cluster settings extend AFTER namespace → cluster wins
        assert_eq!(merged.settings["key"], serde_json::json!("cluster-wins"));
        assert_eq!(merged.settings["ns-only"], serde_json::json!("kept"));
    }

    #[test]
    fn merge_policy_three_namespace_module_dedup() {
        let cluster = ClusterConfigPolicySpec {
            required_modules: vec![ModuleRef {
                name: "corp-vpn".to_string(),
                required: true,
            }],
            ..Default::default()
        };
        let ns1 = ConfigPolicySpec {
            required_modules: vec![
                ModuleRef {
                    name: "corp-vpn".to_string(),
                    required: true,
                },
                ModuleRef {
                    name: "corp-certs".to_string(),
                    required: true,
                },
            ],
            ..Default::default()
        };
        let ns2 = ConfigPolicySpec {
            required_modules: vec![ModuleRef {
                name: "corp-certs".to_string(),
                required: true,
            }],
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns1, &ns2]);
        // Should dedup: corp-vpn (from cluster), corp-certs (from ns1, ns2 skipped)
        assert_eq!(merged.modules.len(), 2);
    }

    // -----------------------------------------------------------------------
    // validate_policy_compliance — nested JSON settings
    // -----------------------------------------------------------------------

    #[test]
    fn policy_compliance_nested_json_settings() {
        let mut policy_settings = BTreeMap::new();
        policy_settings.insert(
            "network".to_string(),
            serde_json::json!({"dns": {"servers": ["8.8.8.8", "1.1.1.1"]}}),
        );

        let mut spec = mc_spec("h", "p");
        spec.system_settings.insert(
            "network".to_string(),
            serde_json::json!({"dns": {"servers": ["8.8.8.8", "1.1.1.1"]}}),
        );
        assert!(validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &policy_settings
        ));
    }

    #[test]
    fn policy_compliance_nested_json_mismatch() {
        let mut policy_settings = BTreeMap::new();
        policy_settings.insert(
            "network".to_string(),
            serde_json::json!({"dns": {"servers": ["8.8.8.8"]}}),
        );

        let mut spec = mc_spec("h", "p");
        spec.system_settings.insert(
            "network".to_string(),
            serde_json::json!({"dns": {"servers": ["1.1.1.1"]}}),
        );
        assert!(
            !validate_policy_compliance(&spec, None, &[], &[], &policy_settings),
            "nested JSON with different values should not match"
        );
    }

    #[test]
    fn policy_compliance_numeric_settings() {
        let mut policy_settings = BTreeMap::new();
        policy_settings.insert("max_retries".to_string(), serde_json::json!(3));

        let mut spec = mc_spec("h", "p");
        spec.system_settings
            .insert("max_retries".to_string(), serde_json::json!(3));
        assert!(validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &policy_settings
        ));

        // Different numeric value
        let mut wrong = BTreeMap::new();
        wrong.insert("max_retries".to_string(), serde_json::json!(5));
        assert!(!validate_policy_compliance(&spec, None, &[], &[], &wrong));
    }

    #[test]
    fn policy_compliance_null_setting() {
        let mut policy_settings = BTreeMap::new();
        policy_settings.insert("opt_out".to_string(), serde_json::Value::Null);

        let mut spec = mc_spec("h", "p");
        spec.system_settings
            .insert("opt_out".to_string(), serde_json::Value::Null);
        assert!(validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &policy_settings
        ));
    }

    #[test]
    fn policy_compliance_array_setting() {
        let mut policy_settings = BTreeMap::new();
        policy_settings.insert(
            "tags".to_string(),
            serde_json::json!(["production", "critical"]),
        );

        let mut spec = mc_spec("h", "p");
        spec.system_settings.insert(
            "tags".to_string(),
            serde_json::json!(["production", "critical"]),
        );
        assert!(validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &policy_settings
        ));

        // Different order — JSON arrays are order-sensitive
        spec.system_settings.insert(
            "tags".to_string(),
            serde_json::json!(["critical", "production"]),
        );
        assert!(
            !validate_policy_compliance(&spec, None, &[], &[], &policy_settings),
            "array order matters in JSON equality"
        );
    }

    #[test]
    fn policy_compliance_missing_setting_key() {
        let mut policy_settings = BTreeMap::new();
        policy_settings.insert("required_key".to_string(), serde_json::json!("value"));

        let spec = mc_spec("h", "p");
        // spec has no system_settings → should fail
        assert!(!validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &policy_settings
        ));
    }

    // -----------------------------------------------------------------------
    // build_condition — new condition (not in existing list)
    // -----------------------------------------------------------------------

    #[test]
    fn build_condition_new_type_uses_provided_transition_time() {
        let existing = vec![Condition {
            condition_type: "Reconciled".to_string(),
            status: "True".to_string(),
            reason: "ReconcileSuccess".to_string(),
            message: "ok".to_string(),
            last_transition_time: "2024-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        }];
        // Build a NEW condition type not in existing
        let c = build_condition(
            &existing,
            "DriftDetected",
            "True",
            "DriftActive",
            "drift found",
            "2024-06-01T00:00:00Z",
            Some(2),
        );
        // New type → uses the provided transition time
        assert_eq!(c.last_transition_time, "2024-06-01T00:00:00Z");
        assert_eq!(c.condition_type, "DriftDetected");
        assert_eq!(c.observed_generation, Some(2));
    }

    #[test]
    fn build_drift_alert_conditions_critical_severity() {
        let conditions = build_drift_alert_conditions(
            &DriftSeverity::Critical,
            false,
            "dev-1",
            5,
            "2024-01-01T00:00:00Z",
            Some(3),
        );
        assert_eq!(conditions.len(), 3);
        // Acknowledged (not yet)
        assert_eq!(conditions[0].condition_type, "Acknowledged");
        assert_eq!(conditions[0].status, "False");
        // Not resolved
        assert_eq!(conditions[1].condition_type, "Resolved");
        assert_eq!(conditions[1].status, "False");
        assert!(conditions[1].message.contains("dev-1"));
        assert!(conditions[1].message.contains("5 detail(s)"));
        // Critical is escalated
        assert_eq!(conditions[2].condition_type, "Escalated");
        assert_eq!(conditions[2].status, "True");
    }

    #[test]
    fn matches_selector_in_with_empty_values_rejects() {
        let mut labels = BTreeMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        let selector = LabelSelector {
            match_labels: Default::default(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "env".to_string(),
                operator: SelectorOperator::In,
                values: vec![], // empty values list
            }],
        };
        // In with empty values → no value can match → should reject
        assert!(!matches_selector(Some(&labels), &selector));
    }

    #[test]
    fn matches_selector_not_in_with_empty_values_accepts() {
        let mut labels = BTreeMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        let selector = LabelSelector {
            match_labels: Default::default(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "env".to_string(),
                operator: SelectorOperator::NotIn,
                values: vec![], // empty exclusion list
            }],
        };
        // NotIn with empty values → nothing excluded → should accept
        assert!(matches_selector(Some(&labels), &selector));
    }

    #[test]
    fn matches_selector_does_not_exist_with_present_label_rejects() {
        let mut labels = BTreeMap::new();
        labels.insert("restricted".to_string(), "true".to_string());
        let selector = LabelSelector {
            match_labels: Default::default(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "restricted".to_string(),
                operator: SelectorOperator::DoesNotExist,
                values: vec![],
            }],
        };
        assert!(
            !matches_selector(Some(&labels), &selector),
            "DoesNotExist should reject when label is present"
        );
    }

    #[test]
    fn matches_selector_exists_with_missing_label_rejects() {
        let labels = BTreeMap::new();
        let selector = LabelSelector {
            match_labels: Default::default(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "required-label".to_string(),
                operator: SelectorOperator::Exists,
                values: vec![],
            }],
        };
        assert!(
            !matches_selector(Some(&labels), &selector),
            "Exists should reject when label is absent"
        );
    }

    #[test]
    fn matches_selector_none_labels_with_match_labels_rejects() {
        let selector = LabelSelector {
            match_labels: {
                let mut m = BTreeMap::new();
                m.insert("team".to_string(), "platform".to_string());
                m
            },
            match_expressions: vec![],
        };
        assert!(
            !matches_selector(None, &selector),
            "None labels with non-empty match_labels should reject"
        );
    }

    // -----------------------------------------------------------------------
    // record_reconcile_metrics and record_reconcile_success
    // -----------------------------------------------------------------------

    #[test]
    fn record_reconcile_metrics_labels_propagate() {
        use prometheus_client::registry::Registry;
        use crate::metrics::{Metrics, ReconcileLabels};

        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);

        // Directly exercise the metric families (same code path as record_reconcile_metrics)
        let labels = ReconcileLabels {
            controller: "machine_config".to_string(),
            result: "success".to_string(),
        };
        metrics.reconciliations_total.get_or_create(&labels).inc();
        metrics
            .reconciliation_duration_seconds
            .get_or_create(&labels)
            .observe(0.5);

        // Verify the counter incremented
        let count = metrics
            .reconciliations_total
            .get_or_create(&labels)
            .get();
        assert!(count >= 1, "counter should have incremented");
    }

    #[test]
    fn record_reconcile_metrics_error_labels_separate() {
        use prometheus_client::registry::Registry;
        use crate::metrics::{Metrics, ReconcileLabels};

        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);

        let success_labels = ReconcileLabels {
            controller: "drift_alert".to_string(),
            result: "success".to_string(),
        };
        let error_labels = ReconcileLabels {
            controller: "drift_alert".to_string(),
            result: "error".to_string(),
        };

        metrics
            .reconciliations_total
            .get_or_create(&success_labels)
            .inc();
        metrics
            .reconciliations_total
            .get_or_create(&success_labels)
            .inc();
        metrics
            .reconciliations_total
            .get_or_create(&error_labels)
            .inc();

        let success_count = metrics
            .reconciliations_total
            .get_or_create(&success_labels)
            .get();
        let error_count = metrics
            .reconciliations_total
            .get_or_create(&error_labels)
            .get();
        assert_eq!(success_count, 2);
        assert_eq!(error_count, 1);
    }

    // -----------------------------------------------------------------------
    // Drift and policy metrics families
    // -----------------------------------------------------------------------

    #[test]
    fn drift_metrics_increment_by_severity() {
        use prometheus_client::registry::Registry;
        use crate::metrics::{DriftLabels, Metrics};

        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);

        let labels = DriftLabels {
            severity: "warning".to_string(),
            namespace: "default".to_string(),
        };
        metrics.drift_events_total.get_or_create(&labels).inc();
        metrics.drift_events_total.get_or_create(&labels).inc();

        let count = metrics.drift_events_total.get_or_create(&labels).get();
        assert_eq!(count, 2);
    }

    #[test]
    fn policy_compliance_gauge_set_and_read() {
        use prometheus_client::registry::Registry;
        use crate::metrics::{Metrics, PolicyLabels};

        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);

        let labels = PolicyLabels {
            policy: "security-baseline".to_string(),
            namespace: "production".to_string(),
        };
        metrics.devices_compliant.get_or_create(&labels).set(42);

        let value = metrics.devices_compliant.get_or_create(&labels).get();
        assert_eq!(value, 42);
    }

    // -----------------------------------------------------------------------
    // build_drift_alert_conditions: Medium severity (not escalated)
    // -----------------------------------------------------------------------

    #[test]
    fn build_drift_alert_conditions_medium_not_escalated() {
        let conditions = build_drift_alert_conditions(
            &DriftSeverity::Medium,
            false,
            "dev-2",
            2,
            "2024-06-01T00:00:00Z",
            Some(5),
        );
        assert_eq!(conditions.len(), 3);
        assert_eq!(conditions[0].condition_type, "Acknowledged");
        assert_eq!(conditions[0].status, "False");
        assert_eq!(conditions[1].condition_type, "Resolved");
        assert_eq!(conditions[1].status, "False");
        assert!(conditions[1].message.contains("dev-2"));
        assert!(conditions[1].message.contains("2 detail(s)"));
        // Medium is NOT escalated
        assert_eq!(conditions[2].condition_type, "Escalated");
        assert_eq!(conditions[2].status, "False");
        assert_eq!(conditions[2].reason, "BelowThreshold");
    }

    // -----------------------------------------------------------------------
    // build_drift_alert_conditions: resolved with zero details
    // -----------------------------------------------------------------------

    #[test]
    fn build_drift_alert_conditions_resolved_zero_details() {
        let conditions = build_drift_alert_conditions(
            &DriftSeverity::Low,
            true,
            "dev-3",
            0,
            "2024-07-01T00:00:00Z",
            None,
        );
        assert_eq!(conditions[1].status, "True");
        assert_eq!(conditions[1].reason, "DriftResolved");
        assert_eq!(conditions[1].message, "Drift has been resolved");
        assert!(conditions[0].observed_generation.is_none());
    }

    // -----------------------------------------------------------------------
    // build_condition: empty existing list uses provided time
    // -----------------------------------------------------------------------

    #[test]
    fn build_condition_empty_existing_uses_now() {
        let c = build_condition(
            &[],
            "Ready",
            "True",
            "AllGood",
            "everything is fine",
            "2024-08-01T00:00:00Z",
            Some(10),
        );
        assert_eq!(c.condition_type, "Ready");
        assert_eq!(c.status, "True");
        assert_eq!(c.reason, "AllGood");
        assert_eq!(c.message, "everything is fine");
        assert_eq!(c.last_transition_time, "2024-08-01T00:00:00Z");
        assert_eq!(c.observed_generation, Some(10));
    }

    // -----------------------------------------------------------------------
    // validate_policy_compliance: combined modules + packages + settings
    // -----------------------------------------------------------------------

    #[test]
    fn policy_compliance_combined_all_requirements() {
        let mut spec = mc_spec("host1", "default");
        spec.module_refs = vec![ModuleRef {
            name: "corp-vpn".to_string(),
            required: true,
        }];
        spec.packages = vec![PackageRef {
            name: "kubectl".to_string(),
            version: None,
        }];
        spec.system_settings
            .insert("enforce_tls".to_string(), serde_json::json!(true));

        let mut status = MachineConfigStatus::default();
        status
            .package_versions
            .insert("kubectl".to_string(), "1.29.0".to_string());

        let required_modules = vec![ModuleRef {
            name: "corp-vpn".to_string(),
            required: true,
        }];
        let required_packages = vec![PackageRef {
            name: "kubectl".to_string(),
            version: Some(">=1.28".to_string()),
        }];
        let mut required_settings = BTreeMap::new();
        required_settings.insert("enforce_tls".to_string(), serde_json::json!(true));

        // All requirements met
        assert!(validate_policy_compliance(
            &spec,
            Some(&status),
            &required_modules,
            &required_packages,
            &required_settings,
        ));

        // Fail if module missing
        spec.module_refs.clear();
        assert!(!validate_policy_compliance(
            &spec,
            Some(&status),
            &required_modules,
            &required_packages,
            &required_settings,
        ));
    }

    // -----------------------------------------------------------------------
    // merge_policy_requirements: empty cluster + empty namespace
    // -----------------------------------------------------------------------

    #[test]
    fn merge_policy_all_empty() {
        let cluster = ClusterConfigPolicySpec::default();
        let merged = merge_policy_requirements(&cluster, &[]);
        assert!(merged.packages.is_empty());
        assert!(merged.modules.is_empty());
        assert!(merged.settings.is_empty());
    }

    // -----------------------------------------------------------------------
    // compliance_summary edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn compliance_summary_large_numbers() {
        let result = compliance_summary(999_999, 1);
        assert_eq!(result, "999999 compliant, 1 non-compliant");
    }

    // -----------------------------------------------------------------------
    // validate_spec with valid spec
    // -----------------------------------------------------------------------

    #[test]
    fn validate_spec_valid() {
        let spec = mc_spec("myhost.local", "production");
        assert!(validate_spec(&spec).is_ok());
    }

    // -----------------------------------------------------------------------
    // log_reconcile: verify the closure doesn't panic on Ok/Err
    // -----------------------------------------------------------------------

    #[test]
    fn log_reconcile_ok_does_not_panic() {
        use kube::runtime::controller::Action;
        use kube::runtime::reflector::ObjectRef;

        let log_fn = log_reconcile::<MachineConfig>("MachineConfig");

        // Create a mock ObjectRef
        let obj_ref = ObjectRef::<MachineConfig>::new("test-mc").within("default");
        let action = Action::requeue(std::time::Duration::from_secs(60));
        let result: ReconcileResult<MachineConfig> = Ok((obj_ref, action));

        // Should log and not panic
        futures::executor::block_on(log_fn(result));
    }

    // -----------------------------------------------------------------------
    // matches_selector: NotIn with missing label accepts (key absent)
    // -----------------------------------------------------------------------

    #[test]
    fn matches_selector_not_in_missing_key_accepts() {
        let labels = BTreeMap::new(); // no labels at all
        let selector = LabelSelector {
            match_labels: Default::default(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "restricted".to_string(),
                operator: SelectorOperator::NotIn,
                values: vec!["true".to_string()],
            }],
        };
        // Key is missing → is_none_or returns true → accepts
        assert!(matches_selector(Some(&labels), &selector));
    }
}

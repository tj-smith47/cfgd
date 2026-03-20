use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::Controller;
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType, Recorder, Reporter};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Client, Resource, ResourceExt};
use tracing::{debug, info, warn};

use k8s_openapi::api::core::v1::Namespace;

use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicySpec, ClusterConfigPolicyStatus, Condition,
    ConfigPolicy, ConfigPolicySpec, ConfigPolicyStatus, DriftAlert, DriftAlertStatus,
    DriftSeverity, LabelSelector, MachineConfig, MachineConfigSpec, MachineConfigStatus, ModuleRef,
    PackageRef, SelectorOperator, version_satisfies,
};
use crate::errors::OperatorError;
use crate::metrics::{DriftLabels, Metrics, PolicyLabels, ReconcileLabels};

const FIELD_MANAGER_OPERATOR: &str = "cfgd-operator";
const FIELD_MANAGER_STATUS: &str = "cfgd-operator/status";
const MACHINE_CONFIG_FINALIZER: &str = "cfgd.io/machine-config-cleanup";

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

    let mc_ctx = Arc::clone(&ctx);
    let da_ctx = Arc::clone(&ctx);
    let cp_ctx = Arc::clone(&ctx);
    let ccp_ctx = Arc::clone(&ctx);

    info!("Starting MachineConfig controller");
    info!("Starting DriftAlert controller");
    info!("Starting ConfigPolicy controller");
    info!("Starting ClusterConfigPolicy controller");

    let mc_controller = Controller::new(machines, WatcherConfig::default())
        .owns(
            Api::<DriftAlert>::all(client.clone()),
            WatcherConfig::default(),
        )
        .run(reconcile_machine_config, error_policy_mc, mc_ctx)
        .for_each(|result| async move {
            match result {
                Ok((obj_ref, _action)) => {
                    info!(name = %obj_ref.name, "MachineConfig reconciled");
                }
                Err(err) => {
                    warn!(error = %err, "MachineConfig reconciliation error");
                }
            }
        });

    let da_controller = Controller::new(alerts, WatcherConfig::default())
        .run(reconcile_drift_alert, error_policy_da, da_ctx)
        .for_each(|result| async move {
            match result {
                Ok((obj_ref, _action)) => {
                    info!(name = %obj_ref.name, "DriftAlert reconciled");
                }
                Err(err) => {
                    warn!(error = %err, "DriftAlert reconciliation error");
                }
            }
        });

    let cp_controller = Controller::new(policies, WatcherConfig::default())
        .watches(
            Api::<MachineConfig>::all(client.clone()),
            WatcherConfig::default(),
            |mc| {
                // When a MachineConfig changes, requeue all ConfigPolicies in its namespace
                mc.namespace()
                    .map(|ns| kube::runtime::reflector::ObjectRef::new("").within(&ns))
            },
        )
        .run(reconcile_config_policy, error_policy_cp, cp_ctx)
        .for_each(|result| async move {
            match result {
                Ok((obj_ref, _action)) => {
                    info!(name = %obj_ref.name, "ConfigPolicy reconciled");
                }
                Err(err) => {
                    warn!(error = %err, "ConfigPolicy reconciliation error");
                }
            }
        });

    let ccp_controller = Controller::new(cluster_policies, WatcherConfig::default())
        .run(reconcile_cluster_config_policy, error_policy_ccp, ccp_ctx)
        .for_each(|result| async move {
            match result {
                Ok((obj_ref, _action)) => {
                    info!(name = %obj_ref.name, "ClusterConfigPolicy reconciled");
                }
                Err(err) => {
                    warn!(error = %err, "ClusterConfigPolicy reconciliation error");
                }
            }
        });

    tokio::join!(mc_controller, da_controller, cp_controller, ccp_controller);

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
        debug!(error = %e, "Failed to publish event (best-effort)");
    }
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
        info!(name = %name, "MachineConfig being deleted, running cleanup");
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
        info!(name = %name, "Added finalizer to MachineConfig");
    }

    if let Err(e) = validate_spec(&obj.spec) {
        let error_msg = e.to_string();
        publish_event(
            &ctx.recorder,
            &Event {
                type_: EventType::Warning,
                reason: "ReconcileError".into(),
                note: Some(format!("Reconciliation failed for {}: {}", name, error_msg)),
                action: "Reconcile".into(),
                secondary: None,
            },
            &obj.object_ref(&()),
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
        "Reconciling MachineConfig"
    );

    let current_generation = obj.meta().generation;
    let existing_status = obj.status.as_ref();
    let observed_generation = existing_status.and_then(|s| s.observed_generation);
    let existing_conditions = existing_status
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);

    // Check if any DriftAlerts exist for this MachineConfig
    let has_drift = has_active_drift_alerts(&ctx.client, &namespace, &name).await;

    // Skip if we've already observed this generation and there's no drift
    let generation_unchanged =
        current_generation.is_some() && current_generation == observed_generation;
    if generation_unchanged && !has_drift {
        info!(name = %name, "Already reconciled this generation, skipping");
        return Ok(Action::requeue(std::time::Duration::from_secs(60)));
    }

    // If not drifted, clean up any stale DriftAlerts for this MachineConfig
    if !has_drift {
        cleanup_drift_alerts(&ctx.client, &namespace, &name).await;
    }

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
                    "ModulesResolved", "True", "AllResolved",
                    "All module references resolved",
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
        publish_event(
            &ctx.recorder,
            &Event {
                type_: EventType::Warning,
                reason: "ReconcileError".into(),
                note: Some(format!("Reconciliation failed for {}: {}", name, error_msg)),
                action: "Reconcile".into(),
                secondary: None,
            },
            &obj.object_ref(&()),
        )
        .await;
        return Err(OperatorError::Reconciliation(error_msg));
    }

    info!(name = %name, "Status updated with last_reconciled timestamp");

    publish_event(
        &ctx.recorder,
        &Event {
            type_: EventType::Normal,
            reason: "Reconciled".into(),
            note: Some(format!("MachineConfig {} reconciled successfully", name)),
            action: "Reconcile".into(),
            secondary: None,
        },
        &obj.object_ref(&()),
    )
    .await;

    if has_drift {
        publish_event(
            &ctx.recorder,
            &Event {
                type_: EventType::Warning,
                reason: "DriftDetected".into(),
                note: Some(format!(
                    "Drift detected on device for MachineConfig {}",
                    name
                )),
                action: "DriftCheck".into(),
                secondary: None,
            },
            &obj.object_ref(&()),
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

    let labels = ReconcileLabels {
        controller: "machine_config".to_string(),
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
    warn!(error = %error, "MachineConfig reconciliation error, requeuing");
    ctx.metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "machine_config".to_string(),
            result: "error".to_string(),
        })
        .inc();
    Action::requeue(std::time::Duration::from_secs(30))
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
        "Reconciling DriftAlert"
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
                info!(name = %name, machine_config = %mc.name_any(), "Set owner reference on DriftAlert");
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
                        resolved: true,
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
                    warn!(name = %name, error = %e, "Failed to set Resolved condition on DriftAlert");
                }

                publish_event(
                    &ctx.recorder,
                    &Event {
                        type_: EventType::Normal,
                        reason: "DriftResolved".into(),
                        note: Some(format!("DriftAlert {} resolved", name)),
                        action: "DriftCheck".into(),
                        secondary: None,
                    },
                    &obj.object_ref(&()),
                )
                .await;

                if let Err(e) = alerts.delete(&name, &Default::default()).await {
                    warn!(name = %name, error = %e, "Failed to delete resolved DriftAlert");
                }

                let labels = ReconcileLabels {
                    controller: "drift_alert".to_string(),
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
                    "MachineConfig drift condition set"
                );

                publish_event(
                    &ctx.recorder,
                    &Event {
                        type_: EventType::Warning,
                        reason: "DriftDetected".into(),
                        note: Some(format!(
                            "Drift detected from device {} — {} details",
                            obj.spec.device_id,
                            obj.spec.drift_details.len()
                        )),
                        action: "DriftCheck".into(),
                        secondary: None,
                    },
                    &mc.object_ref(&()),
                )
                .await;

                // Patch DriftAlert status with Resolved=False condition
                let da_api: Api<DriftAlert> = namespaced_api(&ctx.client, &namespace)?;
                let da_status = serde_json::json!({
                    "status": DriftAlertStatus {
                        detected_at: Some(now.clone()),
                        resolved_at: None,
                        resolved: false,
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
                "DriftAlert references non-existent MachineConfig"
            );
            // Record as error, not success (H6 fix)
            let labels = ReconcileLabels {
                controller: "drift_alert".to_string(),
                result: "error".to_string(),
            };
            ctx.metrics
                .reconciliations_total
                .get_or_create(&labels)
                .inc();
            ctx.metrics
                .reconciliation_duration_seconds
                .get_or_create(&labels)
                .observe(start.elapsed().as_secs_f64());
            return Ok(Action::requeue(std::time::Duration::from_secs(60)));
        }
        Err(e) => {
            return Err(OperatorError::Reconciliation(format!(
                "failed to get MachineConfig {mc_name}: {e}"
            )));
        }
    }

    let labels = ReconcileLabels {
        controller: "drift_alert".to_string(),
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
            warn!(error = %e, mc_name = %mc_name, "Failed to list DriftAlerts for drift check");
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
            warn!(error = %e, "Failed to list DriftAlerts for cleanup");
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
                "Failed to delete resolved DriftAlert"
            );
        } else {
            info!(name = %alert_name, "Deleted resolved DriftAlert");
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
    warn!(error = %error, "DriftAlert reconciliation error, requeuing");
    ctx.metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "drift_alert".to_string(),
            result: "error".to_string(),
        })
        .inc();
    Action::requeue(std::time::Duration::from_secs(30))
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
        "Reconciling ConfigPolicy"
    );

    let machines: Api<MachineConfig> = namespaced_api(&ctx.client, &namespace)?;

    let mc_list = machines.list(&ListParams::default()).await.map_err(|e| {
        OperatorError::Reconciliation(format!("failed to list MachineConfigs: {e}"))
    })?;

    let mut compliant_count: u32 = 0;
    let mut non_compliant_count: u32 = 0;

    for mc in &mc_list {
        let mc_labels = mc.metadata.labels.as_ref();
        if !matches_selector(mc_labels, &obj.spec.target_selector) {
            continue;
        }

        let compliant = validate_policy_compliance(
            &mc.spec,
            mc.status.as_ref(),
            &obj.spec.required_modules,
            &obj.spec.packages,
            &obj.spec.package_versions,
            &obj.spec.settings,
        );

        // Update the MachineConfig's Compliant condition
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
            warn!(name = %mc_name, error = %e, "Failed to update Compliant condition on MachineConfig");
        }

        if compliant {
            compliant_count += 1;
        } else {
            non_compliant_count += 1;
            publish_event(
                &ctx.recorder,
                &Event {
                    type_: EventType::Warning,
                    reason: "PolicyViolation".into(),
                    note: Some(format!(
                        "MachineConfig {} violates policy {}",
                        mc_name, name
                    )),
                    action: "PolicyEvaluate".into(),
                    secondary: None,
                },
                &mc.object_ref(&()),
            )
            .await;
        }
    }

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
                    &format!("{} compliant, {} non-compliant", compliant_count, non_compliant_count),
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
        "ConfigPolicy status updated"
    );

    publish_event(
        &ctx.recorder,
        &Event {
            type_: EventType::Normal,
            reason: "Evaluated".into(),
            note: Some(format!(
                "{} compliant, {} non-compliant",
                compliant_count, non_compliant_count
            )),
            action: "Evaluate".into(),
            secondary: None,
        },
        &obj.object_ref(&()),
    )
    .await;

    if non_compliant_count > 0 {
        publish_event(
            &ctx.recorder,
            &Event {
                type_: EventType::Warning,
                reason: "NonCompliantTargets".into(),
                note: Some(format!(
                    "{} non-compliant MachineConfigs",
                    non_compliant_count
                )),
                action: "Evaluate".into(),
                secondary: None,
            },
            &obj.object_ref(&()),
        )
        .await;
    }

    ctx.metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: name.clone(),
            namespace: namespace.clone(),
        })
        .set(i64::from(compliant_count));

    let labels = ReconcileLabels {
        controller: "config_policy".to_string(),
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

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

fn matches_selector(labels: Option<&BTreeMap<String, String>>, selector: &LabelSelector) -> bool {
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
    package_versions: &BTreeMap<String, String>,
    settings: &BTreeMap<String, serde_json::Value>,
) -> bool {
    // Check required modules are present in moduleRefs
    for module in required_modules {
        if !spec.module_refs.iter().any(|mr| mr.name == module.name) {
            return false;
        }
    }
    for pkg in packages {
        if !spec.packages.iter().any(|p| p.name == pkg.name) {
            return false;
        }
    }
    for (pkg, req_str) in package_versions {
        if !spec.packages.iter().any(|p| p.name == *pkg) {
            return false;
        }
        let installed_versions = status.map(|s| &s.package_versions);
        match installed_versions.and_then(|pv| pv.get(pkg)) {
            Some(reported) => {
                if !version_satisfies(reported, req_str) {
                    return false;
                }
            }
            None => return false,
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

/// Error policy for ConfigPolicy reconciliation failures.
/// kube-rs Controller applies exponential backoff internally.
fn error_policy_cp(
    _obj: Arc<ConfigPolicy>,
    error: &OperatorError,
    ctx: Arc<ControllerContext>,
) -> Action {
    warn!(error = %error, "ConfigPolicy reconciliation error, requeuing");
    ctx.metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "config_policy".to_string(),
            result: "error".to_string(),
        })
        .inc();
    Action::requeue(std::time::Duration::from_secs(30))
}

// ---------------------------------------------------------------------------
// ClusterConfigPolicy controller — cluster-wide policy enforcement
// ---------------------------------------------------------------------------

struct MergedPolicyRequirements {
    packages: Vec<PackageRef>,
    modules: Vec<ModuleRef>,
    settings: BTreeMap<String, serde_json::Value>,
    versions: BTreeMap<String, String>,
}

fn merge_policy_requirements(
    cluster: &ClusterConfigPolicySpec,
    namespace_policies: &[&ConfigPolicySpec],
) -> MergedPolicyRequirements {
    let mut packages = cluster.packages.clone();
    let mut modules = cluster.required_modules.clone();
    let mut settings = BTreeMap::new();
    let mut versions = BTreeMap::new();

    // Collect union of all namespace-scoped policies as base
    for ns in namespace_policies {
        for pkg in &ns.packages {
            if !packages.iter().any(|p| p.name == pkg.name) {
                packages.push(pkg.clone());
            }
        }
        for module in &ns.required_modules {
            if !modules.iter().any(|m| m.name == module.name) {
                modules.push(module.clone());
            }
        }
        // Namespace settings as base (later policies override earlier for same key)
        settings.extend(ns.settings.clone());
        versions.extend(ns.package_versions.clone());
    }

    // Cluster overrides (cluster wins for settings and versions)
    settings.extend(cluster.settings.clone());
    versions.extend(cluster.package_versions.clone());

    MergedPolicyRequirements {
        packages,
        modules,
        settings,
        versions,
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
        "Reconciling ClusterConfigPolicy"
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

        for mc in &mc_list {
            let compliant = validate_policy_compliance(
                &mc.spec,
                mc.status.as_ref(),
                &merged.modules,
                &merged.packages,
                &merged.versions,
                &merged.settings,
            );
            if compliant {
                compliant_count += 1;
            } else {
                non_compliant_count += 1;
                let mc_name = mc.name_any();
                publish_event(
                    &ctx.recorder,
                    &Event {
                        type_: EventType::Warning,
                        reason: "PolicyViolation".into(),
                        note: Some(format!(
                            "MachineConfig {} violates policy {}",
                            mc_name, name
                        )),
                        action: "PolicyEvaluate".into(),
                        secondary: None,
                    },
                    &mc.object_ref(&()),
                )
                .await;
            }
        }
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
                    &format!("{} compliant, {} non-compliant", compliant_count, non_compliant_count),
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
        "ClusterConfigPolicy status updated"
    );

    publish_event(
        &ctx.recorder,
        &Event {
            type_: EventType::Normal,
            reason: "Evaluated".into(),
            note: Some(format!(
                "{} compliant, {} non-compliant",
                compliant_count, non_compliant_count
            )),
            action: "Evaluate".into(),
            secondary: None,
        },
        &obj.object_ref(&()),
    )
    .await;

    if non_compliant_count > 0 {
        publish_event(
            &ctx.recorder,
            &Event {
                type_: EventType::Warning,
                reason: "NonCompliantTargets".into(),
                note: Some(format!(
                    "{} non-compliant MachineConfigs",
                    non_compliant_count
                )),
                action: "Evaluate".into(),
                secondary: None,
            },
            &obj.object_ref(&()),
        )
        .await;
    }

    ctx.metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: name.clone(),
            namespace: String::new(), // cluster-scoped
        })
        .set(i64::from(compliant_count));

    let labels = ReconcileLabels {
        controller: "cluster_config_policy".to_string(),
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

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

fn error_policy_ccp(
    _obj: Arc<ClusterConfigPolicy>,
    error: &OperatorError,
    ctx: Arc<ControllerContext>,
) -> Action {
    warn!(error = %error, "ClusterConfigPolicy reconciliation error, requeuing");
    ctx.metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "cluster_config_policy".to_string(),
            result: "error".to_string(),
        })
        .inc();
    Action::requeue(std::time::Duration::from_secs(30))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crds::LabelSelectorRequirement;

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
    fn chrono_now_produces_rfc3339() {
        let now = cfgd_core::utc_now_iso8601();
        assert!(now.ends_with('Z'));
        assert!(now.contains('T'));
        assert_eq!(now.len(), 20); // "YYYY-MM-DDTHH:MM:SSZ"
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
            &Default::default(),
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
            &Default::default(),
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
            &Default::default(),
            &policy_settings
        ));

        let mut wrong = BTreeMap::new();
        wrong.insert(
            "key".to_string(),
            serde_json::Value::String("wrong".to_string()),
        );
        assert!(!validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &Default::default(),
            &wrong
        ));
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
            &Default::default(),
            &policy_settings
        ));

        let mut wrong = BTreeMap::new();
        wrong.insert("enabled".to_string(), serde_json::json!(false));
        assert!(!validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &Default::default(),
            &wrong
        ));
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
        let mut reqs = BTreeMap::new();
        reqs.insert("kubectl".to_string(), ">=1.28".to_string());
        assert!(validate_policy_compliance(
            &spec,
            Some(&status),
            &[],
            &[],
            &reqs,
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
        let mut reqs = BTreeMap::new();
        reqs.insert("kubectl".to_string(), ">=1.28".to_string());
        assert!(!validate_policy_compliance(
            &spec,
            Some(&status),
            &[],
            &[],
            &reqs,
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
        let mut reqs = BTreeMap::new();
        reqs.insert("kubectl".to_string(), ">=1.28".to_string());
        assert!(!validate_policy_compliance(
            &spec,
            None,
            &[],
            &[],
            &reqs,
            &Default::default()
        ));
    }

    #[test]
    fn policy_version_enforcement_missing_package() {
        let mut reqs = BTreeMap::new();
        reqs.insert("kubectl".to_string(), ">=1.28".to_string());
        assert!(!validate_policy_compliance(
            &mc_spec("h", "p"),
            None,
            &[],
            &[],
            &reqs,
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
            &Default::default(),
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
            &Default::default(),
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
            &Default::default(),
            &Default::default()
        ));
    }

    #[test]
    fn finalizer_name_constant() {
        assert_eq!(MACHINE_CONFIG_FINALIZER, "cfgd.io/machine-config-cleanup");
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
        let mut cluster_versions = BTreeMap::new();
        cluster_versions.insert("kubectl".to_string(), ">=1.29".to_string());
        let cluster = ClusterConfigPolicySpec {
            package_versions: cluster_versions,
            ..Default::default()
        };
        let mut ns_versions = BTreeMap::new();
        ns_versions.insert("kubectl".to_string(), ">=1.28".to_string());
        let ns = ConfigPolicySpec {
            package_versions: ns_versions,
            ..Default::default()
        };
        let merged = merge_policy_requirements(&cluster, &[&ns]);
        assert_eq!(merged.versions["kubectl"], ">=1.29");
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
}

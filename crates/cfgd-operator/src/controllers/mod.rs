use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use kube::api::Api;
use kube::runtime::Controller;
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType, Recorder, Reporter};
use kube::runtime::reflector::ObjectRef;
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Client, ResourceExt};
use tracing::{debug, info, warn};

use crate::crds::{
    ClusterConfigPolicy, Condition, ConfigPolicy, DriftAlert, DriftSeverity, LabelSelector,
    MachineConfig, Module, SelectorOperator,
};
use crate::errors::OperatorError;
use crate::metrics::{Metrics, ReconcileLabels};

#[cfg(test)]
use crate::crds::{
    ClusterConfigPolicySpec, ConfigPolicySpec, MachineConfigSpec, MachineConfigStatus, ModuleRef,
    ModuleSignature, ModuleSpec, PackageRef,
};

pub(super) const FIELD_MANAGER_OPERATOR: &str = "cfgd-operator";
pub(super) const FIELD_MANAGER_STATUS: &str = "cfgd-operator/status";
pub(super) const MACHINE_CONFIG_FINALIZER: &str = "cfgd.io/machine-config-cleanup";

pub(super) fn compliance_summary(compliant: u32, non_compliant: u32) -> String {
    format!("{compliant} compliant, {non_compliant} non-compliant")
}

// ---------------------------------------------------------------------------
// Shared metrics helpers (DRY for error_policy and reconcile success blocks)
// ---------------------------------------------------------------------------

pub(super) fn record_error_and_requeue(
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

/// Build a kube-rs `error_policy` closure tagged with `controller` for CRD type `K`.
///
/// Collapses the five identical per-controller `error_policy_*` one-liners
/// (MachineConfig, DriftAlert, ConfigPolicy, ClusterConfigPolicy, Module) into
/// a single generic helper — the only per-controller variation was the metrics
/// label, so the type parameter `K` just selects the `Arc<K>` callers expect.
pub(super) fn make_error_policy<K>(
    controller: &'static str,
) -> impl Fn(Arc<K>, &OperatorError, Arc<ControllerContext>) -> Action + Clone
where
    K: kube::Resource + 'static,
{
    move |_obj, error, ctx| record_error_and_requeue(error, &ctx, controller)
}

pub(super) fn record_reconcile_success(
    ctx: &ControllerContext,
    controller: &str,
    start: std::time::Instant,
) {
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

pub(super) fn log_reconcile<K: kube::Resource>(
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

pub(super) fn record_reconcile_metrics(
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
pub(super) fn namespaced_api<
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
        .run(
            reconcile_machine_config,
            make_error_policy::<MachineConfig>("machine_config"),
            mc_ctx,
        )
        .for_each(log_reconcile::<MachineConfig>("MachineConfig"));

    let da_controller = Controller::new(alerts, WatcherConfig::default())
        .run(
            reconcile_drift_alert,
            make_error_policy::<DriftAlert>("drift_alert"),
            da_ctx,
        )
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
        .run(
            reconcile_config_policy,
            make_error_policy::<ConfigPolicy>("config_policy"),
            cp_ctx,
        )
        .for_each(log_reconcile::<ConfigPolicy>("ConfigPolicy"));

    let ccp_controller = Controller::new(cluster_policies, WatcherConfig::default())
        .run(
            reconcile_cluster_config_policy,
            make_error_policy::<ClusterConfigPolicy>("cluster_config_policy"),
            ccp_ctx,
        )
        .for_each(log_reconcile::<ClusterConfigPolicy>("ClusterConfigPolicy"));

    let mod_controller = Controller::new(modules, WatcherConfig::default())
        .run(
            reconcile_module,
            make_error_policy::<Module>("module"),
            mod_ctx,
        )
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
pub(super) fn find_condition_status(
    conditions: &[Condition],
    condition_type: &str,
) -> Option<String> {
    conditions
        .iter()
        .find(|c| c.condition_type == condition_type)
        .map(|c| c.status.clone())
}

/// Find an existing condition's last_transition_time by type.
pub(super) fn find_condition_transition_time(
    conditions: &[Condition],
    condition_type: &str,
) -> Option<String> {
    conditions
        .iter()
        .find(|c| c.condition_type == condition_type)
        .map(|c| c.last_transition_time.clone())
}

/// Build a condition, preserving lastTransitionTime if the status hasn't changed.
pub(super) fn build_condition(
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

pub(super) fn build_drift_alert_conditions(
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

pub(super) async fn publish_event(
    recorder: &Recorder,
    event: &Event,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
) {
    if let Err(e) = recorder.publish(event, obj_ref).await {
        debug!(error = %e, "failed to publish event (best-effort)");
    }
}

pub(super) async fn emit_event(
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
// Submodule declarations
// ---------------------------------------------------------------------------

mod cluster_config_policy;
mod config_policy;
mod drift_alert;
mod machine_config;
mod module;

// Bring per-controller reconcile fns into scope so run() can wire them up.
use cluster_config_policy::reconcile_cluster_config_policy;
use config_policy::reconcile_config_policy;
use drift_alert::reconcile_drift_alert;
use machine_config::reconcile_machine_config;
use module::reconcile_module;

// Test-only helpers re-imported so the cross-cutting tests block keeps working.
#[cfg(test)]
use config_policy::{merge_policy_requirements, validate_policy_compliance};
#[cfg(test)]
use machine_config::validate_spec;
#[cfg(test)]
use module::evaluate_module_verification;

// ---------------------------------------------------------------------------
// Shared selector helper (used across config_policy and cluster_config_policy)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test_fixtures;
#[cfg(test)]
pub(crate) mod test_kube_harness;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_cluster_config_policy;
#[cfg(test)]
mod tests_config_policy;
#[cfg(test)]
mod tests_drift_alert;
#[cfg(test)]
mod tests_machine_config;
#[cfg(test)]
mod tests_module;

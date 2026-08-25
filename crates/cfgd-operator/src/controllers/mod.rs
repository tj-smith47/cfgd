use std::collections::BTreeMap;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::core::v1::Namespace;
use kube::api::{Api, Patch, PatchParams};
use kube::core::PartialObjectMeta;
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType, Recorder, Reporter};
use kube::runtime::reflector::{ObjectRef, Store};
use kube::runtime::watcher::{self, Config as WatcherConfig};
use kube::runtime::{Controller, WatchStreamExt, reflector};
use kube::{Client, ResourceExt};
use tracing::{debug, info, warn};

use crate::crds::{
    ClusterConfigPolicy, Condition, ConfigPolicy, DriftAlert, DriftSeverity, LabelSelector,
    MAX_NON_COMPLIANT_MACHINES, MachineConfig, Module, SelectorOperator,
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
pub(super) const CONFIG_POLICY_FINALIZER: &str = "cfgd.io/config-policy-cleanup";
pub(super) const CLUSTER_CONFIG_POLICY_FINALIZER: &str = "cfgd.io/cluster-config-policy-cleanup";

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
    pub stores: ControllerStores,
    pub artifact_platforms: ArtifactPlatformReader,
}

/// How a Module's artifact platforms are read.
///
/// Reading them means fetching the artifact's manifest from its registry, so
/// this is a value the context carries rather than a call the reconcile makes
/// directly: a controller test drives `reconcile_module` end to end, and must
/// reach no registry to do it. Production installs
/// [`ArtifactPlatformReader::from_registry`]; a test installs
/// [`ArtifactPlatformReader::fixed`] and gets the same reconcile with a known
/// answer.
type PlatformLookup = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

#[derive(Clone)]
pub struct ArtifactPlatformReader(PlatformLookup);

impl ArtifactPlatformReader {
    /// The production reader: the platforms the artifact's own manifest names.
    ///
    /// A registry that cannot be reached, or an artifact that declares no
    /// platform, answers an empty list — the `Platforms` column then stays
    /// blank, which is what an unknown platform set looks like. It is not a
    /// reconcile failure: the module is still admissible, and the next
    /// reconcile re-reads.
    #[must_use]
    pub fn from_registry() -> Self {
        Self(Arc::new(
            |reference| match cfgd_core::oci::artifact_platforms(reference) {
                Ok(platforms) => platforms,
                Err(e) => {
                    warn!(reference = %reference, error = %e, "cannot read artifact platforms");
                    Vec::new()
                }
            },
        ))
    }

    /// A reader that answers `platforms` for every reference.
    #[cfg(test)]
    #[must_use]
    pub fn fixed(platforms: Vec<String>) -> Self {
        Self(Arc::new(move |_| platforms.clone()))
    }

    /// Read the platforms `reference` declares. Blocking: callers dispatch it
    /// off the reactor.
    #[must_use]
    pub fn read_platforms(&self, reference: &str) -> Vec<String> {
        (self.0)(reference)
    }
}

/// How long a reconcile waits for a watch cache to finish its initial list
/// before giving up and requeuing. Only ever spent on a cache that has not
/// been populated yet — every later reconcile resolves immediately.
const STORE_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// The watch-backed caches every controller reads cross-resource state from.
///
/// Five of the six are the primary [`Store`] of the controller that roots that
/// resource, so they cost no extra watch: the same stream that triggers a
/// reconcile also populates the cache. `namespaces` is the exception — no
/// controller roots a Namespace — and is fed by a dedicated reflector driven
/// alongside the controllers in [`run`].
#[derive(Clone)]
pub struct ControllerStores {
    pub machine_configs: Store<MachineConfig>,
    pub config_policies: Store<ConfigPolicy>,
    pub cluster_config_policies: Store<ClusterConfigPolicy>,
    pub modules: Store<Module>,
    pub drift_alerts: Store<DriftAlert>,
    /// Metadata-only: the two reads are `metadata.labels` and `metadata.name`.
    pub namespaces: Store<PartialObjectMeta<Namespace>>,
}

/// Wait for `store` to have completed its initial list.
///
/// A cache that is not yet populated is indistinguishable from an empty
/// cluster, and every caller here turns "no objects" into a status it writes —
/// so a not-yet-ready cache must requeue rather than answer.
async fn ready_store<K>(store: &Store<K>, kind: &str) -> Result<(), OperatorError>
where
    K: kube::runtime::reflector::Lookup + Clone + 'static,
    K::DynamicType: Eq + Hash + Clone,
{
    match tokio::time::timeout(STORE_READY_TIMEOUT, store.wait_until_ready()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(OperatorError::Reconciliation(format!(
            "{kind} watch cache stopped before it was populated: {e}"
        ))),
        Err(_) => {
            // Logged as well as returned so it correlates with the per-watch
            // error `warn!`s, which carry the same `kind` field. A timeout with
            // a matching watch error is a watch that cannot establish; a
            // timeout alone is an operator still completing its first list.
            warn!(
                kind = %kind,
                timeout_secs = STORE_READY_TIMEOUT.as_secs(),
                "watch cache never completed its initial list"
            );
            Err(OperatorError::Reconciliation(format!(
                "{kind} watch cache never completed its initial list within {}s — \
                 the operator is still starting up, or the {kind} watch cannot \
                 establish (check RBAC and API server connectivity)",
                STORE_READY_TIMEOUT.as_secs()
            )))
        }
    }
}

/// Order a cache snapshot by `(namespace, name)`.
///
/// A [`Store`] is a hash map, so its snapshot order is arbitrary and differs
/// between processes. Every caller here walks the snapshot performing writes
/// and emitting events, and an operator whose write order changes per restart
/// is one nobody can read a log of.
fn in_stable_order<K: kube::Resource>(mut objects: Vec<Arc<K>>) -> Vec<Arc<K>> {
    objects.sort_by(|a, b| {
        let key = |o: &Arc<K>| {
            (
                o.meta().namespace.clone().unwrap_or_default(),
                o.meta().name.clone().unwrap_or_default(),
            )
        };
        key(a).cmp(&key(b))
    });
    objects
}

impl ControllerStores {
    /// Every MachineConfig in `namespace`.
    pub(super) async fn machine_configs_in(
        &self,
        namespace: &str,
    ) -> Result<Vec<Arc<MachineConfig>>, OperatorError> {
        ready_store(&self.machine_configs, "MachineConfig").await?;
        Ok(in_stable_order(self.machine_configs.state_filter(|mc| {
            mc.metadata.namespace.as_deref() == Some(namespace)
        })))
    }

    /// Every ConfigPolicy in `namespace`.
    pub(super) async fn config_policies_in(
        &self,
        namespace: &str,
    ) -> Result<Vec<Arc<ConfigPolicy>>, OperatorError> {
        ready_store(&self.config_policies, "ConfigPolicy").await?;
        Ok(in_stable_order(self.config_policies.state_filter(|cp| {
            cp.metadata.namespace.as_deref() == Some(namespace)
        })))
    }

    /// Every DriftAlert in `namespace`, or across the cluster when `namespace`
    /// is empty.
    pub(super) async fn drift_alerts_in(
        &self,
        namespace: &str,
    ) -> Result<Vec<Arc<DriftAlert>>, OperatorError> {
        ready_store(&self.drift_alerts, "DriftAlert").await?;
        Ok(in_stable_order(if namespace.is_empty() {
            self.drift_alerts.state()
        } else {
            self.drift_alerts
                .state_filter(|da| da.metadata.namespace.as_deref() == Some(namespace))
        }))
    }

    /// Every ClusterConfigPolicy.
    pub(super) async fn all_cluster_config_policies(
        &self,
    ) -> Result<Vec<Arc<ClusterConfigPolicy>>, OperatorError> {
        ready_store(&self.cluster_config_policies, "ClusterConfigPolicy").await?;
        Ok(in_stable_order(self.cluster_config_policies.state()))
    }

    /// Every Module.
    pub(super) async fn all_modules(&self) -> Result<Vec<Arc<Module>>, OperatorError> {
        ready_store(&self.modules, "Module").await?;
        Ok(in_stable_order(self.modules.state()))
    }

    /// Every Namespace, as metadata only.
    pub(super) async fn all_namespaces(
        &self,
    ) -> Result<Vec<Arc<PartialObjectMeta<Namespace>>>, OperatorError> {
        ready_store(&self.namespaces, "Namespace").await?;
        Ok(in_stable_order(self.namespaces.state()))
    }
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

/// Add `finalizer` to `name`, buying one last reconcile in which to clean up
/// whatever this controller wrote elsewhere.
pub(super) async fn add_finalizer<K>(
    api: &Api<K>,
    name: &str,
    finalizers: &[String],
    finalizer: &str,
) -> Result<(), OperatorError>
where
    K: Clone + serde::de::DeserializeOwned + std::fmt::Debug,
{
    let mut updated: Vec<&str> = finalizers.iter().map(String::as_str).collect();
    updated.push(finalizer);
    patch_finalizers(api, name, &updated, "add finalizer to").await
}

/// Drop `finalizer`, releasing `name` for deletion.
pub(super) async fn remove_finalizer<K>(
    api: &Api<K>,
    name: &str,
    finalizers: &[String],
    finalizer: &str,
) -> Result<(), OperatorError>
where
    K: Clone + serde::de::DeserializeOwned + std::fmt::Debug,
{
    let remaining: Vec<&str> = finalizers
        .iter()
        .map(String::as_str)
        .filter(|f| *f != finalizer)
        .collect();
    patch_finalizers(api, name, &remaining, "remove finalizer from").await
}

async fn patch_finalizers<K>(
    api: &Api<K>,
    name: &str,
    finalizers: &[&str],
    what: &str,
) -> Result<(), OperatorError>
where
    K: Clone + serde::de::DeserializeOwned + std::fmt::Debug,
{
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER_OPERATOR),
        &Patch::Merge(serde_json::json!({ "metadata": { "finalizers": finalizers } })),
    )
    .await
    .map_err(|e| OperatorError::Reconciliation(format!("failed to {what} {name}: {e}")))?;
    Ok(())
}

pub async fn run(client: Client, metrics: Metrics) -> Result<(), OperatorError> {
    let reporter = Reporter {
        controller: "cfgd-operator".into(),
        instance: std::env::var("POD_NAME").ok(),
    };
    let recorder = Recorder::new(client.clone(), reporter);

    let machines: Api<MachineConfig> = Api::all(client.clone());
    let alerts: Api<DriftAlert> = Api::all(client.clone());
    let policies: Api<ConfigPolicy> = Api::all(client.clone());
    let cluster_policies: Api<ClusterConfigPolicy> = Api::all(client.clone());
    let modules: Api<Module> = Api::all(client.clone());

    // Each controller builder owns the reflector behind its primary watch, so
    // taking its store here is what lets every OTHER controller read that
    // resource from a cache instead of listing it per reconcile.
    let mc_builder = Controller::new(machines, WatcherConfig::default());
    let da_builder = Controller::new(alerts, WatcherConfig::default());
    let cp_builder = Controller::new(policies, WatcherConfig::default());
    let ccp_builder = Controller::new(cluster_policies, WatcherConfig::default());
    let mod_builder = Controller::new(modules, WatcherConfig::default());

    // Namespaces are read by the ClusterConfigPolicy controller but rooted by
    // no controller, so this cache carries its own reflector. It is a METADATA
    // watch: the only reads are `metadata.labels` and `metadata.name`, and a
    // full-object cache would hold every namespace's spec, status, annotations
    // and managedFields to answer them.
    let (ns_store, ns_writer) = reflector::store::<PartialObjectMeta<Namespace>>();
    let namespace_cache = reflector(
        ns_writer,
        watcher::watcher(
            Api::<PartialObjectMeta<Namespace>>::all(client.clone()),
            WatcherConfig::default(),
        ),
    )
    .default_backoff()
    .for_each(|event| {
        if let Err(error) = event {
            warn!(kind = "Namespace", error = %error, "watch error");
        }
        futures::future::ready(())
    });

    let stores = ControllerStores {
        machine_configs: mc_builder.store(),
        config_policies: cp_builder.store(),
        cluster_config_policies: ccp_builder.store(),
        modules: mod_builder.store(),
        drift_alerts: da_builder.store(),
        namespaces: ns_store,
    };
    let cp_store = stores.config_policies.clone();

    let ctx = Arc::new(ControllerContext {
        client: client.clone(),
        recorder,
        metrics,
        stores,
        artifact_platforms: ArtifactPlatformReader::from_registry(),
    });

    let mc_ctx = Arc::clone(&ctx);
    let da_ctx = Arc::clone(&ctx);
    let cp_ctx = Arc::clone(&ctx);
    let ccp_ctx = Arc::clone(&ctx);
    let mod_ctx = Arc::clone(&ctx);

    info!(
        "starting controllers: MachineConfig, DriftAlert, ConfigPolicy, ClusterConfigPolicy, Module"
    );

    let mc_controller = mc_builder
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

    let da_controller = da_builder
        .run(
            reconcile_drift_alert,
            make_error_policy::<DriftAlert>("drift_alert"),
            da_ctx,
        )
        .for_each(log_reconcile::<DriftAlert>("DriftAlert"));

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

    let ccp_controller = ccp_builder
        .run(
            reconcile_cluster_config_policy,
            make_error_policy::<ClusterConfigPolicy>("cluster_config_policy"),
            ccp_ctx,
        )
        .for_each(log_reconcile::<ClusterConfigPolicy>("ClusterConfigPolicy"));

    let mod_controller = mod_builder
        .run(
            reconcile_module,
            make_error_policy::<Module>("module"),
            mod_ctx,
        )
        .for_each(log_reconcile::<Module>("Module"));

    // The namespace cache joins the controllers rather than being spawned: a
    // reflector only advances while its stream is polled, and a cache nobody
    // drives never becomes ready.
    tokio::join!(
        mc_controller,
        da_controller,
        cp_controller,
        ccp_controller,
        mod_controller,
        namespace_cache
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Condition helpers
// ---------------------------------------------------------------------------

/// Find an existing condition by type, returning None if not found.
pub(super) fn find_condition<'a>(
    conditions: &'a [Condition],
    condition_type: &str,
) -> Option<&'a Condition> {
    conditions
        .iter()
        .find(|c| c.condition_type == condition_type)
}

/// Find an existing condition's status by type, returning None if not found.
pub(super) fn find_condition_status(
    conditions: &[Condition],
    condition_type: &str,
) -> Option<String> {
    find_condition(conditions, condition_type).map(|c| c.status.clone())
}

/// Find an existing condition's last_transition_time by type.
pub(super) fn find_condition_transition_time(
    conditions: &[Condition],
    condition_type: &str,
) -> Option<String> {
    find_condition(conditions, condition_type).map(|c| c.last_transition_time.clone())
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

/// Return `existing` with `condition` replacing the entry of the same type, or
/// appended when there is none.
///
/// A `Patch::Merge` body replaces an array wholesale (RFC 7386), so a status
/// patch carrying only the condition it computed deletes every sibling
/// condition on the object. A controller that owns one condition of a shared
/// status must send the whole list back.
pub(super) fn upsert_condition(existing: &[Condition], condition: Condition) -> Vec<Condition> {
    let mut conditions: Vec<Condition> = existing.to_vec();
    match conditions
        .iter_mut()
        .find(|c| c.condition_type == condition.condition_type)
    {
        Some(slot) => *slot = condition,
        None => conditions.push(condition),
    }
    conditions
}

/// Sort a policy's violator list and bound it at [`MAX_NON_COMPLIANT_MACHINES`].
///
/// Sorting comes first for two reasons: the cache hands back machines in hash
/// order, so an unsorted list would compare unequal between reconciles and force
/// a write; and it makes the truncation deterministic, so the machines that fall
/// outside a saturated cap are the same ones each time rather than a fresh
/// arbitrary subset.
pub(super) fn sort_and_cap_machines(machines: &mut Vec<String>) {
    machines.sort();
    machines.truncate(MAX_NON_COMPLIANT_MACHINES);
}

/// `namespace/name` identity of a MachineConfig, as persisted in a policy's
/// `status.nonCompliantMachines`.
pub(super) fn machine_key(machine: &MachineConfig) -> String {
    format!(
        "{}/{}",
        machine.metadata.namespace.as_deref().unwrap_or_default(),
        machine.name_any()
    )
}

// ---------------------------------------------------------------------------
// DriftAlert condition builder
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
                "Drift active on device {} — {}",
                device_id,
                cfgd_core::pluralize(details_count, "detail")
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
pub(crate) mod test_fixtures;
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

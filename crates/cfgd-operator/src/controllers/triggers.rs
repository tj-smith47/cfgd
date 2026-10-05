//! What re-runs a reconcile when a resource it reads, other than the one it
//! reconciles, changes.
//!
//! A verdict read off another kind's cache is only as fresh as the last
//! reconcile, so each such read has a trigger here: a mapper from the other
//! kind's event to the objects whose verdict it can move, or a gate that turns
//! a stream the operator already runs into a sweep only when what the verdict
//! reads has changed.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::sync::Arc;

use futures::channel::mpsc;
use futures::{Stream, StreamExt};
use k8s_openapi::api::core::v1::Namespace;
use kube::core::PartialObjectMeta;
use kube::runtime::reflector::{ObjectRef, Store};
use kube::runtime::{WatchStreamExt, watcher};
use kube::{Api, Resource, ResourceExt};
use serde::de::DeserializeOwned;

use crate::crds::{
    BackupPolicy, ClusterConfigPolicy, ConfigPolicy, DriftAlert, MachineConfig, Module,
};

use super::drift_alert::alert_target;
use super::{matches_selector, policy_in_force};

/// Turns a stream the operator already runs into a `reconcile_all_on` sweep,
/// fired only when the value a verdict reads has changed since the last look.
///
/// The first observation always counts as a change: nothing has been compared
/// yet, and one sweep at startup is what an operator does anyway. A full
/// channel already holds a sweep that has not run, and that sweep reads the
/// cache when it runs, so a send it refuses loses nothing.
#[derive(Debug)]
pub(super) struct SweepTrigger<T> {
    last: Option<T>,
    tx: mpsc::Sender<()>,
}

/// A [`SweepTrigger`] and the receiver its controller's `reconcile_all_on`
/// takes.
pub(super) fn sweep_trigger<T>() -> (SweepTrigger<T>, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel(1);
    (SweepTrigger { last: None, tx }, rx)
}

impl<T: PartialEq> SweepTrigger<T> {
    pub(super) fn observe(&mut self, now: T) {
        if self.last.as_ref() == Some(&now) {
            return;
        }
        self.last = Some(now);
        let _ = self.tx.try_send(());
    }
}

/// What a Module verdict reads from the ClusterConfigPolicies: each enforced
/// policy's `allowUnsigned` and trusted registries, in a stable order.
///
/// Status writes, compliance counts and every other field leave it unchanged,
/// so the CCP controller's own requeues and status patches sweep no Module.
pub(super) fn module_security_demands(
    policies: &[Arc<ClusterConfigPolicy>],
) -> Vec<(bool, Vec<String>)> {
    let mut demands: Vec<(bool, Vec<String>)> = policies
        .iter()
        .filter(|p| policy_in_force(&***p))
        .map(|p| {
            let mut registries = p.spec.security.trusted_registries.clone();
            registries.sort();
            (p.spec.security.allow_unsigned, registries)
        })
        .collect();
    demands.sort();
    demands
}

/// Sweeps every ClusterConfigPolicy when a namespace's labels move, which
/// is all a policy's selector reads off a namespace.
///
/// Each namespace's labels are kept by name and only the event's own object is
/// compared, so an annotation or status change costs one map lookup. A relist
/// is compared whole once, at its end, so the initial list of N namespaces
/// costs one sweep.
#[derive(Debug)]
pub(super) struct NamespaceSweep {
    labels: HashMap<String, BTreeMap<String, String>>,
    relisted: Option<HashMap<String, BTreeMap<String, String>>>,
    tx: mpsc::Sender<()>,
}

/// A [`NamespaceSweep`] and the receiver the ClusterConfigPolicy controller's
/// `reconcile_all_on` takes.
pub(super) fn namespace_sweep() -> (NamespaceSweep, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel(1);
    (
        NamespaceSweep {
            labels: HashMap::new(),
            relisted: None,
            tx,
        },
        rx,
    )
}

impl NamespaceSweep {
    pub(super) fn observe(&mut self, event: &watcher::Event<PartialObjectMeta<Namespace>>) {
        let entry = |ns: &PartialObjectMeta<Namespace>| {
            (
                ns.name_any(),
                ns.metadata.labels.clone().unwrap_or_default(),
            )
        };
        let moved = match event {
            watcher::Event::Init => {
                self.relisted = Some(HashMap::new());
                false
            }
            watcher::Event::InitApply(ns) => {
                let (name, labels) = entry(ns);
                self.relisted
                    .get_or_insert_with(HashMap::new)
                    .insert(name, labels);
                false
            }
            watcher::Event::InitDone => {
                let relisted = self.relisted.take().unwrap_or_default();
                let moved = relisted != self.labels;
                self.labels = relisted;
                moved
            }
            watcher::Event::Apply(ns) => {
                let (name, labels) = entry(ns);
                let moved = self.labels.get(&name) != Some(&labels);
                self.labels.insert(name, labels);
                moved
            }
            watcher::Event::Delete(ns) => self.labels.remove(&ns.name_any()).is_some(),
        };
        if moved {
            let _ = self.tx.try_send(());
        }
    }
}

/// Admits a watched object's event only when it can move a reader's verdict:
/// the object is new, it was deleted, or the value `read` takes off it changed.
///
/// `read` names exactly what the reader reads, so the reader's own status
/// patches and every other write it ignores reach no mapper.
pub(super) struct EventGate<K: Resource, S> {
    read: fn(&K) -> S,
    seen: HashMap<ObjectRef<K>, S>,
    relisted: Option<HashMap<ObjectRef<K>, S>>,
}

impl<K, S> EventGate<K, S>
where
    K: Resource<DynamicType = ()>,
    S: PartialEq,
{
    pub(super) fn new(read: fn(&K) -> S) -> Self {
        Self {
            read,
            seen: HashMap::new(),
            relisted: None,
        }
    }

    /// The object `event` admits, if any.
    pub(super) fn admit(&mut self, event: watcher::Event<K>) -> Option<K> {
        match event {
            watcher::Event::Init => {
                self.relisted = Some(HashMap::new());
                None
            }
            watcher::Event::InitApply(obj) => {
                let (key, now) = (ObjectRef::from_obj(&obj), (self.read)(&obj));
                let moved = self.seen.get(&key) != Some(&now);
                self.relisted
                    .get_or_insert_with(HashMap::new)
                    .insert(key, now);
                moved.then_some(obj)
            }
            // An object deleted while the watch was down leaves the map here
            // with no event of its own; each reader's periodic requeue is what
            // recounts it.
            watcher::Event::InitDone => {
                if let Some(relisted) = self.relisted.take() {
                    self.seen = relisted;
                }
                None
            }
            watcher::Event::Apply(obj) => {
                let (key, now) = (ObjectRef::from_obj(&obj), (self.read)(&obj));
                let moved = self.seen.get(&key) != Some(&now);
                self.seen.insert(key, now);
                moved.then_some(obj)
            }
            watcher::Event::Delete(obj) => {
                self.seen.remove(&ObjectRef::from_obj(&obj));
                Some(obj)
            }
        }
    }
}

/// `events` with each event passed through an [`EventGate`] over `read`.
pub(super) fn gated<K, S>(
    events: impl Stream<Item = Result<watcher::Event<K>, watcher::Error>> + Send + 'static,
    read: fn(&K) -> S,
) -> impl Stream<Item = Result<K, watcher::Error>> + Send + 'static
where
    K: Resource<DynamicType = ()> + Send + 'static,
    S: PartialEq + Send + 'static,
{
    let mut gate = EventGate::new(read);
    events.filter_map(move |event| {
        futures::future::ready(match event {
            Ok(event) => gate.admit(event).map(Ok),
            Err(error) => Some(Err(error)),
        })
    })
}

/// A label-selected watch of `api` with every event passed through an
/// [`EventGate`] over `read`: the trigger a controller's `watches_stream`
/// takes for a kind whose writes mostly move nothing its reader reads.
pub(super) fn gated_watch<K, S>(
    api: Api<K>,
    read: fn(&K) -> S,
) -> impl Stream<Item = Result<K, watcher::Error>> + Send + 'static
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + Debug + Send + Sync + 'static,
    S: PartialEq + Send + 'static,
{
    gated(
        watcher::watcher(api, crate::runtime::watch_config()).default_backoff(),
        read,
    )
}

/// Nothing: a reader that only asks whether the object exists.
pub(super) fn existence<K>(_: &K) {}

/// What a ClusterConfigPolicy's compliance count reads off a MachineConfig:
/// its spec and the package versions the device reported.
pub(super) fn compliance_inputs(mc: &MachineConfig) -> (Option<i64>, BTreeMap<String, String>) {
    (
        mc.metadata.generation,
        mc.status
            .as_ref()
            .map(|s| s.package_versions.clone())
            .unwrap_or_default(),
    )
}

/// What a ConfigPolicy's verdict reads off a MachineConfig: the labels its
/// `targetSelector` matches, the spec it checks and the package versions the
/// device reported. The `Compliant` condition the policy writes back is left
/// out, so its own write re-runs nothing.
pub(super) fn config_policy_inputs(
    mc: &MachineConfig,
) -> (
    BTreeMap<String, String>,
    Option<i64>,
    BTreeMap<String, String>,
) {
    let (generation, package_versions) = compliance_inputs(mc);
    (mc.labels().clone(), generation, package_versions)
}

/// What a BackupPolicy reads off a MachineConfig: the labels its selector
/// matches, the spec holding the hostname it schedules for and the units the
/// device reports pinning locally.
pub(super) fn backup_inputs(
    mc: &MachineConfig,
) -> (
    BTreeMap<String, String>,
    Option<i64>,
    BTreeMap<String, String>,
) {
    (
        mc.labels().clone(),
        mc.metadata.generation,
        mc.status
            .as_ref()
            .map(|s| s.backup_schedule_owners.clone())
            .unwrap_or_default(),
    )
}

/// A policy's spec and whether it is in force: the start of a deletion moves
/// the second without the API server bumping the first.
pub(super) fn policy_standing<K: Resource>(policy: &K) -> (Option<i64>, bool) {
    (policy.meta().generation, policy_in_force(policy))
}

/// The machine a DriftAlert names, resolved as the alert controller resolves
/// it: with the alert's existence, all a machine's DriftDetected verdict reads
/// off the alert.
pub(super) fn alert_reach(alert: &DriftAlert) -> (String, String) {
    let (namespace, name) = alert_target(alert);
    (namespace.to_string(), name.to_string())
}

/// The MachineConfig whose DriftDetected verdict `alert` takes part in.
pub(super) fn machine_named_by_alert(alert: DriftAlert) -> Option<ObjectRef<MachineConfig>> {
    let (namespace, name) = alert_target(&alert);
    Some(ObjectRef::new(name).within(namespace))
}

/// The MachineConfigs whose `moduleRefs` name `module`: the ones whose
/// ModulesResolved condition a create or delete of that Module can move.
pub(super) fn machines_naming_module(
    machines: &Store<MachineConfig>,
    module: &Module,
) -> Vec<ObjectRef<MachineConfig>> {
    let module = module.name_any();
    machines
        .state()
        .iter()
        .filter(|mc| mc.spec.module_refs.iter().any(|r| r.name == module))
        .map(|mc| ObjectRef::from_obj(&**mc))
        .collect()
}

/// The ClusterConfigPolicies whose compliance count `machine` takes part in.
pub(super) fn policies_counting_machine(
    policies: &Store<ClusterConfigPolicy>,
    namespaces: &Store<PartialObjectMeta<Namespace>>,
    machine: &MachineConfig,
) -> Vec<ObjectRef<ClusterConfigPolicy>> {
    cluster_policies_reaching(
        policies,
        namespaces,
        &machine.namespace().unwrap_or_default(),
    )
}

/// The ClusterConfigPolicies whose requirements merge with `policy`'s.
pub(super) fn policies_merging_config_policy(
    policies: &Store<ClusterConfigPolicy>,
    namespaces: &Store<PartialObjectMeta<Namespace>>,
    policy: &ConfigPolicy,
) -> Vec<ObjectRef<ClusterConfigPolicy>> {
    cluster_policies_reaching(
        policies,
        namespaces,
        &policy.namespace().unwrap_or_default(),
    )
}

/// The ClusterConfigPolicies in force whose namespace selector reaches
/// `namespace`.
///
/// A namespace the cache does not hold yet is reached by every policy, so a
/// lagging namespace cache costs extra reconciles and never a missed one.
fn cluster_policies_reaching(
    policies: &Store<ClusterConfigPolicy>,
    namespaces: &Store<PartialObjectMeta<Namespace>>,
    namespace: &str,
) -> Vec<ObjectRef<ClusterConfigPolicy>> {
    let known = namespaces.get(&ObjectRef::new(namespace));
    policies
        .state()
        .iter()
        .filter(|p| policy_in_force(&***p))
        .filter(|p| {
            known.as_ref().is_none_or(|ns| {
                matches_selector(ns.metadata.labels.as_ref(), &p.spec.namespace_selector)
            })
        })
        .map(|p| ObjectRef::from_obj(&**p))
        .collect()
}

/// The DriftAlerts whose target is `machine`, resolved as the alert
/// controller resolves it.
pub(super) fn alerts_naming_machine(
    alerts: &Store<DriftAlert>,
    machine: &MachineConfig,
) -> Vec<ObjectRef<DriftAlert>> {
    let target = (machine.namespace().unwrap_or_default(), machine.name_any());
    alerts
        .state()
        .iter()
        .filter(|da| alert_target(da) == (target.0.as_str(), target.1.as_str()))
        .map(|da| ObjectRef::from_obj(&**da))
        .collect()
}

/// The ConfigPolicies in `machine`'s namespace, any of whose selectors may
/// target it.
pub(super) fn config_policies_beside(
    policies: &Store<ConfigPolicy>,
    machine: &MachineConfig,
) -> Vec<ObjectRef<ConfigPolicy>> {
    in_namespace_of(policies, machine)
}

/// The BackupPolicies in `machine`'s namespace, any of which may schedule a
/// unit the machine pins or releases in its status.
pub(super) fn backup_policies_beside(
    policies: &Store<BackupPolicy>,
    machine: &MachineConfig,
) -> Vec<ObjectRef<BackupPolicy>> {
    in_namespace_of(policies, machine)
}

fn in_namespace_of<K>(store: &Store<K>, machine: &MachineConfig) -> Vec<ObjectRef<K>>
where
    K: Resource<DynamicType = ()> + Clone + 'static,
{
    let namespace = machine.namespace();
    store
        .state()
        .iter()
        .filter(|obj| obj.meta().namespace == namespace)
        .map(|obj| ObjectRef::from_obj(&**obj))
        .collect()
}

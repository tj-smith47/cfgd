//! What re-runs a reconcile when a resource it reads, other than the one it
//! reconciles, changes.
//!
//! A verdict read off another kind's cache is only as fresh as the last
//! reconcile, so each such read has a trigger here: a mapper from the other
//! kind's event to the objects whose verdict it can move, or a gate that turns
//! a stream the operator already runs into a sweep only when what the verdict
//! reads has changed.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::channel::mpsc;
use k8s_openapi::api::core::v1::Namespace;
use kube::core::PartialObjectMeta;
use kube::runtime::reflector::ObjectRef;

use crate::crds::{ClusterConfigPolicy, DriftAlert, MachineConfig};

use super::matches_selector;

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
        .filter(|p| p.metadata.deletion_timestamp.is_none())
        .map(|p| {
            let mut registries = p.spec.security.trusted_registries.clone();
            registries.sort();
            (p.spec.security.allow_unsigned, registries)
        })
        .collect();
    demands.sort();
    demands
}

/// What a ClusterConfigPolicy verdict reads from the Namespaces: each one's
/// name and labels. Annotation and status changes leave it unchanged.
pub(super) fn namespace_labels(
    namespaces: &[Arc<PartialObjectMeta<Namespace>>],
) -> Vec<(String, BTreeMap<String, String>)> {
    let mut labels: Vec<_> = namespaces
        .iter()
        .map(|ns| {
            (
                ns.metadata.name.clone().unwrap_or_default(),
                ns.metadata.labels.clone().unwrap_or_default(),
            )
        })
        .collect();
    labels.sort();
    labels
}

/// The MachineConfigs whose `moduleRefs` name `module`: the ones whose
/// ModulesResolved condition a create or delete of that Module can move.
pub(super) fn machine_configs_referencing(
    machines: &[Arc<MachineConfig>],
    module: &str,
) -> Vec<ObjectRef<MachineConfig>> {
    machines
        .iter()
        .filter(|mc| mc.spec.module_refs.iter().any(|r| r.name == module))
        .map(|mc| ObjectRef::from_obj(&**mc))
        .collect()
}

/// The ClusterConfigPolicies whose namespace selector reaches `namespace`: the
/// ones whose compliance counts a MachineConfig or ConfigPolicy there can move.
///
/// A namespace the cache does not hold yet is reached by every policy, so a
/// lagging namespace cache costs extra reconciles and never a missed one.
pub(super) fn cluster_policies_reaching(
    policies: &[Arc<ClusterConfigPolicy>],
    namespaces: &[Arc<PartialObjectMeta<Namespace>>],
    namespace: &str,
) -> Vec<ObjectRef<ClusterConfigPolicy>> {
    let known = namespaces
        .iter()
        .find(|ns| ns.metadata.name.as_deref() == Some(namespace));
    policies
        .iter()
        .filter(|p| {
            known.is_none_or(|ns| {
                matches_selector(ns.metadata.labels.as_ref(), &p.spec.namespace_selector)
            })
        })
        .map(|p| ObjectRef::from_obj(&**p))
        .collect()
}

/// The DriftAlerts whose `machineConfigRef` names the MachineConfig
/// `namespace/name`: the ones whose Resolved condition reads that machine's
/// DriftDetected condition. A reference with no namespace names a machine in
/// the alert's own namespace.
pub(super) fn drift_alerts_naming(
    alerts: &[Arc<DriftAlert>],
    namespace: &str,
    name: &str,
) -> Vec<ObjectRef<DriftAlert>> {
    alerts
        .iter()
        .filter(|da| {
            let target = &da.spec.machine_config_ref;
            let target_ns = target
                .namespace
                .as_deref()
                .or(da.metadata.namespace.as_deref());
            target.name == name && target_ns == Some(namespace)
        })
        .map(|da| ObjectRef::from_obj(&**da))
        .collect()
}

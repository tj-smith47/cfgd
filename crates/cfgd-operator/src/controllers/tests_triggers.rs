//! Tests for `controllers/triggers.rs`: the mappers and sweep gates that
//! re-run a reconcile when a resource it reads changes.
//!
//! A watch-driven reconcile needs a live API server, which the mock harness
//! does not serve, so each trigger is held here as its two halves: the gate
//! and mapper functions that decide whether and where an event goes, and (in
//! `tests.rs`) the source shape that wires each one into `run`.

use std::collections::BTreeMap;
use std::sync::Arc;

use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::core::{ObjectMeta, PartialObjectMeta};
use kube::runtime::reflector::{ObjectRef, Store};
use kube::runtime::watcher::Event;

use super::drift_alert::reports_drift;
use super::test_fixtures::{
    backup_policy, cluster_config_policy_with_spec, config_policy, drift_alert, machine_config,
    machine_config_status_with_drift_detected,
};
use super::test_kube_harness::seeded_store;
use super::triggers::{
    EventGate, alerts_naming_machine, backup_policies_beside, compliance_inputs,
    config_policies_beside, existence, generation, machines_naming_module, module_security_demands,
    namespace_sweep, policies_counting_machine, policies_merging_config_policy, sweep_trigger,
};
use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicySpec, ClusterConfigPolicyStatus, Condition,
    DeviceCompliance, DriftSeverity, LabelSelector, MachineConfig, MachineConfigStatus, Module,
    ModuleRef, SecurityPolicy,
};

fn policy(name: &str, allow_unsigned: bool, registries: &[&str]) -> Arc<ClusterConfigPolicy> {
    Arc::new(cluster_config_policy_with_spec(
        name,
        ClusterConfigPolicySpec {
            security: SecurityPolicy {
                trusted_registries: registries.iter().map(|r| r.to_string()).collect(),
                allow_unsigned,
            },
            ..Default::default()
        },
    ))
}

fn deleting(policy: &Arc<ClusterConfigPolicy>) -> Arc<ClusterConfigPolicy> {
    let mut policy = (**policy).clone();
    policy.metadata.deletion_timestamp = Some(Time(k8s_openapi::jiff::Timestamp::now()));
    Arc::new(policy)
}

fn namespace(name: &str, labels: &[(&str, &str)]) -> Arc<PartialObjectMeta<Namespace>> {
    Arc::new(PartialObjectMeta {
        types: None,
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(
                labels
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            ..Default::default()
        },
        _phantom: Default::default(),
    })
}

fn scoped_policy(name: &str, labels: &[(&str, &str)]) -> Arc<ClusterConfigPolicy> {
    Arc::new(cluster_config_policy_with_spec(
        name,
        ClusterConfigPolicySpec {
            namespace_selector: LabelSelector {
                match_labels: labels
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<BTreeMap<_, _>>(),
                match_expressions: vec![],
            },
            ..Default::default()
        },
    ))
}

fn names<K: kube::Resource>(refs: Vec<ObjectRef<K>>) -> Vec<String>
where
    K::DynamicType: Default,
{
    let mut names: Vec<String> = refs.into_iter().map(|r| r.name).collect();
    names.sort();
    names
}

/// The sequence a policy's life takes through the CCP controller's stream,
/// as the Module sweep sees it: the policy first appears (one sweep), its
/// controller re-reconciles it with nothing changed (no sweep), and its
/// deletion sets the timestamp the finalizer reconcile runs under (one sweep,
/// while the object is still cached). That last sweep is what re-evaluates a
/// Module the policy withheld, ahead of the 60s requeue.
#[test]
fn a_policy_appearing_and_being_deleted_each_sweep_the_modules_once() {
    let strict = policy("strict", false, &[]);
    let (mut trigger, mut rx) = sweep_trigger();

    trigger.observe(module_security_demands(&[Arc::clone(&strict)]));
    assert_eq!(rx.try_recv().ok(), Some(()), "a new policy sweeps");

    trigger.observe(module_security_demands(&[Arc::clone(&strict)]));
    assert!(
        rx.try_recv().is_err(),
        "a reconcile that moved nothing sweeps nothing"
    );

    trigger.observe(module_security_demands(&[deleting(&strict)]));
    assert_eq!(rx.try_recv().ok(), Some(()), "a deleting policy sweeps");
}

#[test]
fn sweeps_the_reader_has_not_taken_yet_coalesce_into_one() {
    let (mut trigger, mut rx) = sweep_trigger();
    for n in 0..5 {
        trigger.observe(n);
    }
    let mut taken = 0;
    while rx.try_recv().is_ok() {
        taken += 1;
    }
    assert!(
        (1..5).contains(&taken),
        "five changes queued while the sweep had not run must not queue five sweeps, took {taken}"
    );
}

#[test]
fn only_the_security_block_of_a_policy_in_force_moves_the_module_demands() {
    let strict = policy("strict", false, &["b.io", "a.io"]);
    let base = module_security_demands(&[Arc::clone(&strict)]);

    let mut with_status = (*strict).clone();
    with_status.status = Some(ClusterConfigPolicyStatus {
        compliant_count: 7,
        ..Default::default()
    });
    with_status.metadata.resource_version = Some("99".to_string());
    assert_eq!(
        module_security_demands(&[Arc::new(with_status)]),
        base,
        "a status write moves no Module verdict"
    );
    assert_eq!(
        module_security_demands(&[policy("strict", false, &["a.io", "b.io"])]),
        base,
        "registry order is not a demand"
    );

    assert_ne!(
        module_security_demands(&[policy("strict", true, &["a.io", "b.io"])]),
        base
    );
    assert_ne!(
        module_security_demands(&[policy("strict", false, &["a.io"])]),
        base
    );
    assert_ne!(module_security_demands(&[deleting(&strict)]), base);
    assert!(module_security_demands(&[deleting(&strict)]).is_empty());
}

fn store_of<K>(objects: Vec<Arc<K>>) -> Store<K>
where
    K: kube::Resource<DynamicType = ()> + Clone + 'static,
{
    seeded_store(objects.into_iter().map(|o| (*o).clone()).collect())
}

fn admitted<K: kube::Resource<DynamicType = ()>, S: PartialEq>(
    gate: &mut EventGate<K, S>,
    event: Event<K>,
) -> bool {
    gate.admit(event).is_some()
}

/// A gate over an object the watch has listed once, so every case below
/// starts from a seen object and a relist already behind it.
fn listed<K, S>(read: fn(&K) -> S, obj: &K) -> EventGate<K, S>
where
    K: kube::Resource<DynamicType = ()> + Clone,
    S: PartialEq,
{
    let mut gate = EventGate::new(read);
    assert!(!admitted(&mut gate, Event::Init));
    assert!(
        admitted(&mut gate, Event::InitApply(obj.clone())),
        "an object the gate has never seen is admitted"
    );
    assert!(!admitted(&mut gate, Event::InitDone));
    gate
}

fn condition(kind: &str, status: &str) -> Condition {
    Condition {
        condition_type: kind.to_string(),
        status: status.to_string(),
        reason: "Test".to_string(),
        message: String::new(),
        last_transition_time: "2026-01-01T00:00:00Z".to_string(),
        observed_generation: None,
    }
}

#[test]
fn a_compliance_gate_admits_only_a_spec_or_package_version_change_of_a_machine() {
    let mc = machine_config("mc-1", "ns-a");
    let mut gate = listed(compliance_inputs, &mc);

    let mut conditions_only = mc.clone();
    conditions_only.status = Some(MachineConfigStatus {
        conditions: vec![condition("Reconciled", "True")],
        ..Default::default()
    });
    conditions_only.metadata.resource_version = Some("2".to_string());
    assert!(
        !admitted(&mut gate, Event::Apply(conditions_only.clone())),
        "a condition patch moves no compliance count"
    );

    let mut compliance_only = conditions_only.clone();
    compliance_only.status.as_mut().unwrap().compliance = Some(DeviceCompliance {
        compliant: 3,
        ..Default::default()
    });
    compliance_only
        .status
        .as_mut()
        .unwrap()
        .backup_schedule_owners
        .insert("dotfiles".to_string(), "local".to_string());
    assert!(
        !admitted(&mut gate, Event::Apply(compliance_only.clone())),
        "a device compliance or backup owner report moves no compliance count"
    );

    let mut versions = compliance_only.clone();
    versions
        .status
        .as_mut()
        .unwrap()
        .package_versions
        .insert("brew/jq".to_string(), "1.7".to_string());
    assert!(
        admitted(&mut gate, Event::Apply(versions.clone())),
        "a reported package version is read by the count"
    );

    let mut respecced = versions.clone();
    respecced.metadata.generation = Some(2);
    assert!(
        admitted(&mut gate, Event::Apply(respecced.clone())),
        "a spec change is read by the count"
    );
    assert!(
        admitted(&mut gate, Event::Delete(respecced)),
        "a deletion is admitted"
    );
    assert!(
        admitted(&mut gate, Event::Apply(machine_config("mc-2", "ns-a"))),
        "a creation is admitted"
    );
}

#[test]
fn an_existence_gate_admits_a_module_created_or_deleted_and_nothing_between() {
    let module = Module::new("nvim", Default::default());
    let mut gate = listed(existence, &module);

    let mut status_write = module.clone();
    status_write.metadata.resource_version = Some("7".to_string());
    status_write.metadata.generation = Some(3);
    assert!(
        !admitted(&mut gate, Event::Apply(status_write.clone())),
        "an update of a Module that exists changes no ModulesResolved verdict"
    );
    assert!(admitted(&mut gate, Event::Delete(status_write)));
    assert!(admitted(&mut gate, Event::Apply(module)));
}

#[test]
fn a_drift_gate_admits_a_machine_whose_drift_condition_flips_and_nothing_else() {
    let mc = machine_config("mc-1", "ns-a");
    let mut gate = listed(reports_drift, &mc);

    let mut reconciled = mc.clone();
    reconciled.status = Some(MachineConfigStatus {
        conditions: vec![condition("Reconciled", "True")],
        ..Default::default()
    });
    assert!(
        !admitted(&mut gate, Event::Apply(reconciled)),
        "a condition the alert does not read admits nothing"
    );

    let mut drifted = mc.clone();
    drifted.status = Some(machine_config_status_with_drift_detected());
    assert!(reports_drift(&drifted));
    assert!(admitted(&mut gate, Event::Apply(drifted.clone())));
    assert!(
        !admitted(&mut gate, Event::Apply(drifted.clone())),
        "the same report again admits nothing"
    );
    assert!(
        admitted(&mut gate, Event::Apply(mc)),
        "drift clearing is admitted"
    );
}

#[test]
fn a_generation_gate_admits_a_config_policy_spec_change_and_not_its_status() {
    let cp = config_policy("cp-1", "ns-a");
    let mut gate = listed(generation, &cp);

    let mut status_write = cp.clone();
    status_write.metadata.resource_version = Some("9".to_string());
    assert!(!admitted(&mut gate, Event::Apply(status_write.clone())));

    let mut respecced = status_write;
    respecced.metadata.generation = Some(2);
    assert!(admitted(&mut gate, Event::Apply(respecced)));
}

/// A relist replays every object as `InitApply`. One the gate already holds in
/// the same state is not news; one whose state moved while the watch was down
/// is, and so is one the gate never held.
#[test]
fn a_relist_admits_only_what_moved_while_the_watch_was_down() {
    let cp = config_policy("cp-1", "ns-a");
    let mut gate = listed(generation, &cp);

    let mut respecced = cp.clone();
    respecced.metadata.generation = Some(2);
    assert!(!admitted(&mut gate, Event::Init));
    assert!(!admitted(&mut gate, Event::InitApply(cp.clone())));
    assert!(admitted(&mut gate, Event::InitApply(respecced.clone())));
    assert!(admitted(
        &mut gate,
        Event::InitApply(config_policy("cp-2", "ns-a"))
    ));
    assert!(!admitted(&mut gate, Event::InitDone));
    assert!(!admitted(&mut gate, Event::Apply(respecced)));
}

#[test]
fn a_namespace_sweep_fires_once_per_relist_and_only_on_a_label_change() {
    let (mut sweep, mut rx) = namespace_sweep();
    let prod = (*namespace("team-a", &[("tier", "prod")])).clone();
    let other = (*namespace("team-b", &[])).clone();

    sweep.observe(&Event::Init);
    sweep.observe(&Event::InitApply(prod.clone()));
    sweep.observe(&Event::InitApply(other.clone()));
    assert!(
        rx.try_recv().is_err(),
        "nothing sweeps before the list ends"
    );
    sweep.observe(&Event::InitDone);
    assert_eq!(rx.try_recv().ok(), Some(()), "the first list sweeps once");

    let mut annotated = prod.clone();
    annotated.metadata.annotations = Some(BTreeMap::from([(
        "e2e/heartbeat".to_string(),
        "1".to_string(),
    )]));
    sweep.observe(&Event::Apply(annotated));
    assert!(
        rx.try_recv().is_err(),
        "an annotation change sweeps nothing"
    );

    sweep.observe(&Event::Init);
    sweep.observe(&Event::InitApply(prod.clone()));
    sweep.observe(&Event::InitApply(other.clone()));
    sweep.observe(&Event::InitDone);
    assert!(
        rx.try_recv().is_err(),
        "a relist that moved nothing sweeps nothing"
    );

    sweep.observe(&Event::Apply(
        (*namespace("team-a", &[("tier", "dev")])).clone(),
    ));
    assert_eq!(rx.try_recv().ok(), Some(()), "a label change sweeps");

    sweep.observe(&Event::Delete(other));
    assert_eq!(rx.try_recv().ok(), Some(()), "a namespace leaving sweeps");
    sweep.observe(&Event::Apply((*namespace("team-c", &[])).clone()));
    assert_eq!(rx.try_recv().ok(), Some(()), "a namespace arriving sweeps");
}

#[test]
fn a_module_event_reaches_only_the_machines_that_name_it() {
    let mut names_it = machine_config("names-it", "ns-a");
    names_it.spec.module_refs = vec![ModuleRef {
        name: "nvim".to_string(),
        required: true,
    }];
    let mut names_other = machine_config("names-other", "ns-b");
    names_other.spec.module_refs = vec![ModuleRef {
        name: "git".to_string(),
        required: false,
    }];
    let machines = store_of(vec![Arc::new(names_it), Arc::new(names_other)]);

    assert_eq!(
        names(machines_naming_module(
            &machines,
            &Module::new("nvim", Default::default())
        )),
        ["names-it"]
    );
    assert!(machines_naming_module(&machines, &Module::new("zsh", Default::default())).is_empty());
}

#[test]
fn a_namespaced_event_reaches_the_policies_in_force_whose_selector_matches_its_namespace() {
    let policies = store_of(vec![
        scoped_policy("prod-only", &[("tier", "prod")]),
        scoped_policy("everywhere", &[]),
        deleting(&scoped_policy("leaving", &[])),
    ]);
    let namespaces = store_of(vec![
        namespace("prod-ns", &[("tier", "prod")]),
        namespace("dev-ns", &[("tier", "dev")]),
    ]);

    assert_eq!(
        names(policies_counting_machine(
            &policies,
            &namespaces,
            &machine_config("mc", "prod-ns")
        )),
        ["everywhere", "prod-only"]
    );
    assert_eq!(
        names(policies_merging_config_policy(
            &policies,
            &namespaces,
            &config_policy("cp", "dev-ns")
        )),
        ["everywhere"]
    );
    assert_eq!(
        names(policies_counting_machine(
            &policies,
            &namespaces,
            &machine_config("mc", "not-cached-yet")
        )),
        ["everywhere", "prod-only"],
        "a namespace the cache has not seen reaches every policy in force"
    );
}

#[test]
fn a_machine_event_reaches_only_the_alerts_that_name_it() {
    let own_ns = drift_alert("own-ns", "ns-a", "mc-1", DriftSeverity::Low);
    let mut cross_ns = drift_alert("cross-ns", "ns-b", "mc-1", DriftSeverity::Low);
    cross_ns.spec.machine_config_ref.namespace = Some("ns-a".to_string());
    let other_ns = drift_alert("other-ns", "ns-b", "mc-1", DriftSeverity::Low);
    let other_mc = drift_alert("other-mc", "ns-a", "mc-2", DriftSeverity::Low);
    let alerts = store_of(
        [own_ns, cross_ns, other_ns, other_mc]
            .map(Arc::new)
            .to_vec(),
    );

    assert_eq!(
        names(alerts_naming_machine(
            &alerts,
            &machine_config("mc-1", "ns-a")
        )),
        ["cross-ns", "own-ns"]
    );
}

#[test]
fn a_machine_event_reaches_the_config_and_backup_policies_of_its_namespace() {
    let cps = store_of(vec![
        Arc::new(config_policy("here", "ns-a")),
        Arc::new(config_policy("there", "ns-b")),
    ]);
    let mc: MachineConfig = machine_config("mc-1", "ns-a");
    assert_eq!(names(config_policies_beside(&cps, &mc)), ["here"]);

    let bps = store_of(vec![
        Arc::new(backup_policy("here", "ns-a", vec![])),
        Arc::new(backup_policy("there", "ns-b", vec![])),
    ]);
    assert_eq!(names(backup_policies_beside(&bps, &mc)), ["here"]);
}

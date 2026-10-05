//! Tests for `controllers/triggers.rs`: the mappers and sweep gates that
//! re-run a reconcile when a resource it reads changes.
//!
//! A watch-driven reconcile needs a live API server, which the mock harness
//! does not serve, so each trigger is held here as its two halves: the pure
//! function that decides which objects an event reaches, and (in
//! `tests.rs`) the source shape that wires it into `run`.

use std::collections::BTreeMap;
use std::sync::Arc;

use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::core::{ObjectMeta, PartialObjectMeta};
use kube::runtime::reflector::ObjectRef;

use super::test_fixtures::{cluster_config_policy_with_spec, drift_alert, machine_config};
use super::triggers::{
    cluster_policies_reaching, drift_alerts_naming, machine_configs_referencing,
    module_security_demands, namespace_labels, sweep_trigger,
};
use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicySpec, ClusterConfigPolicyStatus, DriftSeverity,
    LabelSelector, ModuleRef, SecurityPolicy,
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

#[test]
fn only_a_namespace_label_change_moves_the_namespace_view() {
    let base = namespace_labels(&[namespace("team-a", &[("tier", "prod")])]);

    let mut annotated = (*namespace("team-a", &[("tier", "prod")])).clone();
    annotated.metadata.annotations = Some(BTreeMap::from([(
        "e2e/heartbeat".to_string(),
        "1".to_string(),
    )]));
    assert_eq!(namespace_labels(&[Arc::new(annotated)]), base);

    assert_ne!(
        namespace_labels(&[namespace("team-a", &[("tier", "dev")])]),
        base
    );
    assert_ne!(
        namespace_labels(&[
            namespace("team-a", &[("tier", "prod")]),
            namespace("team-b", &[])
        ]),
        base
    );
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
    let machines = [Arc::new(names_it), Arc::new(names_other)];

    assert_eq!(
        names(machine_configs_referencing(&machines, "nvim")),
        ["names-it"]
    );
    assert!(machine_configs_referencing(&machines, "zsh").is_empty());
}

#[test]
fn a_namespaced_event_reaches_the_policies_whose_selector_matches_its_namespace() {
    let policies = [
        scoped_policy("prod-only", &[("tier", "prod")]),
        scoped_policy("everywhere", &[]),
    ];
    let namespaces = [
        namespace("prod-ns", &[("tier", "prod")]),
        namespace("dev-ns", &[("tier", "dev")]),
    ];

    assert_eq!(
        names(cluster_policies_reaching(&policies, &namespaces, "prod-ns")),
        ["everywhere", "prod-only"]
    );
    assert_eq!(
        names(cluster_policies_reaching(&policies, &namespaces, "dev-ns")),
        ["everywhere"]
    );
    assert_eq!(
        names(cluster_policies_reaching(
            &policies,
            &namespaces,
            "not-cached-yet"
        )),
        ["everywhere", "prod-only"],
        "a namespace the cache has not seen reaches every policy"
    );
}

#[test]
fn a_machine_event_reaches_only_the_alerts_that_name_it() {
    let own_ns = drift_alert("own-ns", "ns-a", "mc-1", DriftSeverity::Low);
    let mut cross_ns = drift_alert("cross-ns", "ns-b", "mc-1", DriftSeverity::Low);
    cross_ns.spec.machine_config_ref.namespace = Some("ns-a".to_string());
    let other_ns = drift_alert("other-ns", "ns-b", "mc-1", DriftSeverity::Low);
    let other_mc = drift_alert("other-mc", "ns-a", "mc-2", DriftSeverity::Low);
    let alerts = [own_ns, cross_ns, other_ns, other_mc].map(Arc::new);

    assert_eq!(
        names(drift_alerts_naming(&alerts, "ns-a", "mc-1")),
        ["cross-ns", "own-ns"]
    );
}

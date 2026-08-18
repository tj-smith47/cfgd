//! Reconcile-fn tests for `controllers/cluster_config_policy.rs`.
#![cfg(test)]

use std::sync::Arc;

use kube::runtime::controller::Action;

use k8s_openapi::api::core::v1::Namespace;
use kube::api::ObjectMeta;
use kube::core::PartialObjectMeta;

use super::ControllerStores;
use super::cluster_config_policy::reconcile_cluster_config_policy;
use super::test_fixtures::{cluster_config_policy_with_spec, config_policy, machine_config};
use super::test_kube_harness::{
    ExpectedCall, MockKubeHarness, empty_stores, expect_event_post, seeded_store, unready_store,
};
use crate::crds::{ConfigPolicy, MAX_NON_COMPLIANT_MACHINES, MachineConfig, ModuleRef};
use crate::metrics::PolicyLabels;

const NS_A: &str = "team-a";
const NS_B: &str = "team-b";

fn cluster_policy_path(name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/clusterconfigpolicies/{name}")
}

/// The namespace cache is a metadata watch, so its entries are
/// `PartialObjectMeta` — the shape `metadata_watcher` really delivers.
fn namespace(name: &str, labels: &[(&str, &str)]) -> PartialObjectMeta<Namespace> {
    PartialObjectMeta {
        types: None,
        _phantom: std::marker::PhantomData,
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: if labels.is_empty() {
                None
            } else {
                Some(
                    labels
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                )
            },
            ..Default::default()
        },
    }
}

/// Caches holding the cluster state a ClusterConfigPolicy reconcile reads.
fn stores_with(
    namespaces: &[&str],
    machine_configs: Vec<MachineConfig>,
    config_policies: Vec<ConfigPolicy>,
) -> ControllerStores {
    ControllerStores {
        namespaces: seeded_store(namespaces.iter().map(|n| namespace(n, &[])).collect()),
        machine_configs: seeded_store(machine_configs),
        config_policies: seeded_store(config_policies),
        ..empty_stores()
    }
}

// -----------------------------------------------------------------------
// All-compliant happy path
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_cluster_config_policy_with_no_namespaces_marks_all_compliant() {
    let ccp = cluster_config_policy_with_spec("ccp-empty", Default::default());

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // 1. PATCH CCP /status
            ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-empty")))
                .returning_json(&ccp),
            // 2. POST Evaluated event (cluster-scoped event posts to default namespace).
            expect_event_post("default"),
        ],
        stores_with(&[], vec![], vec![]),
    );

    let action = reconcile_cluster_config_policy(Arc::new(ccp), ctx.clone())
        .await
        .unwrap();
    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        2,
        "namespaces and their contents come from watch caches — no LISTs"
    );

    let ccp_status = report.captured[0].body_json();
    assert_eq!(ccp_status["status"]["compliantCount"], 0);
    assert_eq!(ccp_status["status"]["nonCompliantCount"], 0);
    let enforced = ccp_status["status"]["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Enforced")
        .unwrap();
    assert_eq!(enforced["status"], "True");
    assert_eq!(enforced["reason"], "AllCompliant");
}

#[tokio::test]
async fn reconcile_cluster_config_policy_aggregates_compliant_counts_across_namespaces() {
    let ccp_spec = crate::crds::ClusterConfigPolicySpec {
        required_modules: vec![ModuleRef {
            name: "kubectl".to_string(),
            required: true,
        }],
        ..Default::default()
    };
    let ccp = cluster_config_policy_with_spec("ccp-multi", ccp_spec);

    // mc-a is compliant (has kubectl), mc-b is not.
    let mut mc_a = machine_config("mc-a", NS_A);
    mc_a.spec.module_refs = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];
    let mc_b = machine_config("mc-b", NS_B);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // mc-b is non-compliant — emit PolicyViolation event in NS_B.
            expect_event_post(NS_B),
            // CCP status patch
            ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-multi")))
                .returning_json(&ccp),
            // Cluster-scoped events post to default namespace ("").
            expect_event_post("default"), // Evaluated
            expect_event_post("default"), // NonCompliantTargets
        ],
        stores_with(&[NS_A, NS_B], vec![mc_a, mc_b], vec![]),
    );

    reconcile_cluster_config_policy(Arc::new(ccp), ctx.clone())
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        4,
        "two namespaces used to cost five LISTs; the caches make it zero"
    );

    let ccp_status = report.captured[1].body_json();
    assert_eq!(ccp_status["status"]["compliantCount"], 1);
    assert_eq!(ccp_status["status"]["nonCompliantCount"], 1);
    let enforced = ccp_status["status"]["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Enforced")
        .unwrap();
    assert_eq!(enforced["status"], "False");
    assert_eq!(enforced["reason"], "NonCompliantTargets");

    // Cluster-scoped policies record the metric with empty namespace label.
    let count = ctx
        .metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: "ccp-multi".to_string(),
            namespace: String::new(),
        })
        .get();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn reconcile_cluster_config_policy_merges_namespace_policies_into_evaluation() {
    let ccp_spec = crate::crds::ClusterConfigPolicySpec {
        required_modules: vec![ModuleRef {
            name: "kubectl".to_string(),
            required: true,
        }],
        ..Default::default()
    };
    let ccp = cluster_config_policy_with_spec("ccp-merge", ccp_spec);

    // The namespace policy adds an extra required module.
    let mut ns_policy = config_policy("ns-extra", NS_A);
    ns_policy.spec.required_modules = vec![ModuleRef {
        name: "helm".to_string(),
        required: true,
    }];

    // mc has only kubectl, missing helm — the merged requirements include both.
    let mut mc = machine_config("mc1", NS_A);
    mc.spec.module_refs = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // mc1 is non-compliant due to merged requirements
            expect_event_post(NS_A), // PolicyViolation
            ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-merge")))
                .returning_json(&ccp),
            expect_event_post("default"), // Evaluated
            expect_event_post("default"), // NonCompliantTargets
        ],
        stores_with(&[NS_A], vec![mc], vec![ns_policy]),
    );

    reconcile_cluster_config_policy(Arc::new(ccp), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    let ccp_status = report.captured[1].body_json();
    assert_eq!(ccp_status["status"]["nonCompliantCount"], 1);
}

#[tokio::test]
async fn reconcile_cluster_config_policy_filters_namespaces_by_namespace_selector() {
    use std::collections::BTreeMap;

    let mut match_labels = BTreeMap::new();
    match_labels.insert("tier".to_string(), "prod".to_string());
    let ccp_spec = crate::crds::ClusterConfigPolicySpec {
        namespace_selector: crate::crds::LabelSelector {
            match_labels,
            match_expressions: vec![],
        },
        ..Default::default()
    };
    let ccp = cluster_config_policy_with_spec("ccp-scoped", ccp_spec);

    // Two namespaces, only one labeled tier=prod. The machine in NS_B violates
    // the policy, so it would be counted (and would emit an event) if the
    // namespace selector were ignored.
    let mut mc_b = machine_config("mc-b", NS_B);
    mc_b.spec.module_refs = vec![];

    let stores = ControllerStores {
        namespaces: seeded_store(vec![
            namespace(NS_A, &[("tier", "prod")]),
            namespace(NS_B, &[]),
        ]),
        machine_configs: seeded_store(vec![mc_b]),
        ..empty_stores()
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-scoped")))
                .returning_json(&ccp),
            expect_event_post("default"),
        ],
        stores,
    );

    reconcile_cluster_config_policy(Arc::new(ccp), ctx)
        .await
        .unwrap();
    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        2,
        "NS_B must NOT be iterated (no tier=prod label)"
    );
    let ccp_status = report.captured[0].body_json();
    assert_eq!(
        ccp_status["status"]["compliantCount"], 0,
        "the machine in the unselected namespace must not be counted"
    );
    assert_eq!(ccp_status["status"]["nonCompliantCount"], 0);
}

/// The cluster policy aggregates one tally per namespace, and each of those
/// tallies arrives with its list already capped. Counting the concatenation
/// would therefore report the cap rather than the total, so the exact count is
/// accumulated from the tallies themselves.
#[tokio::test]
async fn reconcile_cluster_config_policy_counts_every_violator_above_the_cap() {
    let over_cap = MAX_NON_COMPLIANT_MACHINES + 1;
    let ccp_spec = crate::crds::ClusterConfigPolicySpec {
        required_modules: vec![ModuleRef {
            name: "kubectl".to_string(),
            required: true,
        }],
        ..Default::default()
    };
    let mut ccp = cluster_config_policy_with_spec("ccp-cap", ccp_spec);

    // Every machine in one namespace violates, so a single per-namespace tally
    // is the one that overflows the cap.
    let machines: Vec<MachineConfig> = (0..over_cap)
        .map(|i| machine_config(&format!("mc-{i:04}"), NS_A))
        .collect();

    // Already reported, so no machine transitions and no violation event fires.
    let mut reported: Vec<String> = machines
        .iter()
        .map(|mc| format!("{NS_A}/{}", mc.metadata.name.clone().unwrap_or_default()))
        .collect();
    reported.sort();
    ccp.status = Some(crate::crds::ClusterConfigPolicyStatus {
        compliant_count: 0,
        non_compliant_count: 0,
        non_compliant_machines: reported,
        conditions: vec![],
    });

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-cap")))
                .returning_json(&ccp),
            expect_event_post("default"), // Evaluated
            expect_event_post("default"), // NonCompliantTargets
        ],
        stores_with(&[NS_A], machines, vec![]),
    );

    reconcile_cluster_config_policy(Arc::new(ccp), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    let status = report.captured[0].body_json()["status"].clone();

    assert_eq!(
        status["nonCompliantCount"],
        serde_json::json!(over_cap),
        "the count must be the exact total even when a namespace exceeds the cap"
    );
    let listed = status["nonCompliantMachines"]
        .as_array()
        .expect("nonCompliantMachines array");
    assert_eq!(
        listed.len(),
        MAX_NON_COMPLIANT_MACHINES,
        "the enumeration is bounded at the cap"
    );
    assert_eq!(
        listed[0],
        format!("{NS_A}/mc-0000"),
        "truncation follows the sort, so which machines fall outside is deterministic"
    );
}

// -----------------------------------------------------------------------
// Unpopulated caches → propagated as Err
// -----------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn reconcile_cluster_config_policy_when_namespace_cache_is_unpopulated_returns_error() {
    let ccp = cluster_config_policy_with_spec("ccp-nslfail", Default::default());

    // The writer is held for the length of the assertion: dropping it would
    // resolve the wait with `WriterDropped` instead of timing out.
    let (namespaces, _writer) = unready_store();
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![],
        ControllerStores {
            namespaces,
            ..empty_stores()
        },
    );

    let result = reconcile_cluster_config_policy(Arc::new(ccp), ctx).await;
    let err = result.expect_err("an unpopulated namespace cache must propagate");
    assert!(err.to_string().contains("Namespace watch cache"), "{err}");

    let report = harness.finish().await;
    assert!(report.captured.is_empty());
}

#[tokio::test(start_paused = true)]
async fn reconcile_cluster_config_policy_when_machine_config_cache_is_unpopulated_returns_error() {
    let ccp = cluster_config_policy_with_spec("ccp-mcfail", Default::default());

    let (machine_configs, _writer) = unready_store();
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![],
        ControllerStores {
            namespaces: seeded_store(vec![namespace(NS_A, &[])]),
            machine_configs,
            ..empty_stores()
        },
    );

    let result = reconcile_cluster_config_policy(Arc::new(ccp), ctx).await;
    let err = result.expect_err("an unpopulated machine cache must propagate");
    let msg = err.to_string();
    assert!(msg.contains("MachineConfig watch cache"), "{msg}");

    let report = harness.finish().await;
    assert!(report.captured.is_empty());
}

// -----------------------------------------------------------------------
// Patch-on-change
// -----------------------------------------------------------------------

/// The same evaluation twice writes once: the second reconcile compares its
/// computed status against the persisted one and finds nothing to say.
#[tokio::test]
async fn reconcile_cluster_config_policy_writes_nothing_when_the_evaluation_is_unchanged() {
    let mut ccp = cluster_config_policy_with_spec("ccp-steady", Default::default());
    let mc = machine_config("mc-a", NS_A);

    // First pass: no status yet, so the status is written and events fire.
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-steady")))
                .returning_json(&ccp),
            expect_event_post("default"),
        ],
        stores_with(&[NS_A], vec![mc.clone()], vec![]),
    );
    reconcile_cluster_config_policy(Arc::new(ccp.clone()), ctx)
        .await
        .unwrap();
    let first = harness.finish().await;
    let patched = first.captured[0].body_json();
    ccp.status =
        Some(serde_json::from_value(patched["status"].clone()).expect("status round-trips"));

    // Second pass: identical inputs, identical verdict, zero API calls.
    let (ctx, _registry, harness) =
        MockKubeHarness::with_stores(vec![], stores_with(&[NS_A], vec![mc], vec![]));
    reconcile_cluster_config_policy(Arc::new(ccp), ctx)
        .await
        .unwrap();
    let second = harness.finish().await;
    assert!(
        second.captured.is_empty(),
        "an unchanged evaluation must make no API call"
    );
}

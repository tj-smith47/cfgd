//! Reconcile-fn tests for `controllers/cluster_config_policy.rs`.
#![cfg(test)]

use std::sync::Arc;

use kube::runtime::controller::Action;

use super::cluster_config_policy::reconcile_cluster_config_policy;
use super::test_fixtures::{cluster_config_policy_with_spec, config_policy, machine_config};
use super::test_kube_harness::{ExpectedCall, MockKubeHarness, expect_event_post};
use crate::crds::{ConfigPolicy, MachineConfig, ModuleRef};
use crate::metrics::PolicyLabels;

const NS_A: &str = "team-a";
const NS_B: &str = "team-b";

fn namespaces_path() -> &'static str {
    "/api/v1/namespaces"
}

fn machine_configs_path(namespace: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{namespace}/machineconfigs")
}

fn config_policies_path(namespace: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{namespace}/configpolicies")
}

fn cluster_policy_path(name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/clusterconfigpolicies/{name}")
}

fn ns_list(namespaces: &[&str]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = namespaces
        .iter()
        .map(|n| {
            serde_json::json!({
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": { "name": n },
            })
        })
        .collect();
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "NamespaceList",
        "items": items,
        "metadata": {},
    })
}

fn mc_list(items: &[MachineConfig]) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "MachineConfigList",
        "items": items,
        "metadata": {},
    })
}

fn cp_list(items: &[ConfigPolicy]) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "ConfigPolicyList",
        "items": items,
        "metadata": {},
    })
}

// -----------------------------------------------------------------------
// All-compliant happy path
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_cluster_config_policy_with_no_namespaces_marks_all_compliant() {
    let ccp = cluster_config_policy_with_spec("ccp-empty", Default::default());

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. LIST namespaces — empty.
        ExpectedCall::list(namespaces_path()).returning_json(&ns_list(&[])),
        // No per-namespace LISTs.
        // 2. PATCH CCP /status
        ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-empty")))
            .returning_json(&ccp),
        // 3. POST Evaluated event (cluster-scoped event posts to default namespace).
        expect_event_post("default"),
    ]);

    let action = reconcile_cluster_config_policy(Arc::new(ccp), ctx.clone())
        .await
        .unwrap();
    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 3);

    let ccp_status = report.captured[1].body_json();
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
    let mut ccp_spec: crate::crds::ClusterConfigPolicySpec = Default::default();
    ccp_spec.required_modules = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];
    let ccp = cluster_config_policy_with_spec("ccp-multi", ccp_spec);

    // mc-a is compliant (has kubectl), mc-b is not.
    let mut mc_a = machine_config("mc-a", NS_A);
    mc_a.spec.module_refs = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];
    let mc_b = machine_config("mc-b", NS_B);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(namespaces_path()).returning_json(&ns_list(&[NS_A, NS_B])),
        // team-a:
        ExpectedCall::list(machine_configs_path(NS_A)).returning_json(&mc_list(&[mc_a])),
        ExpectedCall::list(config_policies_path(NS_A)).returning_json(&cp_list(&[])),
        // team-b:
        ExpectedCall::list(machine_configs_path(NS_B)).returning_json(&mc_list(&[mc_b])),
        ExpectedCall::list(config_policies_path(NS_B)).returning_json(&cp_list(&[])),
        // mc-b is non-compliant — emit PolicyViolation event in NS_B.
        expect_event_post(NS_B),
        // CCP status patch
        ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-multi")))
            .returning_json(&ccp),
        // Cluster-scoped events post to default namespace ("").
        expect_event_post("default"), // Evaluated
        expect_event_post("default"), // NonCompliantTargets
    ]);

    reconcile_cluster_config_policy(Arc::new(ccp), ctx.clone())
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 9);

    let ccp_status = report.captured[6].body_json();
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

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(namespaces_path()).returning_json(&ns_list(&[NS_A])),
        ExpectedCall::list(machine_configs_path(NS_A)).returning_json(&mc_list(&[mc])),
        ExpectedCall::list(config_policies_path(NS_A)).returning_json(&cp_list(&[ns_policy])),
        // mc1 is non-compliant due to merged requirements
        expect_event_post(NS_A), // PolicyViolation
        ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-merge")))
            .returning_json(&ccp),
        expect_event_post("default"), // Evaluated
        expect_event_post("default"), // NonCompliantTargets
    ]);

    reconcile_cluster_config_policy(Arc::new(ccp), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    let ccp_status = report.captured[4].body_json();
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

    // Two namespaces, only one labeled tier=prod.
    let nslist = serde_json::json!({
        "apiVersion": "v1",
        "kind": "NamespaceList",
        "metadata": {},
        "items": [
            {
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": {
                    "name": NS_A,
                    "labels": { "tier": "prod" },
                },
            },
            {
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": { "name": NS_B },
            },
        ],
    });

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(namespaces_path()).returning_json(&nslist),
        // Only NS_A is iterated:
        ExpectedCall::list(machine_configs_path(NS_A)).returning_json(&mc_list(&[])),
        ExpectedCall::list(config_policies_path(NS_A)).returning_json(&cp_list(&[])),
        ExpectedCall::patch_status(format!("{}/status", cluster_policy_path("ccp-scoped")))
            .returning_json(&ccp),
        expect_event_post("default"),
    ]);

    reconcile_cluster_config_policy(Arc::new(ccp), ctx)
        .await
        .unwrap();
    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        5,
        "NS_B must NOT be iterated (no tier=prod label)"
    );
}

// -----------------------------------------------------------------------
// LIST namespaces failure → propagated as Err
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_cluster_config_policy_when_namespace_list_fails_returns_error() {
    let ccp = cluster_config_policy_with_spec("ccp-nslfail", Default::default());

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(namespaces_path()).returning_server_error(500, "etcd"),
    ]);

    let result = reconcile_cluster_config_policy(Arc::new(ccp), ctx).await;
    let err = result.expect_err("namespace list failure must propagate");
    assert!(
        err.to_string().contains("failed to list namespaces"),
        "{err}"
    );

    let _ = harness.finish().await;
}

#[tokio::test]
async fn reconcile_cluster_config_policy_when_per_namespace_mc_list_fails_returns_error() {
    let ccp = cluster_config_policy_with_spec("ccp-mcfail", Default::default());

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(namespaces_path()).returning_json(&ns_list(&[NS_A])),
        ExpectedCall::list(machine_configs_path(NS_A)).returning_server_error(500, "etcd"),
    ]);

    let result = reconcile_cluster_config_policy(Arc::new(ccp), ctx).await;
    let err = result.expect_err("per-ns MC list failure must propagate");
    let msg = err.to_string();
    assert!(msg.contains("failed to list MachineConfigs"), "{msg}");

    let _ = harness.finish().await;
}

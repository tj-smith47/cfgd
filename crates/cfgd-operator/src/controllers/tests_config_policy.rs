//! Reconcile-fn tests for `controllers/config_policy.rs`.
#![cfg(test)]

use std::sync::Arc;

use kube::runtime::controller::Action;

use super::config_policy::reconcile_config_policy;
use super::test_fixtures::{config_policy, machine_config, machine_config_path, mc_list};
use super::test_kube_harness::{ExpectedCall, MockKubeHarness, expect_event_post};
use crate::crds::{
    LabelSelector, LabelSelectorRequirement, ModuleRef, PackageRef, SelectorOperator,
};
use crate::metrics::{PolicyLabels, ReconcileLabels};

const NS: &str = "cfgd-system";

fn machine_configs_path() -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{NS}/machineconfigs")
}

fn config_policy_path(name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{NS}/configpolicies/{name}")
}

// -----------------------------------------------------------------------
// Empty selector → all MCs are targets
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_config_policy_with_empty_selector_targets_all_machine_configs() {
    let policy = config_policy("empty-policy", NS);
    let mc1 = machine_config("mc1", NS);
    let mc2 = machine_config("mc2", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. LIST MachineConfigs
        ExpectedCall::list(machine_configs_path())
            .returning_json(&mc_list(&[mc1.clone(), mc2.clone()])),
        // 2. PATCH each MC's /status to set Compliant=True (empty policy, both compliant)
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc1")))
            .returning_json(&mc1),
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc2")))
            .returning_json(&mc2),
        // (Both are compliant — no PolicyViolation events.)
        // 3. PATCH ConfigPolicy /status
        ExpectedCall::patch_status(format!("{}/status", config_policy_path("empty-policy")))
            .returning_json(&policy),
        // 4. POST event (Evaluated)
        expect_event_post(NS),
    ]);

    let action = reconcile_config_policy(Arc::new(policy), ctx.clone())
        .await
        .unwrap();
    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 5);

    // Per-MC patch sets Compliant=True.
    let mc1_status = report.captured[1].body_json();
    let conditions = mc1_status["status"]["conditions"]
        .as_array()
        .expect("conditions");
    assert_eq!(conditions[0]["type"], "Compliant");
    assert_eq!(conditions[0]["status"], "True");
    assert_eq!(conditions[0]["reason"], "PolicyCompliant");

    // Policy /status: Enforced=True with AllCompliant.
    let policy_status = report.captured[3].body_json();
    let policy_conditions = policy_status["status"]["conditions"]
        .as_array()
        .expect("conditions");
    let enforced = policy_conditions
        .iter()
        .find(|c| c["type"] == "Enforced")
        .unwrap();
    assert_eq!(enforced["status"], "True");
    assert_eq!(enforced["reason"], "AllCompliant");
    assert_eq!(policy_status["status"]["compliantCount"], 2);
    assert_eq!(policy_status["status"]["nonCompliantCount"], 0);

    let count = ctx
        .metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: "empty-policy".to_string(),
            namespace: NS.to_string(),
        })
        .get();
    assert_eq!(count, 2);
}

// -----------------------------------------------------------------------
// Required-module non-compliance
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_config_policy_marks_mc_non_compliant_when_required_module_missing() {
    let mut policy = config_policy("require-mod", NS);
    policy.spec.required_modules = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];

    let mc = machine_config("mc-no-kubectl", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_path())
            .returning_json(&mc_list(std::slice::from_ref(&mc))),
        ExpectedCall::patch_status(format!(
            "{}/status",
            machine_config_path(NS, "mc-no-kubectl")
        ))
        .returning_json(&mc),
        // PolicyViolation event for the non-compliant MC
        expect_event_post(NS),
        ExpectedCall::patch_status(format!("{}/status", config_policy_path("require-mod")))
            .returning_json(&policy),
        expect_event_post(NS), // Evaluated
        expect_event_post(NS), // NonCompliantTargets
    ]);

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 6);

    // MC's Compliant condition recorded as False.
    let mc_status = report.captured[1].body_json();
    let cond = &mc_status["status"]["conditions"][0];
    assert_eq!(cond["status"], "False");
    assert_eq!(cond["reason"], "PolicyViolation");

    // Policy status: Enforced=False with NonCompliantTargets reason.
    let policy_status = report.captured[3].body_json();
    let enforced = policy_status["status"]["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Enforced")
        .unwrap();
    assert_eq!(enforced["status"], "False");
    assert_eq!(enforced["reason"], "NonCompliantTargets");
}

// -----------------------------------------------------------------------
// Package version mismatch
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_config_policy_marks_non_compliant_when_package_version_does_not_satisfy() {
    let mut policy = config_policy("ver-policy", NS);
    policy.spec.packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: Some(">=1.30".to_string()),
    }];

    let mut mc = machine_config("mc-ver", NS);
    mc.spec.packages = vec![PackageRef {
        name: "kubectl".to_string(),
        version: None,
    }];
    // Status reports installed version 1.28 (does not satisfy >=1.30)
    mc.status = Some(crate::crds::MachineConfigStatus {
        last_reconciled: Some("2026-01-01T00:00:00Z".to_string()),
        observed_generation: Some(1),
        conditions: vec![],
        package_versions: [("kubectl".to_string(), "1.28.0".to_string())]
            .into_iter()
            .collect(),
    });

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_path()).returning_json(&mc_list(&[mc.clone()])),
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-ver")))
            .returning_json(&mc),
        expect_event_post(NS), // PolicyViolation
        ExpectedCall::patch_status(format!("{}/status", config_policy_path("ver-policy")))
            .returning_json(&policy),
        expect_event_post(NS), // Evaluated
        expect_event_post(NS), // NonCompliantTargets
    ]);

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    let mc_status = report.captured[1].body_json();
    let cond = &mc_status["status"]["conditions"][0];
    assert_eq!(cond["status"], "False");
}

// -----------------------------------------------------------------------
// Label-selector filtering
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_config_policy_only_targets_machine_configs_matching_selector() {
    let mut policy = config_policy("scoped-policy", NS);
    let mut match_labels = std::collections::BTreeMap::new();
    match_labels.insert("env".to_string(), "prod".to_string());
    policy.spec.target_selector = LabelSelector {
        match_labels,
        match_expressions: vec![],
    };

    let mut prod_mc = machine_config("mc-prod", NS);
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("env".to_string(), "prod".to_string());
    prod_mc.metadata.labels = Some(labels);

    let dev_mc = machine_config("mc-dev", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_path())
            .returning_json(&mc_list(&[prod_mc.clone(), dev_mc])),
        // Only prod_mc gets a PATCH /status
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-prod")))
            .returning_json(&prod_mc),
        ExpectedCall::patch_status(format!("{}/status", config_policy_path("scoped-policy")))
            .returning_json(&policy),
        expect_event_post(NS),
    ]);

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        4,
        "dev MC must NOT be patched (no env=prod label)"
    );
    // No PATCH on /machineconfigs/mc-dev/status.
    assert!(
        report
            .find(http::Method::PATCH, "/machineconfigs/mc-dev/status")
            .is_none()
    );
}

#[tokio::test]
async fn reconcile_config_policy_with_match_expressions_does_not_exist_excludes_labeled_mc() {
    let mut policy = config_policy("excl-policy", NS);
    policy.spec.target_selector = LabelSelector {
        match_labels: Default::default(),
        match_expressions: vec![LabelSelectorRequirement {
            key: "env".to_string(),
            operator: SelectorOperator::DoesNotExist,
            values: vec![],
        }],
    };

    // mc1 has env label → excluded by DoesNotExist.
    let mut mc1 = machine_config("mc1", NS);
    mc1.metadata.labels = Some({
        let mut m = std::collections::BTreeMap::new();
        m.insert("env".to_string(), "any".to_string());
        m
    });
    // mc2 has no env label → included.
    let mc2 = machine_config("mc2", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_path()).returning_json(&mc_list(&[mc1, mc2.clone()])),
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc2")))
            .returning_json(&mc2),
        ExpectedCall::patch_status(format!("{}/status", config_policy_path("excl-policy")))
            .returning_json(&policy),
        expect_event_post(NS),
    ]);

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();
    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 4);
}

// -----------------------------------------------------------------------
// LIST MCs failure → propagated as Err
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_config_policy_when_list_machineconfigs_fails_returns_error() {
    let policy = config_policy("err-policy", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_path()).returning_server_error(500, "etcd melted"),
    ]);

    let result = reconcile_config_policy(Arc::new(policy), ctx.clone()).await;
    let err = result.expect_err("LIST MCs failure must propagate");
    let msg = err.to_string();
    assert!(msg.contains("failed to list MachineConfigs"), "{msg}");

    let _ = harness.finish().await;

    let count = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "config_policy".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(count, 0);
}

// -----------------------------------------------------------------------
// Policy /status patch failure → propagated as Err
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_config_policy_when_status_patch_fails_returns_error() {
    let policy = config_policy("statuserr-policy", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_path()).returning_json(&mc_list(&[])),
        ExpectedCall::patch_status(format!("{}/status", config_policy_path("statuserr-policy")))
            .returning_server_error(500, "etcd melted"),
    ]);

    let result = reconcile_config_policy(Arc::new(policy), ctx).await;
    let err = result.expect_err("ConfigPolicy /status PATCH failure must propagate");
    assert!(
        err.to_string()
            .contains("failed to update ConfigPolicy status"),
        "{err}"
    );

    let _ = harness.finish().await;
}

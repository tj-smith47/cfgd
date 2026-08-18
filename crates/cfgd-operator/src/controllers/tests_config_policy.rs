//! Reconcile-fn tests for `controllers/config_policy.rs`.
#![cfg(test)]

use std::sync::Arc;

use kube::runtime::controller::Action;

use super::ControllerStores;
use super::config_policy::reconcile_config_policy;
use super::test_fixtures::{config_policy, machine_config, machine_config_path};
use super::test_kube_harness::{
    ExpectedCall, MockKubeHarness, empty_stores, expect_event_post, seeded_store, unready_store,
};
use crate::crds::{
    Condition, LabelSelector, LabelSelectorRequirement, MachineConfig, ModuleRef, PackageRef,
    SelectorOperator,
};
use crate::metrics::{PolicyLabels, ReconcileLabels};

const NS: &str = "cfgd-system";

fn config_policy_path(name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{NS}/configpolicies/{name}")
}

/// Caches holding exactly these MachineConfigs — the state a reconcile reads
/// instead of listing.
fn stores_with(machine_configs: Vec<MachineConfig>) -> ControllerStores {
    ControllerStores {
        machine_configs: seeded_store(machine_configs),
        ..empty_stores()
    }
}

/// A `Compliant` condition as the ConfigPolicy controller writes it, for
/// seeding a machine that has already been evaluated.
fn compliant_condition(status: &str, policy: &str) -> Condition {
    let (reason, message) = if status == "True" {
        ("PolicyCompliant", format!("Compliant with policy {policy}"))
    } else {
        ("PolicyViolation", format!("Violates policy {policy}"))
    };
    Condition {
        condition_type: "Compliant".to_string(),
        status: status.to_string(),
        reason: reason.to_string(),
        message,
        last_transition_time: "2026-01-01T00:00:00Z".to_string(),
        observed_generation: Some(1),
    }
}

// -----------------------------------------------------------------------
// Empty selector → all MCs are targets
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_config_policy_with_empty_selector_targets_all_machine_configs() {
    let policy = config_policy("empty-policy", NS);
    let mc1 = machine_config("mc1", NS);
    let mc2 = machine_config("mc2", NS);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // 1. PATCH each MC's /status to set Compliant=True (empty policy, both compliant)
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc1")))
                .returning_json(&mc1),
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc2")))
                .returning_json(&mc2),
            // (Both are compliant — no PolicyViolation events.)
            // 2. PATCH ConfigPolicy /status
            ExpectedCall::patch_status(format!("{}/status", config_policy_path("empty-policy")))
                .returning_json(&policy),
            // 3. POST event (Evaluated)
            expect_event_post(NS),
        ],
        stores_with(vec![mc1.clone(), mc2.clone()]),
    );

    let action = reconcile_config_policy(Arc::new(policy), ctx.clone())
        .await
        .unwrap();
    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        4,
        "the MachineConfigs come from the watch cache — no LIST is issued"
    );
    assert!(
        report.find(http::Method::GET, "/machineconfigs").is_none(),
        "a reconcile must not LIST MachineConfigs"
    );

    // Per-MC patch sets Compliant=True. The store hands machines back in hash
    // order, so find the patch by path rather than by position.
    let mc1_status = report
        .find(http::Method::PATCH, "/machineconfigs/mc1/status")
        .expect("mc1 patched")
        .body_json();
    let conditions = mc1_status["status"]["conditions"]
        .as_array()
        .expect("conditions");
    assert_eq!(conditions[0]["type"], "Compliant");
    assert_eq!(conditions[0]["status"], "True");
    assert_eq!(conditions[0]["reason"], "PolicyCompliant");

    // Policy /status: Enforced=True with AllCompliant.
    let policy_status = report.captured[2].body_json();
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
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
        ],
        stores_with(vec![mc.clone()]),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 5);

    // MC's Compliant condition recorded as False.
    let mc_status = report.captured[0].body_json();
    let cond = &mc_status["status"]["conditions"][0];
    assert_eq!(cond["status"], "False");
    assert_eq!(cond["reason"], "PolicyViolation");

    // Policy status: Enforced=False with NonCompliantTargets reason.
    let policy_status = report.captured[2].body_json();
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-ver")))
                .returning_json(&mc),
            expect_event_post(NS), // PolicyViolation
            ExpectedCall::patch_status(format!("{}/status", config_policy_path("ver-policy")))
                .returning_json(&policy),
            expect_event_post(NS), // Evaluated
            expect_event_post(NS), // NonCompliantTargets
        ],
        stores_with(vec![mc.clone()]),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    let mc_status = report.captured[0].body_json();
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // Only prod_mc gets a PATCH /status
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-prod")))
                .returning_json(&prod_mc),
            ExpectedCall::patch_status(format!("{}/status", config_policy_path("scoped-policy")))
                .returning_json(&policy),
            expect_event_post(NS),
        ],
        stores_with(vec![prod_mc.clone(), dev_mc]),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        3,
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc2")))
                .returning_json(&mc2),
            ExpectedCall::patch_status(format!("{}/status", config_policy_path("excl-policy")))
                .returning_json(&policy),
            expect_event_post(NS),
        ],
        stores_with(vec![mc1, mc2.clone()]),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();
    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 3);
}

// -----------------------------------------------------------------------
// Unpopulated MachineConfig cache → propagated as Err
// -----------------------------------------------------------------------

/// An empty cache and an unpopulated cache look identical from the outside, and
/// answering from the second writes "0 compliant, 0 non-compliant" over a real
/// evaluation. The reconcile must requeue instead.
#[tokio::test(start_paused = true)]
async fn reconcile_config_policy_when_machine_config_cache_is_unpopulated_returns_error() {
    let policy = config_policy("err-policy", NS);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![],
        ControllerStores {
            machine_configs: unready_store(),
            ..empty_stores()
        },
    );

    let result = reconcile_config_policy(Arc::new(policy), ctx.clone()).await;
    let err = result.expect_err("an unpopulated cache must propagate");
    let msg = err.to_string();
    assert!(msg.contains("MachineConfig watch cache"), "{msg}");

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "nothing may be written from an unpopulated cache"
    );

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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                config_policy_path("statuserr-policy")
            ))
            .returning_server_error(500, "etcd melted"),
        ],
        empty_stores(),
    );

    let result = reconcile_config_policy(Arc::new(policy), ctx).await;
    let err = result.expect_err("ConfigPolicy /status PATCH failure must propagate");
    assert!(
        err.to_string()
            .contains("failed to update ConfigPolicy status"),
        "{err}"
    );

    let _ = harness.finish().await;
}

// -----------------------------------------------------------------------
// Patch-on-change and event-on-transition
// -----------------------------------------------------------------------

/// A policy whose persisted status already describes the evaluation writes
/// nothing at all: no MachineConfig patch, no policy patch, no events. This is
/// the 60s requeue that used to cost an etcd write and a watch fan-out per
/// policy per minute forever.
#[tokio::test]
async fn reconcile_config_policy_writes_nothing_when_the_evaluation_is_unchanged() {
    let mut policy = config_policy("steady-policy", NS);
    policy.spec.required_modules = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];

    // The machine already carries the Compliant=False this evaluation produces.
    let mut mc = machine_config("mc-bad", NS);
    mc.status = Some(crate::crds::MachineConfigStatus {
        last_reconciled: None,
        observed_generation: Some(1),
        conditions: vec![compliant_condition("False", "steady-policy")],
        package_versions: Default::default(),
    });

    // ...and the policy already records the resulting status.
    policy.status = Some(crate::crds::ConfigPolicyStatus {
        compliant_count: 0,
        non_compliant_count: 1,
        non_compliant_machines: vec![format!("{NS}/mc-bad")],
        conditions: vec![Condition {
            condition_type: "Enforced".to_string(),
            status: "False".to_string(),
            reason: "NonCompliantTargets".to_string(),
            message: "0 compliant, 1 non-compliant".to_string(),
            last_transition_time: "2026-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        }],
    });

    let (ctx, _registry, harness) =
        MockKubeHarness::with_stores(vec![], stores_with(vec![mc.clone()]));

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "an unchanged evaluation must make no API call, got: {:?}",
        report
            .captured
            .iter()
            .map(|c| format!("{} {}", c.method, c.path))
            .collect::<Vec<_>>()
    );
}

/// Ten identical reconciles of an unevaluated policy produce exactly one round
/// of writes — the first one — because every later round compares equal to what
/// the first persisted.
#[tokio::test]
async fn reconcile_config_policy_repeated_reconciles_patch_status_once() {
    let policy = config_policy("once-policy", NS);
    let mc = machine_config("mc-ok", NS);

    // Reconcile 1: the machine has no Compliant condition and the policy has no
    // status, so both are written and the Evaluated event fires.
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-ok")))
                .returning_json(&mc),
            ExpectedCall::patch_status(format!("{}/status", config_policy_path("once-policy")))
                .returning_json(&policy),
            expect_event_post(NS),
        ],
        stores_with(vec![mc.clone()]),
    );
    reconcile_config_policy(Arc::new(policy.clone()), ctx)
        .await
        .unwrap();
    let first = harness.finish().await;
    let first_patches = first
        .captured
        .iter()
        .filter(|c| c.method == http::Method::PATCH)
        .count();
    assert_eq!(first_patches, 2, "first reconcile writes both statuses");

    // Feed the persisted results back in, exactly as the watch caches would.
    let mut settled_mc = mc.clone();
    let mc_patch = first
        .find(http::Method::PATCH, "/machineconfigs/mc-ok/status")
        .expect("mc patched")
        .body_json();
    settled_mc.status =
        Some(serde_json::from_value(mc_patch["status"].clone()).expect("status round-trips"));
    let policy_patch = first
        .find(http::Method::PATCH, "/configpolicies/once-policy/status")
        .expect("policy patched")
        .body_json();
    let mut settled_policy = policy.clone();
    settled_policy.status =
        Some(serde_json::from_value(policy_patch["status"].clone()).expect("status round-trips"));

    // Reconciles 2..=10: nothing changed, so nothing is written.
    for _ in 0..9 {
        let (ctx, _registry, harness) =
            MockKubeHarness::with_stores(vec![], stores_with(vec![settled_mc.clone()]));
        reconcile_config_policy(Arc::new(settled_policy.clone()), ctx)
            .await
            .unwrap();
        let report = harness.finish().await;
        assert!(
            report.captured.is_empty(),
            "a repeat reconcile must write nothing"
        );
    }
}

/// A machine already recorded in `status.nonCompliantMachines` produces no new
/// event; one that is not yet recorded does. The memory is the policy's own
/// persisted status, so it survives an operator restart.
#[tokio::test]
async fn reconcile_config_policy_emits_violation_event_only_for_newly_violating_machines() {
    let mut policy = config_policy("transition-policy", NS);
    policy.spec.required_modules = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];
    policy.status = Some(crate::crds::ConfigPolicyStatus {
        compliant_count: 0,
        non_compliant_count: 1,
        non_compliant_machines: vec![format!("{NS}/mc-known")],
        conditions: vec![],
    });

    // Both machines violate; only mc-new is unknown to the persisted status.
    let mut known = machine_config("mc-known", NS);
    known.status = Some(crate::crds::MachineConfigStatus {
        last_reconciled: None,
        observed_generation: Some(1),
        conditions: vec![compliant_condition("False", "transition-policy")],
        package_versions: Default::default(),
    });
    let new = machine_config("mc-new", NS);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // mc-known's condition is already correct — no patch. mc-new's is new.
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-new")))
                .returning_json(&new),
            expect_event_post(NS), // PolicyViolation, for mc-new only
            ExpectedCall::patch_status(format!(
                "{}/status",
                config_policy_path("transition-policy")
            ))
            .returning_json(&policy),
            expect_event_post(NS), // Evaluated
            expect_event_post(NS), // NonCompliantTargets
        ],
        stores_with(vec![known, new.clone()]),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    let violation_events = report
        .captured
        .iter()
        .filter(|c| {
            c.method == http::Method::POST
                && String::from_utf8_lossy(&c.body).contains("PolicyViolation")
        })
        .count();
    assert_eq!(
        violation_events, 1,
        "only the newly violating machine gets an event"
    );
    assert!(
        String::from_utf8_lossy(&report.captured[1].body).contains("mc-new"),
        "the event names the machine that just started violating"
    );

    // The persisted set now names both machines.
    let policy_status = report
        .find(
            http::Method::PATCH,
            "/configpolicies/transition-policy/status",
        )
        .expect("policy patched")
        .body_json();
    assert_eq!(
        policy_status["status"]["nonCompliantMachines"],
        serde_json::json!([format!("{NS}/mc-known"), format!("{NS}/mc-new")])
    );
}

/// The Compliant condition the controller owns is patched back with the
/// machine's other conditions intact — a merge patch replaces an array
/// wholesale, so sending only the one condition deletes the rest.
#[tokio::test]
async fn reconcile_config_policy_preserves_sibling_conditions_on_the_machine() {
    let policy = config_policy("sibling-policy", NS);
    let mut mc = machine_config("mc-sib", NS);
    mc.status = Some(crate::crds::MachineConfigStatus {
        last_reconciled: None,
        observed_generation: Some(1),
        conditions: vec![Condition {
            condition_type: "Reconciled".to_string(),
            status: "True".to_string(),
            reason: "ReconcileSuccess".to_string(),
            message: "ok".to_string(),
            last_transition_time: "2026-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        }],
        package_versions: Default::default(),
    });

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-sib")))
                .returning_json(&mc),
            ExpectedCall::patch_status(format!("{}/status", config_policy_path("sibling-policy")))
                .returning_json(&policy),
            expect_event_post(NS),
        ],
        stores_with(vec![mc.clone()]),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    let patched = report.captured[0].body_json();
    let types: Vec<String> = patched["status"]["conditions"]
        .as_array()
        .expect("conditions")
        .iter()
        .map(|c| c["type"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        types,
        vec!["Reconciled".to_string(), "Compliant".to_string()],
        "the patch must carry the machine's existing conditions plus Compliant"
    );
}

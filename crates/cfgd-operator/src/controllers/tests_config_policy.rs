//! Reconcile-fn tests for `controllers/config_policy.rs`.
#![cfg(test)]

use std::sync::Arc;

use http::Method;
use kube::ResourceExt;
use kube::runtime::controller::Action;

use super::config_policy::reconcile_config_policy;
use super::test_fixtures::{config_policy, machine_config, machine_config_path, new_config_policy};
use super::test_kube_harness::{
    ExpectedCall, MockKubeHarness, empty_stores, expect_event_post, seeded_store, unready_store,
};
use super::{CONFIG_POLICY_FINALIZER, ControllerStores};
use crate::crds::{
    Condition, LabelSelector, LabelSelectorRequirement, MAX_NON_COMPLIANT_MACHINES, MachineConfig,
    ModuleRef, PackageRef, SelectorOperator,
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

    // The writer is held for the length of the assertion: dropping it would
    // resolve the wait with `WriterDropped` instead of timing out.
    let (machine_configs, _writer) = unready_store();
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![],
        ControllerStores {
            machine_configs,
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

/// A machine is targeted so the reconcile reaches the policy `/status` write
/// the way production does — through the per-machine condition loop — rather
/// than short-circuiting past it on an empty cache.
#[tokio::test]
async fn reconcile_config_policy_when_status_patch_fails_returns_error() {
    let policy = config_policy("statuserr-policy", NS);
    let mc = machine_config("mc-statuserr", NS);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-statuserr")
            ))
            .returning_json(&mc),
            ExpectedCall::patch_status(format!(
                "{}/status",
                config_policy_path("statuserr-policy")
            ))
            .returning_server_error(500, "etcd melted"),
        ],
        stores_with(vec![mc.clone()]),
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

/// The status a previous evaluation of these machines could actually have
/// persisted: the exact count beside a list the schema's `maxItems` permits.
/// Seeding all of them instead would be a fixture the API server rejects, and it
/// would hide the very degradation the cap documents.
fn persisted_violator_memory(machines: &[MachineConfig]) -> crate::crds::ConfigPolicyStatus {
    let mut remembered: Vec<String> = machines
        .iter()
        .map(|mc| format!("{NS}/{}", mc.name_any()))
        .collect();
    remembered.sort();
    let total = u32::try_from(remembered.len()).unwrap_or(u32::MAX);
    remembered.truncate(MAX_NON_COMPLIANT_MACHINES);
    crate::crds::ConfigPolicyStatus {
        compliant_count: 0,
        non_compliant_count: total,
        non_compliant_machines: remembered,
        conditions: vec![],
    }
}

/// The violator list is an enumeration inside a status object every operator
/// replica watches, so it is bounded; the count beside it is not. A policy
/// violated by more machines than the cap reports the exact total and lists the
/// first `MAX_NON_COMPLIANT_MACHINES` of them in sorted order.
#[tokio::test]
async fn reconcile_config_policy_caps_the_violator_list_but_not_the_count() {
    let over_cap = MAX_NON_COMPLIANT_MACHINES + 1;
    let mut policy = config_policy("cap-policy", NS);
    policy.spec.required_modules = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];

    // Every machine violates, and each already carries the exact Compliant
    // condition the controller would write — so the per-machine loop patches
    // nothing and the queue stays about the policy's own status.
    let machines: Vec<MachineConfig> = (0..over_cap)
        .map(|i| {
            let mut mc = machine_config(&format!("mc-{i:04}"), NS);
            mc.status = Some(crate::crds::MachineConfigStatus {
                last_reconciled: None,
                observed_generation: Some(1),
                conditions: vec![compliant_condition("False", "cap-policy")],
                package_versions: Default::default(),
            });
            mc
        })
        .collect();

    policy.status = Some(persisted_violator_memory(&machines));

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // The one machine outside the persisted memory reads as a new
            // violator on every evaluation. See the re-fire test below.
            expect_event_post(NS), // PolicyViolation, mc-0500
            ExpectedCall::patch_status(format!("{}/status", config_policy_path("cap-policy")))
                .returning_json(&policy),
            expect_event_post(NS), // Evaluated
            expect_event_post(NS), // NonCompliantTargets
        ],
        stores_with(machines),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    let status = report
        .find(
            Method::PATCH,
            &format!("{}/status", config_policy_path("cap-policy")),
        )
        .expect("the policy status patch must have been captured")
        .body_json()["status"]
        .clone();

    assert_eq!(
        status["nonCompliantCount"],
        serde_json::json!(over_cap),
        "the count is the exact total and is never capped"
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
        format!("{NS}/mc-0000"),
        "truncation follows the sort, so which machines fall outside is deterministic"
    );
    assert_eq!(
        listed[MAX_NON_COMPLIANT_MACHINES - 1],
        format!("{NS}/mc-{:04}", MAX_NON_COMPLIANT_MACHINES - 1)
    );
}

/// The documented degradation above the cap, pinned. `PolicyViolation` fires
/// once per machine because the policy's persisted `nonCompliantMachines` is the
/// transition memory, and a machine truncated out of that list is never in it —
/// so it reads as a new violator on every evaluation, forever. The docs promise
/// exactly this; without a test the promise is a paragraph.
#[tokio::test]
async fn reconcile_config_policy_refires_violation_events_for_machines_past_the_cap() {
    let over_cap = MAX_NON_COMPLIANT_MACHINES + 1;
    let outside_the_cap = format!("mc-{:04}", MAX_NON_COMPLIANT_MACHINES);
    let mut policy = config_policy("refire-policy", NS);
    policy.spec.required_modules = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];

    let machines: Vec<MachineConfig> = (0..over_cap)
        .map(|i| {
            let mut mc = machine_config(&format!("mc-{i:04}"), NS);
            mc.status = Some(crate::crds::MachineConfigStatus {
                last_reconciled: None,
                observed_generation: Some(1),
                conditions: vec![compliant_condition("False", "refire-policy")],
                package_versions: Default::default(),
            });
            mc
        })
        .collect();
    policy.status = Some(persisted_violator_memory(&machines));

    let assert_refired = |report: &super::test_kube_harness::HarnessReport, pass: &str| {
        let violation = report.captured[0].body_json();
        assert_eq!(
            violation["reason"], "PolicyViolation",
            "{pass}: the first call must be the violation event"
        );
        assert_eq!(
            violation["regarding"]["name"], outside_the_cap,
            "{pass}: only the machine truncated out of the transition memory re-fires"
        );
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            expect_event_post(NS), // PolicyViolation
            ExpectedCall::patch_status(format!("{}/status", config_policy_path("refire-policy")))
                .returning_json(&policy),
            expect_event_post(NS), // Evaluated
            expect_event_post(NS), // NonCompliantTargets
        ],
        stores_with(machines.clone()),
    );
    reconcile_config_policy(Arc::new(policy.clone()), ctx)
        .await
        .unwrap();
    let first = harness.finish().await;
    assert_refired(&first, "first evaluation");

    // What the operator persisted is what the next evaluation reads, and it
    // still cannot hold the machine outside the cap.
    policy.status = Some(
        serde_json::from_value(
            first
                .find(
                    Method::PATCH,
                    &format!("{}/status", config_policy_path("refire-policy")),
                )
                .expect("the policy status patch must have been captured")
                .body_json()["status"]
                .clone(),
        )
        .expect("the patched status must round-trip"),
    );

    // Nothing about the cluster moved, so patch-on-change writes no status and
    // announces no evaluation. The violation event fires anyway: that is the
    // degradation, isolated to a single call.
    let (ctx, _registry, harness) =
        MockKubeHarness::with_stores(vec![expect_event_post(NS)], stores_with(machines));
    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();
    let second = harness.finish().await;
    assert_eq!(
        second.captured.len(),
        1,
        "an unchanged evaluation past the cap makes exactly one call, the re-fired event"
    );
    assert_refired(&second, "second evaluation");
}

// -----------------------------------------------------------------------
// Finalizer: the verdict is retired with the policy that made it
// -----------------------------------------------------------------------

/// A policy on its first reconcile is registered before it judges anything: the
/// verdict it is about to write onto each machine can only be retired by a last
/// reconcile, and only a finalizer guarantees one.
#[tokio::test]
async fn reconcile_config_policy_adds_finalizer_when_missing() {
    let policy = new_config_policy("fresh-policy", NS);
    assert!(policy.metadata.finalizers.is_none());

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch(config_policy_path("fresh-policy"))
                .with_query_contains("fieldManager=cfgd-operator")
                .returning_json(&policy),
            ExpectedCall::patch_status(format!("{}/status", config_policy_path("fresh-policy")))
                .returning_json(&policy),
            expect_event_post(NS), // Evaluated
        ],
        stores_with(vec![]),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    let added = report.captured[0].body_json();
    assert_eq!(
        added["metadata"]["finalizers"],
        serde_json::json!([CONFIG_POLICY_FINALIZER])
    );
}

/// Deleting a policy retires the verdict it left behind. The machine controller
/// carries `Compliant` forward verbatim and holds no policy lookup of its own,
/// so nothing else can clear it: without this the machine would name a policy
/// that no longer exists for as long as the machine exists.
#[tokio::test]
async fn reconcile_config_policy_clears_its_verdict_from_machines_on_deletion() {
    let mut policy = config_policy("doomed-policy", NS);
    policy.spec.required_modules = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];
    policy.metadata.deletion_timestamp = Some(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(k8s_openapi::jiff::Timestamp::now()),
    );

    // The machine as the policy left it: judged non-compliant, naming the policy.
    let mut judged = machine_config("mc-judged", NS);
    judged.status = Some(crate::crds::MachineConfigStatus {
        last_reconciled: None,
        observed_generation: Some(1),
        conditions: vec![
            Condition {
                condition_type: "Reconciled".to_string(),
                status: "True".to_string(),
                reason: "ReconcileSuccess".to_string(),
                message: "ok".to_string(),
                last_transition_time: "2026-01-01T00:00:00Z".to_string(),
                observed_generation: Some(1),
            },
            compliant_condition("False", "doomed-policy"),
        ],
        package_versions: Default::default(),
    });

    // A machine no policy ever judged has nothing to retire, so it is not written to.
    let untouched = machine_config("mc-unjudged", NS);

    // What the API server holds NOW: the cache copy plus a DriftDetected the
    // machine controller wrote after the cache was populated. The reset is
    // built from this read, so the concurrent write survives it.
    let mut judged_live = judged.clone();
    if let Some(status) = judged_live.status.as_mut() {
        status.conditions.push(Condition {
            condition_type: "DriftDetected".to_string(),
            status: "True".to_string(),
            reason: "DriftAlertsActive".to_string(),
            message: "1 active drift alert(s)".to_string(),
            last_transition_time: "2026-01-02T00:00:00Z".to_string(),
            observed_generation: Some(1),
        });
    }

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::get(machine_config_path(NS, "mc-judged")).returning_json(&judged_live),
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-judged")))
                .returning_json(&judged_live),
            ExpectedCall::get(machine_config_path(NS, "mc-unjudged")).returning_json(&untouched),
            ExpectedCall::patch(config_policy_path("doomed-policy")).returning_json(&policy),
        ],
        stores_with(vec![judged.clone(), untouched.clone()]),
    );

    let action = reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();
    assert_eq!(action, Action::await_change());

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        4,
        "only the judged machine is written to, then the finalizer is dropped"
    );

    let conditions = report
        .find(Method::PATCH, "/machineconfigs/mc-judged/status")
        .expect("mc-judged reset")
        .body_json()["status"]["conditions"]
        .as_array()
        .expect("conditions array")
        .clone();
    let compliant = conditions
        .iter()
        .find(|c| c["type"] == "Compliant")
        .expect("Compliant condition");
    assert_eq!(compliant["status"], "Unknown");
    assert_eq!(compliant["reason"], "NotEvaluated");
    assert_eq!(
        compliant["message"], "Awaiting policy evaluation",
        "the reset is the same triple the machine controller synthesizes for a never-judged machine"
    );
    assert!(
        conditions.iter().any(|c| c["type"] == "Reconciled"),
        "the machine's other conditions must survive the reset"
    );
    assert!(
        conditions.iter().any(|c| c["type"] == "DriftDetected"),
        "a condition present only on the live object must survive: the reset is \
         built from the API server's copy, not the watch cache's"
    );

    assert_eq!(
        report.captured[3].body_json()["metadata"]["finalizers"],
        serde_json::json!([]),
        "the finalizer is dropped only after the verdicts are cleared"
    );
}

/// A machine relabelled out of the selector after being judged non-compliant is
/// no longer in the selector match at deletion time, but the policy's own
/// `status.nonCompliantMachines` still remembers it: the clear covers the union
/// of both, so the stale `Compliant=False` is retired anyway. A remembered
/// machine that no longer exists is skipped without failing the deletion.
#[tokio::test]
async fn deleting_a_policy_clears_a_remembered_machine_the_selector_no_longer_matches() {
    let mut policy = config_policy("recall-policy", NS);
    let mut match_labels = std::collections::BTreeMap::new();
    match_labels.insert("env".to_string(), "prod".to_string());
    policy.spec.target_selector = LabelSelector {
        match_labels,
        match_expressions: vec![],
    };
    policy.status = Some(crate::crds::ConfigPolicyStatus {
        compliant_count: 0,
        non_compliant_count: 2,
        non_compliant_machines: vec![format!("{NS}/mc-gone"), format!("{NS}/mc-relabelled")],
        conditions: vec![],
    });
    policy.metadata.deletion_timestamp = Some(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(k8s_openapi::jiff::Timestamp::now()),
    );

    // Judged while it carried env=prod, then relabelled: the selector no longer
    // matches it, so only the status memory can name it.
    let mut relabelled = machine_config("mc-relabelled", NS);
    relabelled.metadata.labels = Some({
        let mut m = std::collections::BTreeMap::new();
        m.insert("env".to_string(), "dev".to_string());
        m
    });
    relabelled.status = Some(crate::crds::MachineConfigStatus {
        last_reconciled: None,
        observed_generation: Some(1),
        conditions: vec![compliant_condition("False", "recall-policy")],
        package_versions: Default::default(),
    });

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::get(machine_config_path(NS, "mc-gone")).returning_404("mc-gone"),
            ExpectedCall::get(machine_config_path(NS, "mc-relabelled")).returning_json(&relabelled),
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-relabelled")
            ))
            .returning_json(&relabelled),
            ExpectedCall::patch(config_policy_path("recall-policy")).returning_json(&policy),
        ],
        stores_with(vec![relabelled.clone()]),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 4);
    let compliant = report
        .find(Method::PATCH, "/machineconfigs/mc-relabelled/status")
        .expect("the remembered machine must be reset")
        .body_json()["status"]["conditions"][0]
        .clone();
    assert_eq!(compliant["type"], "Compliant");
    assert_eq!(compliant["status"], "Unknown");
    assert_eq!(compliant["reason"], "NotEvaluated");
    assert_eq!(
        report.captured[3].body_json()["metadata"]["finalizers"],
        serde_json::json!([])
    );
}

/// One machine the API server refuses cannot strand the deleted policy: the
/// clear is best effort per machine, so the next machine is still reset and the
/// finalizer still comes off.
#[tokio::test]
async fn a_machine_the_api_server_refuses_does_not_strand_the_deleted_policy() {
    let mut policy = config_policy("refused-policy", NS);
    policy.metadata.deletion_timestamp = Some(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(k8s_openapi::jiff::Timestamp::now()),
    );

    let judged = |name: &str| {
        let mut mc = machine_config(name, NS);
        mc.status = Some(crate::crds::MachineConfigStatus {
            last_reconciled: None,
            observed_generation: Some(1),
            conditions: vec![compliant_condition("False", "refused-policy")],
            package_versions: Default::default(),
        });
        mc
    };
    let mc_a = judged("mc-a");
    let mc_b = judged("mc-b");

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::get(machine_config_path(NS, "mc-a")).returning_json(&mc_a),
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-a")))
                .returning_server_error(500, "etcd melted"),
            ExpectedCall::get(machine_config_path(NS, "mc-b")).returning_json(&mc_b),
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-b")))
                .returning_json(&mc_b),
            ExpectedCall::patch(config_policy_path("refused-policy")).returning_json(&policy),
        ],
        stores_with(vec![mc_a.clone(), mc_b.clone()]),
    );

    let action = reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();
    assert_eq!(action, Action::await_change());

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 5);
    let reset = report
        .find(Method::PATCH, "/machineconfigs/mc-b/status")
        .expect("the machine after the refused one must still be reset")
        .body_json();
    assert_eq!(reset["status"]["conditions"][0]["status"], "Unknown");
    assert_eq!(
        report.captured[4].body_json()["metadata"]["finalizers"],
        serde_json::json!([]),
        "the finalizer must come off despite the refused machine"
    );
}

/// A repeat deletion reconcile (the finalizer removal failed last time) finds
/// the verdict already reset and writes nothing to the machine: the cleared
/// triple is a steady state, not a new thing to patch.
#[tokio::test]
async fn a_repeat_deletion_reconcile_does_not_rewrite_an_already_cleared_verdict() {
    let mut policy = config_policy("repeat-policy", NS);
    policy.metadata.deletion_timestamp = Some(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(k8s_openapi::jiff::Timestamp::now()),
    );

    let mut cleared = machine_config("mc-cleared", NS);
    cleared.status = Some(crate::crds::MachineConfigStatus {
        last_reconciled: None,
        observed_generation: Some(1),
        conditions: vec![Condition {
            condition_type: "Compliant".to_string(),
            status: "Unknown".to_string(),
            reason: "NotEvaluated".to_string(),
            message: "Awaiting policy evaluation".to_string(),
            last_transition_time: "2026-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        }],
        package_versions: Default::default(),
    });

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::get(machine_config_path(NS, "mc-cleared")).returning_json(&cleared),
            ExpectedCall::patch(config_policy_path("repeat-policy")).returning_json(&policy),
        ],
        stores_with(vec![cleared.clone()]),
    );

    reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        2,
        "an already-cleared verdict must not be rewritten: {:?}",
        report
            .captured
            .iter()
            .map(|c| format!("{} {}", c.method, c.path))
            .collect::<Vec<_>>()
    );
    assert!(
        report
            .find(Method::PATCH, "/machineconfigs/mc-cleared/status")
            .is_none()
    );
}

/// Deleting a policy retires its gauge series along with its verdicts: nothing
/// re-sets a deleted policy's `devices_compliant`, so an unremoved series would
/// export the last count for the life of the process. The deletion pass also
/// counts as a reconciliation, so a deletion-heavy period does not read as a
/// dead controller on the liveness metric.
#[tokio::test]
async fn deleting_a_policy_removes_its_gauge_series_and_records_the_reconcile() {
    let mut policy = config_policy("metrics-doomed", NS);
    policy.metadata.deletion_timestamp = Some(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(k8s_openapi::jiff::Timestamp::now()),
    );

    let (ctx, registry, harness) = MockKubeHarness::with_stores(
        vec![ExpectedCall::patch(config_policy_path("metrics-doomed")).returning_json(&policy)],
        stores_with(vec![]),
    );

    // The series as a prior successful reconcile left it, plus a sibling
    // policy's series that must survive the targeted removal.
    ctx.metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: "metrics-doomed".to_string(),
            namespace: NS.to_string(),
        })
        .set(12);
    ctx.metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: "metrics-survivor".to_string(),
            namespace: NS.to_string(),
        })
        .set(3);

    let mut before = String::new();
    prometheus_client::encoding::text::encode(&mut before, &registry).expect("encode");
    assert!(
        before.contains("metrics-doomed"),
        "precondition: the series must exist before the deletion reconcile"
    );

    reconcile_config_policy(Arc::new(policy), ctx.clone())
        .await
        .unwrap();
    let _ = harness.finish().await;

    let mut after = String::new();
    prometheus_client::encoding::text::encode(&mut after, &registry).expect("encode");
    assert!(
        !after.contains("metrics-doomed"),
        "the deleted policy's series must be removed: {after}"
    );
    assert!(
        after.contains("metrics-survivor"),
        "another policy's series must survive the removal: {after}"
    );

    let success = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "config_policy".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(success, 1, "a deletion pass is a successful reconciliation");
}

/// The reset is the machine controller's own never-evaluated triple, so the two
/// agree by construction: the machine controller carries it forward unchanged
/// and writes nothing.
#[tokio::test]
async fn a_machine_whose_policy_was_deleted_reaches_steady_state() {
    let mut mc = machine_config("mc-orphaned", NS);
    mc.metadata.finalizers = Some(vec![super::MACHINE_CONFIG_FINALIZER.to_string()]);
    mc.status = Some(crate::crds::MachineConfigStatus {
        last_reconciled: Some("2026-01-01T00:00:00Z".to_string()),
        observed_generation: Some(1),
        conditions: vec![Condition {
            condition_type: "Compliant".to_string(),
            status: "Unknown".to_string(),
            reason: "NotEvaluated".to_string(),
            message: "Awaiting policy evaluation".to_string(),
            last_transition_time: "2026-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        }],
        package_versions: Default::default(),
    });

    let alert = super::test_fixtures::drift_alert(
        "alert-orphaned",
        NS,
        "mc-orphaned",
        crate::crds::DriftSeverity::Medium,
    );

    // Drift keeps the reconcile off the early-return arm, so it really rebuilds
    // and compares the condition rather than skipping the question.
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-orphaned")
            ))
            .returning_json(&mc),
            expect_event_post(NS), // Reconciled
            expect_event_post(NS), // DriftDetected
        ],
        ControllerStores {
            drift_alerts: seeded_store(vec![alert]),
            ..empty_stores()
        },
    );
    super::machine_config::reconcile_machine_config(Arc::new(mc.clone()), ctx)
        .await
        .unwrap();
    let first = harness.finish().await;
    mc.status = Some(
        serde_json::from_value(first.captured[0].body_json()["status"].clone())
            .expect("the patched status must round-trip"),
    );

    let alert = super::test_fixtures::drift_alert(
        "alert-orphaned",
        NS,
        "mc-orphaned",
        crate::crds::DriftSeverity::Medium,
    );
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![],
        ControllerStores {
            drift_alerts: seeded_store(vec![alert]),
            ..empty_stores()
        },
    );
    super::machine_config::reconcile_machine_config(Arc::new(mc), ctx)
        .await
        .unwrap();
    let second = harness.finish().await;
    assert!(
        second.captured.is_empty(),
        "a cleared verdict is a steady state, not a new thing to write: {:?}",
        second
            .captured
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect::<Vec<_>>()
    );
}

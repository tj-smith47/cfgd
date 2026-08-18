//! Reconcile-fn tests for `controllers/machine_config.rs`.
#![cfg(test)]

use std::sync::Arc;

use kube::runtime::controller::Action;

use super::machine_config::reconcile_machine_config;
use super::test_fixtures::{machine_config, machine_config_path};
use super::test_kube_harness::{
    ExpectedCall, MockKubeHarness, empty_stores, expect_event_post, seeded_store, unready_store,
};
use super::{ControllerStores, MACHINE_CONFIG_FINALIZER};
use crate::crds::{Condition, DriftAlert, MachineConfigStatus, ModuleRef};
use crate::metrics::ReconcileLabels;

const NS: &str = "cfgd-system";

fn stores_with_drift(alerts: Vec<DriftAlert>) -> ControllerStores {
    ControllerStores {
        drift_alerts: seeded_store(alerts),
        ..empty_stores()
    }
}

// -----------------------------------------------------------------------
// Happy path & finalizer management
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_adds_finalizer_when_missing() {
    let mc = machine_config("mc-noface", NS);
    assert!(mc.metadata.finalizers.is_none());

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // 1. PATCH metadata to add finalizer
            ExpectedCall::patch(machine_config_path(NS, "mc-noface"))
                .with_query_contains("fieldManager=cfgd-operator")
                .returning_json(&mc),
            // 2. PATCH /status — the drift read is served by the cache.
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-noface")))
                .returning_json(&mc),
            // 3. POST event (Reconciled)
            expect_event_post(NS),
        ],
        empty_stores(),
    );

    let action = reconcile_machine_config(Arc::new(mc), ctx.clone())
        .await
        .expect("happy path");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        3,
        "the DriftAlert read must cost no API call"
    );

    // Finalizer-add patch contains MACHINE_CONFIG_FINALIZER.
    let finalizer_patch = report.captured[0].body_json();
    let finalizers = finalizer_patch["metadata"]["finalizers"]
        .as_array()
        .expect("finalizers array");
    assert!(
        finalizers.iter().any(|f| f == MACHINE_CONFIG_FINALIZER),
        "finalizer-add patch must include {MACHINE_CONFIG_FINALIZER}: {finalizer_patch}"
    );

    let success = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "machine_config".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(success, 1);
}

#[tokio::test]
async fn reconcile_machine_config_removes_finalizer_on_deletion_then_returns_await_change() {
    let mut mc = machine_config("mc-deleting", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);
    mc.metadata.deletion_timestamp = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
        k8s_openapi::jiff::Timestamp::now(),
    ));

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch(machine_config_path(NS, "mc-deleting"))
                .with_query_contains("fieldManager=cfgd-operator")
                .returning_json(&mc),
        ],
        empty_stores(),
    );

    let action = reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();
    assert_eq!(
        action,
        Action::await_change(),
        "deletion-with-finalizer path returns Action::await_change"
    );

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 1);

    // Patch removes the finalizer (resulting list is empty).
    let body = report.captured[0].body_json();
    let finalizers = body["metadata"]["finalizers"]
        .as_array()
        .expect("finalizers array (possibly empty)");
    assert!(
        !finalizers.iter().any(|f| f == MACHINE_CONFIG_FINALIZER),
        "finalizer must be removed in delete path: {body}"
    );
}

#[tokio::test]
async fn reconcile_machine_config_when_deletion_and_no_finalizer_skips_patch_and_proceeds() {
    let mut mc = machine_config("mc-deleted-clean", NS);
    mc.metadata.deletion_timestamp = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
        k8s_openapi::jiff::Timestamp::now(),
    ));
    // No finalizer present — fall through to the normal flow but without
    // the add-finalizer patch (because deletion_timestamp is set).

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // No finalizer-add or finalizer-remove patch.
            // 1. PATCH /status
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-deleted-clean")
            ))
            .returning_json(&mc),
            // 2. POST event
            expect_event_post(NS),
        ],
        empty_stores(),
    );

    reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();
    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 2);
}

// -----------------------------------------------------------------------
// Validation failure path
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_returns_invalid_spec_error_when_hostname_empty() {
    let mut mc = machine_config("mc-bad", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);
    mc.spec.hostname.clear(); // make spec invalid

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // Validation fails before reaching the PATCH chain. Only the
            // event POST goes out.
            expect_event_post(NS),
        ],
        empty_stores(),
    );

    let result = reconcile_machine_config(Arc::new(mc), ctx).await;
    let err = result.expect_err("invalid spec must propagate");
    let msg = err.to_string();
    assert!(
        msg.contains("hostname"),
        "error must mention the bad field: {msg}"
    );

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        1,
        "validation-failure path emits exactly one event"
    );
}

// -----------------------------------------------------------------------
// Generation-unchanged short-circuit
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_skips_when_generation_observed_and_no_drift() {
    let mut mc = machine_config("mc-cached", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);
    mc.metadata.generation = Some(7);
    mc.status = Some(MachineConfigStatus {
        last_reconciled: Some("2026-01-01T00:00:00Z".to_string()),
        observed_generation: Some(7),
        conditions: vec![],
        package_versions: Default::default(),
    });

    // The drift check is a cache read, so an already-observed generation with
    // no drift performs no API call at all.
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], empty_stores());

    let action = reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();
    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert!(report.captured.is_empty());
}

// -----------------------------------------------------------------------
// Drift detection paths
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_emits_drift_event_when_active_alerts_match() {
    let mut mc = machine_config("mc-drifted", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);

    // Active DriftAlert for this MC, seeded into the cache the reconcile reads.
    let alert = super::test_fixtures::drift_alert(
        "alert-on-drifted",
        NS,
        "mc-drifted",
        crate::crds::DriftSeverity::Medium,
    );

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // 1. PATCH /status
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-drifted")))
                .returning_json(&mc),
            // 2. POST event (Reconciled)
            expect_event_post(NS),
            // 3. POST event (DriftDetected) — extra event because has_drift
            expect_event_post(NS),
        ],
        stores_with_drift(vec![alert]),
    );

    reconcile_machine_config(Arc::new(mc), ctx.clone())
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 3);

    // Status patch records DriftDetected=True.
    let status_body = report.captured[0].body_json();
    let conditions = status_body["status"]["conditions"]
        .as_array()
        .expect("conditions array");
    let drift_cond = conditions
        .iter()
        .find(|c| c["type"] == "DriftDetected")
        .expect("DriftDetected condition present");
    assert_eq!(drift_cond["status"], "True");

    // The drift_events_total counter was bumped (warning-severity branch).
    let drift_count = ctx
        .metrics
        .drift_events_total
        .get_or_create(&crate::metrics::DriftLabels {
            severity: "warning".to_string(),
            namespace: NS.to_string(),
        })
        .get();
    assert_eq!(drift_count, 1);
}

/// A machine whose drift is already recorded is the one case the generation
/// skip above cannot cover: `has_drift` is true, so the reconcile runs in full
/// every 60s. It must still write nothing and announce nothing while the
/// situation holds, or a drifted fleet costs one etcd write and two events per
/// machine per minute for as long as the drift lasts.
#[tokio::test]
async fn reconcile_machine_config_drifted_repeated_reconciles_write_once() {
    let mut mc = machine_config("mc-steady-drift", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);

    let alert = super::test_fixtures::drift_alert(
        "alert-on-steady",
        NS,
        "mc-steady-drift",
        crate::crds::DriftSeverity::Medium,
    );

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-steady-drift")
            ))
            .returning_json(&mc),
            expect_event_post(NS), // Reconciled
            expect_event_post(NS), // DriftDetected
        ],
        stores_with_drift(vec![alert]),
    );

    reconcile_machine_config(Arc::new(mc.clone()), ctx.clone())
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 3, "the first pass records the drift");

    // Feed the status the operator just wrote back onto the object, which is
    // what the next reconcile reads off the watch.
    mc.status = Some(
        serde_json::from_value(report.captured[0].body_json()["status"].clone())
            .expect("the patched status must round-trip"),
    );

    let alert = super::test_fixtures::drift_alert(
        "alert-on-steady",
        NS,
        "mc-steady-drift",
        crate::crds::DriftSeverity::Medium,
    );
    let (ctx, _registry, harness) =
        MockKubeHarness::with_stores(vec![], stores_with_drift(vec![alert]));

    for _ in 0..10 {
        reconcile_machine_config(Arc::new(mc.clone()), ctx.clone())
            .await
            .unwrap();
    }

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "a machine whose drift is already recorded must make no API call at all, \
         but made: {:?}",
        report
            .captured
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect::<Vec<_>>()
    );
}

// -----------------------------------------------------------------------
// Compliant is owned by the policy controllers
// -----------------------------------------------------------------------

/// A `Compliant` condition as the ConfigPolicy controller writes it.
fn policy_written_compliant(policy: &str) -> Condition {
    Condition {
        condition_type: "Compliant".to_string(),
        status: "False".to_string(),
        reason: "PolicyViolation".to_string(),
        message: format!("Violates policy {policy}"),
        last_transition_time: "2026-01-01T00:00:00Z".to_string(),
        observed_generation: Some(1),
    }
}

/// The machine controller rebuilds every condition on the object each pass,
/// `Compliant` included, and must carry the policy controller's own reason and
/// message through untouched. Substituting its own text is a claim about a
/// verdict it did not reach, and `Condition` compares by every field, so the
/// policy controller reads the substitution back as a change and rewrites it.
#[tokio::test]
async fn reconcile_machine_config_carries_the_policys_compliant_message_forward() {
    let mut mc = machine_config("mc-policy-text", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);
    mc.status = Some(MachineConfigStatus {
        last_reconciled: Some("2026-01-01T00:00:00Z".to_string()),
        observed_generation: Some(1),
        conditions: vec![policy_written_compliant("p")],
        package_versions: Default::default(),
    });

    // Drift keeps the reconcile off the early-return arm, which is the path on
    // which the machine controller rebuilds and republishes the condition.
    let alert = super::test_fixtures::drift_alert(
        "alert-policy-text",
        NS,
        "mc-policy-text",
        crate::crds::DriftSeverity::Medium,
    );

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-policy-text")
            ))
            .returning_json(&mc),
            expect_event_post(NS), // Reconciled
            expect_event_post(NS), // DriftDetected
        ],
        stores_with_drift(vec![alert]),
    );

    reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();

    let report = harness.finish().await;
    let conditions = report.captured[0].body_json()["status"]["conditions"]
        .as_array()
        .expect("conditions array")
        .clone();
    let compliant = conditions
        .iter()
        .find(|c| c["type"] == "Compliant")
        .expect("Compliant condition must survive the rewrite");

    assert_eq!(
        compliant["message"], "Violates policy p",
        "the machine controller must not rewrite the policy's Compliant message"
    );
    assert_eq!(compliant["reason"], "PolicyViolation");
    assert_eq!(compliant["status"], "False");
}

/// The two controllers watch each other's writes: a MachineConfig status write
/// requeues every ConfigPolicy in the namespace, and the policy's patch of the
/// machine requeues the machine. If they disagree about any field of
/// `Compliant`, each one's patch-on-change guard sees the difference the other
/// just created and the pair spins at watch speed for as long as the drift
/// lasts. Steady state is the assertion: after one machine write, neither
/// controller has anything left to say about the machine.
#[tokio::test]
async fn a_drifted_policy_targeted_machine_reaches_steady_state() {
    let mut mc = machine_config("mc-pingpong", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);
    mc.status = Some(MachineConfigStatus {
        last_reconciled: Some("2026-01-01T00:00:00Z".to_string()),
        observed_generation: Some(1),
        conditions: vec![policy_written_compliant("pp-policy")],
        package_versions: Default::default(),
    });

    let drift = || {
        super::test_fixtures::drift_alert(
            "alert-pingpong",
            NS,
            "mc-pingpong",
            crate::crds::DriftSeverity::Medium,
        )
    };

    // The machine controller records the drift once.
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-pingpong")
            ))
            .returning_json(&mc),
            expect_event_post(NS), // Reconciled
            expect_event_post(NS), // DriftDetected
        ],
        stores_with_drift(vec![drift()]),
    );
    reconcile_machine_config(Arc::new(mc.clone()), ctx)
        .await
        .unwrap();
    let first = harness.finish().await;
    mc.status = Some(
        serde_json::from_value(first.captured[0].body_json()["status"].clone())
            .expect("the patched status must round-trip"),
    );

    // The policy controller, requeued by that write, finds its own verdict
    // already on the machine and patches nothing. Its own status is seeded as
    // already-evaluated, so the only call it could make is the machine patch.
    let mut policy = super::test_fixtures::config_policy("pp-policy", NS);
    policy.spec.required_modules = vec![ModuleRef {
        name: "kubectl".to_string(),
        required: true,
    }];
    policy.status = Some(crate::crds::ConfigPolicyStatus {
        compliant_count: 0,
        non_compliant_count: 1,
        non_compliant_machines: vec![format!("{NS}/mc-pingpong")],
        conditions: vec![],
    });
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "/apis/cfgd.io/v1alpha1/namespaces/{NS}/configpolicies/pp-policy/status"
            ))
            .returning_json(&policy),
            expect_event_post(NS), // Evaluated
            expect_event_post(NS), // NonCompliantTargets
        ],
        ControllerStores {
            machine_configs: seeded_store(vec![mc.clone()]),
            ..empty_stores()
        },
    );
    super::config_policy::reconcile_config_policy(Arc::new(policy), ctx)
        .await
        .unwrap();
    let second = harness.finish().await;
    assert!(
        second
            .find(
                http::Method::PATCH,
                &format!("{}/status", machine_config_path(NS, "mc-pingpong"))
            )
            .is_none(),
        "the policy controller must not rewrite the machine's Compliant condition, but did: {:?}",
        second
            .captured
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect::<Vec<_>>()
    );

    // And the machine controller, requeued by nothing new, writes nothing.
    let (ctx, _registry, harness) =
        MockKubeHarness::with_stores(vec![], stores_with_drift(vec![drift()]));
    reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();
    let third = harness.finish().await;
    assert!(
        third.captured.is_empty(),
        "a drifted policy-targeted machine must reach steady state, but made: {:?}",
        third
            .captured
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect::<Vec<_>>()
    );
}

// -----------------------------------------------------------------------
// Module resolution branch
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_resolves_module_refs_and_records_modules_resolved_status() {
    let mut mc = machine_config("mc-with-modules", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);
    mc.spec.module_refs = vec![ModuleRef {
        name: "missing-mod".to_string(),
        required: true,
    }];

    // Both cross-resource reads (DriftAlerts, Modules) are cache reads.
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-with-modules")
            ))
            .returning_json(&mc),
            expect_event_post(NS),
        ],
        empty_stores(),
    );

    reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 2);

    // The status-patch must contain ModulesResolved=False with reason ModulesNotFound.
    let status_body = report.captured[0].body_json();
    let conditions = status_body["status"]["conditions"]
        .as_array()
        .expect("conditions array");
    let resolved = conditions
        .iter()
        .find(|c| c["type"] == "ModulesResolved")
        .expect("ModulesResolved condition present");
    assert_eq!(resolved["status"], "False");
    assert_eq!(resolved["reason"], "ModulesNotFound");
    assert!(
        resolved["message"]
            .as_str()
            .unwrap_or("")
            .contains("missing-mod"),
        "message must name the missing module: {resolved}"
    );
}

// -----------------------------------------------------------------------
// Status patch failure → ReconcileError event
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_when_status_patch_fails_emits_reconcile_error_event() {
    let mut mc = machine_config("mc-statuserr", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-statuserr")
            ))
            .returning_server_error(500, "etcd melted"),
            expect_event_post(NS),
        ],
        empty_stores(),
    );

    let result = reconcile_machine_config(Arc::new(mc), ctx).await;
    let err = result.expect_err("status patch failure must propagate as Err");
    let msg = err.to_string();
    assert!(
        msg.contains("failed to update status"),
        "error must reference the failing operation: {msg}"
    );

    let _ = harness.finish().await;
}

// -----------------------------------------------------------------------
// Existing-Compliant condition preservation
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_preserves_existing_compliant_condition_status() {
    let mut mc = machine_config("mc-with-compliant", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);
    mc.metadata.generation = Some(2);
    // The status records observed_generation=1 (so reconcile does NOT
    // short-circuit) and an existing Compliant=True condition that the
    // reconcile must preserve in its emitted patch.
    mc.status = Some(MachineConfigStatus {
        last_reconciled: Some("2025-12-01T00:00:00Z".to_string()),
        observed_generation: Some(1),
        conditions: vec![Condition {
            condition_type: "Compliant".to_string(),
            status: "True".to_string(),
            reason: "PolicyCompliant".to_string(),
            message: "set by ConfigPolicy controller".to_string(),
            last_transition_time: "2026-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        }],
        package_versions: Default::default(),
    });

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path(NS, "mc-with-compliant")
            ))
            .returning_json(&mc),
            expect_event_post(NS),
        ],
        empty_stores(),
    );

    reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[0].body_json();
    let conditions = status_body["status"]["conditions"]
        .as_array()
        .expect("conditions array");
    let compliant = conditions
        .iter()
        .find(|c| c["type"] == "Compliant")
        .expect("Compliant condition present");
    assert_eq!(
        compliant["status"], "True",
        "Compliant condition value must be preserved across reconcile"
    );
    assert_eq!(compliant["reason"], "PolicyCompliant");
}

// -----------------------------------------------------------------------
// Unpopulated caches
// -----------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn reconcile_machine_config_when_drift_alert_cache_is_unpopulated_returns_error() {
    let mut mc = machine_config("mc-nocache", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);

    // The writer is held for the length of the assertion: dropping it would
    // resolve the wait with `WriterDropped` instead of timing out.
    let (drift_alerts, _writer) = unready_store();
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![],
        ControllerStores {
            drift_alerts,
            ..empty_stores()
        },
    );

    let result = reconcile_machine_config(Arc::new(mc), ctx).await;
    let err = result.expect_err("an unpopulated DriftAlert cache must propagate");
    assert!(err.to_string().contains("DriftAlert watch cache"), "{err}");

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "a reconcile that cannot read its caches must not write a status"
    );
}

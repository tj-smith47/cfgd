//! Reconcile-fn tests for `controllers/machine_config.rs`.
#![cfg(test)]

use std::sync::Arc;

use kube::runtime::controller::Action;

use super::MACHINE_CONFIG_FINALIZER;
use super::machine_config::reconcile_machine_config;
use super::test_fixtures::{machine_config, machine_config_path};
use super::test_kube_harness::{ExpectedCall, MockKubeHarness, expect_event_post};
use crate::crds::{Condition, MachineConfigStatus, ModuleRef};
use crate::metrics::ReconcileLabels;

const NS: &str = "cfgd-system";

fn drift_alerts_path() -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{NS}/driftalerts")
}

fn modules_list_path() -> &'static str {
    "/apis/cfgd.io/v1alpha1/modules"
}

fn empty_drift_list() -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "DriftAlertList",
        "items": [],
        "metadata": {},
    })
}

fn empty_module_list() -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "ModuleList",
        "items": [],
        "metadata": {},
    })
}

// -----------------------------------------------------------------------
// Happy path & finalizer management
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_adds_finalizer_when_missing() {
    let mc = machine_config("mc-noface", NS);
    assert!(mc.metadata.finalizers.is_none());

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. PATCH metadata to add finalizer
        ExpectedCall::patch(machine_config_path(NS, "mc-noface"))
            .with_query_contains("fieldManager=cfgd-operator")
            .returning_json(&mc),
        // 2. LIST DriftAlerts (has_active_drift_alerts)
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        // 3. LIST DriftAlerts (cleanup_drift_alerts, since !has_drift)
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        // 4. PATCH /status (no LIST modules — module_refs is empty)
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-noface")))
            .returning_json(&mc),
        // 5. POST event (Reconciled)
        expect_event_post(NS),
    ]);

    let action = reconcile_machine_config(Arc::new(mc), ctx.clone())
        .await
        .expect("happy path");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 5);

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
        chrono::Utc::now(),
    ));

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::patch(machine_config_path(NS, "mc-deleting"))
            .with_query_contains("fieldManager=cfgd-operator")
            .returning_json(&mc),
    ]);

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
        chrono::Utc::now(),
    ));
    // No finalizer present — fall through to the normal flow but without
    // the add-finalizer patch (because deletion_timestamp is set).

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // No finalizer-add or finalizer-remove patch.
        // 1. LIST DriftAlerts (has_active_drift_alerts)
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        // 2. LIST DriftAlerts (cleanup_drift_alerts)
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        // 3. PATCH /status
        ExpectedCall::patch_status(format!(
            "{}/status",
            machine_config_path(NS, "mc-deleted-clean")
        ))
        .returning_json(&mc),
        // 4. POST event
        expect_event_post(NS),
    ]);

    reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();
    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 4);
}

// -----------------------------------------------------------------------
// Validation failure path
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_returns_invalid_spec_error_when_hostname_empty() {
    let mut mc = machine_config("mc-bad", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);
    mc.spec.hostname.clear(); // make spec invalid

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // Validation fails before reaching the LIST/PATCH chain. Only the
        // event POST goes out.
        expect_event_post(NS),
    ]);

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

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // Only 1 LIST — has_active_drift_alerts (returns no drift).
        // No cleanup_drift_alerts because reconcile short-circuits before that.
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
    ]);

    let action = reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();
    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 1);
}

// -----------------------------------------------------------------------
// Drift detection paths
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_machine_config_emits_drift_event_when_active_alerts_match() {
    let mut mc = machine_config("mc-drifted", NS);
    mc.metadata.finalizers = Some(vec![MACHINE_CONFIG_FINALIZER.to_string()]);

    // Active DriftAlert for this MC:
    let drift_list = serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "DriftAlertList",
        "metadata": {},
        "items": [super::test_fixtures::drift_alert(
            "alert-on-drifted",
            NS,
            "mc-drifted",
            crate::crds::DriftSeverity::Medium,
        )],
    });

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. has_active_drift_alerts → returns true
        ExpectedCall::list(drift_alerts_path()).returning_json(&drift_list),
        // No cleanup_drift_alerts call (because has_drift)
        // 2. PATCH /status
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-drifted")))
            .returning_json(&mc),
        // 3. POST event (Reconciled)
        expect_event_post(NS),
        // 4. POST event (DriftDetected) — extra event because has_drift
        expect_event_post(NS),
    ]);

    reconcile_machine_config(Arc::new(mc), ctx.clone())
        .await
        .unwrap();

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 4);

    // Status patch records DriftDetected=True.
    let status_body = report.captured[1].body_json();
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

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        // resolve_module_refs LISTs all Modules (cluster-scoped).
        ExpectedCall::list(modules_list_path()).returning_json(&empty_module_list()),
        ExpectedCall::patch_status(format!(
            "{}/status",
            machine_config_path(NS, "mc-with-modules")
        ))
        .returning_json(&mc),
        expect_event_post(NS),
    ]);

    reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 5);

    // The status-patch must contain ModulesResolved=False with reason ModulesNotFound.
    let status_body = report.captured[3].body_json();
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

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        ExpectedCall::patch_status(format!(
            "{}/status",
            machine_config_path(NS, "mc-statuserr")
        ))
        .returning_server_error(500, "etcd melted"),
        expect_event_post(NS),
    ]);

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
// Existing-Compliant condition preservation (C1 fix verification)
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

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        ExpectedCall::list(drift_alerts_path()).returning_json(&empty_drift_list()),
        ExpectedCall::patch_status(format!(
            "{}/status",
            machine_config_path(NS, "mc-with-compliant")
        ))
        .returning_json(&mc),
        expect_event_post(NS),
    ]);

    reconcile_machine_config(Arc::new(mc), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[2].body_json();
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

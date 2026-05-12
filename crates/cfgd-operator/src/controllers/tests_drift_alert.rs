//! Reconcile-fn tests for `controllers/drift_alert.rs`.
//!
//! Each test drives `reconcile_drift_alert` end-to-end through the
//! `MockKubeHarness`, asserting on the kube API call sequence and on the
//! emitted metrics + events. See `test_kube_harness.rs` for the harness
//! shape and `test_fixtures.rs` for the CRD object builders.
#![cfg(test)]

use std::sync::Arc;

use http::Method;
use kube::ResourceExt;
use kube::runtime::controller::Action;

use super::drift_alert::{cleanup_drift_alerts, has_active_drift_alerts, reconcile_drift_alert};
use super::test_fixtures::{
    drift_alert, machine_config, machine_config_owner_ref, machine_config_path,
    machine_config_status_with_drift_detected,
};
use super::test_kube_harness::{ExpectedCall, MockKubeHarness, expect_event_post};
use crate::crds::DriftSeverity;
use crate::metrics::ReconcileLabels;

const NS: &str = "cfgd-system";

fn drift_alerts_path(namespace: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{namespace}/driftalerts")
}

fn drift_alert_path(namespace: &str, name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{namespace}/driftalerts/{name}")
}

// -----------------------------------------------------------------------
// reconcile_drift_alert — happy paths and error branches
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_drift_alert_when_machine_config_missing_records_error_metric_and_requeues() {
    let alert = drift_alert("alert-1", NS, "missing-mc", DriftSeverity::Medium);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::get(machine_config_path(NS, "missing-mc")).returning_404("missing-mc"),
    ]);

    let action = reconcile_drift_alert(Arc::new(alert), ctx.clone())
        .await
        .expect("404 path returns Ok with requeue, not Err");

    assert_eq!(
        action,
        Action::requeue(std::time::Duration::from_secs(60)),
        "404 path requeues after 60s"
    );

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 1);

    let count = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "drift_alert".to_string(),
            result: "error".to_string(),
        })
        .get();
    assert_eq!(
        count, 1,
        "404-on-MC must record an error metric (H6 fix), not success"
    );

    let success_count = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "drift_alert".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(success_count, 0, "404 path must not record a success");
}

#[tokio::test]
async fn reconcile_drift_alert_when_machine_config_get_returns_5xx_propagates_error() {
    let alert = drift_alert("alert-2", NS, "broken-mc", DriftSeverity::High);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::get(machine_config_path(NS, "broken-mc"))
            .returning_server_error(500, "kube apiserver melted"),
    ]);

    let result = reconcile_drift_alert(Arc::new(alert), ctx).await;
    let err = result.expect_err("non-404 server error must propagate as Err");
    let msg = err.to_string();
    assert!(
        msg.contains("failed to get MachineConfig"),
        "error should reference the MC fetch path: {msg}"
    );

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 1);
}

#[tokio::test]
async fn reconcile_drift_alert_with_no_owner_ref_patches_owner_ref_then_drift_status() {
    let alert = drift_alert("alert-3", NS, "mc-1", DriftSeverity::Medium);
    let mc = machine_config("mc-1", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. GET MachineConfig
        ExpectedCall::get(machine_config_path(NS, "mc-1")).returning_json(&mc),
        // 2. PATCH DriftAlert metadata to set ownerReferences
        ExpectedCall::patch(drift_alert_path(NS, "alert-3"))
            .with_query_contains("fieldManager=cfgd-operator")
            .returning_json(&alert),
        // 3. PATCH MachineConfig /status to set DriftDetected condition
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-1")))
            .with_query_contains("fieldManager=cfgd-operator%2Fstatus")
            .returning_json(&mc),
        // 4. POST events.k8s.io Event for DriftDetected
        expect_event_post(NS),
        // 5. PATCH DriftAlert /status with Resolved=False conditions
        ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-3")))
            .with_query_contains("fieldManager=cfgd-operator%2Fstatus")
            .returning_json(&alert),
    ]);

    let action = reconcile_drift_alert(Arc::new(alert.clone()), ctx.clone())
        .await
        .expect("happy path returns Ok");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 5);

    // 1st patch is the owner-ref patch on the DriftAlert.
    let owner_patch_body = report.captured[1].body_json();
    let owners = &owner_patch_body["metadata"]["ownerReferences"];
    assert!(
        owners.is_array() && !owners.as_array().unwrap().is_empty(),
        "owner-ref patch must populate ownerReferences: {owner_patch_body}"
    );
    assert_eq!(owners[0]["kind"], "MachineConfig");
    assert_eq!(owners[0]["name"], "mc-1");
    assert_eq!(owners[0]["uid"], "uid-mc-1");
    assert_eq!(owners[0]["controller"], true);
    assert_eq!(owners[0]["blockOwnerDeletion"], true);

    // 2nd patch sets MachineConfig.status.conditions[*].type=DriftDetected,status=True.
    let mc_status_body = report.captured[2].body_json();
    let conditions = mc_status_body["status"]["conditions"]
        .as_array()
        .expect("status.conditions array");
    let drift_cond = conditions
        .iter()
        .find(|c| c["type"] == "DriftDetected")
        .expect("DriftDetected condition present");
    assert_eq!(drift_cond["status"], "True");
    assert_eq!(drift_cond["reason"], "DriftActive");

    // The drift_events_total counter was bumped.
    let drift_count = ctx
        .metrics
        .drift_events_total
        .get_or_create(&crate::metrics::DriftLabels {
            severity: format!("{:?}", DriftSeverity::Medium),
            namespace: NS.to_string(),
        })
        .get();
    assert_eq!(
        drift_count, 1,
        "drift_events_total must increment on first detection"
    );
}

#[tokio::test]
async fn reconcile_drift_alert_with_existing_owner_ref_skips_owner_patch() {
    let mut alert = drift_alert("alert-4", NS, "mc-2", DriftSeverity::Low);
    alert
        .owner_references_mut()
        .push(machine_config_owner_ref("mc-2"));

    let mc = machine_config("mc-2", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. GET MachineConfig
        ExpectedCall::get(machine_config_path(NS, "mc-2")).returning_json(&mc),
        // 2. PATCH MachineConfig /status (no owner-ref patch since it's already set)
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-2")))
            .returning_json(&mc),
        // 3. POST event
        expect_event_post(NS),
        // 4. PATCH DriftAlert /status
        ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-4")))
            .returning_json(&alert),
    ]);

    let action = reconcile_drift_alert(Arc::new(alert), ctx)
        .await
        .expect("happy path with owner ref already set");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        4,
        "owner-ref already present, so no owner-ref PATCH on the DriftAlert"
    );
    // No PATCH on the DriftAlert metadata path (only its /status).
    let metadata_patch_count = report
        .captured
        .iter()
        .filter(|r| r.method == Method::PATCH && r.path == drift_alert_path(NS, "alert-4"))
        .count();
    assert_eq!(metadata_patch_count, 0);
}

#[tokio::test]
async fn reconcile_drift_alert_when_drift_already_recorded_skips_drift_status_patch() {
    let mut alert = drift_alert("alert-5", NS, "mc-3", DriftSeverity::Medium);
    alert
        .owner_references_mut()
        .push(machine_config_owner_ref("mc-3"));

    let mut mc = machine_config("mc-3", NS);
    mc.status = Some(machine_config_status_with_drift_detected());

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. GET MachineConfig (already has DriftDetected=True)
        ExpectedCall::get(machine_config_path(NS, "mc-3")).returning_json(&mc),
        // No further calls — has_drift_condition is true and drift_details is non-empty,
        // so neither the resolve-and-delete branch nor the set-drift branch fires.
    ]);

    let action = reconcile_drift_alert(Arc::new(alert), ctx.clone())
        .await
        .expect("idempotent path");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        1,
        "only the GET should land — no patch, no event"
    );

    let success = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "drift_alert".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(
        success, 1,
        "idempotent path still records reconcile success"
    );
}

#[tokio::test]
async fn reconcile_drift_alert_when_resolved_deletes_alert_and_emits_event() {
    let mut alert = drift_alert("alert-6", NS, "mc-4", DriftSeverity::Low);
    alert.spec.drift_details.clear();
    alert
        .owner_references_mut()
        .push(machine_config_owner_ref("mc-4"));

    // MC has no DriftDetected condition — combined with empty drift_details,
    // this is the "resolve & delete" path.
    let mc = machine_config("mc-4", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. GET MachineConfig
        ExpectedCall::get(machine_config_path(NS, "mc-4")).returning_json(&mc),
        // 2. PATCH DriftAlert /status with Resolved=True
        ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-6")))
            .returning_json(&alert),
        // 3. POST event for DriftResolved
        expect_event_post(NS),
        // 4. DELETE the resolved DriftAlert
        ExpectedCall::delete(drift_alert_path(NS, "alert-6")).returning_json(&alert),
    ]);

    let action = reconcile_drift_alert(Arc::new(alert), ctx.clone())
        .await
        .expect("resolve path returns Ok");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 4);

    // The resolution-status patch must include Resolved=True.
    let status_body = report.captured[1].body_json();
    let conditions = status_body["status"]["conditions"]
        .as_array()
        .expect("conditions array");
    let resolved = conditions
        .iter()
        .find(|c| c["type"] == "Resolved")
        .expect("Resolved condition present");
    assert_eq!(resolved["status"], "True");
    assert_eq!(resolved["reason"], "DriftResolved");

    let success = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "drift_alert".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(success, 1);
}

#[tokio::test]
async fn reconcile_drift_alert_when_machine_config_missing_uid_returns_error() {
    let alert = drift_alert("alert-7", NS, "no-uid-mc", DriftSeverity::Medium);
    let mut mc = machine_config("no-uid-mc", NS);
    mc.metadata.uid = None;

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::get(machine_config_path(NS, "no-uid-mc")).returning_json(&mc),
    ]);

    let result = reconcile_drift_alert(Arc::new(alert), ctx).await;
    let err = result.expect_err("missing UID must short-circuit (H7 fix)");
    let msg = err.to_string();
    assert!(
        msg.contains("has no UID"),
        "error must reference the missing-UID guard: {msg}"
    );

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 1);
}

#[tokio::test]
async fn reconcile_drift_alert_high_severity_emits_escalated_condition_in_status_patch() {
    let mut alert = drift_alert("alert-8", NS, "mc-5", DriftSeverity::Critical);
    alert
        .owner_references_mut()
        .push(machine_config_owner_ref("mc-5"));

    let mc = machine_config("mc-5", NS);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::get(machine_config_path(NS, "mc-5")).returning_json(&mc),
        ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-5")))
            .returning_json(&mc),
        expect_event_post(NS),
        ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-8")))
            .returning_json(&alert),
    ]);

    let _ = reconcile_drift_alert(Arc::new(alert), ctx).await.unwrap();
    let report = harness.finish().await;

    let da_status_body = report.captured[3].body_json();
    let conditions = da_status_body["status"]["conditions"]
        .as_array()
        .expect("conditions array");
    let escalated = conditions
        .iter()
        .find(|c| c["type"] == "Escalated")
        .expect("Escalated condition present");
    assert_eq!(
        escalated["status"], "True",
        "Critical severity must produce Escalated=True"
    );
    assert_eq!(escalated["reason"], "SeverityThreshold");
}

// -----------------------------------------------------------------------
// has_active_drift_alerts and cleanup_drift_alerts
// -----------------------------------------------------------------------

#[tokio::test]
async fn has_active_drift_alerts_returns_true_when_alert_matches() {
    let alert = drift_alert("alert-active", NS, "mc-x", DriftSeverity::Low);
    let list = serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "DriftAlertList",
        "items": [alert],
        "metadata": {},
    });

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(drift_alerts_path(NS)).returning_raw(serde_json::to_vec(&list).unwrap()),
    ]);

    let active = has_active_drift_alerts(&ctx.client, NS, "mc-x").await;
    assert!(active);

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 1);
}

#[tokio::test]
async fn has_active_drift_alerts_returns_false_when_no_match() {
    let alert = drift_alert("alert-other", NS, "different-mc", DriftSeverity::Low);
    let list = serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "DriftAlertList",
        "items": [alert],
        "metadata": {},
    });

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(drift_alerts_path(NS)).returning_raw(serde_json::to_vec(&list).unwrap()),
    ]);

    let active = has_active_drift_alerts(&ctx.client, NS, "mc-x").await;
    assert!(!active);

    let _ = harness.finish().await;
}

#[tokio::test]
async fn has_active_drift_alerts_returns_false_when_list_fails() {
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(drift_alerts_path(NS)).returning_server_error(500, "list failed"),
    ]);

    let active = has_active_drift_alerts(&ctx.client, NS, "mc-x").await;
    assert!(
        !active,
        "list failure must be treated as no-active-drift (best-effort)"
    );

    let _ = harness.finish().await;
}

#[tokio::test]
async fn cleanup_drift_alerts_deletes_only_matching_alerts() {
    let match_alert = drift_alert("alert-match", NS, "mc-cleanup", DriftSeverity::Medium);
    let other_alert = drift_alert("alert-other", NS, "mc-different", DriftSeverity::Low);
    let list = serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "DriftAlertList",
        "items": [match_alert.clone(), other_alert],
        "metadata": {},
    });

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(drift_alerts_path(NS)).returning_raw(serde_json::to_vec(&list).unwrap()),
        ExpectedCall::delete(drift_alert_path(NS, "alert-match")).returning_json(&match_alert),
    ]);

    cleanup_drift_alerts(&ctx.client, NS, "mc-cleanup").await;

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 2);
    assert!(
        report
            .find(Method::DELETE, "/driftalerts/alert-match")
            .is_some(),
        "matching alert must be deleted"
    );
    assert!(
        report
            .find(Method::DELETE, "/driftalerts/alert-other")
            .is_none(),
        "non-matching alert must NOT be deleted"
    );
}

#[tokio::test]
async fn cleanup_drift_alerts_skips_when_list_fails() {
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(drift_alerts_path(NS)).returning_server_error(500, "list failed"),
    ]);

    cleanup_drift_alerts(&ctx.client, NS, "anything").await;

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        1,
        "list failure short-circuits — no DELETEs follow"
    );
}

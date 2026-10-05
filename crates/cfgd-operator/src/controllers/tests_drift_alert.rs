//! Reconcile-fn tests for `controllers/drift_alert.rs`.
//!
//! Each test drives `reconcile_drift_alert` end-to-end through the
//! `MockKubeHarness`, asserting on the kube API call sequence and on the
//! emitted metrics + events. See `test_kube_harness.rs` for the harness
//! shape and `test_fixtures.rs` for the CRD object builders.

use std::sync::Arc;

use http::Method;
use kube::ResourceExt;
use kube::runtime::controller::Action;

use super::ControllerStores;
use super::drift_alert::{has_active_drift_alerts, reconcile_drift_alert};
use super::test_fixtures::{
    drift_alert, machine_config, machine_config_owner_ref, machine_config_path,
    machine_config_status_with_drift_detected,
};
use super::test_kube_harness::{
    ExpectedCall, MockKubeHarness, empty_stores, expect_event_post, seeded_store, unready_store,
};
use crate::crds::{Condition, DriftSeverity};
use crate::metrics::ReconcileLabels;

const NS: &str = "cfgd-system";

fn machines(mcs: Vec<crate::crds::MachineConfig>) -> ControllerStores {
    ControllerStores {
        machine_configs: seeded_store(mcs),
        ..empty_stores()
    }
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], machines(vec![]));

    let action = reconcile_drift_alert(Arc::new(alert), ctx.clone())
        .await
        .expect("a missing machine returns Ok with requeue, not Err");

    assert_eq!(
        action,
        Action::requeue(std::time::Duration::from_secs(60)),
        "a missing machine requeues after 60s"
    );

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "the machine is read from the watch cache, so a missing one costs no API call"
    );

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
        "a missing MachineConfig must record an error metric, not success"
    );

    let success_count = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "drift_alert".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(
        success_count, 0,
        "a missing machine must not record a success"
    );
}

/// An unpopulated MachineConfig cache is no evidence that the machine is
/// gone, so the reconcile errors and the controller retries it.
#[tokio::test(start_paused = true)]
async fn reconcile_drift_alert_errors_when_the_machine_cache_is_not_populated() {
    let alert = drift_alert("alert-2", NS, "mc-unready", DriftSeverity::High);
    let (machine_configs, _writer) = unready_store();
    let stores = ControllerStores {
        machine_configs,
        ..empty_stores()
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], stores);

    let err = reconcile_drift_alert(Arc::new(alert), ctx)
        .await
        .expect_err("an unpopulated cache must not answer");
    assert!(
        err.to_string().contains("MachineConfig watch cache"),
        "error must name the cache that was not ready: {err}"
    );

    let report = harness.finish().await;
    assert!(report.captured.is_empty());
}

#[tokio::test]
async fn reconcile_drift_alert_with_no_owner_ref_patches_owner_ref_then_drift_status() {
    let alert = drift_alert("alert-3", NS, "mc-1", DriftSeverity::Medium);
    let mc = machine_config("mc-1", NS);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // 1. PATCH DriftAlert metadata to set ownerReferences
            ExpectedCall::patch(drift_alert_path(NS, "alert-3"))
                .with_query_contains("fieldManager=cfgd-operator")
                .returning_json(&alert),
            // 2. PATCH MachineConfig /status to set DriftDetected condition
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-1")))
                .with_query_contains("fieldManager=cfgd-operator%2Fstatus")
                .returning_json(&mc),
            // 3. POST events.k8s.io Event for DriftDetected
            expect_event_post(NS),
            // 4. PATCH DriftAlert /status with Resolved=False conditions
            ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-3")))
                .with_query_contains("fieldManager=cfgd-operator%2Fstatus")
                .returning_json(&alert),
        ],
        machines(vec![mc.clone()]),
    );

    let action = reconcile_drift_alert(Arc::new(alert.clone()), ctx.clone())
        .await
        .expect("happy path returns Ok");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 4);

    // 1st patch is the owner-ref patch on the DriftAlert.
    let owner_patch_body = report.captured[0].body_json();
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
    let mc_status_body = report.captured[1].body_json();
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // 1. PATCH MachineConfig /status (no owner-ref patch since it's already set)
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-2")))
                .returning_json(&mc),
            // 2. POST event
            expect_event_post(NS),
            // 3. PATCH DriftAlert /status
            ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-4")))
                .returning_json(&alert),
        ],
        machines(vec![mc.clone()]),
    );

    let action = reconcile_drift_alert(Arc::new(alert), ctx)
        .await
        .expect("happy path with owner ref already set");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        3,
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

/// The DriftDetected condition this controller owns is patched back with the
/// machine's other conditions intact — a merge patch replaces an array
/// wholesale, so sending only the one condition deletes the rest. Losing
/// `Compliant` here silently resets the machine's policy verdict.
#[tokio::test]
async fn reconcile_drift_alert_preserves_sibling_conditions_on_the_machine() {
    let mut alert = drift_alert("alert-sib", NS, "mc-sib", DriftSeverity::High);
    alert
        .owner_references_mut()
        .push(machine_config_owner_ref("mc-sib"));

    let mut mc = machine_config("mc-sib", NS);
    mc.status = Some(crate::crds::MachineConfigStatus {
        last_reconciled: Some("2026-01-01T00:00:00Z".to_string()),
        backup_schedule_owners: Default::default(),
        compliance: None,
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
            Condition {
                condition_type: "Compliant".to_string(),
                status: "True".to_string(),
                reason: "PolicyCompliant".to_string(),
                message: "Compliant with policy p".to_string(),
                last_transition_time: "2026-01-01T00:00:00Z".to_string(),
                observed_generation: Some(1),
            },
        ],
        package_versions: Default::default(),
    });

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-sib")))
                .returning_json(&mc),
            expect_event_post(NS),
            ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-sib")))
                .returning_json(&alert),
        ],
        machines(vec![mc.clone()]),
    );

    reconcile_drift_alert(Arc::new(alert), ctx)
        .await
        .expect("drift is recorded on the machine");

    let report = harness.finish().await;
    let types: Vec<String> = report.captured[0].body_json()["status"]["conditions"]
        .as_array()
        .expect("conditions")
        .iter()
        .map(|c| c["type"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        types,
        vec![
            "Reconciled".to_string(),
            "Compliant".to_string(),
            "DriftDetected".to_string()
        ],
        "the patch must carry the machine's existing conditions plus DriftDetected"
    );
}

#[tokio::test]
async fn reconcile_drift_alert_when_drift_already_recorded_skips_drift_status_patch() {
    let mut alert = drift_alert("alert-5", NS, "mc-3", DriftSeverity::Medium);
    alert
        .owner_references_mut()
        .push(machine_config_owner_ref("mc-3"));

    let mut mc = machine_config("mc-3", NS);
    mc.status = Some(machine_config_status_with_drift_detected());

    let (ctx, _registry, harness) =
        MockKubeHarness::with_stores(vec![], machines(vec![mc.clone()]));

    let action = reconcile_drift_alert(Arc::new(alert), ctx.clone())
        .await
        .expect("idempotent path");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "the machine is read from the cache, and nothing is written"
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // 1. PATCH DriftAlert /status with Resolved=True
            ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-6")))
                .returning_json(&alert),
            // 2. POST event for DriftResolved
            expect_event_post(NS),
            // 3. DELETE the resolved DriftAlert
            ExpectedCall::delete(drift_alert_path(NS, "alert-6")).returning_json(&alert),
        ],
        machines(vec![mc.clone()]),
    );

    let action = reconcile_drift_alert(Arc::new(alert), ctx.clone())
        .await
        .expect("resolve path returns Ok");

    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 3);

    // The resolution-status patch must include Resolved=True.
    let status_body = report.captured[0].body_json();
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], machines(vec![mc]));

    let result = reconcile_drift_alert(Arc::new(alert), ctx).await;
    let err = result.expect_err("missing UID must short-circuit");
    let msg = err.to_string();
    assert!(
        msg.contains("has no UID"),
        "error must reference the missing-UID guard: {msg}"
    );

    let report = harness.finish().await;
    assert!(report.captured.is_empty());
}

#[tokio::test]
async fn reconcile_drift_alert_high_severity_emits_escalated_condition_in_status_patch() {
    let mut alert = drift_alert("alert-8", NS, "mc-5", DriftSeverity::Critical);
    alert
        .owner_references_mut()
        .push(machine_config_owner_ref("mc-5"));

    let mc = machine_config("mc-5", NS);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", machine_config_path(NS, "mc-5")))
                .returning_json(&mc),
            expect_event_post(NS),
            ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-8")))
                .returning_json(&alert),
        ],
        machines(vec![mc.clone()]),
    );

    let _ = reconcile_drift_alert(Arc::new(alert), ctx).await.unwrap();
    let report = harness.finish().await;

    let da_status_body = report.captured[2].body_json();
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

/// The gateway creates every alert in its own namespace and names the
/// machine's namespace in the reference, so the alert reads and patches the
/// machine there.
#[tokio::test]
async fn reconcile_drift_alert_reads_and_patches_a_machine_in_another_namespace() {
    let mut alert = drift_alert("alert-cross", NS, "mc-team", DriftSeverity::Medium);
    alert.spec.machine_config_ref.namespace = Some("team-a".to_string());
    alert
        .owner_references_mut()
        .push(machine_config_owner_ref("mc-team"));

    let mc = machine_config("mc-team", "team-a");

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!(
                "{}/status",
                machine_config_path("team-a", "mc-team")
            ))
            .returning_json(&mc),
            expect_event_post("team-a"),
            ExpectedCall::patch_status(format!("{}/status", drift_alert_path(NS, "alert-cross")))
                .returning_json(&alert),
        ],
        machines(vec![mc.clone()]),
    );

    reconcile_drift_alert(Arc::new(alert), ctx)
        .await
        .expect("the machine in the referenced namespace is found");

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 3);
}

/// The alert's own status patch re-runs it, and the machine cache can still
/// predate the DriftDetected patch that went out with it. A detection the
/// alert already records is not reported a second time.
#[tokio::test]
async fn reconcile_drift_alert_does_not_report_a_detection_it_already_recorded() {
    let mut alert = drift_alert("alert-seen", NS, "mc-6", DriftSeverity::Medium);
    alert
        .owner_references_mut()
        .push(machine_config_owner_ref("mc-6"));
    alert.status = Some(crate::crds::DriftAlertStatus {
        detected_at: Some("2026-01-01T00:00:00Z".to_string()),
        resolved_at: None,
        conditions: vec![],
    });

    let mc = machine_config("mc-6", NS);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], machines(vec![mc]));

    reconcile_drift_alert(Arc::new(alert), ctx.clone())
        .await
        .expect("an already reported alert reconciles cleanly");

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "no machine patch, event or alert patch, got {} calls",
        report.captured.len()
    );
    let drift_count = ctx
        .metrics
        .drift_events_total
        .get_or_create(&crate::metrics::DriftLabels {
            severity: format!("{:?}", DriftSeverity::Medium),
            namespace: NS.to_string(),
        })
        .get();
    assert_eq!(drift_count, 0, "the detection is counted once");
}

// -----------------------------------------------------------------------
// has_active_drift_alerts
// -----------------------------------------------------------------------

#[tokio::test]
async fn has_active_drift_alerts_returns_true_when_alert_matches() {
    let alert = drift_alert("alert-active", NS, "mc-x", DriftSeverity::Low);
    let stores = ControllerStores {
        drift_alerts: seeded_store(vec![alert]),
        ..empty_stores()
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], stores);

    let active = has_active_drift_alerts(&ctx.stores, NS, "mc-x")
        .await
        .expect("a populated cache answers");
    assert!(active);

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "the drift check reads the watch cache — it must make no API call"
    );
}

#[tokio::test]
async fn has_active_drift_alerts_returns_false_when_no_match() {
    let alert = drift_alert("alert-other", NS, "different-mc", DriftSeverity::Low);
    let stores = ControllerStores {
        drift_alerts: seeded_store(vec![alert]),
        ..empty_stores()
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], stores);

    let active = has_active_drift_alerts(&ctx.stores, NS, "mc-x")
        .await
        .expect("a populated cache answers");
    assert!(!active);

    let _ = harness.finish().await;
}

#[tokio::test]
async fn has_active_drift_alerts_ignores_alerts_in_another_namespace() {
    let alert = drift_alert("alert-elsewhere", "other-ns", "mc-x", DriftSeverity::Low);
    let stores = ControllerStores {
        drift_alerts: seeded_store(vec![alert]),
        ..empty_stores()
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], stores);

    let active = has_active_drift_alerts(&ctx.stores, NS, "mc-x")
        .await
        .expect("a populated cache answers");
    assert!(
        !active,
        "an alert naming no namespace names a machine in its own namespace"
    );

    let _ = harness.finish().await;
}

/// An alert in the gateway's namespace naming a machine in another one is
/// that machine's alert, matched by the same target the alert controller and
/// the watch resolve.
#[tokio::test]
async fn has_active_drift_alerts_finds_an_alert_naming_the_machine_from_another_namespace() {
    let mut alert = drift_alert("alert-cross", "cfgd-gateway", "mc-x", DriftSeverity::Low);
    alert.spec.machine_config_ref.namespace = Some(NS.to_string());
    let stores = ControllerStores {
        drift_alerts: seeded_store(vec![alert]),
        ..empty_stores()
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], stores);

    assert!(
        has_active_drift_alerts(&ctx.stores, NS, "mc-x")
            .await
            .expect("a populated cache answers")
    );
    assert!(
        !has_active_drift_alerts(&ctx.stores, "cfgd-gateway", "mc-x")
            .await
            .expect("a populated cache answers"),
        "the alert's own namespace is not its machine's"
    );

    let _ = harness.finish().await;
}

/// The old LIST-based check answered `false` when the API call failed, which
/// clears the DriftDetected condition on evidence the operator never had. A
/// cache that has not completed its initial list is that same "no evidence"
/// state, and must requeue instead of answering.
#[tokio::test(start_paused = true)]
async fn has_active_drift_alerts_errors_when_the_cache_is_not_populated() {
    // The writer is held for the length of the assertion: dropping it would
    // resolve the wait with `WriterDropped` instead of timing out.
    let (drift_alerts, _writer) = unready_store();
    let stores = ControllerStores {
        drift_alerts,
        ..empty_stores()
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], stores);

    let err = has_active_drift_alerts(&ctx.stores, NS, "mc-x")
        .await
        .expect_err("an unpopulated cache must not answer 'no drift'");
    assert!(
        err.to_string().contains("DriftAlert watch cache"),
        "error must name the cache that was not ready: {err}"
    );

    let _ = harness.finish().await;
}

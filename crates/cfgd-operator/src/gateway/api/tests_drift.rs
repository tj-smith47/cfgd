//! Tests for the gateway drift submodule.
//!
//! Drives `create_drift_alert_crd` and `find_machine_config_for_device`
//! through the shared `MockKubeHarness` (promoted to `pub(crate)` in
//! `controllers/test_kube_harness.rs`). Each test queues the exact kube
//! API call sequence and asserts on the captured request bodies.
#![cfg(test)]

use http::Method;

use super::drift::{create_drift_alert_crd, find_machine_config_for_device};
use super::*;
use crate::controllers::test_fixtures::mc_list;
use crate::controllers::test_kube_harness::{ExpectedCall, MockKubeHarness};
use crate::crds::{
    DriftAlert, DriftAlertSpec, DriftDetail, DriftSeverity, MachineConfig, MachineConfigReference,
    MachineConfigSpec,
};

const HOSTNAME: &str = "host-prod";
const DEVICE_ID: &str = "dev-001";
const TIMESTAMP: &str = "2026-05-08T12:34:56Z";

// "default" comes from the harness `Client::new(mock_service, "default")`.
fn drift_alerts_create_path() -> &'static str {
    "/apis/cfgd.io/v1alpha1/namespaces/default/driftalerts"
}

fn machine_configs_list_path() -> &'static str {
    "/apis/cfgd.io/v1alpha1/machineconfigs"
}

fn mc_for(name: &str, hostname: &str) -> MachineConfig {
    MachineConfig {
        metadata: kube::api::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some("default".to_string()),
            uid: Some(format!("uid-{name}")),
            generation: Some(1),
            ..Default::default()
        },
        spec: MachineConfigSpec {
            hostname: hostname.to_string(),
            profile: "default".to_string(),
            module_refs: vec![],
            packages: vec![],
            files: vec![],
            system_settings: Default::default(),
        },
        status: None,
    }
}

fn detail() -> DriftDetailInput {
    DriftDetailInput {
        field: "packages.kubectl".to_string(),
        expected: "1.28".to_string(),
        actual: "1.27".to_string(),
    }
}

// Stripped timestamp used in alert name (replaces ':', '-', 'T', 'Z').
fn stripped_ts() -> String {
    cfgd_core::iso8601_to_filename_safe(TIMESTAMP)
}

fn expected_alert_name() -> String {
    format!("drift-{}-{}", DEVICE_ID, stripped_ts())
}

/// The API server echoes the created object back, and the client deserializes
/// it. A stub body missing `spec` fails to deserialize, which sends the create
/// path down its unparsable-body arm — so a test using one exercises that arm
/// rather than the ordinary success it is named for.
fn created_alert_response(mc_ref: &str) -> DriftAlert {
    let mut alert = DriftAlert::new(
        &expected_alert_name(),
        DriftAlertSpec {
            device_id: DEVICE_ID.to_string(),
            machine_config_ref: MachineConfigReference {
                name: mc_ref.to_string(),
                namespace: None,
            },
            drift_details: vec![DriftDetail {
                field: detail().field,
                expected: detail().expected,
                actual: detail().actual,
            }],
            severity: DriftSeverity::Medium,
        },
    );
    alert.metadata.namespace = Some("default".to_string());
    alert
}

// -----------------------------------------------------------------------
// create_drift_alert_crd
// -----------------------------------------------------------------------

#[tokio::test]
async fn create_drift_alert_crd_uses_matched_machine_config_name_for_label_and_ref() {
    let mc = mc_for("mc-prod", HOSTNAME);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. find_machine_config_for_device LIST — one MachineConfig with matching hostname.
        ExpectedCall::list(machine_configs_list_path())
            .returning_json(&mc_list(std::slice::from_ref(&mc))),
        // 2. POST DriftAlert — accept (201).
        ExpectedCall::post(drift_alerts_create_path())
            .with_status(201)
            .returning_json(&created_alert_response("mc-prod")),
    ]);

    create_drift_alert_crd(&ctx.client, DEVICE_ID, HOSTNAME, &[detail()], TIMESTAMP)
        .await
        .expect("happy path returns Ok");

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 2);

    let post = report
        .find(Method::POST, "/driftalerts")
        .expect("POST DriftAlert must have been captured");
    let body = post.body_json();

    assert_eq!(
        body["metadata"]["name"],
        expected_alert_name(),
        "alert name must be deterministic from device_id + stripped timestamp"
    );
    assert_eq!(
        body["spec"]["machineConfigRef"]["name"], "mc-prod",
        "machineConfigRef must point at the matched MachineConfig name"
    );
    assert_eq!(
        body["metadata"]["labels"][cfgd_core::LABEL_MACHINE_CONFIG],
        "mc-prod",
        "label cfgd.io/machine-config must equal the matched MC name"
    );
    assert_eq!(
        body["metadata"]["labels"][cfgd_core::LABEL_DEVICE_ID],
        DEVICE_ID,
        "label cfgd.io/device-id must equal the device_id"
    );
    assert_eq!(
        body["spec"]["deviceId"], DEVICE_ID,
        "spec.deviceId must equal the device_id"
    );
    assert_eq!(
        body["spec"]["severity"], "Medium",
        "default severity is Medium"
    );
    assert_eq!(
        body["spec"]["driftDetails"][0]["field"], "packages.kubectl",
        "drift detail field is preserved verbatim"
    );
}

#[tokio::test]
async fn create_drift_alert_crd_falls_back_to_synthetic_name_when_no_mc_matches() {
    // No matching MachineConfig — list returns one MC with a *different* hostname.
    let mc = mc_for("mc-other", "host-other");
    let synthetic = format!("{HOSTNAME}-mc");

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_list_path()).returning_json(&mc_list(&[mc])),
        ExpectedCall::post(drift_alerts_create_path())
            .with_status(201)
            .returning_json(&created_alert_response(&synthetic)),
    ]);

    create_drift_alert_crd(&ctx.client, DEVICE_ID, HOSTNAME, &[detail()], TIMESTAMP)
        .await
        .expect("synthetic-name fallback path returns Ok");

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 2);

    let post = report.find(Method::POST, "/driftalerts").unwrap();
    let body = post.body_json();
    assert_eq!(
        body["spec"]["machineConfigRef"]["name"], synthetic,
        "machineConfigRef must use synthetic `<hostname>-mc` when no MC matches"
    );
    assert_eq!(
        body["metadata"]["labels"][cfgd_core::LABEL_MACHINE_CONFIG],
        synthetic,
        "label cfgd.io/machine-config must mirror the synthetic ref"
    );
}

#[tokio::test]
async fn create_drift_alert_crd_treats_409_conflict_as_already_exists_no_retry() {
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_list_path()).returning_json(&mc_list(&[])),
        // First (and only) POST attempt returns 409 Conflict — function exits early.
        ExpectedCall::post(drift_alerts_create_path()).returning_server_error(409, "AlreadyExists"),
    ]);

    create_drift_alert_crd(&ctx.client, DEVICE_ID, HOSTNAME, &[detail()], TIMESTAMP)
        .await
        .expect("409 is treated as Ok, not Err");

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        2,
        "409 must short-circuit retry — exactly one POST attempt"
    );
}

/// A 2xx whose body does not deserialize means the object was created and only
/// the server's echo of it is unreadable. Retrying would POST a second time and
/// take a 409 to learn what the first response already said.
#[tokio::test(start_paused = true)]
async fn create_drift_alert_crd_does_not_retry_a_created_alert_with_an_unparsable_body() {
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_list_path()).returning_json(&mc_list(&[])),
        // 201, but the body is not a DriftAlert — the client cannot parse it.
        ExpectedCall::post(drift_alerts_create_path())
            .with_status(201)
            .returning_json(&serde_json::json!({ "kind": "DriftAlert" })),
    ]);

    create_drift_alert_crd(&ctx.client, DEVICE_ID, HOSTNAME, &[detail()], TIMESTAMP)
        .await
        .expect("a created alert is Ok even when its echo cannot be parsed");

    let report = harness.finish().await;
    let posts: Vec<_> = report
        .captured
        .iter()
        .filter(|r| r.method == Method::POST && r.path.ends_with("/driftalerts"))
        .collect();
    assert_eq!(
        posts.len(),
        1,
        "an unparsable 2xx body is success, not a failed create to retry"
    );
}

#[tokio::test(start_paused = true)]
async fn create_drift_alert_crd_retries_on_server_error_then_succeeds() {
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_list_path()).returning_json(&mc_list(&[])),
        // Attempt 1 → 500 → retried (with virtual time advance via `start_paused`).
        ExpectedCall::post(drift_alerts_create_path()).returning_server_error(500, "etcd"),
        // Attempt 2 → 201 → success.
        ExpectedCall::post(drift_alerts_create_path())
            .with_status(201)
            .returning_json(&created_alert_response(&format!("{HOSTNAME}-mc"))),
    ]);

    create_drift_alert_crd(&ctx.client, DEVICE_ID, HOSTNAME, &[detail()], TIMESTAMP)
        .await
        .expect("retry path returns Ok after second attempt succeeds");

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        3,
        "must retry once after the first 5xx"
    );

    let posts: Vec<_> = report
        .captured
        .iter()
        .filter(|r| r.method == Method::POST && r.path.ends_with("/driftalerts"))
        .collect();
    assert_eq!(posts.len(), 2, "exactly two POSTs (original + one retry)");
}

// -----------------------------------------------------------------------
// find_machine_config_for_device — direct
// -----------------------------------------------------------------------

#[tokio::test]
async fn find_machine_config_for_device_returns_synthetic_name_when_list_fails() {
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(machine_configs_list_path())
            .returning_server_error(500, "kube apiserver melted"),
    ]);

    let resolved = find_machine_config_for_device(&ctx.client, HOSTNAME).await;

    assert_eq!(
        resolved,
        format!("{HOSTNAME}-mc"),
        "list errors must fall through to the synthetic `<hostname>-mc` name"
    );

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 1, "exactly one LIST attempted");
}

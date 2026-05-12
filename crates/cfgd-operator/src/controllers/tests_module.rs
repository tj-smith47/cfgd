//! Reconcile-fn tests for `controllers/module.rs`.
//!
//! `reconcile_module` evaluates Module availability (against
//! `ClusterConfigPolicy.security`) and signature verification, then
//! patches the Module's `/status` and emits Available/Verified events.
#![cfg(test)]

use std::sync::Arc;

use kube::runtime::controller::Action;

use super::module::{evaluate_module_verification, reconcile_module};
use super::test_kube_harness::{ExpectedCall, MockKubeHarness, expect_event_post};
use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicySpec, CosignSignature, Module, ModuleSignature,
    ModuleSpec, SecurityPolicy,
};
use crate::metrics::ReconcileLabels;

const VALID_PEM: &str = concat!(
    "-----BEGIN PUBLIC KEY-----\n",
    "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAExjj1ywH6tT2hUDhWGv7zL3y2zWpf\n",
    "+0LiNz39c6T1eD/3gG2sWrgtHfJV4WbzZX1L1Lz8gQXn49fTxV5J7G5XHQ==\n",
    "-----END PUBLIC KEY-----\n",
);

fn module_path(name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/modules/{name}")
}

fn cluster_config_policies_path() -> &'static str {
    "/apis/cfgd.io/v1alpha1/clusterconfigpolicies"
}

fn ccp_list(items: &[ClusterConfigPolicy]) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "ClusterConfigPolicyList",
        "items": items,
        "metadata": {},
    })
}

fn make_module(name: &str, spec: ModuleSpec) -> Module {
    Module {
        metadata: kube::api::ObjectMeta {
            name: Some(name.to_string()),
            uid: Some(format!("uid-{name}")),
            generation: Some(1),
            ..Default::default()
        },
        spec,
        status: None,
    }
}

// -----------------------------------------------------------------------
// reconcile_module — happy paths
// -----------------------------------------------------------------------

#[tokio::test]
async fn reconcile_module_with_no_artifact_records_local_only_status_with_keyless_signature() {
    let spec = ModuleSpec {
        signature: Some(ModuleSignature {
            cosign: Some(CosignSignature {
                keyless: true,
                certificate_identity: Some("https://github.com/example/.*".to_string()),
                certificate_oidc_issuer: Some(
                    "https://token.actions.githubusercontent.com".to_string(),
                ),
                ..Default::default()
            }),
        }),
        ..Default::default()
    };
    let module = make_module("local-mod", spec);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // No LIST CCPs because oci_artifact is None — short-circuits.
        // 1. PATCH /status
        ExpectedCall::patch_status(format!("{}/status", module_path("local-mod")))
            .returning_json(&module),
        // 2. POST event for Available (Normal, "Available")
        expect_event_post("default"),
        // 3. POST event for Verified (Normal, "Verified")
        expect_event_post("default"),
    ]);

    let action = reconcile_module(Arc::new(module), ctx.clone())
        .await
        .unwrap();
    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 3);

    let status_body = report.captured[0].body_json();
    let conditions = status_body["status"]["conditions"]
        .as_array()
        .expect("conditions");
    let available = conditions
        .iter()
        .find(|c| c["type"] == "Available")
        .unwrap();
    assert_eq!(available["status"], "True");
    assert_eq!(available["reason"], "NoArtifact");

    let verified = conditions.iter().find(|c| c["type"] == "Verified").unwrap();
    assert_eq!(verified["status"], "True");
    assert_eq!(verified["reason"], "SignatureConfigured");
    assert_eq!(status_body["status"]["verified"], true);
    assert!(
        status_body["status"]["signatureDigest"]
            .as_str()
            .unwrap_or("")
            .starts_with("keyless:")
    );

    let success = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "module".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(success, 1);
}

#[tokio::test]
async fn reconcile_module_with_artifact_lists_cluster_config_policies_and_records_available() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let module = make_module("ghcr-mod", spec);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // 1. LIST ClusterConfigPolicies (no policies — fail-open path).
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(&[])),
        // 2. PATCH /status
        ExpectedCall::patch_status(format!("{}/status", module_path("ghcr-mod")))
            .returning_json(&module),
        // 3. Available event
        expect_event_post("default"),
        // 4. Verified event (status=False because no signature)
        expect_event_post("default"),
    ]);

    reconcile_module(Arc::new(module), ctx).await.unwrap();
    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 4);

    let status_body = report.captured[1].body_json();
    let conditions = status_body["status"]["conditions"].as_array().unwrap();
    let available = conditions
        .iter()
        .find(|c| c["type"] == "Available")
        .unwrap();
    assert_eq!(available["status"], "True");
    assert_eq!(available["reason"], "ArtifactAvailable");

    let verified = conditions.iter().find(|c| c["type"] == "Verified").unwrap();
    assert_eq!(verified["status"], "False");
    assert_eq!(verified["reason"], "NotSigned");
}

#[tokio::test]
async fn reconcile_module_with_invalid_oci_reference_records_invalid_reference() {
    let spec = ModuleSpec {
        oci_artifact: Some("definitely not a valid oci ref".to_string()),
        ..Default::default()
    };
    let module = make_module("bad-ref", spec);

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // No LIST: invalid reference short-circuits before policy lookup.
        ExpectedCall::patch_status(format!("{}/status", module_path("bad-ref")))
            .returning_json(&module),
        expect_event_post("default"), // Available (false, PullFailed)
        expect_event_post("default"), // Verified
    ]);

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[0].body_json();
    let conditions = status_body["status"]["conditions"].as_array().unwrap();
    let available = conditions
        .iter()
        .find(|c| c["type"] == "Available")
        .unwrap();
    assert_eq!(available["status"], "False");
    assert_eq!(available["reason"], "InvalidReference");
}

#[tokio::test]
async fn reconcile_module_with_unsigned_disallowed_and_no_signature_records_violation() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let module = make_module("unsigned-mod", spec);

    let ccp_spec = ClusterConfigPolicySpec {
        security: SecurityPolicy {
            trusted_registries: vec![],
            allow_unsigned: false,
        },
        ..Default::default()
    };
    let ccp = ClusterConfigPolicy {
        metadata: kube::api::ObjectMeta {
            name: Some("strict".to_string()),
            uid: Some("uid-strict".to_string()),
            ..Default::default()
        },
        spec: ccp_spec,
        status: None,
    };

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(&[ccp])),
        ExpectedCall::patch_status(format!("{}/status", module_path("unsigned-mod")))
            .returning_json(&module),
        expect_event_post("default"), // Available=False
        expect_event_post("default"), // Verified
    ]);

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[1].body_json();
    let available = status_body["status"]["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Available")
        .unwrap()
        .clone();
    assert_eq!(available["status"], "False");
    assert_eq!(available["reason"], "UnsignedNotAllowed");
}

#[tokio::test]
async fn reconcile_module_with_trusted_registry_violation_records_status() {
    let spec = ModuleSpec {
        oci_artifact: Some("untrusted.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let module = make_module("untrusted-mod", spec);

    let ccp_spec = ClusterConfigPolicySpec {
        security: SecurityPolicy {
            trusted_registries: vec!["ghcr.io/*".to_string()],
            allow_unsigned: true,
        },
        ..Default::default()
    };
    let ccp = ClusterConfigPolicy {
        metadata: kube::api::ObjectMeta {
            name: Some("trusted".to_string()),
            uid: Some("uid-trusted".to_string()),
            ..Default::default()
        },
        spec: ccp_spec,
        status: None,
    };

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(&[ccp])),
        ExpectedCall::patch_status(format!("{}/status", module_path("untrusted-mod")))
            .returning_json(&module),
        expect_event_post("default"),
        expect_event_post("default"),
    ]);

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[1].body_json();
    let available = status_body["status"]["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Available")
        .unwrap()
        .clone();
    assert_eq!(available["status"], "False");
    assert_eq!(available["reason"], "TrustedRegistryViolation");
}

#[tokio::test]
async fn reconcile_module_status_patch_failure_propagates_as_error() {
    let module = make_module("statuserr-mod", ModuleSpec::default());

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::patch_status(format!("{}/status", module_path("statuserr-mod")))
            .returning_server_error(500, "etcd"),
    ]);

    let result = reconcile_module(Arc::new(module), ctx).await;
    let err = result.expect_err("status PATCH failure must propagate");
    assert!(
        err.to_string().contains("failed to update Module status"),
        "{err}"
    );

    let _ = harness.finish().await;
}

// -----------------------------------------------------------------------
// evaluate_module_verification — pure-fn tests (no harness needed)
// -----------------------------------------------------------------------

#[test]
fn evaluate_module_verification_returns_not_signed_when_signature_absent() {
    let r = evaluate_module_verification(&None);
    assert_eq!(r.status, "False");
    assert_eq!(r.reason, "NotSigned");
    assert!(r.signature_digest.is_none());
}

#[test]
fn evaluate_module_verification_returns_not_signed_when_cosign_absent() {
    let sig = ModuleSignature { cosign: None };
    let r = evaluate_module_verification(&Some(sig));
    assert_eq!(r.status, "False");
    assert_eq!(r.reason, "NotSigned");
}

#[test]
fn evaluate_module_verification_returns_signature_invalid_when_pem_garbage() {
    let sig = ModuleSignature {
        cosign: Some(CosignSignature {
            public_key: Some("not pem".to_string()),
            keyless: false,
            ..Default::default()
        }),
    };
    let r = evaluate_module_verification(&Some(sig));
    assert_eq!(r.status, "False");
    assert_eq!(r.reason, "SignatureInvalid");
}

#[test]
fn evaluate_module_verification_returns_signature_invalid_when_no_key_and_not_keyless() {
    let sig = ModuleSignature {
        cosign: Some(CosignSignature {
            public_key: None,
            keyless: false,
            ..Default::default()
        }),
    };
    let r = evaluate_module_verification(&Some(sig));
    assert_eq!(r.status, "False");
    assert_eq!(r.reason, "SignatureInvalid");
}

#[test]
fn evaluate_module_verification_returns_configured_when_valid_pem_provided() {
    let sig = ModuleSignature {
        cosign: Some(CosignSignature {
            public_key: Some(VALID_PEM.to_string()),
            keyless: false,
            ..Default::default()
        }),
    };
    let r = evaluate_module_verification(&Some(sig));
    assert_eq!(r.status, "True");
    assert_eq!(r.reason, "SignatureConfigured");
    assert!(r.signature_digest.is_some());
    assert!(r.signature_digest.unwrap().starts_with("sha256:"));
}

#[test]
fn evaluate_module_verification_keyless_with_explicit_identity_records_descriptor() {
    let sig = ModuleSignature {
        cosign: Some(CosignSignature {
            keyless: true,
            certificate_identity: Some("user@example.com".to_string()),
            certificate_oidc_issuer: Some("https://accounts.google.com".to_string()),
            ..Default::default()
        }),
    };
    let r = evaluate_module_verification(&Some(sig));
    assert_eq!(r.status, "True");
    let digest = r.signature_digest.unwrap();
    assert!(digest.contains("user@example.com"));
    assert!(digest.contains("accounts.google.com"));
}

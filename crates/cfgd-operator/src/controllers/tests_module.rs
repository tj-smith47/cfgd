//! Reconcile-fn tests for `controllers/module.rs`.
//!
//! `reconcile_module` evaluates Module availability (against
//! `ClusterConfigPolicy.security`) and signature verification, then
//! patches the Module's `/status` and emits Available/Verified events.
#![cfg(test)]

use std::sync::Arc;

use kube::runtime::controller::Action;

use super::module::{evaluate_module_verification, reconcile_module};
use super::test_kube_harness::{
    ExpectedCall, MockKubeHarness, empty_stores, expect_event_post, seeded_store,
};
use super::{ArtifactFactsReader, ControllerStores};
use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicySpec, CosignSignature, Module, ModuleSignature,
    ModuleSpec, ModuleStatus, SecurityPolicy,
};
use crate::metrics::ReconcileLabels;
use cfgd_core::oci::ArtifactFacts;

const VALID_PEM: &str = concat!(
    "-----BEGIN PUBLIC KEY-----\n",
    "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAExjj1ywH6tT2hUDhWGv7zL3y2zWpf\n",
    "+0LiNz39c6T1eD/3gG2sWrgtHfJV4WbzZX1L1Lz8gQXn49fTxV5J7G5XHQ==\n",
    "-----END PUBLIC KEY-----\n",
);

fn module_path(name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/modules/{name}")
}

fn stores_with_ccps(policies: Vec<ClusterConfigPolicy>) -> ControllerStores {
    ControllerStores {
        cluster_config_policies: seeded_store(policies),
        ..empty_stores()
    }
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
async fn reconcile_module_reads_cluster_config_policies_from_cache_and_records_available() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let module = make_module("ghcr-mod", spec);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            // 1. PATCH /status — the policy read is served by the cache, not a LIST.
            ExpectedCall::patch_status(format!("{}/status", module_path("ghcr-mod")))
                .returning_json(&module),
            // 2. Available event
            expect_event_post("default"),
            // 3. Verified event (status=False because no signature)
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
    );

    reconcile_module(Arc::new(module), ctx).await.unwrap();
    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        3,
        "the ClusterConfigPolicy read must cost no API call"
    );

    let status_body = report.captured[0].body_json();
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
async fn reconcile_module_records_the_platforms_its_artifact_declares() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let module = make_module("platform-mod", spec);

    let (ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("platform-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        ArtifactFactsReader::fixed(ArtifactFacts {
            platforms: vec!["linux/amd64".to_string()],
            ..Default::default()
        }),
    );

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[0].body_json();
    assert_eq!(
        status_body["status"]["availablePlatforms"],
        serde_json::json!(["linux/amd64"]),
    );
    assert_eq!(status_body["status"]["platformsSummary"], "linux/amd64");
}

#[tokio::test]
async fn reconcile_module_records_the_attestations_its_artifact_carries() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let module = make_module("attested-mod", spec);

    let (ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("attested-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        ArtifactFactsReader::fixed(ArtifactFacts {
            platforms: vec!["linux/amd64".to_string()],
            attestations: vec!["slsaprovenance1".to_string()],
        }),
    );

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[0].body_json();
    assert_eq!(
        status_body["status"]["attestations"],
        serde_json::json!(["slsaprovenance1"]),
    );
}

#[tokio::test]
async fn reconcile_module_records_no_attestation_for_an_unattested_artifact() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let module = make_module("unattested-mod", spec);

    let (ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("unattested-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        ArtifactFactsReader::fixed(ArtifactFacts {
            platforms: vec!["linux/amd64".to_string()],
            ..Default::default()
        }),
    );

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[0].body_json();
    assert!(
        status_body["status"]["attestations"].is_null(),
        "an empty attestation list is omitted, not written as []: {}",
        status_body["status"]
    );
}

#[tokio::test]
async fn reconcile_module_keeps_recorded_attestations_when_the_artifact_is_unchanged() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let mut module = make_module("cached-attestation-mod", spec);
    module.status = Some(ModuleStatus {
        resolved_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        attestations: vec!["spdx".to_string()],
        ..Default::default()
    });

    let (ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("cached-attestation-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        // A reader that would answer differently, so a re-read is visible.
        ArtifactFactsReader::fixed(ArtifactFacts {
            platforms: vec!["linux/amd64".to_string()],
            attestations: vec!["slsaprovenance1".to_string()],
        }),
    );

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[0].body_json();
    assert_eq!(
        status_body["status"]["attestations"],
        serde_json::json!(["spdx"]),
        "an unchanged artifact reference must cost no registry round-trip"
    );
    let platforms = &status_body["status"]["availablePlatforms"];
    assert!(
        platforms.is_null() || platforms.as_array().is_some_and(Vec::is_empty),
        "the recorded read answered both facts: neither half may come from a second \
         visit, so a status recording attestations and no platform keeps both: {platforms}"
    );
}

#[tokio::test]
async fn reconcile_module_keeps_recorded_platforms_when_the_artifact_is_unchanged() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let mut module = make_module("cached-platform-mod", spec);
    module.status = Some(ModuleStatus {
        resolved_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        available_platforms: vec!["linux/arm64".to_string()],
        ..Default::default()
    });

    let (ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("cached-platform-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        // A reader that would answer differently, so a re-read is visible.
        ArtifactFactsReader::fixed(ArtifactFacts {
            platforms: vec!["linux/amd64".to_string()],
            ..Default::default()
        }),
    );

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[0].body_json();
    assert_eq!(
        status_body["status"]["availablePlatforms"],
        serde_json::json!(["linux/arm64"]),
        "an unchanged artifact reference must cost no registry round-trip"
    );
}

#[tokio::test]
async fn reconcile_module_records_the_signature_verdict_as_one_word() {
    let signed_spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        signature: Some(ModuleSignature {
            cosign: Some(CosignSignature {
                public_key: Some(VALID_PEM.to_string()),
                ..Default::default()
            }),
        }),
        ..Default::default()
    };
    let signed = make_module("signed-mod", signed_spec);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("signed-mod")))
                .returning_json(&signed),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
    );
    reconcile_module(Arc::new(signed), ctx).await.unwrap();
    let report = harness.finish().await;
    assert_eq!(
        report.captured[0].body_json()["status"]["signature"],
        cfgd_crd::SIGNATURE_VERIFIED,
    );

    let unsigned_spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let unsigned = make_module("unsigned-verdict-mod", unsigned_spec);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("unsigned-verdict-mod")))
                .returning_json(&unsigned),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
    );
    reconcile_module(Arc::new(unsigned), ctx).await.unwrap();
    let report = harness.finish().await;
    assert_eq!(
        report.captured[0].body_json()["status"]["signature"],
        cfgd_crd::SIGNATURE_UNSIGNED,
    );
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("unsigned-mod")))
                .returning_json(&module),
            expect_event_post("default"), // Available=False
            expect_event_post("default"), // Verified
        ],
        stores_with_ccps(vec![ccp]),
    );

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[0].body_json();
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("untrusted-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![ccp]),
    );

    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    let status_body = report.captured[0].body_json();
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
// Patch-on-change / event-on-change
// -----------------------------------------------------------------------

/// Ten reconciles of a Module nobody touched write the status once and emit
/// the Available/Verified pair once: every later pass computes the same status
/// it already persisted and says nothing.
#[tokio::test]
async fn reconcile_module_repeated_reconciles_patch_status_once() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let mut module = make_module("steady-mod", spec);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("steady-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
    );
    reconcile_module(Arc::new(module.clone()), ctx)
        .await
        .unwrap();
    let first = harness.finish().await;
    assert_eq!(first.captured.len(), 3);

    module.status = Some(
        serde_json::from_value(first.captured[0].body_json()["status"].clone())
            .expect("status round-trips"),
    );

    for _ in 0..10 {
        let (ctx, _registry, harness) =
            MockKubeHarness::with_stores(vec![], stores_with_ccps(vec![]));
        reconcile_module(Arc::new(module.clone()), ctx)
            .await
            .unwrap();
        let report = harness.finish().await;
        assert!(
            report.captured.is_empty(),
            "an unchanged Module status must make no API call"
        );
    }
}

/// Every reconcile stamps the generation it read, and a spec edit is written
/// through even when every verdict comes out the same. Without the stamp a
/// reader has no way to tell a status describing the spec it just applied from
/// one describing the spec it replaced, and the equality short-circuit above
/// would keep the OLD status on a re-specced Module indefinitely.
#[tokio::test]
async fn reconcile_module_stamps_the_generation_its_status_describes() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let mut module = make_module("stamped-mod", spec);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("stamped-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
    );
    reconcile_module(Arc::new(module.clone()), ctx)
        .await
        .unwrap();
    let first = harness.finish().await;
    let status = first.captured[0].body_json()["status"].clone();
    assert_eq!(
        status["observedGeneration"], 1,
        "the status must name the generation it was computed from: {status}"
    );
    assert!(
        status.get("platformsSummary").is_none(),
        "no known platform must leave the column's field absent, so the cell \
         is empty rather than an empty list: {status}"
    );

    // The spec moves on. Every verdict is unchanged, so only the stamp
    // differs — and that alone must still be written.
    module.status = Some(serde_json::from_value(status).expect("status round-trips"));
    module.metadata.generation = Some(2);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("stamped-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
    );
    reconcile_module(Arc::new(module.clone()), ctx)
        .await
        .unwrap();
    let second = harness.finish().await;
    assert_eq!(
        second.captured[0].body_json()["status"]["observedGeneration"],
        2,
        "a re-specced Module must have its status re-stamped"
    );
}

/// A changed verdict is still announced: the same Module under a policy that
/// now forbids unsigned artifacts patches and re-emits.
#[tokio::test]
async fn reconcile_module_emits_again_when_the_verdict_changes() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let mut module = make_module("turning-mod", spec);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("turning-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
    );
    reconcile_module(Arc::new(module.clone()), ctx)
        .await
        .unwrap();
    let first = harness.finish().await;
    module.status = Some(
        serde_json::from_value(first.captured[0].body_json()["status"].clone())
            .expect("status round-trips"),
    );

    let strict = ClusterConfigPolicy {
        metadata: kube::api::ObjectMeta {
            name: Some("strict".to_string()),
            uid: Some("uid-strict".to_string()),
            ..Default::default()
        },
        spec: ClusterConfigPolicySpec {
            security: SecurityPolicy {
                trusted_registries: vec![],
                allow_unsigned: false,
            },
            ..Default::default()
        },
        status: None,
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("turning-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![strict]),
    );
    reconcile_module(Arc::new(module), ctx).await.unwrap();
    let second = harness.finish().await;
    assert_eq!(second.captured.len(), 3);
    let available = second.captured[0].body_json()["status"]["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Available")
        .unwrap()
        .clone();
    assert_eq!(available["reason"], "UnsignedNotAllowed");
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

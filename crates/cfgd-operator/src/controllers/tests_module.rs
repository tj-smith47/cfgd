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
    ExpectedCall, MockKubeHarness, empty_stores, expect_event_post, expect_event_series_patch,
    seeded_store,
};
use super::{ArtifactFactsReader, ArtifactVerifier, ControllerStores};
use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicySpec, CosignSignature, Module, ModuleSignature,
    ModuleSpec, ModuleStatus, SecurityPolicy,
};
use crate::metrics::ReconcileLabels;
use cfgd_core::oci::{ArtifactFacts, SignatureCheck};

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

    // A local-only module has no artifact to check a signature on, so the
    // keyless block it declares is a configuration and never a verdict.
    let verified = conditions.iter().find(|c| c["type"] == "Verified").unwrap();
    assert_eq!(verified["status"], "Unknown");
    assert_eq!(verified["reason"], "NoVerifiableArtifact");
    assert_eq!(status_body["status"]["verified"], false);
    assert_eq!(
        status_body["status"]["signature"],
        cfgd_crd::SIGNATURE_UNKNOWN,
    );
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

/// Reconcile `spec` with `check` installed as the only verifier, and return
/// the status the controller patched.
async fn status_under_verifier(
    name: &str,
    spec: ModuleSpec,
    check: SignatureCheck,
) -> serde_json::Value {
    status_under_policies(name, spec, check, vec![]).await
}

/// [`status_under_verifier`] with cluster policies in force.
async fn status_under_policies(
    name: &str,
    spec: ModuleSpec,
    check: SignatureCheck,
    policies: Vec<ClusterConfigPolicy>,
) -> serde_json::Value {
    let module = make_module(name, spec);
    let (ctx, _registry, harness) = MockKubeHarness::with_registry_seams(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path(name)))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(policies),
        ArtifactFactsReader::fixed(Default::default()),
        ArtifactVerifier::fixed(check),
    );
    reconcile_module(Arc::new(module), ctx).await.unwrap();
    harness.finish().await.captured[0].body_json()["status"].clone()
}

/// A cluster policy that admits only modules with a verified signature.
fn strict_ccp() -> ClusterConfigPolicy {
    ClusterConfigPolicy {
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
    }
}

/// The one sentence the withholding gate writes into the Available condition.
/// Pinned as the literal, because the reason code alone cannot say WHICH gate
/// produced the verdict.
const WITHHELD_MESSAGE: &str =
    "Module signature did not verify but unsigned modules are not allowed";

fn signed_spec() -> ModuleSpec {
    ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        signature: Some(ModuleSignature {
            cosign: Some(CosignSignature {
                public_key: Some(VALID_PEM.to_string()),
                ..Default::default()
            }),
        }),
        ..Default::default()
    }
}

fn condition(status: &serde_json::Value, kind: &str) -> serde_json::Value {
    status["conditions"]
        .as_array()
        .expect("conditions")
        .iter()
        .find(|c| c["type"] == kind)
        .unwrap_or_else(|| panic!("no {kind} condition"))
        .clone()
}

/// A configured key is not a checked signature. The word the whole cluster
/// reads comes from the verifier and from nothing else, so an artifact the
/// verifier rejects — including one carrying no signature at all — can never
/// reconcile to `verified`.
#[tokio::test]
async fn a_rejected_artifact_never_reconciles_to_verified() {
    let status = status_under_verifier(
        "rejected-mod",
        signed_spec(),
        SignatureCheck::Rejected("no matching signatures".to_string()),
    )
    .await;

    assert_eq!(status["signature"], cfgd_crd::SIGNATURE_UNVERIFIED);
    assert_eq!(status["verified"], false);
    let verified = condition(&status, "Verified");
    assert_eq!(verified["status"], "False");
    assert_eq!(verified["reason"], "SignatureInvalid");
    assert!(
        verified["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no matching signatures"),
        "the condition must carry what the verifier actually said, got {verified:?}"
    );
}

/// `verified` is reachable, and only through a verifier that accepted the
/// artifact.
#[tokio::test]
async fn an_accepted_artifact_reconciles_to_verified() {
    let status = status_under_verifier("verified-mod", signed_spec(), SignatureCheck::Valid).await;

    assert_eq!(status["signature"], cfgd_crd::SIGNATURE_VERIFIED);
    assert_eq!(status["verified"], true);
    let verified = condition(&status, "Verified");
    assert_eq!(verified["status"], "True");
    assert_eq!(verified["reason"], "SignatureVerified");
}

/// A check that could not run says so. `unknown` is neither `verified` (a
/// claim nothing checked) nor `unverified` (a claim about a signature nobody
/// looked at), and the condition is `Unknown` rather than `False`.
#[tokio::test]
async fn a_check_that_cannot_run_reconciles_to_unknown() {
    let status = status_under_verifier(
        "unknown-mod",
        signed_spec(),
        SignatureCheck::Undetermined("cosign is not installed".to_string()),
    )
    .await;

    assert_eq!(status["signature"], cfgd_crd::SIGNATURE_UNKNOWN);
    assert_eq!(status["verified"], false);
    let verified = condition(&status, "Verified");
    assert_eq!(verified["status"], "Unknown");
    assert_eq!(verified["reason"], "VerificationUnavailable");
    assert!(
        verified["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cosign is not installed"),
        "the reason the check could not run must reach the condition, got {verified:?}"
    );
}

/// A verifier that accepts everything still cannot make an unsigned module
/// `verified`: nothing is checked for a spec that declares no signature.
#[tokio::test]
async fn a_module_declaring_no_signature_stays_unsigned() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let status = status_under_verifier("undeclared-mod", spec, SignatureCheck::Valid).await;

    assert_eq!(status["signature"], cfgd_crd::SIGNATURE_UNSIGNED);
    assert_eq!(status["verified"], false);
    assert_eq!(condition(&status, "Verified")["reason"], "NotSigned");
}

/// Every component that reaches a module's registry exposes the same registry
/// configuration surface in the chart.
///
/// The operator reads each Module's platforms, attestations and signature; the
/// CSI driver pulls the layers it mounts; the agent pulls the modules it
/// applies. A component whose deployment takes no `extraEnv` is one nobody can
/// point at a private or plain-HTTP registry — which is how the operator came
/// to render a blank `PLATFORMS` column in a cluster whose CSI driver mounted
/// the same artifact happily.
#[test]
fn every_registry_reading_component_exposes_the_same_registry_knob() {
    const TEMPLATES: &[(&str, &str)] = &[
        (
            "operator",
            include_str!("../../../../chart/cfgd/templates/operator-deployment.yaml"),
        ),
        (
            "csiDriver",
            include_str!("../../../../chart/cfgd/templates/csi-daemonset.yaml"),
        ),
        (
            "agent",
            include_str!("../../../../chart/cfgd/templates/agent-daemonset.yaml"),
        ),
    ];
    let values: serde_json::Value =
        serde_yaml::from_str(include_str!("../../../../chart/cfgd/values.yaml"))
            .expect("chart values must parse");
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../../chart/cfgd/values.schema.json"))
            .expect("chart values schema must parse");

    for (component, template) in TEMPLATES {
        assert!(
            template.contains(&format!(".Values.{component}.extraEnv")),
            "the {component} template renders no extraEnv, so its registry cannot be configured"
        );
        assert!(
            !values[component]["extraEnv"].is_null(),
            "values.yaml declares no {component}.extraEnv default"
        );
        assert!(
            !schema["properties"][component]["properties"]["extraEnv"].is_null(),
            "values.schema.json declares no {component}.extraEnv, so a chart user gets no validation"
        );
    }
}

/// A registry that answered nothing is not asked again by the very next
/// reconcile. Every status patch and every event this controller writes lands
/// as a watch event that triggers another reconcile, so a read whose failure
/// is remembered nowhere is a registry round-trip per event, forever.
#[tokio::test]
async fn a_registry_that_answered_nothing_is_not_revisited_on_the_next_reconcile() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let visits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&visits);
    let reader = ArtifactFactsReader(Arc::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        ArtifactFacts::default()
    }));

    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let module = Arc::new(make_module("backoff-mod", spec));
    let status_path = format!("{}/status", module_path("backoff-mod"));

    let (ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(status_path.clone()).returning_json(&*module),
            expect_event_post("default"),
            expect_event_post("default"),
            ExpectedCall::patch_status(status_path).returning_json(&*module),
            // The second reconcile publishes the SAME two events, and kube's
            // recorder increments the existing series rather than posting a
            // second copy.
            expect_event_series_patch("default"),
            expect_event_series_patch("default"),
        ],
        stores_with_ccps(vec![]),
        reader,
    );

    reconcile_module(Arc::clone(&module), Arc::clone(&ctx))
        .await
        .unwrap();
    reconcile_module(module, ctx).await.unwrap();
    harness.finish().await;

    assert_eq!(
        visits.load(Ordering::SeqCst),
        1,
        "a fact read that answered nothing must not be repeated within the requeue window"
    );
}

#[tokio::test]
async fn reconcile_module_records_the_signature_verdict_as_one_word() {
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

/// A module declaring no signature is withheld under `allowUnsigned: false` —
/// and it is the VERDICT gate that withholds it. A verifier that would accept
/// anything is installed, so the only thing that can reach `UnsignedNotAllowed`
/// is the `unsigned` verdict the spec itself produced, carrying the verdict
/// gate's own sentence.
#[tokio::test]
async fn reconcile_module_with_unsigned_disallowed_and_no_signature_records_violation() {
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };
    let status = status_under_policies(
        "unsigned-mod",
        spec,
        SignatureCheck::Valid,
        vec![strict_ccp()],
    )
    .await;

    assert_eq!(status["signature"], cfgd_crd::SIGNATURE_UNSIGNED);
    assert_eq!(status["verified"], false);
    let available = condition(&status, "Available");
    assert_eq!(available["status"], "False");
    assert_eq!(available["reason"], "UnsignedNotAllowed");
    assert_eq!(
        available["message"], WITHHELD_MESSAGE,
        "the verdict gate must be what withheld the module, got {available:?}"
    );
}

/// The sibling on the other side of the same gate: a module that DOES declare a
/// key is withheld just the same when the verifier rejects its artifact. A
/// declared key is a promise of a signature, not a signature.
#[tokio::test]
async fn reconcile_module_with_unsigned_disallowed_and_a_rejected_signature_records_violation() {
    let status = status_under_policies(
        "rejected-strict-mod",
        signed_spec(),
        SignatureCheck::Rejected("no matching signatures".to_string()),
        vec![strict_ccp()],
    )
    .await;

    assert_eq!(status["signature"], cfgd_crd::SIGNATURE_UNVERIFIED);
    assert_eq!(status["verified"], false);
    let verified = condition(&status, "Verified");
    assert_eq!(verified["status"], "False");
    assert_eq!(verified["reason"], "SignatureInvalid");
    let available = condition(&status, "Available");
    assert_eq!(available["status"], "False");
    assert_eq!(available["reason"], "UnsignedNotAllowed");
    assert_eq!(available["message"], WITHHELD_MESSAGE);
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

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("turning-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![strict_ccp()]),
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

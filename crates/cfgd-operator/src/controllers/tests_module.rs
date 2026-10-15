//! Reconcile-fn tests for `controllers/module.rs`.
//!
//! `reconcile_module` evaluates Module availability (against
//! `ClusterConfigPolicy.security`) and signature verification, then
//! patches the Module's `/status` and emits Available/Verified events.

use std::sync::Arc;

use kube::runtime::controller::Action;

use super::module::{evaluate_module_verification, reconcile_module};
use super::test_kube_harness::{
    ExpectedCall, MockKubeHarness, empty_stores, expect_event_post, expect_event_series_patch,
    seeded_store,
};
use super::{
    ArtifactFactsReader, ArtifactVerifier, Clock, ControllerContext, ControllerStores,
    REGISTRY_RETRY_AFTER, RegistryBackoff,
};
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

    // A reader that would answer differently, so a re-read is visible.
    let (reader, visits) = counting_reader(ArtifactFacts {
        platforms: vec!["linux/amd64".to_string()],
        attestations: vec!["slsaprovenance1".to_string()],
    });
    let (ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("cached-attestation-mod")))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        reader,
    );

    // This process read the tag a moment ago and found what the status records.
    ctx.registry_backoff.record_read(
        "ghcr.io/example/mod:v1",
        ArtifactFacts {
            attestations: vec!["spdx".to_string()],
            ..Default::default()
        },
        ctx.clock.now(),
    );
    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    assert_eq!(
        visits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a tag read inside the requeue period is not read again"
    );
    let status_body = report.captured[0].body_json();
    assert_eq!(
        status_body["status"]["attestations"],
        serde_json::json!(["spdx"]),
        "a tag read inside the requeue period costs no registry round-trip"
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

    // A reader that would answer differently, so a re-read is visible.
    let (reader, visits) = counting_reader(ArtifactFacts {
        platforms: vec!["linux/amd64".to_string()],
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
        reader,
    );

    // This process read the tag a moment ago and found what the status records.
    ctx.registry_backoff.record_read(
        "ghcr.io/example/mod:v1",
        ArtifactFacts {
            platforms: vec!["linux/arm64".to_string()],
            ..Default::default()
        },
        ctx.clock.now(),
    );
    reconcile_module(Arc::new(module), ctx).await.unwrap();

    let report = harness.finish().await;
    assert_eq!(
        visits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a tag read inside the requeue period is not read again"
    );
    let status_body = report.captured[0].body_json();
    assert_eq!(
        status_body["status"]["availablePlatforms"],
        serde_json::json!(["linux/arm64"]),
        "a tag read inside the requeue period costs no registry round-trip"
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
/// the same artifact happily. `extraVolumes` / `extraVolumeMounts` carry the
/// registry login (a `config.json` from an image pull secret that
/// `DOCKER_CONFIG` names); an operator without them answered a registry that
/// asks for a login with 401 and left the column blank the same way.
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
        for key in ["extraEnv", "extraVolumes", "extraVolumeMounts"] {
            assert!(
                template.contains(&format!(".Values.{component}.{key}")),
                "the {component} template renders no {key}, so its registry cannot be configured"
            );
            assert!(
                !values[component][key].is_null(),
                "values.yaml declares no {component}.{key} default"
            );
            assert!(
                !schema["properties"][component]["properties"][key].is_null(),
                "values.schema.json declares no {component}.{key}, so a chart user gets no validation"
            );
        }
    }
}

/// A registry that answered nothing is not asked again by the very next
/// reconcile. Every status patch and every event this controller writes lands
/// as a watch event that triggers another reconcile, so a read whose failure
/// is remembered nowhere is a registry round-trip per event, forever.
#[tokio::test]
async fn a_registry_that_answered_nothing_is_not_revisited_on_the_next_reconcile() {
    use std::sync::atomic::Ordering;

    let (reader, visits) = counting_reader(ArtifactFacts::default());

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

/// A failed registry visit cools for 5, 10, 20, 40 and then 60 seconds after
/// each consecutive failure, and a success starts the count over.
#[test]
fn a_failed_registry_visit_is_retried_on_a_stepped_schedule() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let ms = Duration::from_millis(1);
    let backoff = RegistryBackoff::default();
    for (failures, secs) in [(1, 5), (2, 10), (3, 20), (4, 40), (5, 60), (6, 60)] {
        backoff.record_failure("facts:a".to_string(), String::new(), t0);
        let delay = Duration::from_secs(secs);
        assert_eq!(
            backoff.retry_in("facts:a", t0),
            Some(delay),
            "after {failures} consecutive failures"
        );
        assert!(
            backoff.cooling("facts:a", t0 + delay - ms).is_some(),
            "{failures} failures: still cooling just before {secs}s"
        );
        assert!(
            backoff.cooling("facts:a", t0 + delay).is_none(),
            "{failures} failures: no longer cooling at {secs}s"
        );
        assert_eq!(backoff.retry_in("facts:a", t0 + delay), None);
    }
    backoff.clear("facts:a");
    assert!(
        backoff.cooling("facts:a", t0).is_none(),
        "a cleared key is not cooling"
    );
    backoff.record_failure("facts:a".to_string(), String::new(), t0);
    assert_eq!(
        backoff.retry_in("facts:a", t0),
        Some(Duration::from_secs(5)),
        "the first failure after a clear counts as the first"
    );
}

/// Entries that can no longer change an answer are dropped by the next
/// recorded visit a requeue after the last drop, and no sooner; a key quiet
/// that long starts its count over.
#[test]
fn a_registry_visit_quiet_for_two_requeues_is_forgotten() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let secs = Duration::from_secs;
    let facts = || ArtifactFacts {
        platforms: vec!["linux/amd64".to_string()],
        ..Default::default()
    };
    let backoff = RegistryBackoff::default();
    backoff.record_failure("facts:a".to_string(), String::new(), t0);
    backoff.record_read("r", facts(), t0);
    backoff.record_failure("facts:e".to_string(), String::new(), t0 + secs(10));
    assert_eq!(backoff.held(), (2, 1));

    backoff.record_failure("facts:b".to_string(), String::new(), t0 + secs(125));
    assert_eq!(
        backoff.held(),
        (2, 0),
        "a failure two requeues old and a read one requeue old are dropped, a younger \
         failure is kept"
    );

    backoff.record_failure("facts:c".to_string(), String::new(), t0 + secs(135));
    assert_eq!(
        backoff.held(),
        (3, 0),
        "a requeue has not passed since the last drop, so nothing is dropped"
    );

    backoff.record_failure("facts:a".to_string(), String::new(), t0 + secs(185));
    assert_eq!(
        backoff.held(),
        (3, 0),
        "the failure at 10s is dropped at 185s"
    );
    assert_eq!(
        backoff.retry_in("facts:a", t0 + secs(185)),
        Some(secs(5)),
        "a key whose failure was dropped starts its count over"
    );
}

/// A registry down past the ceiling is retried once per requeue, so a drop
/// that runs just ahead of the retry keeps the failure count that put it there.
#[test]
fn a_failure_is_kept_until_its_retry_has_had_a_requeue_to_run() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let backoff = RegistryBackoff::default();
    for _ in 0..5 {
        backoff.record_failure("facts:down".to_string(), String::new(), t0);
    }
    let retry = t0 + REGISTRY_RETRY_AFTER + Duration::from_secs(1);
    backoff.record_failure("facts:other".to_string(), String::new(), retry);
    backoff.record_failure("facts:down".to_string(), String::new(), retry);
    assert_eq!(
        backoff.retry_in("facts:down", retry),
        Some(REGISTRY_RETRY_AFTER),
        "the sixth failure in a row cools for the ceiling"
    );
}

/// A key whose last failure is two requeues old starts its count over even
/// when no other visit has dropped that failure in the meantime.
#[test]
fn a_failure_two_requeues_old_starts_the_count_over_without_a_drop() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let backoff = RegistryBackoff::default();
    for _ in 0..6 {
        backoff.record_failure("facts:down".to_string(), String::new(), t0);
    }
    let later = t0 + Duration::from_secs(125);
    backoff.record_failure("facts:down".to_string(), String::new(), later);
    assert_eq!(
        backoff.retry_in("facts:down", later),
        Some(Duration::from_secs(5)),
        "a failure 125s after the last one is the first of a new run"
    );
}

/// Two reconciles of one module, with the reader answering `answers` in turn
/// and the reconciles run `at` after the instant `seed` is handed. Returns the
/// two requeues, the two status bodies written, and how many reads were made.
async fn two_fact_reconciles(
    name: &str,
    artifact: &str,
    saved: Option<Vec<String>>,
    answers: Vec<Vec<String>>,
    at: [std::time::Duration; 2],
    seed: impl FnOnce(&RegistryBackoff, std::time::Instant),
) -> (Action, Action, serde_json::Value, serde_json::Value, usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let visits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&visits);
    let reader = ArtifactFactsReader(Arc::new(move |_| {
        let at = counted.fetch_add(1, Ordering::SeqCst);
        ArtifactFacts {
            platforms: answers.get(at).cloned().unwrap_or_default(),
            attestations: Vec::new(),
        }
    }));
    let spec = ModuleSpec {
        oci_artifact: Some(artifact.to_string()),
        ..Default::default()
    };
    let mut module = make_module(name, spec);
    module.status = saved.map(|platforms| ModuleStatus {
        resolved_artifact: Some(artifact.to_string()),
        available_platforms: platforms,
        ..Default::default()
    });
    let module = Arc::new(module);
    let status_path = format!("{}/status", module_path(name));
    let (mut ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(status_path.clone()).returning_json(&*module),
            expect_event_post("default"),
            expect_event_post("default"),
            ExpectedCall::patch_status(status_path).returning_json(&*module),
            expect_event_series_patch("default"),
            expect_event_series_patch("default"),
        ],
        stores_with_ccps(vec![]),
        reader,
    );
    let t0 = std::time::Instant::now();
    let clock = held_clock(&mut ctx, t0);
    seed(&ctx.registry_backoff, t0);
    clock.set(t0 + at[0]);
    let first = reconcile_module(Arc::clone(&module), Arc::clone(&ctx))
        .await
        .unwrap();
    clock.set(t0 + at[1]);
    let second = reconcile_module(module, ctx).await.unwrap();
    let report = harness.finish().await;
    (
        first,
        second,
        report.captured[0].body_json()["status"].clone(),
        report.captured[3].body_json()["status"].clone(),
        visits.load(std::sync::atomic::Ordering::SeqCst),
    )
}

/// Give `ctx` a clock held at `at`, and hand back a handle that moves it.
fn held_clock(ctx: &mut Arc<ControllerContext>, at: std::time::Instant) -> Clock {
    let clock = Clock::fixed(at);
    Arc::get_mut(ctx)
        .expect("a fresh harness context has no other holder")
        .clock = clock.clone();
    clock
}

/// A reader answering `facts` for every reference, and the count of reads it
/// served.
fn counting_reader(
    facts: ArtifactFacts,
) -> (ArtifactFactsReader, Arc<std::sync::atomic::AtomicUsize>) {
    let visits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&visits);
    let reader = ArtifactFactsReader(Arc::new(move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        facts.clone()
    }));
    (reader, visits)
}

fn platforms(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_string()).collect()
}

/// One empty answer costs five seconds: the reconcile after the first retry
/// is due reads again and records what the registry now names.
#[tokio::test]
async fn an_empty_fact_read_is_retried_once_its_first_retry_is_due() {
    let (_, _, first, second, visits) = two_fact_reconciles(
        "retry-mod",
        "ghcr.io/example/mod:v1",
        None,
        vec![vec![], platforms(&["linux/amd64"])],
        [std::time::Duration::ZERO, std::time::Duration::from_secs(5)],
        |_, _| {},
    )
    .await;
    assert_eq!(visits, 2, "the read is retried 5s after one failure");
    assert_eq!(first["availablePlatforms"], serde_json::json!([]));
    assert_eq!(
        second["availablePlatforms"],
        serde_json::json!(["linux/amd64"])
    );
}

/// The requeue reaches the pending retry of a failed read, and falls back to
/// the steady period once the read succeeds.
#[tokio::test]
async fn reconcile_module_requeues_at_the_pending_retry_of_a_failed_read() {
    let (first, second, _, _, _) = two_fact_reconciles(
        "requeue-mod",
        "ghcr.io/example/mod:v1",
        None,
        vec![vec![], platforms(&["linux/amd64"])],
        [std::time::Duration::ZERO, std::time::Duration::from_secs(5)],
        |_, _| {},
    )
    .await;
    assert_eq!(
        first,
        Action::requeue(std::time::Duration::from_secs(5)),
        "one failed read requeues at its first retry"
    );
    assert_eq!(
        second,
        Action::requeue(REGISTRY_RETRY_AFTER),
        "a successful read requeues at the steady period"
    );
}

/// A tag's facts are trusted for one requeue period after this process read
/// them, then read again, so a second push to the tag is picked up.
#[tokio::test]
async fn a_tags_facts_are_read_again_once_the_last_read_is_a_requeue_old() {
    let artifact = "ghcr.io/example/mod:v1";
    let (_, _, inside, after, visits) = two_fact_reconciles(
        "tag-mod",
        artifact,
        Some(platforms(&["linux/amd64"])),
        vec![platforms(&["linux/amd64", "linux/arm64"])],
        [
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(60),
        ],
        |backoff, t0| {
            backoff.record_read(
                artifact,
                ArtifactFacts {
                    platforms: platforms(&["linux/amd64"]),
                    ..Default::default()
                },
                t0,
            )
        },
    )
    .await;
    assert_eq!(
        inside["availablePlatforms"],
        serde_json::json!(["linux/amd64"]),
        "a read 30s old is reused"
    );
    assert_eq!(visits, 1, "a read 30s old is reused, one 60s old is not");
    assert_eq!(
        after["availablePlatforms"],
        serde_json::json!(["linux/amd64", "linux/arm64"])
    );
}

/// A digest names one artifact for good, so facts recorded for it are never
/// read again, even by a process that has not read them itself.
#[tokio::test]
async fn a_digest_pinned_artifact_with_recorded_facts_is_never_read_again() {
    let artifact = format!("ghcr.io/example/mod@sha256:{}", "a".repeat(64));
    let (_, _, first, second, visits) = two_fact_reconciles(
        "digest-mod",
        &artifact,
        Some(platforms(&["linux/amd64"])),
        vec![platforms(&["linux/arm64"])],
        [std::time::Duration::ZERO, REGISTRY_RETRY_AFTER * 2],
        |_, _| {},
    )
    .await;
    assert_eq!(visits, 0, "a digest with recorded facts is not read");
    assert_eq!(
        first["availablePlatforms"],
        serde_json::json!(["linux/amd64"])
    );
    assert_eq!(
        second["availablePlatforms"],
        serde_json::json!(["linux/amd64"])
    );
}

/// A tag refresh that answers empty keeps the platforms an earlier read found.
#[tokio::test]
async fn an_empty_tag_refresh_keeps_the_recorded_platforms() {
    let (_, _, first, second, visits) = two_fact_reconciles(
        "keep-mod",
        "ghcr.io/example/mod:v1",
        Some(platforms(&["linux/amd64"])),
        vec![vec![], vec![]],
        [std::time::Duration::ZERO, std::time::Duration::from_secs(5)],
        |_, _| {},
    )
    .await;
    assert_eq!(visits, 2, "a tag never read by this process is read");
    for status in [first, second] {
        assert_eq!(
            status["availablePlatforms"],
            serde_json::json!(["linux/amd64"]),
            "an empty answer keeps the recorded platforms"
        );
    }
}

/// A tag refresh that answered empty cools, and a reconcile while it cools
/// keeps the recorded platforms without a visit.
#[tokio::test]
async fn a_cooling_tag_refresh_keeps_the_recorded_platforms_without_a_visit() {
    let (_, _, first, second, visits) = two_fact_reconciles(
        "cooling-keep-mod",
        "ghcr.io/example/mod:v1",
        Some(platforms(&["linux/amd64"])),
        vec![vec![], vec![]],
        [std::time::Duration::ZERO, std::time::Duration::from_secs(1)],
        |_, _| {},
    )
    .await;
    assert_eq!(visits, 1, "a cooling refresh is not visited");
    for status in [first, second] {
        assert_eq!(
            status["availablePlatforms"],
            serde_json::json!(["linux/amd64"]),
            "a cooling refresh keeps the recorded platforms"
        );
    }
}

/// Two Modules naming one tag take the same answer: the first reconcile's
/// read answers the second inside the requeue period, whatever the second's
/// own status records.
#[tokio::test]
async fn every_module_naming_a_tag_takes_its_last_read_inside_the_requeue_period() {
    let artifact = "ghcr.io/example/mod:v1";
    let (reader, visits) = counting_reader(ArtifactFacts {
        platforms: platforms(&["linux/amd64", "linux/arm64"]),
        ..Default::default()
    });
    let saved = |name: &str| {
        let mut module = make_module(
            name,
            ModuleSpec {
                oci_artifact: Some(artifact.to_string()),
                ..Default::default()
            },
        );
        module.status = Some(ModuleStatus {
            resolved_artifact: Some(artifact.to_string()),
            available_platforms: platforms(&["linux/amd64"]),
            ..Default::default()
        });
        Arc::new(module)
    };
    let (a, b) = (saved("tag-reader-a"), saved("tag-reader-b"));
    let (mut ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path("tag-reader-a")))
                .returning_json(&*a),
            expect_event_post("default"),
            expect_event_post("default"),
            ExpectedCall::patch_status(format!("{}/status", module_path("tag-reader-b")))
                .returning_json(&*b),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        reader,
    );
    let t0 = std::time::Instant::now();
    let clock = held_clock(&mut ctx, t0);
    reconcile_module(a, Arc::clone(&ctx)).await.unwrap();
    clock.set(t0 + std::time::Duration::from_secs(1));
    reconcile_module(b, ctx).await.unwrap();
    let report = harness.finish().await;

    assert_eq!(
        visits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the second Module reuses the first one's read"
    );
    for at in [0, 3] {
        assert_eq!(
            report.captured[at].body_json()["status"]["availablePlatforms"],
            serde_json::json!(["linux/amd64", "linux/arm64"]),
            "both Modules record what the one read answered"
        );
    }
}

/// A failed visit is stamped when it returns, so a slow one cools for its
/// whole first step after it ends, and the requeue reaches that retry.
#[tokio::test]
async fn a_slow_failed_visit_cools_for_its_first_step_after_it_returns() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let t0 = std::time::Instant::now();
    let clock = Clock::fixed(t0);
    let visits = Arc::new(AtomicUsize::new(0));
    let reader = {
        let (clock, visits) = (clock.clone(), Arc::clone(&visits));
        ArtifactFactsReader(Arc::new(move |_| {
            visits.fetch_add(1, Ordering::SeqCst);
            clock.set(clock.now() + std::time::Duration::from_secs(120));
            ArtifactFacts::default()
        }))
    };
    let module = Arc::new(make_module(
        "slow-visit-mod",
        ModuleSpec {
            oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
            ..Default::default()
        },
    ));
    let status_path = format!("{}/status", module_path("slow-visit-mod"));
    let (mut ctx, _registry, harness) = MockKubeHarness::with_facts(
        vec![
            ExpectedCall::patch_status(status_path.clone()).returning_json(&*module),
            expect_event_post("default"),
            expect_event_post("default"),
            ExpectedCall::patch_status(status_path).returning_json(&*module),
            expect_event_series_patch("default"),
            expect_event_series_patch("default"),
        ],
        stores_with_ccps(vec![]),
        reader,
    );
    Arc::get_mut(&mut ctx)
        .expect("a fresh harness context has no other holder")
        .clock = clock.clone();

    let requeue = reconcile_module(Arc::clone(&module), Arc::clone(&ctx))
        .await
        .unwrap();
    clock.set(clock.now() + std::time::Duration::from_secs(1));
    reconcile_module(module, ctx).await.unwrap();
    harness.finish().await;

    assert_eq!(
        requeue,
        Action::requeue(std::time::Duration::from_secs(5)),
        "the requeue reaches the retry the failure scheduled when it returned"
    );
    assert_eq!(
        visits.load(Ordering::SeqCst),
        1,
        "a visit that returned 1s ago is still cooling"
    );
}

/// The requeue is measured once every visit has returned, so time a slow
/// visit spent counts against the retries already pending.
#[tokio::test]
async fn the_requeue_is_measured_once_the_visits_return() {
    let t0 = std::time::Instant::now();
    let clock = Clock::fixed(t0);
    let reader = {
        let clock = clock.clone();
        ArtifactFactsReader(Arc::new(move |_| {
            clock.set(clock.now() + std::time::Duration::from_secs(3));
            ArtifactFacts::default()
        }))
    };
    let name = "measured-requeue-mod";
    let module = make_module(name, signed_spec());
    let (mut ctx, _registry, harness) = MockKubeHarness::with_registry_seams(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path(name)))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        reader,
        ArtifactVerifier::fixed(SignatureCheck::Undetermined(
            "registry unreachable".to_string(),
        )),
    );
    Arc::get_mut(&mut ctx)
        .expect("a fresh harness context has no other holder")
        .clock = clock;
    let requeue = reconcile_module(Arc::new(module), ctx).await.unwrap();
    harness.finish().await;
    assert_eq!(
        requeue,
        Action::requeue(std::time::Duration::from_secs(2)),
        "the check failed at 0s and the read took 3s, so the check's retry is 2s away"
    );
}

/// While a failed signature check cools, the Verified condition stays as that
/// visit wrote it, so a reconcile in between writes no status and posts no
/// event.
#[tokio::test]
async fn a_cooling_signature_check_leaves_the_status_as_the_failed_visit_wrote_it() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let name = "cooling-verify-mod";
    let checks = Arc::new(AtomicUsize::new(0));
    let verifier = || {
        let checks = Arc::clone(&checks);
        ArtifactVerifier(Arc::new(move |_, _| {
            checks.fetch_add(1, Ordering::SeqCst);
            SignatureCheck::Undetermined("registry unreachable".to_string())
        }))
    };
    let module = make_module(name, signed_spec());
    let (mut ctx, _registry, harness) = MockKubeHarness::with_registry_seams(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path(name)))
                .returning_json(&module),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        ArtifactFactsReader::fixed(Default::default()),
        verifier(),
    );
    let t0 = std::time::Instant::now();
    let clock = held_clock(&mut ctx, t0);
    reconcile_module(Arc::new(module), Arc::clone(&ctx))
        .await
        .unwrap();
    let written = harness.finish().await.captured[0].body_json()["status"].clone();

    // The next reconcile sees the status the failed visit wrote, as a watch
    // event would deliver it, through a harness that expects no call at all.
    let mut module = make_module(name, signed_spec());
    module.status = Some(serde_json::from_value(written).expect("the written status reads back"));
    let (mut cooling, _registry, harness) = MockKubeHarness::with_registry_seams(
        vec![],
        stores_with_ccps(vec![]),
        ArtifactFactsReader::fixed(Default::default()),
        verifier(),
    );
    {
        let shared =
            Arc::get_mut(&mut cooling).expect("a fresh harness context has no other holder");
        shared.registry_backoff = ctx.registry_backoff.clone();
        shared.clock = clock.clone();
    }
    clock.set(t0 + std::time::Duration::from_secs(1));
    reconcile_module(Arc::new(module), cooling).await.unwrap();
    harness.finish().await;

    assert_eq!(
        checks.load(Ordering::SeqCst),
        1,
        "a cooling check is not run"
    );
}

/// A signature check is cooled per artifact AND verification identity: one
/// Module's undetermined keyless check neither answers for nor is cleared by
/// another Module that verifies the same artifact with its own key.
#[tokio::test]
async fn a_cooling_signature_check_answers_only_for_its_own_verification_identity() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let keyless_spec = ModuleSpec {
        signature: Some(ModuleSignature {
            cosign: Some(CosignSignature {
                keyless: true,
                ..Default::default()
            }),
        }),
        ..signed_spec()
    };
    let keyless_name = "keyless-unconstrained-mod";
    let keyless = make_module(keyless_name, keyless_spec);
    let (mut ctx, _registry, harness) = MockKubeHarness::with_registry_seams(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path(keyless_name)))
                .returning_json(&keyless),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![]),
        ArtifactFactsReader::fixed(Default::default()),
        ArtifactVerifier::fixed(SignatureCheck::Undetermined(
            "keyless verification needs an identity".to_string(),
        )),
    );
    let t0 = std::time::Instant::now();
    let clock = held_clock(&mut ctx, t0);
    reconcile_module(Arc::new(keyless), Arc::clone(&ctx))
        .await
        .unwrap();
    harness.finish().await;

    let keyless_key = "verify:ghcr.io/example/mod:v1#keyless:*@*";
    let during = t0 + std::time::Duration::from_secs(1);
    let cooling_before = ctx.registry_backoff.retry_in(keyless_key, during);
    assert!(
        cooling_before.is_some(),
        "the keyless check failed, so its own key cools"
    );

    let keyed_name = "keyed-mod";
    let keyed = make_module(keyed_name, signed_spec());
    let checks = Arc::new(AtomicUsize::new(0));
    let verifier = {
        let checks = Arc::clone(&checks);
        ArtifactVerifier(Arc::new(move |_, _| {
            checks.fetch_add(1, Ordering::SeqCst);
            SignatureCheck::Valid
        }))
    };
    let (mut keyed_ctx, _registry, harness) = MockKubeHarness::with_registry_seams(
        vec![
            ExpectedCall::patch_status(format!("{}/status", module_path(keyed_name)))
                .returning_json(&keyed),
            expect_event_post("default"),
            expect_event_post("default"),
        ],
        stores_with_ccps(vec![strict_ccp()]),
        ArtifactFactsReader::fixed(Default::default()),
        verifier,
    );
    {
        let shared =
            Arc::get_mut(&mut keyed_ctx).expect("a fresh harness context has no other holder");
        shared.registry_backoff = ctx.registry_backoff.clone();
        shared.clock = clock.clone();
    }
    clock.set(during);
    reconcile_module(Arc::new(keyed), keyed_ctx).await.unwrap();
    let status = harness.finish().await.captured[0].body_json()["status"].clone();

    assert_eq!(
        checks.load(Ordering::SeqCst),
        1,
        "the keyed Module's check runs inside the keyless one's cooling"
    );
    assert_eq!(condition(&status, "Verified")["status"], "True");
    assert_eq!(status["signature"], cfgd_crd::SIGNATURE_VERIFIED);
    assert_eq!(
        condition(&status, "Available")["status"],
        "True",
        "a verified Module is not withheld by allowUnsigned: false"
    );
    assert_eq!(
        ctx.registry_backoff.retry_in(keyless_key, during),
        cooling_before,
        "the keyed Module's valid check leaves the keyless failure in place"
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

/// A policy being deleted holds back nothing. Its deletion reaches the Module
/// controller as the sweep its finalizer reconcile triggers, while the object
/// is still cached with its deletion timestamp; a module that policy withheld
/// must come back Available on that sweep, because no event follows the
/// policy's final removal. The policy carries both gates (unsigned refused, a
/// registry list the artifact is not on), so a read that still counted it
/// through either gate fails here.
#[tokio::test]
async fn reconcile_module_under_a_deleting_policy_is_no_longer_withheld() {
    let mut doomed = strict_ccp();
    doomed.spec.security.trusted_registries = vec!["other.io".to_string()];
    doomed.metadata.deletion_timestamp = Some(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(k8s_openapi::jiff::Timestamp::now()),
    );
    let spec = ModuleSpec {
        oci_artifact: Some("ghcr.io/example/mod:v1".to_string()),
        ..Default::default()
    };

    let withheld = status_under_policies(
        "freed-mod",
        spec.clone(),
        SignatureCheck::Valid,
        vec![strict_ccp()],
    )
    .await;
    assert_eq!(condition(&withheld, "Available")["status"], "False");

    let status =
        status_under_policies("freed-mod", spec, SignatureCheck::Valid, vec![doomed]).await;
    let available = condition(&status, "Available");
    assert_eq!(
        available["status"], "True",
        "a deleting policy must not withhold the module, got {available:?}"
    );
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

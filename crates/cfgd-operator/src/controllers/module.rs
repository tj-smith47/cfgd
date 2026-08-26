use std::sync::Arc;

use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Resource, ResourceExt};
use tracing::{info, warn};

use crate::crds::{
    Module, ModuleRef, ModuleSignature, ModuleSpec, ModuleStatus, is_valid_oci_reference,
    is_valid_pem_public_key,
};
use crate::errors::OperatorError;
use cfgd_core::oci::{ArtifactFacts, SignatureCheck};

use super::{
    ControllerContext, ControllerStores, FIELD_MANAGER_STATUS, build_condition, emit_event,
    record_reconcile_success,
};
pub(super) async fn reconcile_module(
    obj: Arc<Module>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();

    info!(
        name = %name,
        oci_artifact = ?obj.spec.oci_artifact,
        has_signature = obj.spec.signature.is_some(),
        packages = obj.spec.packages.len(),
        "reconciling Module"
    );

    let current_generation = obj.meta().generation;
    let existing_conditions = obj
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);
    let now = cfgd_core::utc_now_iso8601();

    let mut conditions = Vec::new();

    // Evaluate Available condition
    let (avail_status, avail_reason, avail_message, avail_event) =
        evaluate_module_availability(&ctx.stores, &name, &obj.spec).await;

    // Determine resolved artifact (just echo the reference if valid)
    let resolved_artifact = obj.spec.oci_artifact.clone();

    // Evaluate Verified condition. The spec's signature block is judged first
    // — a key that is not a key can be rejected without a registry visit —
    // and only a usable one is then checked against the artifact itself.
    let ver = verify_module_signature(
        &ctx,
        &obj,
        avail_status,
        resolved_artifact.as_deref(),
        evaluate_module_verification(&obj.spec.signature),
    )
    .await;
    let verified = ver.status == "True";

    // The unsigned policy is settled last, and here alone, because it is about
    // the VERDICT: availability judges the reference, the verifier judges the
    // artifact, and only afterwards is there a verdict to hold a module back
    // over. A module declaring no key and one whose declared key rejected its
    // artifact are the same failure to this gate.
    let (avail_status, avail_reason, avail_message, avail_event) =
        withhold_unverified(&ctx.stores, &name, ver.verdict)
            .await
            .unwrap_or((avail_status, avail_reason, avail_message, avail_event));

    conditions.push(build_condition(
        existing_conditions,
        "Available",
        avail_status,
        avail_reason,
        avail_message,
        &now,
        current_generation,
    ));
    conditions.push(build_condition(
        existing_conditions,
        "Verified",
        ver.status,
        ver.reason,
        &ver.condition_message(),
        &now,
        current_generation,
    ));

    let facts =
        resolve_artifact_facts(&ctx, &obj, avail_status, resolved_artifact.as_deref()).await;
    let ArtifactFacts {
        platforms: available_platforms,
        attestations,
    } = facts;
    let desired = ModuleStatus {
        // Stamped on every reconcile, so a reader can tell whether the verdict
        // below describes the spec it just applied or the one it replaced.
        // This also keeps the equality check honest: a spec-only edit bumps
        // the generation, so the status is rewritten even when every verdict
        // came out the same.
        observed_generation: current_generation,
        resolved_artifact,
        platforms_summary: ModuleStatus::summarize_platforms(&available_platforms),
        available_platforms,
        verified,
        signature: Some(ver.verdict.to_string()),
        signature_digest: ver.signature_digest,
        attestations,
        conditions,
    };

    // Both conditions carry their existing lastTransitionTime forward while
    // their status holds, so a re-evaluation that reached the same verdict
    // compares equal — and neither the write nor the pair of events it
    // announces has anything new to say.
    if obj.status.as_ref() != Some(&desired) {
        let modules_api: Api<Module> = Api::all(ctx.client.clone());
        modules_api
            .patch_status(
                &name,
                &PatchParams::apply(FIELD_MANAGER_STATUS),
                &Patch::Merge(serde_json::json!({ "status": desired })),
            )
            .await
            .map_err(|e| {
                OperatorError::Reconciliation(format!(
                    "failed to update Module status for {name}: {e}"
                ))
            })?;

        info!(name = %name, "module status updated");

        // Emit availability event
        emit_event(
            &ctx.recorder,
            &obj.object_ref(&()),
            avail_event.0,
            avail_event.1,
            avail_event.2,
            "Reconcile",
        )
        .await;

        // Emit verification event
        emit_event(
            &ctx.recorder,
            &obj.object_ref(&()),
            ver.event.0,
            ver.event.1,
            ver.event.2,
            "Reconcile",
        )
        .await;
    }

    record_reconcile_success(&ctx, "module", start);

    Ok(Action::requeue(super::REGISTRY_RETRY_AFTER))
}
/// What the module's artifact declares — its platforms and its attestations —
/// read off the OCI manifests beside it.
///
/// Re-read only when nothing is recorded for the artifact the spec now names:
/// the reconcile requeues every 60 seconds, and a registry round-trip per
/// module per minute buys nothing while the reference is unchanged. A module
/// with no artifact, or one whose artifact is not admissible, has no manifest
/// to read and answers empty.
///
/// Both facts are recovered from the previous status together, because they
/// were read together: reusing one while re-reading the other would describe
/// one artifact with two visits' answers.
async fn resolve_artifact_facts(
    ctx: &ControllerContext,
    obj: &Module,
    avail_status: &str,
    artifact: Option<&str>,
) -> ArtifactFacts {
    let Some(artifact) = artifact else {
        return ArtifactFacts::default();
    };
    if avail_status != "True" {
        return ArtifactFacts::default();
    }
    if let Some(prev) = obj.status.as_ref()
        && prev.resolved_artifact.as_deref() == Some(artifact)
        && !(prev.available_platforms.is_empty() && prev.attestations.is_empty())
    {
        return ArtifactFacts {
            platforms: prev.available_platforms.clone(),
            attestations: prev.attestations.clone(),
        };
    }

    let key = format!("facts:{artifact}");
    let now = std::time::Instant::now();
    if ctx.registry_backoff.cooling(&key, now) {
        return ArtifactFacts::default();
    }

    let reader = ctx.artifact_facts.clone();
    let reference = artifact.to_string();
    let facts =
        match cfgd_core::spawn_blocking_with_test_home(move || reader.read_facts(&reference)).await
        {
            Ok(facts) => facts,
            Err(e) => {
                warn!(artifact = %artifact, error = %e, "artifact fact read did not complete");
                ArtifactFacts::default()
            }
        };

    // An empty answer is what an unreachable registry and an artifact that
    // declares nothing both look like, and neither is worth a fresh visit on
    // the next watch event — only on the next requeue.
    if facts.platforms.is_empty() && facts.attestations.is_empty() {
        ctx.registry_backoff.record_failure(key, now);
    } else {
        ctx.registry_backoff.clear(&key);
    }
    facts
}

/// Deny a module that a `ClusterConfigPolicy` requires to be signed and whose
/// signature did not verify.
///
/// `allowUnsigned: false` is a demand that admitted modules be signed, and a
/// declared key is only a promise of one: the artifact it names can carry no
/// signature at all, or one the key rejects, and a check that could not run
/// establishes neither. Any verdict short of `verified` therefore withholds
/// the module — the failure mode a security gate is allowed to have.
///
/// `None` means the gate has nothing to say and availability stands as
/// evaluated; the cache read costs no API call, and an unreadable cache
/// leaves the module admitted exactly as [`evaluate_module_availability`]
/// does.
async fn withhold_unverified<'a>(
    stores: &ControllerStores,
    module_name: &str,
    verdict: &str,
) -> Option<(&'a str, &'a str, &'a str, (EventType, &'a str, String))> {
    if verdict == cfgd_crd::SIGNATURE_VERIFIED {
        return None;
    }
    let policies = stores.all_cluster_config_policies().await.ok()?;
    if policies.iter().all(|ccp| ccp.spec.security.allow_unsigned) {
        return None;
    }
    Some((
        "False",
        "UnsignedNotAllowed",
        "Module signature did not verify but unsigned modules are not allowed",
        (
            EventType::Warning,
            "UnsignedNotAllowed",
            format!(
                "Module {module_name} signature is {verdict} but policy requires a verified signature"
            ),
        ),
    ))
}

async fn evaluate_module_availability<'a>(
    stores: &ControllerStores,
    module_name: &str,
    spec: &ModuleSpec,
) -> (&'a str, &'a str, &'a str, (EventType, &'a str, String)) {
    let oci_ref = match &spec.oci_artifact {
        None => {
            return (
                "True",
                "NoArtifact",
                "Module is local-only (no OCI artifact)",
                (
                    EventType::Normal,
                    "Available",
                    format!("Module {} is local-only", module_name),
                ),
            );
        }
        Some(r) => r,
    };

    // Validate OCI reference format
    if !is_valid_oci_reference(oci_ref) {
        return (
            "False",
            "InvalidReference",
            "OCI artifact reference is invalid",
            (
                EventType::Warning,
                "PullFailed",
                format!(
                    "Module {} has invalid OCI reference: {}",
                    module_name, oci_ref
                ),
            ),
        );
    }

    // Read all ClusterConfigPolicies for security constraints
    let ccp_list = match stores.all_cluster_config_policies().await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "ClusterConfigPolicy cache unavailable for Module validation");
            // An unreadable policy cache allows the module: fail-open for availability
            return (
                "True",
                "ArtifactAvailable",
                "OCI artifact reference is valid",
                (
                    EventType::Normal,
                    "Available",
                    format!("Module {} artifact available: {}", module_name, oci_ref),
                ),
            );
        }
    };

    // Collect all trusted registries from ClusterConfigPolicies
    let all_trusted_registries: Vec<&String> = ccp_list
        .iter()
        .flat_map(|ccp| ccp.spec.security.trusted_registries.iter())
        .collect();

    // Check trusted registries (only if any are configured)
    if !all_trusted_registries.is_empty() {
        let matches_registry = all_trusted_registries.iter().any(|pattern| {
            if let Some(prefix) = pattern.strip_suffix('*') {
                oci_ref.starts_with(prefix)
            } else {
                oci_ref.starts_with(pattern.as_str())
            }
        });

        if !matches_registry {
            return (
                "False",
                "TrustedRegistryViolation",
                "OCI artifact is not from a trusted registry",
                (
                    EventType::Warning,
                    "TrustedRegistryViolation",
                    format!(
                        "Module {} artifact {} is not from a trusted registry",
                        module_name, oci_ref
                    ),
                ),
            );
        }
    }

    (
        "True",
        "ArtifactAvailable",
        "OCI artifact reference is valid",
        (
            EventType::Normal,
            "Available",
            format!("Module {} artifact available: {}", module_name, oci_ref),
        ),
    )
}
pub(super) struct ModuleVerificationResult {
    pub(super) status: &'static str,
    pub(super) reason: &'static str,
    pub(super) message: &'static str,
    pub(super) event: (EventType, &'static str, String),
    /// SHA256 fingerprint of the public key, or keyless identity description.
    pub(super) signature_digest: Option<String>,
    /// The one word [`ModuleStatus::signature`] records, drawn from the
    /// `cfgd_crd::SIGNATURE_*` vocabulary.
    pub(super) verdict: &'static str,
    /// What the verifier itself said, when it said anything — cosign's own
    /// rejection, or why the check could not run. Appended to `message` in the
    /// condition so the fixed sentence stays greppable and the varying part
    /// still reaches a `kubectl describe`.
    pub(super) detail: Option<String>,
}

impl ModuleVerificationResult {
    /// The Verified condition's message: the fixed sentence, plus whatever the
    /// verifier reported.
    pub(super) fn condition_message(&self) -> String {
        match &self.detail {
            Some(detail) => format!("{}: {detail}", self.message),
            None => self.message.to_string(),
        }
    }
}

/// Check the module's artifact against the signature its spec declares.
///
/// `config` is [`evaluate_module_verification`]'s verdict on the spec alone.
/// A spec that cannot yield a usable key is already settled and is returned
/// untouched; only a usable one reaches the verifier, and only the verifier
/// can produce `verified`.
///
/// Three outcomes are kept apart, because collapsing them is how a
/// configuration check came to be printed as a verification:
///
/// - the verifier accepted the artifact — `verified`;
/// - the verifier rejected it — `unverified`, a Warning event;
/// - the check could not run at all (no cosign, unreachable registry, no
///   artifact to check) — `unknown`, an `Unknown` condition and a Warning
///   event naming the reason. Never `verified`, and never `unverified`
///   either: nothing was learned about the signature.
///
/// A registry that just failed is not visited again until
/// [`super::REGISTRY_RETRY_AFTER`] has passed, so an unreachable registry
/// costs one visit per requeue rather than one per watch event.
async fn verify_module_signature(
    ctx: &ControllerContext,
    obj: &Module,
    avail_status: &str,
    artifact: Option<&str>,
    config: ModuleVerificationResult,
) -> ModuleVerificationResult {
    // The spec itself settled the verdict: no signature declared, or one whose
    // key cannot be used. Nothing to check against the artifact.
    if config.status != "True" {
        return config;
    }
    let Some(cosign) = obj.spec.signature.as_ref().and_then(|s| s.cosign.as_ref()) else {
        return config;
    };
    let name = obj.name_any();

    let Some(artifact) = artifact.filter(|_| avail_status == "True") else {
        return cannot_verify(
            "NoVerifiableArtifact",
            "Module declares a signature but has no admissible artifact to verify",
            &name,
            config.signature_digest,
            None,
        );
    };

    let key = format!("verify:{artifact}");
    let now = std::time::Instant::now();
    if ctx.registry_backoff.cooling(&key, now) {
        return cannot_verify(
            "VerificationUnavailable",
            "Signature could not be checked",
            &name,
            config.signature_digest,
            Some(
                "the last check of this artifact failed; retrying on the next requeue".to_string(),
            ),
        );
    }

    let verifier = ctx.artifact_verifier.clone();
    let reference = artifact.to_string();
    let cosign = cosign.clone();
    let check =
        cfgd_core::spawn_blocking_with_test_home(move || verifier.check(&reference, &cosign)).await;

    match check {
        Ok(SignatureCheck::Valid) => {
            ctx.registry_backoff.clear(&key);
            ModuleVerificationResult {
                status: "True",
                reason: "SignatureVerified",
                message: "Artifact signature verified against the declared cosign key",
                event: (
                    EventType::Normal,
                    "Verified",
                    format!("Module {name} artifact signature verified: {artifact}"),
                ),
                signature_digest: config.signature_digest,
                verdict: cfgd_crd::SIGNATURE_VERIFIED,
                detail: None,
            }
        }
        Ok(SignatureCheck::Rejected(why)) => {
            ctx.registry_backoff.clear(&key);
            ModuleVerificationResult {
                status: "False",
                reason: "SignatureInvalid",
                message: "Artifact signature was rejected by the declared cosign key",
                event: (
                    EventType::Warning,
                    "SignatureInvalid",
                    format!("Module {name} artifact signature was rejected: {why}"),
                ),
                signature_digest: config.signature_digest,
                verdict: cfgd_crd::SIGNATURE_UNVERIFIED,
                detail: Some(why),
            }
        }
        Ok(SignatureCheck::Undetermined(why)) => {
            ctx.registry_backoff.record_failure(key, now);
            cannot_verify(
                "VerificationUnavailable",
                "Signature could not be checked",
                &name,
                config.signature_digest,
                Some(why),
            )
        }
        Err(e) => {
            ctx.registry_backoff.record_failure(key, now);
            cannot_verify(
                "VerificationUnavailable",
                "Signature could not be checked",
                &name,
                config.signature_digest,
                Some(format!("the check did not complete: {e}")),
            )
        }
    }
}

/// The Verified condition for a check that did not happen.
///
/// `Unknown` rather than `False`, because `False` is the operator saying the
/// signature is bad — a claim nothing here supports.
fn cannot_verify(
    reason: &'static str,
    message: &'static str,
    module_name: &str,
    signature_digest: Option<String>,
    detail: Option<String>,
) -> ModuleVerificationResult {
    let announced = match &detail {
        Some(detail) => format!("Module {module_name} signature could not be checked: {detail}"),
        None => format!("Module {module_name} signature could not be checked"),
    };
    ModuleVerificationResult {
        status: "Unknown",
        reason,
        message,
        event: (EventType::Warning, "VerificationUnavailable", announced),
        signature_digest,
        verdict: cfgd_crd::SIGNATURE_UNKNOWN,
        detail,
    }
}

pub(super) fn evaluate_module_verification(
    signature: &Option<ModuleSignature>,
) -> ModuleVerificationResult {
    match signature {
        None => ModuleVerificationResult {
            status: "False",
            reason: "NotSigned",
            message: "No signature configuration present",
            event: (
                EventType::Normal,
                "Verified",
                "Module has no signature configuration".to_string(),
            ),
            signature_digest: None,
            verdict: cfgd_crd::SIGNATURE_UNSIGNED,
            detail: None,
        },
        Some(sig) => match &sig.cosign {
            None => ModuleVerificationResult {
                status: "False",
                reason: "NotSigned",
                message: "No cosign signature configured",
                event: (
                    EventType::Normal,
                    "Verified",
                    "Module has no cosign signature configured".to_string(),
                ),
                signature_digest: None,
                verdict: cfgd_crd::SIGNATURE_UNSIGNED,
                detail: None,
            },
            Some(cosign) => {
                // Keyless mode — no public key needed
                if cosign.keyless {
                    let identity_desc = format!(
                        "keyless:{}@{}",
                        cosign.certificate_identity.as_deref().unwrap_or("*"),
                        cosign.certificate_oidc_issuer.as_deref().unwrap_or("*"),
                    );
                    return ModuleVerificationResult {
                        status: "True",
                        reason: "SignatureConfigured",
                        message: "Keyless cosign verification configured (Fulcio/Rekor)",
                        event: (
                            EventType::Normal,
                            "Verified",
                            "Module has keyless cosign verification configured".to_string(),
                        ),
                        signature_digest: Some(identity_desc),
                        // A usable configuration is not a verdict: the word is
                        // whatever the verifier goes on to answer, and this
                        // stands in only for the check never having run.
                        verdict: cfgd_crd::SIGNATURE_UNKNOWN,
                        detail: None,
                    };
                }
                // Static key mode — validate PEM
                match &cosign.public_key {
                    Some(pk) if is_valid_pem_public_key(pk) => {
                        let fingerprint = cfgd_core::sha256_digest(pk.as_bytes());
                        ModuleVerificationResult {
                            status: "True",
                            reason: "SignatureConfigured",
                            message: "Cosign public key is configured and valid",
                            event: (
                                EventType::Normal,
                                "Verified",
                                "Module has valid cosign signature configuration".to_string(),
                            ),
                            signature_digest: Some(fingerprint),
                            verdict: cfgd_crd::SIGNATURE_UNKNOWN,
                            detail: None,
                        }
                    }
                    Some(_) => ModuleVerificationResult {
                        status: "False",
                        reason: "SignatureInvalid",
                        message: "Cosign public key is not valid PEM",
                        event: (
                            EventType::Warning,
                            "SignatureInvalid",
                            "Module cosign public key is not valid PEM".to_string(),
                        ),
                        signature_digest: None,
                        verdict: cfgd_crd::SIGNATURE_UNVERIFIED,
                        detail: None,
                    },
                    None => ModuleVerificationResult {
                        status: "False",
                        reason: "SignatureInvalid",
                        message: "Cosign signature configured but no public key or keyless mode",
                        event: (
                            EventType::Warning,
                            "SignatureInvalid",
                            "No public key and keyless not enabled".to_string(),
                        ),
                        signature_digest: None,
                        verdict: cfgd_crd::SIGNATURE_UNVERIFIED,
                        detail: None,
                    },
                }
            }
        },
    }
}
pub(super) async fn resolve_module_refs(
    stores: &ControllerStores,
    module_refs: &[ModuleRef],
) -> (&'static str, &'static str, String) {
    if module_refs.is_empty() {
        return (
            "True",
            "AllResolved",
            "No module references to resolve".to_string(),
        );
    }

    let module_list = match stores.all_modules().await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "Module cache unavailable for moduleRef resolution");
            return (
                "Unknown",
                "ResolutionError",
                "Module cache is not populated".to_string(),
            );
        }
    };

    let existing_names: Vec<String> = module_list.iter().map(|m| m.name_any()).collect();
    let missing: Vec<&str> = module_refs
        .iter()
        .filter(|mr| !existing_names.iter().any(|n| n == &mr.name))
        .map(|mr| mr.name.as_str())
        .collect();

    if missing.is_empty() {
        (
            "True",
            "AllResolved",
            "All module references resolved".to_string(),
        )
    } else {
        (
            "False",
            "ModulesNotFound",
            format!("Missing modules: {}", missing.join(", ")),
        )
    }
}

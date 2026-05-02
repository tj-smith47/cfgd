use std::sync::Arc;

use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Client, Resource, ResourceExt};
use tracing::{info, warn};

use crate::crds::{
    ClusterConfigPolicy, Module, ModuleRef, ModuleSignature, ModuleSpec, ModuleStatus,
    is_valid_oci_reference, is_valid_pem_public_key,
};
use crate::errors::OperatorError;

use super::{
    ControllerContext, FIELD_MANAGER_STATUS, build_condition, emit_event, record_reconcile_success,
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
        evaluate_module_availability(&ctx.client, &name, &obj.spec).await;

    conditions.push(build_condition(
        existing_conditions,
        "Available",
        avail_status,
        avail_reason,
        avail_message,
        &now,
        current_generation,
    ));

    // Evaluate Verified condition
    let ver = evaluate_module_verification(&obj.spec.signature);

    conditions.push(build_condition(
        existing_conditions,
        "Verified",
        ver.status,
        ver.reason,
        ver.message,
        &now,
        current_generation,
    ));

    // Determine resolved artifact (just echo the reference if valid)
    let resolved_artifact = obj.spec.oci_artifact.clone();
    let verified = ver.status == "True";

    let status = serde_json::json!({
        "status": ModuleStatus {
            resolved_artifact,
            available_platforms: vec![],
            verified,
            signature_digest: ver.signature_digest,
            attestations: vec![],
            conditions,
        }
    });

    let modules_api: Api<Module> = Api::all(ctx.client.clone());
    modules_api
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER_STATUS),
            &Patch::Merge(status),
        )
        .await
        .map_err(|e| {
            OperatorError::Reconciliation(format!("failed to update Module status for {name}: {e}"))
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

    record_reconcile_success(&ctx, "module", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}
async fn evaluate_module_availability<'a>(
    client: &Client,
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
    let ccp_api: Api<ClusterConfigPolicy> = Api::all(client.clone());
    let ccp_list = match ccp_api.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "failed to list ClusterConfigPolicies for Module validation");
            // If we can't list policies, allow the module (fail-open for availability)
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
    let mut all_trusted_registries: Vec<String> = Vec::new();
    let mut any_disallow_unsigned = false;

    for ccp in &ccp_list {
        let security = &ccp.spec.security;
        all_trusted_registries.extend(security.trusted_registries.clone());
        if !security.allow_unsigned {
            any_disallow_unsigned = true;
        }
    }

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

    // Check unsigned policy
    if any_disallow_unsigned {
        let has_cosign_key = spec
            .signature
            .as_ref()
            .and_then(|s| s.cosign.as_ref())
            .is_some_and(|c| c.keyless || c.public_key.as_ref().is_some_and(|pk| !pk.is_empty()));

        if !has_cosign_key {
            return (
                "False",
                "UnsignedNotAllowed",
                "Module has no signature but unsigned modules are not allowed",
                (
                    EventType::Warning,
                    "UnsignedNotAllowed",
                    format!(
                        "Module {} has no signature but policy requires signing",
                        module_name
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
                    },
                }
            }
        },
    }
}
pub(super) async fn resolve_module_refs(
    client: &Client,
    module_refs: &[ModuleRef],
) -> (&'static str, &'static str, String) {
    if module_refs.is_empty() {
        return (
            "True",
            "AllResolved",
            "No module references to resolve".to_string(),
        );
    }

    let modules_api: Api<Module> = Api::all(client.clone());
    let module_list = match modules_api.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "failed to list Modules for moduleRef resolution");
            return (
                "Unknown",
                "ResolutionError",
                "Failed to list Module resources".to_string(),
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

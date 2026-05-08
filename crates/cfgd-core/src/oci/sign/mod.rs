// Cosign signing + verification + SLSA in-toto attestations.
// All shell-out goes through `crate::cosign_cmd()` (the controlled cosign layer
// per module-boundaries.md).

use crate::errors::OciError;

/// Sign an OCI artifact with cosign.
///
/// If `key_path` is Some, uses `cosign sign --key <path>`.
/// If `key_path` is None, uses keyless signing (Fulcio/Rekor via OIDC).
pub fn sign_artifact(artifact_ref: &str, key_path: Option<&str>) -> Result<(), OciError> {
    crate::require_cosign().map_err(|_| OciError::ToolNotFound {
        tool: "cosign".to_string(),
    })?;

    let mut cmd = crate::cosign_cmd();
    cmd.arg("sign");

    if let Some(key) = key_path {
        cmd.arg("--key").arg(key);
    } else {
        cmd.arg("--yes");
    }

    cmd.arg(artifact_ref);

    let output = cmd.output().map_err(|e| OciError::SigningError {
        message: format!("failed to run cosign: {e}"),
    })?;

    if !output.status.success() {
        return Err(OciError::SigningError {
            message: format!(
                "cosign sign failed: {}",
                crate::stderr_lossy_trimmed(&output)
            ),
        });
    }

    tracing::info!(reference = artifact_ref, "artifact signed with cosign");
    Ok(())
}

/// Options for cosign verification (signature or attestation).
pub struct VerifyOptions<'a> {
    /// Path to cosign public key for static key verification.
    pub key: Option<&'a str>,
    /// Certificate identity regexp for keyless verification.
    pub identity: Option<&'a str>,
    /// Certificate OIDC issuer regexp for keyless verification.
    pub issuer: Option<&'a str>,
}

/// Validate that keyless verification has at least one identity constraint.
fn validate_verify_options(opts: &VerifyOptions<'_>) -> Result<(), OciError> {
    if opts.key.is_none() && opts.identity.is_none() && opts.issuer.is_none() {
        return Err(OciError::VerificationFailed {
            reference: String::new(),
            message: "keyless verification requires identity or issuer constraint (use --key, or provide VerifyOptions.identity/issuer)".to_string(),
        });
    }
    Ok(())
}

/// Apply verification args to a cosign command.
fn apply_verify_args(cmd: &mut std::process::Command, opts: &VerifyOptions<'_>) {
    if let Some(key) = opts.key {
        cmd.arg("--key").arg(key);
    } else {
        let identity = opts.identity.unwrap_or(".*");
        let issuer = opts.issuer.unwrap_or(".*");
        cmd.arg("--certificate-identity-regexp").arg(identity);
        cmd.arg("--certificate-oidc-issuer-regexp").arg(issuer);
    }
}

/// Verify the cosign signature on an OCI artifact.
///
/// Uses `cosign verify --key <path>` for static key, or keyless verification
/// with certificate identity/issuer constraints from `VerifyOptions`.
pub fn verify_signature(artifact_ref: &str, opts: &VerifyOptions<'_>) -> Result<(), OciError> {
    validate_verify_options(opts)?;

    crate::require_cosign().map_err(|_| OciError::ToolNotFound {
        tool: "cosign".to_string(),
    })?;

    let mut cmd = crate::cosign_cmd();
    cmd.arg("verify");
    apply_verify_args(&mut cmd, opts);
    cmd.arg(artifact_ref);

    let output = cmd.output().map_err(|e| OciError::VerificationFailed {
        reference: artifact_ref.to_string(),
        message: format!("failed to run cosign: {e}"),
    })?;

    if !output.status.success() {
        return Err(OciError::VerificationFailed {
            reference: artifact_ref.to_string(),
            message: format!(
                "cosign verify failed: {}",
                crate::stderr_lossy_trimmed(&output)
            ),
        });
    }

    tracing::info!(reference = artifact_ref, "signature verified");
    Ok(())
}

// ---------------------------------------------------------------------------
// Attestations (SLSA provenance / in-toto)
// ---------------------------------------------------------------------------

/// Generate a SLSA v1 provenance predicate JSON for a module artifact.
pub fn generate_slsa_provenance(
    artifact_ref: &str,
    digest: &str,
    source_repo: &str,
    source_commit: &str,
) -> Result<String, OciError> {
    let now = crate::utc_now_iso8601();
    serde_json::to_string_pretty(&serde_json::json!({
        "_type": "https://in-toto.io/Statement/v1",
        "predicateType": "https://slsa.dev/provenance/v1",
        "subject": [{
            "name": artifact_ref,
            "digest": {
                "sha256": crate::strip_sha256_prefix(digest),
            }
        }],
        "predicate": {
            "buildDefinition": {
                "buildType": "https://cfgd.io/ModuleBuild/v1",
                "externalParameters": {
                    "source": {
                        "uri": source_repo,
                        "digest": { "gitCommit": source_commit },
                    }
                },
            },
            "runDetails": {
                "builder": {
                    "id": "https://cfgd.io/builder/v1",
                },
                "metadata": {
                    "invocationId": &now,
                    "startedOn": &now,
                }
            }
        }
    }))
    .map_err(|e| OciError::AttestationError {
        message: format!("failed to serialize SLSA provenance: {e}"),
    })
}

/// Attach an in-toto attestation to an OCI artifact using cosign.
pub fn attach_attestation(
    artifact_ref: &str,
    attestation_path: &str,
    key_path: Option<&str>,
) -> Result<(), OciError> {
    crate::require_cosign().map_err(|_| OciError::ToolNotFound {
        tool: "cosign".to_string(),
    })?;

    let mut cmd = crate::cosign_cmd();
    cmd.arg("attest");

    if let Some(key) = key_path {
        cmd.arg("--key").arg(key);
    } else {
        cmd.arg("--yes");
    }

    cmd.arg("--predicate")
        .arg(attestation_path)
        .arg("--type")
        .arg("slsaprovenance")
        .arg(artifact_ref);

    let output = cmd.output().map_err(|e| OciError::AttestationError {
        message: format!("failed to run cosign attest: {e}"),
    })?;

    if !output.status.success() {
        return Err(OciError::AttestationError {
            message: format!(
                "cosign attest failed: {}",
                crate::stderr_lossy_trimmed(&output)
            ),
        });
    }

    tracing::info!(reference = artifact_ref, "attestation attached");
    Ok(())
}

/// Verify an in-toto attestation on an OCI artifact.
pub fn verify_attestation(
    artifact_ref: &str,
    predicate_type: &str,
    opts: &VerifyOptions<'_>,
) -> Result<(), OciError> {
    validate_verify_options(opts)?;

    crate::require_cosign().map_err(|_| OciError::ToolNotFound {
        tool: "cosign".to_string(),
    })?;

    let mut cmd = crate::cosign_cmd();
    cmd.arg("verify-attestation");
    apply_verify_args(&mut cmd, opts);
    cmd.arg("--type").arg(predicate_type).arg(artifact_ref);

    let output = cmd.output().map_err(|e| OciError::AttestationError {
        message: format!("failed to run cosign verify-attestation: {e}"),
    })?;

    if !output.status.success() {
        return Err(OciError::AttestationError {
            message: format!(
                "attestation verification failed: {}",
                crate::stderr_lossy_trimmed(&output)
            ),
        });
    }

    tracing::info!(reference = artifact_ref, "attestation verified");
    Ok(())
}

#[cfg(test)]
mod tests;

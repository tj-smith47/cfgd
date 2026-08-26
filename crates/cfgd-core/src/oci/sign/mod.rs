// Cosign signing + verification + SLSA in-toto attestations.
// All shell-out goes through `crate::cosign_cmd()` (the controlled cosign layer
// per module-boundaries.md).

use crate::errors::OciError;
use crate::oci::OciReference;

/// Tell cosign to reach `artifact_ref`'s registry over plain HTTP wherever cfgd
/// itself does.
///
/// cosign resolves the manifest through its own client, so a registry cfgd
/// reads over HTTP — anything named in `OCI_INSECURE_REGISTRIES` — is one
/// cosign would otherwise attempt a TLS handshake against. An unparseable
/// reference is left alone: cosign reports the reference error itself, and
/// guessing a scheme for a string neither side could parse would only widen
/// what is sent in the clear.
fn apply_registry_scheme(cmd: &mut std::process::Command, artifact_ref: &str) {
    if OciReference::parse(artifact_ref).is_ok_and(|r| r.uses_plain_http()) {
        cmd.arg("--allow-insecure-registry");
    }
}

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
        // Keyed signing is offline PKI: never upload to the public Rekor
        // transparency log (that would leak private module signatures to
        // public infra and trigger an interactive consent prompt).
        cmd.arg("--tlog-upload=false");
    }

    // Always skip cosign's interactive consent prompt so signing works
    // non-interactively on every path.
    cmd.arg("--yes");

    apply_registry_scheme(&mut cmd, artifact_ref);
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

    tracing::debug!(reference = artifact_ref, "artifact signed with cosign");
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

/// Anchor a cosign `--certificate-identity-regexp` / `--certificate-oidc-issuer-regexp`
/// pattern to require a full match of the certificate subject.
///
/// cosign hands these patterns straight to Go's `regexp.MatchString`, which matches
/// if the pattern is found ANYWHERE in the subject — an operator-supplied
/// `alice@example.com` also matches `evil-alice@example.com.attacker.io`. Wrapping the
/// pattern in a non-capturing group before anchoring keeps `^`/`$` binding the whole
/// pattern rather than just its first/last alternative when the pattern contains `|`.
fn anchor_regexp(pattern: &str) -> String {
    if pattern.starts_with('^') && pattern.ends_with('$') {
        return pattern.to_string();
    }
    format!("^(?:{pattern})$")
}

/// Apply verification args to a cosign command.
fn apply_verify_args(cmd: &mut std::process::Command, opts: &VerifyOptions<'_>) {
    if let Some(key) = opts.key {
        cmd.arg("--key").arg(key);
        // Keyed signatures are offline (no Rekor entry), so skip the
        // transparency-log lookup that keyless verification relies on.
        cmd.arg("--insecure-ignore-tlog=true");
    } else {
        let identity = anchor_regexp(opts.identity.unwrap_or(".*"));
        let issuer = anchor_regexp(opts.issuer.unwrap_or(".*"));
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
    apply_registry_scheme(&mut cmd, artifact_ref);
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

    tracing::debug!(reference = artifact_ref, "signature verified");
    Ok(())
}

/// What a signature check concluded, keeping "the signature is bad" and "I
/// could not look" apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureCheck {
    /// cosign checked the artifact against the caller's key and accepted it.
    Valid,
    /// cosign reached the artifact and rejected it — there is no signature, or
    /// none the key accepts. A verdict ABOUT the signature.
    Rejected(String),
    /// The check could not be performed: cosign is not installed, the registry
    /// is unreachable, the credentials were refused. NOT a verdict about the
    /// signature, and never grounds for calling an artifact unverified.
    Undetermined(String),
}

/// The cosign failures that mean "I resolved the artifact and its signature
/// does not hold".
///
/// Read as an allow-list rather than a deny-list on purpose: an unrecognized
/// failure is [`SignatureCheck::Undetermined`], which nobody may print as a
/// verdict, where the reverse default would let a DNS failure be reported as a
/// bad signature.
const COSIGN_REJECTIONS: &[&str] = &[
    "no matching signatures",
    "no signatures found",
    "MANIFEST_UNKNOWN",
    "invalid signature",
];

/// [`verify_signature`] as a three-way verdict rather than a `Result`.
///
/// Every `Err` from `verify_signature` looks alike to a caller matching on
/// `Result`, so turning one into "unverified" claims cosign rejected the
/// artifact — which a missing cosign binary or an unreachable registry does
/// not support. Reach for this wherever the outcome is DISPLAYED or recorded;
/// `verify_signature` itself stays the right call where any failure is fatal.
#[must_use]
pub fn check_signature(artifact_ref: &str, opts: &VerifyOptions<'_>) -> SignatureCheck {
    match verify_signature(artifact_ref, opts) {
        Ok(()) => SignatureCheck::Valid,
        Err(OciError::ToolNotFound { tool }) => {
            SignatureCheck::Undetermined(format!("{tool} is not installed"))
        }
        Err(e) => {
            let message = e.to_string();
            if COSIGN_REJECTIONS.iter().any(|m| message.contains(m)) {
                SignatureCheck::Rejected(message)
            } else {
                SignatureCheck::Undetermined(message)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Attestations (SLSA provenance / in-toto)
// ---------------------------------------------------------------------------

/// Generate a SLSA v1 provenance *predicate body* for an artifact.
///
/// Returns only the predicate (`buildDefinition` + `runDetails`), NOT a full
/// in-toto Statement. `cosign attest --type slsaprovenance1` wraps this body into
/// the statement itself and sets the `subject` from the artifact's resolved
/// digest. Emitting a full statement here (with its own `_type`/`predicateType`/
/// `subject`) makes cosign read that outer object as the predicate and reject it
/// with "provenance predicate: required field builder missing", because the
/// statement's top level has no `builder`.
pub fn generate_slsa_provenance(
    source_repo: &str,
    source_commit: &str,
) -> Result<String, OciError> {
    let now = crate::utc_now_iso8601();
    serde_json::to_string_pretty(&serde_json::json!({
        "buildDefinition": {
            "buildType": "https://cfgd.io/ModuleBuild/v1",
            "externalParameters": {
                "source": {
                    "uri": source_repo,
                    "digest": { "gitCommit": source_commit },
                }
            },
            "internalParameters": {},
            "resolvedDependencies": [],
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
        // Keyed attestation is offline PKI: keep predicates out of the
        // public Rekor transparency log (mirrors keyed signing).
        cmd.arg("--tlog-upload=false");
    }

    // Always skip the interactive consent prompt for non-interactive use.
    cmd.arg("--yes");

    apply_registry_scheme(&mut cmd, artifact_ref);
    cmd.arg("--predicate")
        .arg(attestation_path)
        .arg("--type")
        .arg("slsaprovenance1")
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

    tracing::debug!(reference = artifact_ref, "attestation attached");
    Ok(())
}

/// The `--type` name cosign knows a predicate-type URI by.
///
/// The two spellings are not interchangeable: `cosign attest --type
/// slsaprovenance1` records `https://slsa.dev/provenance/v1` in the manifest
/// annotation, so a reader echoing the URI back would be naming a string
/// [`verify_attestation`] does not accept. This is the ONE fold between the
/// wire vocabulary and the flag vocabulary, and every type cosign has a short
/// name for is listed. A predicate some other tool attached is reported
/// verbatim — which is also what `--type` takes for a type it has no name for.
#[must_use]
pub fn attestation_type_name(predicate_type: &str) -> String {
    COSIGN_PREDICATE_TYPES
        .iter()
        .find(|(uri, _)| *uri == predicate_type)
        .map_or(predicate_type, |(_, name)| *name)
        .to_string()
}

/// Every predicate-type URI cosign has a `--type` name for, and that name.
///
/// A URI appears once per name it folds to; two URIs sharing a name (the
/// CycloneDX and in-toto Link revisions) are two rows, because the fold runs
/// one way — from what a manifest recorded to what a flag accepts. Public
/// because the right column IS the vocabulary a `--type` argument and a
/// recorded attestation type are both drawn from, and a surface naming one
/// checks itself against this list rather than against a copy of it.
pub const COSIGN_PREDICATE_TYPES: &[(&str, &str)] = &[
    ("https://slsa.dev/provenance/v0.2", "slsaprovenance"),
    ("https://slsa.dev/provenance/v1", "slsaprovenance1"),
    ("https://spdx.dev/Document", "spdx"),
    ("https://cyclonedx.org/bom", "cyclonedx"),
    ("https://cyclonedx.org/schema/bom", "cyclonedx"),
    ("https://in-toto.io/Link/v1", "link"),
    ("https://in-toto.io/Link/v0.3", "link"),
    ("https://cosign.sigstore.dev/attestation/vuln/v1", "vuln"),
    ("https://cosign.sigstore.dev/attestation/v1", "custom"),
    ("https://openvex.dev/ns", "openvex"),
];

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
    apply_registry_scheme(&mut cmd, artifact_ref);
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

    tracing::debug!(reference = artifact_ref, "attestation verified");
    Ok(())
}

#[cfg(test)]
mod tests;

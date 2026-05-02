// Cosign signing + verification + SLSA in-toto attestations.
// All shell-out goes through `crate::cosign_cmd()` (the controlled cosign layer
// per module-boundaries.md).

use crate::errors::OciError;

/// Sign an OCI artifact with cosign.
///
/// If `key_path` is Some, uses `cosign sign --key <path>`.
/// If `key_path` is None, uses keyless signing (Fulcio/Rekor via OIDC).
pub fn sign_artifact(artifact_ref: &str, key_path: Option<&str>) -> Result<(), OciError> {
    crate::require_tool("cosign", None).map_err(|_| OciError::ToolNotFound {
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

    crate::require_tool("cosign", None).map_err(|_| OciError::ToolNotFound {
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
    crate::require_tool("cosign", None).map_err(|_| OciError::ToolNotFound {
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

    crate::require_tool("cosign", None).map_err(|_| OciError::ToolNotFound {
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
mod tests {
    use super::*;

    // --- Signing ---

    #[test]
    fn sign_artifact_rejects_when_cosign_missing() {
        if crate::command_available("cosign") {
            return;
        }
        let result = sign_artifact("ghcr.io/test/mod:v1", None);
        assert!(matches!(result, Err(OciError::ToolNotFound { .. })));
    }

    #[test]
    fn verify_signature_rejects_keyless_without_identity() {
        let result = verify_signature(
            "ghcr.io/test/mod:v1",
            &VerifyOptions {
                key: None,
                identity: None,
                issuer: None,
            },
        );
        assert!(matches!(result, Err(OciError::VerificationFailed { .. })));
    }

    #[test]
    fn verify_signature_rejects_when_cosign_missing() {
        if crate::command_available("cosign") {
            return;
        }
        let result = verify_signature(
            "ghcr.io/test/mod:v1",
            &VerifyOptions {
                key: Some("cosign.pub"),
                identity: None,
                issuer: None,
            },
        );
        assert!(matches!(result, Err(OciError::ToolNotFound { .. })));
    }

    // --- Attestations ---

    #[test]
    fn attach_attestation_rejects_when_cosign_missing() {
        if crate::command_available("cosign") {
            return;
        }
        let result = attach_attestation("ghcr.io/test/mod:v1", "provenance.json", None);
        assert!(matches!(result, Err(OciError::ToolNotFound { .. })));
    }

    #[test]
    fn verify_attestation_rejects_keyless_without_identity() {
        let result = verify_attestation(
            "ghcr.io/test/mod:v1",
            "slsaprovenance",
            &VerifyOptions {
                key: None,
                identity: None,
                issuer: None,
            },
        );
        assert!(matches!(result, Err(OciError::VerificationFailed { .. })));
    }

    #[test]
    fn verify_attestation_rejects_when_cosign_missing() {
        if crate::command_available("cosign") {
            return;
        }
        let result = verify_attestation(
            "ghcr.io/test/mod:v1",
            "slsaprovenance",
            &VerifyOptions {
                key: Some("cosign.pub"),
                identity: None,
                issuer: None,
            },
        );
        assert!(matches!(result, Err(OciError::ToolNotFound { .. })));
    }

    #[test]
    fn generate_slsa_provenance_creates_valid_json() {
        let prov = generate_slsa_provenance(
            "ghcr.io/test/mod:v1",
            "sha256:abc123",
            "https://github.com/myorg/myrepo",
            "abc123def",
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&prov).unwrap();
        assert_eq!(parsed["predicateType"], "https://slsa.dev/provenance/v1");
        assert_eq!(parsed["subject"][0]["name"], "ghcr.io/test/mod:v1");
        assert_eq!(parsed["subject"][0]["digest"]["sha256"], "abc123");
    }

    // --- VerifyOptions ---

    #[test]
    fn verify_options_default_keyless() {
        let opts = VerifyOptions {
            key: None,
            identity: Some("user@example.com"),
            issuer: Some("https://accounts.google.com"),
        };
        assert!(opts.key.is_none());
        assert_eq!(opts.identity.unwrap(), "user@example.com");
        assert_eq!(opts.issuer.unwrap(), "https://accounts.google.com");
    }

    // --- validate_verify_options ---

    #[test]
    fn validate_verify_options_accepts_key_only() {
        let opts = VerifyOptions {
            key: Some("cosign.pub"),
            identity: None,
            issuer: None,
        };
        let result = validate_verify_options(&opts);
        assert!(result.is_ok(), "key-only verification should be valid");
    }

    #[test]
    fn validate_verify_options_accepts_identity_only() {
        let opts = VerifyOptions {
            key: None,
            identity: Some("user@example.com"),
            issuer: None,
        };
        let result = validate_verify_options(&opts);
        assert!(result.is_ok(), "identity-only verification should be valid");
    }

    #[test]
    fn validate_verify_options_accepts_issuer_only() {
        let opts = VerifyOptions {
            key: None,
            identity: None,
            issuer: Some("https://accounts.google.com"),
        };
        let result = validate_verify_options(&opts);
        assert!(result.is_ok(), "issuer-only verification should be valid");
    }

    #[test]
    fn validate_verify_options_accepts_all_fields() {
        let opts = VerifyOptions {
            key: Some("cosign.pub"),
            identity: Some("user@example.com"),
            issuer: Some("https://accounts.google.com"),
        };
        let result = validate_verify_options(&opts);
        assert!(result.is_ok(), "all-fields verification should be valid");
    }

    #[test]
    fn validate_verify_options_rejects_all_none() {
        let opts = VerifyOptions {
            key: None,
            identity: None,
            issuer: None,
        };
        let result = validate_verify_options(&opts);
        assert!(result.is_err(), "all-none should be rejected");
        let err = result.unwrap_err();
        let err_msg = format!("{err}");
        assert!(
            err_msg.contains("keyless verification requires identity or issuer constraint"),
            "error message should explain the constraint, got: {err_msg}"
        );
    }

    // --- apply_verify_args ---

    #[test]
    fn apply_verify_args_with_key() {
        let mut cmd = std::process::Command::new("echo");
        let opts = VerifyOptions {
            key: Some("/path/to/cosign.pub"),
            identity: None,
            issuer: None,
        };
        apply_verify_args(&mut cmd, &opts);
        // Extract the args from the command
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, vec!["--key", "/path/to/cosign.pub"]);
    }

    #[test]
    fn apply_verify_args_keyless_with_identity_and_issuer() {
        let mut cmd = std::process::Command::new("echo");
        let opts = VerifyOptions {
            key: None,
            identity: Some("user@example.com"),
            issuer: Some("https://accounts.google.com"),
        };
        apply_verify_args(&mut cmd, &opts);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "--certificate-identity-regexp",
                "user@example.com",
                "--certificate-oidc-issuer-regexp",
                "https://accounts.google.com"
            ]
        );
    }

    #[test]
    fn apply_verify_args_keyless_with_identity_only_defaults_issuer() {
        let mut cmd = std::process::Command::new("echo");
        let opts = VerifyOptions {
            key: None,
            identity: Some("ci@github.com"),
            issuer: None,
        };
        apply_verify_args(&mut cmd, &opts);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        // When no issuer is provided, it defaults to ".*"
        assert_eq!(
            args,
            vec![
                "--certificate-identity-regexp",
                "ci@github.com",
                "--certificate-oidc-issuer-regexp",
                ".*"
            ]
        );
    }

    #[test]
    fn apply_verify_args_keyless_with_issuer_only_defaults_identity() {
        let mut cmd = std::process::Command::new("echo");
        let opts = VerifyOptions {
            key: None,
            identity: None,
            issuer: Some("https://token.actions.githubusercontent.com"),
        };
        apply_verify_args(&mut cmd, &opts);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        // When no identity is provided, it defaults to ".*"
        assert_eq!(
            args,
            vec![
                "--certificate-identity-regexp",
                ".*",
                "--certificate-oidc-issuer-regexp",
                "https://token.actions.githubusercontent.com"
            ]
        );
    }

    #[test]
    fn apply_verify_args_key_takes_precedence_over_keyless() {
        let mut cmd = std::process::Command::new("echo");
        let opts = VerifyOptions {
            key: Some("my.pub"),
            identity: Some("user@example.com"),
            issuer: Some("https://issuer.example.com"),
        };
        apply_verify_args(&mut cmd, &opts);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        // When key is provided, only --key should be added (no certificate args)
        assert_eq!(args, vec!["--key", "my.pub"]);
    }

    // --- generate_slsa_provenance: digest prefix stripping ---

    #[test]
    fn generate_slsa_provenance_strips_sha256_prefix() {
        let prov = generate_slsa_provenance(
            "ghcr.io/test/mod:v1",
            "sha256:deadbeef1234",
            "https://github.com/org/repo",
            "abc123",
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&prov).unwrap();
        // The sha256 prefix should be stripped from the digest value
        assert_eq!(parsed["subject"][0]["digest"]["sha256"], "deadbeef1234");
    }

    #[test]
    fn generate_slsa_provenance_handles_plain_digest() {
        let prov = generate_slsa_provenance(
            "ghcr.io/test/mod:v1",
            "plaindigest",
            "https://github.com/org/repo",
            "abc123",
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&prov).unwrap();
        // When no sha256: prefix, the whole string is used
        assert_eq!(parsed["subject"][0]["digest"]["sha256"], "plaindigest");
    }

    #[test]
    fn generate_slsa_provenance_includes_source_info() {
        let prov = generate_slsa_provenance(
            "ghcr.io/myorg/mymod:v2",
            "sha256:abcdef",
            "https://github.com/myorg/myrepo",
            "deadbeef123",
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&prov).unwrap();
        assert_eq!(parsed["_type"], "https://in-toto.io/Statement/v1");
        assert_eq!(
            parsed["predicate"]["buildDefinition"]["externalParameters"]["source"]["uri"],
            "https://github.com/myorg/myrepo"
        );
        assert_eq!(
            parsed["predicate"]["buildDefinition"]["externalParameters"]["source"]["digest"]["gitCommit"],
            "deadbeef123"
        );
        assert_eq!(
            parsed["predicate"]["runDetails"]["builder"]["id"],
            "https://cfgd.io/builder/v1"
        );
    }

    // --- generate_slsa_provenance: detailed JSON structure ---

    #[test]
    fn generate_slsa_provenance_complete_structure() {
        let prov = generate_slsa_provenance(
            "ghcr.io/org/mod:v2.0.0",
            "sha256:deadbeef1234",
            "https://github.com/org/config",
            "abc123def456",
        )
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&prov).unwrap();

        // Top-level fields
        assert_eq!(
            parsed["_type"], "https://in-toto.io/Statement/v1",
            "should be in-toto v1 statement"
        );
        assert_eq!(
            parsed["predicateType"], "https://slsa.dev/provenance/v1",
            "should be SLSA v1 provenance"
        );

        // Subject
        let subject = &parsed["subject"][0];
        assert_eq!(subject["name"], "ghcr.io/org/mod:v2.0.0");
        assert_eq!(
            subject["digest"]["sha256"], "deadbeef1234",
            "sha256: prefix should be stripped"
        );

        // Predicate
        let predicate = &parsed["predicate"];
        assert_eq!(
            predicate["buildDefinition"]["buildType"],
            "https://cfgd.io/ModuleBuild/v1"
        );
        assert_eq!(
            predicate["buildDefinition"]["externalParameters"]["source"]["uri"],
            "https://github.com/org/config"
        );
        assert_eq!(
            predicate["buildDefinition"]["externalParameters"]["source"]["digest"]["gitCommit"],
            "abc123def456"
        );
        assert_eq!(
            predicate["runDetails"]["builder"]["id"],
            "https://cfgd.io/builder/v1"
        );

        // Metadata timestamps should be non-empty
        let invocation_id = predicate["runDetails"]["metadata"]["invocationId"]
            .as_str()
            .unwrap();
        assert!(
            !invocation_id.is_empty(),
            "invocationId should not be empty"
        );
    }

    #[test]
    fn generate_slsa_provenance_strips_sha256_prefix_v2() {
        let prov =
            generate_slsa_provenance("ghcr.io/test:v1", "sha256:abcdef", "repo", "commit").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&prov).unwrap();
        assert_eq!(
            parsed["subject"][0]["digest"]["sha256"], "abcdef",
            "sha256: prefix should be stripped from digest"
        );
    }

    #[test]
    fn generate_slsa_provenance_bare_digest() {
        let prov = generate_slsa_provenance("ghcr.io/test:v1", "abcdef", "repo", "commit").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&prov).unwrap();
        assert_eq!(
            parsed["subject"][0]["digest"]["sha256"], "abcdef",
            "bare digest should pass through as-is"
        );
    }

    // --- validate_verify_options ---

    #[test]
    fn validate_verify_options_key_only_passes() {
        let opts = VerifyOptions {
            key: Some("cosign.pub"),
            identity: None,
            issuer: None,
        };
        assert!(validate_verify_options(&opts).is_ok());
    }

    #[test]
    fn validate_verify_options_identity_only_passes() {
        let opts = VerifyOptions {
            key: None,
            identity: Some("user@example.com"),
            issuer: None,
        };
        assert!(validate_verify_options(&opts).is_ok());
    }

    #[test]
    fn validate_verify_options_issuer_only_passes() {
        let opts = VerifyOptions {
            key: None,
            identity: None,
            issuer: Some("https://accounts.google.com"),
        };
        assert!(validate_verify_options(&opts).is_ok());
    }

    #[test]
    fn validate_verify_options_all_none_fails() {
        let opts = VerifyOptions {
            key: None,
            identity: None,
            issuer: None,
        };
        let result = validate_verify_options(&opts);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("keyless verification requires"),
            "should explain the requirement: {msg}"
        );
    }

    // --- apply_verify_args: verify args are set correctly ---

    #[test]
    fn apply_verify_args_with_key_only() {
        let mut cmd = std::process::Command::new("echo");
        let opts = VerifyOptions {
            key: Some("/path/to/cosign.pub"),
            identity: None,
            issuer: None,
        };
        apply_verify_args(&mut cmd, &opts);
        // Can't easily inspect Command args, but verify it doesn't panic
    }

    #[test]
    fn apply_verify_args_keyless_defaults() {
        let mut cmd = std::process::Command::new("echo");
        let opts = VerifyOptions {
            key: None,
            identity: None,
            issuer: Some("https://issuer.example.com"),
        };
        apply_verify_args(&mut cmd, &opts);
        // Again, just verify no panic; the defaults for identity are ".*"
    }
}

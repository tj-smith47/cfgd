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

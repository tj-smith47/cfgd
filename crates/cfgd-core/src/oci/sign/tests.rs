use super::*;
use crate::test_helpers::EnvVarGuard;

// --- Signing ---

#[test]
#[serial_test::serial]
fn sign_artifact_rejects_when_cosign_missing() {
    // require_cosign() honors CFGD_COSIGN_BIN before consulting PATH —
    // a parallel CosignTestShim test that set the seam to a fake cosign
    // would force ToolNotFound to NOT fire here. Pin the env to bypass.
    let _g = EnvVarGuard::unset("CFGD_COSIGN_BIN");
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
#[serial_test::serial]
fn verify_signature_rejects_when_cosign_missing() {
    let _g = EnvVarGuard::unset("CFGD_COSIGN_BIN");
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
#[serial_test::serial]
fn attach_attestation_rejects_when_cosign_missing() {
    let _g = EnvVarGuard::unset("CFGD_COSIGN_BIN");
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
#[serial_test::serial]
fn verify_attestation_rejects_when_cosign_missing() {
    let _g = EnvVarGuard::unset("CFGD_COSIGN_BIN");
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
    assert_eq!(
        args,
        vec![
            "--key",
            "/path/to/cosign.pub",
            "--insecure-ignore-tlog=true"
        ]
    );
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
    // When key is provided, only --key (+ offline tlog skip) should be added
    // (no certificate args).
    assert_eq!(args, vec!["--key", "my.pub", "--insecure-ignore-tlog=true"]);
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

// ---------------------------------------------------------------------------
// Fake-cosign shim tests — drive every cosign-shelling code path without a
// real cosign on PATH. Each test sets `CFGD_COSIGN_BIN` to a generated shell
// script that records its argv to a log file and exits with a chosen status.
//
// Tests are `#[cfg(unix)]` (the shim is a /bin/sh script) and `#[serial]`
// because env-var mutation is process-global.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod fake_cosign {
    use super::*;
    use crate::test_helpers::CosignTestShim;
    use serial_test::serial;

    // --- sign_artifact ---

    #[test]
    #[serial]
    fn sign_artifact_keyless_invokes_sign_yes_subcommand_and_returns_ok() {
        let guard = CosignTestShim::install();
        let result = sign_artifact("ghcr.io/test/mod:v1", None);
        assert!(result.is_ok(), "happy keyless sign returns Ok: {result:?}");
        let argv = guard.argv_log();
        assert!(argv.contains("sign"), "argv must include 'sign': {argv}");
        assert!(argv.contains("--yes"), "keyless sign passes --yes: {argv}");
        assert!(
            !argv.contains("--tlog-upload=false"),
            "keyless sign keeps Rekor (no --tlog-upload=false): {argv}"
        );
        assert!(
            argv.contains("ghcr.io/test/mod:v1"),
            "argv ends with artifact ref: {argv}"
        );
    }

    #[test]
    #[serial]
    fn sign_artifact_with_key_path_passes_key_flag() {
        let guard = CosignTestShim::install();
        let result = sign_artifact("ghcr.io/test/mod:v1", Some("/keys/cosign.key"));
        assert!(result.is_ok(), "with-key sign returns Ok");
        let argv = guard.argv_log();
        assert!(
            argv.contains("--key /keys/cosign.key"),
            "argv must include --key: {argv}"
        );
        assert!(
            argv.contains("--yes"),
            "with-key sign passes --yes for non-interactive use: {argv}"
        );
        assert!(
            argv.contains("--tlog-upload=false"),
            "keyed sign is offline: must disable Rekor tlog upload: {argv}"
        );
    }

    #[test]
    #[serial]
    fn sign_artifact_propagates_cosign_failure_with_stderr_message() {
        let _guard = CosignTestShim::builder()
            .with_exit(1)
            .with_stderr("rekor unreachable")
            .install();
        let result = sign_artifact("ghcr.io/test/mod:v1", None);
        let err = result.expect_err("non-zero cosign must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("rekor unreachable"),
            "error must surface stderr: {msg}"
        );
        assert!(
            msg.contains("cosign sign failed"),
            "error prefixes with `cosign sign failed`: {msg}"
        );
    }

    // --- verify_signature ---

    #[test]
    #[serial]
    fn verify_signature_keyless_passes_identity_and_issuer_constraints() {
        let guard = CosignTestShim::install();
        let result = verify_signature(
            "ghcr.io/myorg/mod:v1",
            &VerifyOptions {
                key: None,
                identity: Some("user@example.com"),
                issuer: Some("https://accounts.google.com"),
            },
        );
        assert!(
            result.is_ok(),
            "happy keyless verify returns Ok: {result:?}"
        );
        let argv = guard.argv_log();
        assert!(
            argv.contains("verify"),
            "argv must include 'verify': {argv}"
        );
        assert!(
            argv.contains("--certificate-identity-regexp user@example.com"),
            "argv must pin identity: {argv}"
        );
        assert!(
            argv.contains("--certificate-oidc-issuer-regexp https://accounts.google.com"),
            "argv must pin issuer: {argv}"
        );
    }

    #[test]
    #[serial]
    fn verify_signature_with_key_takes_priority_over_identity() {
        let guard = CosignTestShim::install();
        let result = verify_signature(
            "ghcr.io/myorg/mod:v1",
            &VerifyOptions {
                key: Some("/keys/cosign.pub"),
                identity: None,
                issuer: None,
            },
        );
        assert!(result.is_ok());
        let argv = guard.argv_log();
        assert!(
            argv.contains("--key /keys/cosign.pub"),
            "argv must include --key: {argv}"
        );
        assert!(
            argv.contains("--insecure-ignore-tlog=true"),
            "keyed verify skips the tlog (offline signatures): {argv}"
        );
        assert!(
            !argv.contains("--certificate-identity-regexp"),
            "--key path must NOT add identity-regexp: {argv}"
        );
    }

    #[test]
    #[serial]
    fn verify_signature_propagates_cosign_failure_with_artifact_ref() {
        let _guard = CosignTestShim::builder()
            .with_exit(1)
            .with_stderr("signature mismatch")
            .install();
        let result = verify_signature(
            "ghcr.io/myorg/mod:v1",
            &VerifyOptions {
                key: Some("/keys/cosign.pub"),
                identity: None,
                issuer: None,
            },
        );
        let err = result.expect_err("non-zero verify must error");
        match err {
            OciError::VerificationFailed { reference, message } => {
                assert_eq!(reference, "ghcr.io/myorg/mod:v1");
                assert!(
                    message.contains("signature mismatch"),
                    "stderr surfaced in message: {message}"
                );
            }
            other => panic!("expected VerificationFailed, got {other:?}"),
        }
    }

    // --- attach_attestation ---

    #[test]
    #[serial]
    fn attach_attestation_passes_predicate_and_type_flags() {
        let guard = CosignTestShim::install();
        let result = attach_attestation(
            "ghcr.io/myorg/mod:v1",
            "/tmp/provenance.json",
            Some("/keys/cosign.key"),
        );
        assert!(result.is_ok(), "attach returns Ok: {result:?}");
        let argv = guard.argv_log();
        assert!(
            argv.contains("attest"),
            "argv must include 'attest': {argv}"
        );
        assert!(
            argv.contains("--predicate /tmp/provenance.json"),
            "argv pins predicate path: {argv}"
        );
        assert!(
            argv.contains("--type slsaprovenance"),
            "argv pins predicate type: {argv}"
        );
        assert!(
            argv.contains("--key /keys/cosign.key"),
            "with-key attest passes --key: {argv}"
        );
        assert!(
            argv.contains("--yes"),
            "with-key attest passes --yes for non-interactive use: {argv}"
        );
        assert!(
            argv.contains("--tlog-upload=false"),
            "keyed attest is offline: must disable Rekor tlog upload: {argv}"
        );
    }

    #[test]
    #[serial]
    fn attach_attestation_keyless_uses_yes_flag() {
        let guard = CosignTestShim::install();
        let result = attach_attestation("ghcr.io/myorg/mod:v1", "/tmp/p.json", None);
        assert!(result.is_ok());
        let argv = guard.argv_log();
        assert!(
            argv.contains("--yes"),
            "keyless attest passes --yes: {argv}"
        );
        assert!(
            !argv.contains("--tlog-upload=false"),
            "keyless attest keeps Rekor (no --tlog-upload=false): {argv}"
        );
        assert!(
            !argv.contains("--key"),
            "keyless attest must NOT pass --key: {argv}"
        );
    }

    // --- verify_attestation ---

    #[test]
    #[serial]
    fn verify_attestation_runs_verify_attestation_subcommand() {
        let guard = CosignTestShim::install();
        let result = verify_attestation(
            "ghcr.io/myorg/mod:v1",
            "slsaprovenance",
            &VerifyOptions {
                key: Some("/keys/cosign.pub"),
                identity: None,
                issuer: None,
            },
        );
        assert!(result.is_ok(), "verify_attestation returns Ok: {result:?}");
        let argv = guard.argv_log();
        assert!(
            argv.contains("verify-attestation"),
            "argv must use verify-attestation subcommand: {argv}"
        );
        assert!(
            argv.contains("--type slsaprovenance"),
            "argv pins --type: {argv}"
        );
        assert!(
            argv.contains("ghcr.io/myorg/mod:v1"),
            "argv ends with artifact ref: {argv}"
        );
    }

    #[test]
    #[serial]
    fn verify_attestation_propagates_failure_with_stderr() {
        let _guard = CosignTestShim::builder()
            .with_exit(1)
            .with_stderr("no matching attestations")
            .install();
        let result = verify_attestation(
            "ghcr.io/myorg/mod:v1",
            "slsaprovenance",
            &VerifyOptions {
                key: Some("/keys/cosign.pub"),
                identity: None,
                issuer: None,
            },
        );
        let err = result.expect_err("non-zero verify-attestation must error");
        match err {
            OciError::AttestationError { message } => {
                assert!(
                    message.contains("no matching attestations"),
                    "stderr surfaced: {message}"
                );
            }
            other => panic!("expected AttestationError, got {other:?}"),
        }
    }

    // --- attach_attestation failure (non-zero cosign exit) ---

    #[test]
    #[serial]
    fn attach_attestation_propagates_failure_with_stderr() {
        let _guard = CosignTestShim::builder()
            .with_exit(1)
            .with_stderr("predicate rejected")
            .install();
        let result = attach_attestation("ghcr.io/myorg/mod:v1", "/tmp/p.json", None);
        let err = result.expect_err("non-zero attest must error");
        match err {
            OciError::AttestationError { message } => {
                assert!(
                    message.contains("predicate rejected"),
                    "stderr surfaced in message: {message}"
                );
                assert!(
                    message.contains("cosign attest failed"),
                    "message prefixes with `cosign attest failed`: {message}"
                );
            }
            other => panic!("expected AttestationError, got {other:?}"),
        }
    }

    // --- IO spawn failure: CFGD_COSIGN_BIN points at a non-executable file ---
    //
    // `require_cosign()` only checks that the seam path is a regular file, so a
    // non-executable file passes the availability gate but makes `cmd.output()`
    // fail to spawn. These cover the `failed to run cosign: {e}` map_err arms
    // that the happy-path shim tests never reach.

    /// Create a regular but non-executable file and point CFGD_COSIGN_BIN at it.
    /// Returns the guard (restores prior env on drop) and keeps the tempdir alive.
    fn install_unspawnable_cosign() -> (EnvVarGuard, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("fake-cosign-noexec");
        // Mode 0o644 — a real file (passes require_cosign) that cannot be exec'd.
        std::fs::write(&bin, "not an executable\n").expect("write file");
        let guard = EnvVarGuard::set("CFGD_COSIGN_BIN", bin.to_str().unwrap());
        (guard, dir)
    }

    #[test]
    #[serial]
    fn sign_artifact_maps_spawn_failure_to_signing_error() {
        let (_g, _dir) = install_unspawnable_cosign();
        let result = sign_artifact("ghcr.io/test/mod:v1", None);
        match result {
            Err(OciError::SigningError { message }) => {
                assert!(
                    message.contains("failed to run cosign"),
                    "spawn failure must map to `failed to run cosign`: {message}"
                );
            }
            other => panic!("expected SigningError from spawn failure, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn verify_signature_maps_spawn_failure_to_verification_error() {
        let (_g, _dir) = install_unspawnable_cosign();
        let result = verify_signature(
            "ghcr.io/test/mod:v1",
            &VerifyOptions {
                key: Some("/keys/cosign.pub"),
                identity: None,
                issuer: None,
            },
        );
        match result {
            Err(OciError::VerificationFailed { reference, message }) => {
                assert_eq!(reference, "ghcr.io/test/mod:v1");
                assert!(
                    message.contains("failed to run cosign"),
                    "spawn failure must map to `failed to run cosign`: {message}"
                );
            }
            other => panic!("expected VerificationFailed from spawn failure, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn attach_attestation_maps_spawn_failure_to_attestation_error() {
        let (_g, _dir) = install_unspawnable_cosign();
        let result = attach_attestation("ghcr.io/test/mod:v1", "/tmp/p.json", None);
        match result {
            Err(OciError::AttestationError { message }) => {
                assert!(
                    message.contains("failed to run cosign attest"),
                    "spawn failure must map to `failed to run cosign attest`: {message}"
                );
            }
            other => panic!("expected AttestationError from spawn failure, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn verify_attestation_maps_spawn_failure_to_attestation_error() {
        let (_g, _dir) = install_unspawnable_cosign();
        let result = verify_attestation(
            "ghcr.io/test/mod:v1",
            "slsaprovenance",
            &VerifyOptions {
                key: Some("/keys/cosign.pub"),
                identity: None,
                issuer: None,
            },
        );
        match result {
            Err(OciError::AttestationError { message }) => {
                assert!(
                    message.contains("failed to run cosign verify-attestation"),
                    "spawn failure must map to `failed to run cosign verify-attestation`: {message}"
                );
            }
            other => panic!("expected AttestationError from spawn failure, got {other:?}"),
        }
    }
}

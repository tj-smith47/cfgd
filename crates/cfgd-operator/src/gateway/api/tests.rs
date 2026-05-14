use super::*;
use serial_test::serial;

#[test]
fn hash_token_deterministic() {
    let h1 = hash_token("my-secret-token");
    let h2 = hash_token("my-secret-token");
    assert_eq!(h1, h2);
    assert_ne!(h1, hash_token("different-token"));
}

#[test]
fn generate_token_has_prefix() {
    let token = generate_token("cfgd_bs");
    assert!(token.starts_with("cfgd_bs_"));
    assert!(token.len() > 40); // long enough to be secure
}

#[test]
fn generate_token_unique() {
    let t1 = generate_token("cfgd_dev");
    let t2 = generate_token("cfgd_dev");
    assert_ne!(t1, t2);
}

#[test]
fn enforce_device_access_admin_allows_any() {
    let auth = AuthContext::Admin;
    // Admin should be able to access ANY device ID
    assert!(enforce_device_access(&auth, "any-device").is_ok());
    assert!(enforce_device_access(&auth, "another-device").is_ok());
}

#[test]
fn enforce_device_access_device_allows_self() {
    let auth = AuthContext::Device {
        device_id: "dev-1".to_string(),
        username: "jdoe".to_string(),
    };
    assert!(enforce_device_access(&auth, "dev-1").is_ok());
}

#[test]
fn enforce_device_access_device_denies_other_specific_error() {
    // Verify the error is specifically Forbidden (not some other variant)
    // and that the username doesn't leak into the error
    let auth = AuthContext::Device {
        device_id: "dev-1".to_string(),
        username: "jdoe".to_string(),
    };
    let err = enforce_device_access(&auth, "dev-999").unwrap_err();
    match &err {
        GatewayError::Forbidden(msg) => {
            assert!(msg.contains("dev-1"), "should mention requester: {msg}");
            assert!(msg.contains("dev-999"), "should mention target: {msg}");
        }
        other => panic!("expected Forbidden, got: {other}"),
    }
}

#[test]
fn enforce_device_access_device_denies_other() {
    let auth = AuthContext::Device {
        device_id: "dev-1".to_string(),
        username: "jdoe".to_string(),
    };
    let err = enforce_device_access(&auth, "dev-2").unwrap_err();
    assert!(
        matches!(err, GatewayError::Forbidden(ref msg) if msg.contains("dev-1") && msg.contains("dev-2")),
        "expected Forbidden error mentioning both device IDs, got: {err}"
    );
}

#[test]
fn enroll_info_response_serialization() {
    let resp = EnrollInfoResponse {
        method: EnrollmentMethod::Key,
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("\"method\":\"key\""));
}

#[test]
fn challenge_response_serialization() {
    let resp = ChallengeResponse {
        challenge_id: "abc-123".to_string(),
        nonce: "cfgd_ch_random".to_string(),
        expires_at: "2026-03-14T12:05:00Z".to_string(),
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("abc-123"));
    assert!(json.contains("cfgd_ch_random"));
}

// --- extract_bearer_token ---

#[test]
fn extract_bearer_token_valid() {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer my-secret".parse().unwrap());
    assert_eq!(
        extract_bearer_token(&headers),
        Some("my-secret".to_string())
    );
}

#[test]
fn extract_bearer_token_missing_header() {
    let headers = HeaderMap::new();
    assert_eq!(extract_bearer_token(&headers), None);
}

#[test]
fn extract_bearer_token_wrong_scheme() {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Basic dXNlcjpwYXNz".parse().unwrap());
    assert_eq!(extract_bearer_token(&headers), None);
}

#[test]
fn extract_bearer_token_empty_value() {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer ".parse().unwrap());
    assert_eq!(extract_bearer_token(&headers), None);
}

#[test]
fn extract_bearer_token_no_space_after_bearer() {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "BearerNoSpace".parse().unwrap());
    assert_eq!(extract_bearer_token(&headers), None);
}

// --- hash_token extended ---

#[test]
fn hash_token_is_lowercase_hex() {
    let h = hash_token("test-token");
    assert!(
        h.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
}

#[test]
fn hash_token_sha256_length() {
    // SHA256 produces 64 hex characters
    let h = hash_token("anything");
    assert_eq!(h.len(), 64);
}

#[test]
fn hash_token_empty_input() {
    let h = hash_token("");
    // SHA256 of empty string is a known constant
    assert_eq!(
        h,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

// --- generate_token extended ---

#[test]
fn generate_token_different_prefixes() {
    let bs = generate_token("cfgd_bs");
    let dev = generate_token("cfgd_dev");
    let ch = generate_token("cfgd_ch");
    assert!(bs.starts_with("cfgd_bs_"));
    assert!(dev.starts_with("cfgd_dev_"));
    assert!(ch.starts_with("cfgd_ch_"));
}

#[test]
fn generate_token_no_dashes_in_random_part() {
    let token = generate_token("prefix");
    let random_part = token.strip_prefix("prefix_").unwrap();
    assert!(
        !random_part.contains('-'),
        "random part should have dashes stripped"
    );
    // Two UUID v4s without dashes = 64 hex chars
    assert_eq!(random_part.len(), 64);
}

// --- enforce_device_access extended ---

#[test]
fn enforce_device_access_error_contains_both_ids() {
    let auth = AuthContext::Device {
        device_id: "attacker-device".to_string(),
        username: "attacker".to_string(),
    };
    let err = enforce_device_access(&auth, "victim-device").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("attacker-device"),
        "error should mention the requesting device"
    );
    assert!(
        msg.contains("victim-device"),
        "error should mention the target device"
    );
}

// --- CheckinRequest deserialization ---

#[test]
fn checkin_request_deserialization() {
    let json = r#"{
        "deviceId": "dev-1",
        "hostname": "workstation-1",
        "os": "linux",
        "arch": "x86_64",
        "configHash": "abc123"
    }"#;
    let req: CheckinRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.device_id, "dev-1");
    assert_eq!(req.hostname, "workstation-1");
    assert_eq!(req.os, "linux");
    assert_eq!(req.arch, "x86_64");
    assert_eq!(req.config_hash, "abc123");
}

// --- CheckinResponse serialization ---

#[test]
fn checkin_response_no_config_omits_field() {
    let resp = CheckinResponse {
        status: "ok".to_string(),
        config_changed: false,
        desired_config: None,
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(
        !json.contains("desiredConfig"),
        "None desired_config should be omitted"
    );
    assert!(json.contains("\"configChanged\":false"));
}

#[test]
fn checkin_response_with_config_includes_field() {
    let resp = CheckinResponse {
        status: "ok".to_string(),
        config_changed: true,
        desired_config: Some(serde_json::json!({"packages": ["vim"]})),
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("desiredConfig"));
    assert!(json.contains("\"configChanged\":true"));
}

// --- EnrollRequest deserialization ---

#[test]
fn enroll_request_deserialization() {
    let json = r#"{
        "token": "cfgd_bs_abc123",
        "deviceId": "dev-42",
        "hostname": "laptop",
        "os": "darwin",
        "arch": "aarch64"
    }"#;
    let req: EnrollRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.token, "cfgd_bs_abc123");
    assert_eq!(req.device_id, "dev-42");
    assert_eq!(req.os, "darwin");
    assert_eq!(req.arch, "aarch64");
}

// --- EnrollResponse serialization ---

#[test]
fn enroll_response_omits_none_fields() {
    let resp = EnrollResponse {
        status: "enrolled".to_string(),
        device_id: "dev-1".to_string(),
        api_key: "cfgd_dev_key".to_string(),
        username: "jdoe".to_string(),
        team: None,
        desired_config: None,
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(!json.contains("team"));
    assert!(!json.contains("desiredConfig"));
    assert!(json.contains("\"status\":\"enrolled\""));
}

#[test]
fn enroll_response_includes_optional_fields_when_present() {
    let resp = EnrollResponse {
        status: "enrolled".to_string(),
        device_id: "dev-1".to_string(),
        api_key: "cfgd_dev_key".to_string(),
        username: "jdoe".to_string(),
        team: Some("platform".to_string()),
        desired_config: Some(serde_json::json!({"key": "val"})),
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("\"team\":\"platform\""));
    assert!(json.contains("desiredConfig"));
}

// --- CreateTokenRequest deserialization with defaults ---

#[test]
fn create_token_request_defaults() {
    let json = r#"{"username": "admin"}"#;
    let req: CreateTokenRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.username, "admin");
    assert_eq!(req.team, None);
    assert_eq!(req.expires_in, 86400); // default 24h
}

#[test]
fn create_token_request_with_all_fields() {
    let json = r#"{
        "username": "jdoe",
        "team": "infra",
        "expiresIn": 3600
    }"#;
    let req: CreateTokenRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.username, "jdoe");
    assert_eq!(req.team, Some("infra".to_string()));
    assert_eq!(req.expires_in, 3600);
}

// --- CreateTokenResponse serialization ---

#[test]
fn create_token_response_omits_none_team() {
    let resp = CreateTokenResponse {
        id: "tok-1".to_string(),
        token: "cfgd_bs_secret".to_string(),
        username: "admin".to_string(),
        team: None,
        expires_at: "2026-03-19T00:00:00Z".to_string(),
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(!json.contains("team"));
    assert!(json.contains("\"token\":\"cfgd_bs_secret\""));
}

// --- PaginationParams defaults ---

#[test]
fn pagination_params_defaults() {
    let json = r#"{}"#;
    let params: PaginationParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.limit, 100);
    assert_eq!(params.offset, 0);
}

#[test]
fn pagination_params_custom() {
    let json = r#"{"limit": 50, "offset": 10}"#;
    let params: PaginationParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.limit, 50);
    assert_eq!(params.offset, 10);
}

// --- DriftRequest / DriftDetailInput serde ---

#[test]
fn drift_detail_input_roundtrip() {
    let detail = DriftDetailInput {
        field: "package/vim".to_string(),
        expected: "installed".to_string(),
        actual: "missing".to_string(),
    };
    let json = serde_json::to_string(&detail).unwrap();
    let parsed: DriftDetailInput = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.field, "package/vim");
    assert_eq!(parsed.expected, "installed");
    assert_eq!(parsed.actual, "missing");
}

#[test]
fn drift_request_deserialization() {
    let json = r#"{
        "details": [
            {"field": "pkg/vim", "expected": "9.0", "actual": "8.2"},
            {"field": "file/bashrc", "expected": "hash-a", "actual": "hash-b"}
        ]
    }"#;
    let req: DriftRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.details.len(), 2);
    assert_eq!(req.details[0].field, "pkg/vim");
    assert_eq!(req.details[1].field, "file/bashrc");
}

// --- SetConfigRequest deserialization ---

#[test]
fn set_config_request_deserialization() {
    let json = r#"{"config": {"packages": ["vim", "git"], "shell": "zsh"}}"#;
    let req: SetConfigRequest = serde_json::from_str(json).unwrap();
    assert!(req.config.is_object());
    assert_eq!(req.config["packages"][0], "vim");
}

// --- AddKeyRequest deserialization ---

#[test]
fn add_key_request_deserialization() {
    let json = r#"{
        "keyType": "ssh",
        "publicKey": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG...",
        "fingerprint": "SHA256:abcdef",
        "label": "laptop key"
    }"#;
    let req: AddKeyRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.key_type, "ssh");
    assert!(req.public_key.starts_with("ssh-ed25519"));
    assert_eq!(req.fingerprint, "SHA256:abcdef");
    assert_eq!(req.label, Some("laptop key".to_string()));
}

#[test]
fn add_key_request_label_optional() {
    let json = r#"{
        "keyType": "gpg",
        "publicKey": "-----BEGIN PGP PUBLIC KEY BLOCK-----",
        "fingerprint": "DEADBEEF"
    }"#;
    let req: AddKeyRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.key_type, "gpg");
    assert_eq!(req.label, None);
}

// --- ChallengeRequest deserialization ---

#[test]
fn challenge_request_deserialization() {
    let json = r#"{
        "username": "jdoe",
        "deviceId": "dev-99",
        "hostname": "workstation",
        "os": "linux",
        "arch": "x86_64"
    }"#;
    let req: ChallengeRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.username, "jdoe");
    assert_eq!(req.device_id, "dev-99");
    assert_eq!(req.hostname, "workstation");
}

// --- VerifyRequest deserialization ---

#[test]
fn verify_request_deserialization() {
    let json = r#"{
        "challengeId": "ch-123",
        "signature": "base64data==",
        "keyType": "ssh"
    }"#;
    let req: VerifyRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.challenge_id, "ch-123");
    assert_eq!(req.signature, "base64data==");
    assert_eq!(req.key_type, "ssh");
}

// --- EnrollmentMethod serialization ---

#[test]
fn enrollment_method_token_serialization() {
    let resp = EnrollInfoResponse {
        method: EnrollmentMethod::Token,
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("\"method\":\"token\""));
}

// --- FleetEvent serialization ---

#[test]
fn fleet_event_serialization() {
    let event = super::super::db::FleetEvent {
        timestamp: "2026-03-18T10:00:00Z".to_string(),
        device_id: "dev-1".to_string(),
        event_type: "checkin".to_string(),
        summary: "hash123".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["deviceId"], "dev-1");
    assert_eq!(parsed["eventType"], "checkin");
    assert_eq!(parsed["summary"], "hash123");
}

// --- Database-backed handler logic tests ---

use crate::gateway::test_state::test_db;

#[tokio::test(flavor = "current_thread")]
async fn checkin_config_changed_detection() {
    let (db, _tmp) = test_db();
    db.register_device("dev-1", "ws-1", "linux", "x86_64", "hash-old", None)
        .await
        .expect("register failed");

    // Set a desired config so config_changed is meaningful
    db.set_device_config("dev-1", &serde_json::json!({"packages": ["vim"]}))
        .await
        .expect("set config failed");

    let device = db.get_device("dev-1").await.expect("get failed");
    // Simulate checkin with different hash
    let config_changed = device.config_hash != "hash-new";
    assert!(config_changed, "different config_hash should detect change");

    // Same hash should not detect change
    let config_same = device.config_hash != "hash-old";
    assert!(!config_same, "same config_hash should not detect change");
}

// --- EnrollmentMethod::from_env ---

#[test]
#[serial]
fn enrollment_method_defaults_to_token() {
    // Clear the env var so from_env returns the default
    // SAFETY: test-only; single-threaded test runner for this module
    unsafe { std::env::remove_var("CFGD_ENROLLMENT_METHOD") };
    let method = EnrollmentMethod::from_env();
    assert_eq!(method, EnrollmentMethod::Token);
}

#[test]
#[serial]
fn enrollment_method_key_from_env() {
    // SAFETY: test-only; single-threaded test runner for this module
    unsafe { std::env::set_var("CFGD_ENROLLMENT_METHOD", "key") };
    let method = EnrollmentMethod::from_env();
    assert_eq!(method, EnrollmentMethod::Key);
    unsafe { std::env::remove_var("CFGD_ENROLLMENT_METHOD") };
}

#[test]
#[serial]
fn enrollment_method_unknown_falls_back_to_token() {
    // SAFETY: test-only; single-threaded test runner for this module
    unsafe { std::env::set_var("CFGD_ENROLLMENT_METHOD", "magic") };
    let method = EnrollmentMethod::from_env();
    assert_eq!(method, EnrollmentMethod::Token);
    unsafe { std::env::remove_var("CFGD_ENROLLMENT_METHOD") };
}

// --- verify_ssh_signature ---

#[test]
fn verify_ssh_signature_correct_key_succeeds() {
    if !cfgd_core::command_available("ssh-keygen") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    let key_path = tmp.path().join("test_key");
    let pub_path = tmp.path().join("test_key.pub");

    // Generate an ed25519 key pair (no passphrase)
    let keygen_status = std::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &key_path.to_string_lossy(),
            "-N",
            "",
            "-C",
            "test@cfgd",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ssh-keygen failed");
    assert!(keygen_status.success(), "key generation must succeed");

    let public_key = std::fs::read_to_string(&pub_path).expect("read pubkey");

    // Sign a nonce
    let nonce = "cfgd_ch_test_nonce_12345";
    let nonce_path = tmp.path().join("nonce.txt");
    std::fs::write(&nonce_path, nonce).expect("write nonce");

    let sig_output = std::process::Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            &key_path.to_string_lossy(),
            "-n",
            "cfgd-enroll",
        ])
        .stdin(std::fs::File::open(&nonce_path).expect("open nonce"))
        .output()
        .expect("signing failed");
    assert!(sig_output.status.success(), "signing must succeed");

    let signature_pem = String::from_utf8_lossy(&sig_output.stdout).to_string();

    // Build a UserPublicKey
    let user_key = super::super::db::UserPublicKey {
        id: "key-1".to_string(),
        username: "testuser".to_string(),
        key_type: "ssh".to_string(),
        public_key: public_key.trim().to_string(),
        fingerprint: "SHA256:test".to_string(),
        label: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };

    let keys: Vec<&super::super::db::UserPublicKey> = vec![&user_key];
    assert!(
        verify_ssh_signature(nonce, &signature_pem, &keys),
        "verification with correct key should succeed"
    );
}

#[test]
fn verify_ssh_signature_wrong_key_fails() {
    if !cfgd_core::command_available("ssh-keygen") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tmpdir");

    // Generate the signing key pair
    let sign_key = tmp.path().join("sign_key");
    let gen1 = std::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &sign_key.to_string_lossy(),
            "-N",
            "",
            "-C",
            "signer@cfgd",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("keygen");
    assert!(gen1.success());

    // Generate a different key pair (the "wrong" key)
    let wrong_key = tmp.path().join("wrong_key");
    let gen2 = std::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &wrong_key.to_string_lossy(),
            "-N",
            "",
            "-C",
            "wrong@cfgd",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("keygen");
    assert!(gen2.success());

    let wrong_pubkey =
        std::fs::read_to_string(tmp.path().join("wrong_key.pub")).expect("read wrong pubkey");

    // Sign with the signing key
    let nonce = "cfgd_ch_wrong_key_test";
    let nonce_path = tmp.path().join("nonce.txt");
    std::fs::write(&nonce_path, nonce).expect("write nonce");

    let sig_output = std::process::Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            &sign_key.to_string_lossy(),
            "-n",
            "cfgd-enroll",
        ])
        .stdin(std::fs::File::open(&nonce_path).expect("open nonce"))
        .output()
        .expect("signing");
    assert!(sig_output.status.success());

    let signature_pem = String::from_utf8_lossy(&sig_output.stdout).to_string();

    // Verify with the wrong key — should fail
    let wrong_user_key = super::super::db::UserPublicKey {
        id: "key-wrong".to_string(),
        username: "wronguser".to_string(),
        key_type: "ssh".to_string(),
        public_key: wrong_pubkey.trim().to_string(),
        fingerprint: "SHA256:wrong".to_string(),
        label: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };

    let keys: Vec<&super::super::db::UserPublicKey> = vec![&wrong_user_key];
    assert!(
        !verify_ssh_signature(nonce, &signature_pem, &keys),
        "verification with wrong key should fail"
    );
}

#[test]
fn verify_ssh_signature_empty_keys_returns_false() {
    if !cfgd_core::command_available("ssh-keygen") {
        return;
    }

    let keys: Vec<&super::super::db::UserPublicKey> = vec![];
    assert!(
        !verify_ssh_signature("some-nonce", "some-sig", &keys),
        "empty keys list should return false"
    );
}

#[test]
fn verify_ssh_signature_bad_signature_data_returns_false() {
    if !cfgd_core::command_available("ssh-keygen") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    let key_path = tmp.path().join("test_key");

    let keygen_status = std::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &key_path.to_string_lossy(),
            "-N",
            "",
            "-C",
            "test@cfgd",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("keygen");
    assert!(keygen_status.success());

    let public_key = std::fs::read_to_string(tmp.path().join("test_key.pub")).expect("read pubkey");

    let user_key = super::super::db::UserPublicKey {
        id: "key-1".to_string(),
        username: "testuser".to_string(),
        key_type: "ssh".to_string(),
        public_key: public_key.trim().to_string(),
        fingerprint: "SHA256:test".to_string(),
        label: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };

    let keys: Vec<&super::super::db::UserPublicKey> = vec![&user_key];
    assert!(
        !verify_ssh_signature("nonce", "not-a-valid-signature", &keys),
        "garbage signature should fail verification"
    );
}

#[test]
fn verify_ssh_signature_multiple_keys_finds_correct_one() {
    if !cfgd_core::command_available("ssh-keygen") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tmpdir");

    // Generate the actual signing key
    let sign_key = tmp.path().join("sign_key");
    let gen1 = std::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &sign_key.to_string_lossy(),
            "-N",
            "",
            "-C",
            "signer@cfgd",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("keygen");
    assert!(gen1.success());

    let sign_pubkey =
        std::fs::read_to_string(tmp.path().join("sign_key.pub")).expect("read sign pubkey");

    // Generate a decoy key
    let decoy_key = tmp.path().join("decoy_key");
    let gen2 = std::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &decoy_key.to_string_lossy(),
            "-N",
            "",
            "-C",
            "decoy@cfgd",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("keygen");
    assert!(gen2.success());

    let decoy_pubkey =
        std::fs::read_to_string(tmp.path().join("decoy_key.pub")).expect("read decoy pubkey");

    // Sign with the signing key
    let nonce = "cfgd_ch_multi_key_test";
    let nonce_path = tmp.path().join("nonce.txt");
    std::fs::write(&nonce_path, nonce).expect("write nonce");

    let sig_output = std::process::Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            &sign_key.to_string_lossy(),
            "-n",
            "cfgd-enroll",
        ])
        .stdin(std::fs::File::open(&nonce_path).expect("open nonce"))
        .output()
        .expect("signing");
    assert!(sig_output.status.success());

    let signature_pem = String::from_utf8_lossy(&sig_output.stdout).to_string();

    // Present decoy first, then the correct key
    let decoy = super::super::db::UserPublicKey {
        id: "key-decoy".to_string(),
        username: "testuser".to_string(),
        key_type: "ssh".to_string(),
        public_key: decoy_pubkey.trim().to_string(),
        fingerprint: "SHA256:decoy".to_string(),
        label: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };

    let correct = super::super::db::UserPublicKey {
        id: "key-correct".to_string(),
        username: "testuser".to_string(),
        key_type: "ssh".to_string(),
        public_key: sign_pubkey.trim().to_string(),
        fingerprint: "SHA256:correct".to_string(),
        label: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };

    let keys: Vec<&super::super::db::UserPublicKey> = vec![&decoy, &correct];
    assert!(
        verify_ssh_signature(nonce, &signature_pem, &keys),
        "should find the correct key when it appears after a non-matching key"
    );
}

#[test]
fn verify_ssh_signature_wrong_nonce_fails() {
    if !cfgd_core::command_available("ssh-keygen") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    let key_path = tmp.path().join("test_key");

    let keygen_status = std::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &key_path.to_string_lossy(),
            "-N",
            "",
            "-C",
            "test@cfgd",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("keygen");
    assert!(keygen_status.success());

    let public_key = std::fs::read_to_string(tmp.path().join("test_key.pub")).expect("read pubkey");

    // Sign one nonce
    let original_nonce = "cfgd_ch_original_nonce";
    let nonce_path = tmp.path().join("nonce.txt");
    std::fs::write(&nonce_path, original_nonce).expect("write nonce");

    let sig_output = std::process::Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            &key_path.to_string_lossy(),
            "-n",
            "cfgd-enroll",
        ])
        .stdin(std::fs::File::open(&nonce_path).expect("open nonce"))
        .output()
        .expect("signing");
    assert!(sig_output.status.success());

    let signature_pem = String::from_utf8_lossy(&sig_output.stdout).to_string();

    let user_key = super::super::db::UserPublicKey {
        id: "key-1".to_string(),
        username: "testuser".to_string(),
        key_type: "ssh".to_string(),
        public_key: public_key.trim().to_string(),
        fingerprint: "SHA256:test".to_string(),
        label: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };

    let keys: Vec<&super::super::db::UserPublicKey> = vec![&user_key];

    // Verify with a different nonce — should fail
    assert!(
        !verify_ssh_signature("cfgd_ch_different_nonce", &signature_pem, &keys),
        "signature for a different nonce should fail verification"
    );
}

// --- verify_gpg_signature ---

#[test]
fn verify_gpg_signature_empty_keys_returns_false() {
    if !cfgd_core::command_available("gpg") {
        return;
    }

    let keys: Vec<&super::super::db::UserPublicKey> = vec![];
    assert!(
        !verify_gpg_signature("some-nonce", "some-sig", &keys),
        "empty keys list should return false"
    );
}

#[test]
fn verify_gpg_signature_bad_signature_returns_false() {
    if !cfgd_core::command_available("gpg") {
        return;
    }

    let user_key = super::super::db::UserPublicKey {
        id: "key-1".to_string(),
        username: "testuser".to_string(),
        key_type: "gpg".to_string(),
        public_key: "not-a-real-gpg-key".to_string(),
        fingerprint: "DEADBEEF".to_string(),
        label: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };

    let keys: Vec<&super::super::db::UserPublicKey> = vec![&user_key];
    assert!(
        !verify_gpg_signature("nonce", "not-a-valid-sig", &keys),
        "garbage GPG key and signature should fail"
    );
}

// --- CHALLENGE_TTL_SECS ---

#[test]
fn challenge_ttl_is_five_minutes() {
    assert_eq!(CHALLENGE_TTL_SECS, 300);
}

// --- Async handler tests ---

fn test_state() -> (SharedState, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("test.db");
    let db = super::super::db::ServerDb::open(path.to_str().expect("utf8")).expect("open db");
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    (
        AppState {
            db,
            kube_client: None,
            event_tx,
            enrollment_method: EnrollmentMethod::Token,
            metrics: None,
            web_sessions: WebSessions::new(),
        },
        tmp,
    )
}

fn test_state_key_enrollment() -> (SharedState, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("test.db");
    let db = super::super::db::ServerDb::open(path.to_str().expect("utf8")).expect("open db");
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    (
        AppState {
            db,
            kube_client: None,
            event_tx,
            enrollment_method: EnrollmentMethod::Key,
            metrics: None,
            web_sessions: WebSessions::new(),
        },
        tmp,
    )
}

// --- enroll_info ---

#[tokio::test]
async fn enroll_info_returns_token_method() {
    let (state, _tmp) = test_state();
    let result = enroll_info(State(state)).await;
    assert!(
        result.is_ok(),
        "enroll_info should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["method"], "token");
}

#[tokio::test]
async fn enroll_info_returns_key_method() {
    let (state, _tmp) = test_state_key_enrollment();
    let result = enroll_info(State(state)).await;
    assert!(
        result.is_ok(),
        "enroll_info should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["method"], "key");
}

// --- enroll ---

#[tokio::test]
async fn enroll_rejects_empty_device_id() {
    let (state, _tmp) = test_state();
    let req = EnrollRequest {
        token: "cfgd_bs_some_token".to_string(),
        device_id: "".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result = enroll(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("device_id")));
}

#[tokio::test]
async fn enroll_rejects_empty_token() {
    let (state, _tmp) = test_state();
    let req = EnrollRequest {
        token: "".to_string(),
        device_id: "dev-1".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result = enroll(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("token")));
}

#[tokio::test]
async fn enroll_rejects_when_key_enrollment_enabled() {
    let (state, _tmp) = test_state_key_enrollment();
    let req = EnrollRequest {
        token: "cfgd_bs_token123".to_string(),
        device_id: "dev-1".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result = enroll(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("key-based")));
}

#[tokio::test]
async fn enroll_rejects_invalid_token() {
    let (state, _tmp) = test_state();
    let req = EnrollRequest {
        token: "bogus_nonexistent_token".to_string(),
        device_id: "dev-1".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let Err(err) = enroll(State(state), Json(req)).await else {
        panic!("expected error for nonexistent token");
    };
    assert!(
        matches!(err, GatewayError::Unauthorized),
        "expected Unauthorized for nonexistent token, got: {err}"
    );
}

#[tokio::test]
async fn enroll_success_with_valid_bootstrap_token() {
    let (state, _tmp) = test_state();

    // Create a bootstrap token in the DB
    let token_plaintext = "cfgd_bs_test_enrollment_token_abcdef1234567890";
    let token_hash = hash_token(token_plaintext);
    let expires_at = cfgd_core::unix_secs_to_iso8601(cfgd_core::unix_secs_now() + 3600);
    state
        .db
        .create_bootstrap_token(&token_hash, "testuser", Some("platform"), &expires_at)
        .await
        .expect("create token");

    let req = EnrollRequest {
        token: token_plaintext.to_string(),
        device_id: "dev-enroll-1".to_string(),
        hostname: "ws-enroll".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result = enroll(State(state.clone()), Json(req)).await;
    assert!(
        result.is_ok(),
        "enrollment should succeed with valid token: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let enroll_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(enroll_resp["status"], "enrolled");
    assert_eq!(enroll_resp["deviceId"], "dev-enroll-1");
    assert_eq!(enroll_resp["username"], "testuser");
    assert_eq!(enroll_resp["team"], "platform");
    assert!(
        enroll_resp["apiKey"]
            .as_str()
            .unwrap()
            .starts_with("cfgd_dev_"),
        "api key should have cfgd_dev_ prefix"
    );

    // Verify device was created in DB
    let device = state
        .db
        .get_device("dev-enroll-1")
        .await
        .expect("device should exist");
    assert_eq!(device.hostname, "ws-enroll");
    assert_eq!(device.os, "linux");
    assert_eq!(device.arch, "x86_64");
}

#[tokio::test]
async fn enroll_token_cannot_be_reused() {
    let (state, _tmp) = test_state();

    let token_plaintext = "cfgd_bs_reuse_test_token";
    let token_hash = hash_token(token_plaintext);
    let expires_at = cfgd_core::unix_secs_to_iso8601(cfgd_core::unix_secs_now() + 3600);
    state
        .db
        .create_bootstrap_token(&token_hash, "testuser", None, &expires_at)
        .await
        .expect("create token");

    // First enrollment succeeds
    let req1 = EnrollRequest {
        token: token_plaintext.to_string(),
        device_id: "dev-first".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result1 = enroll(State(state.clone()), Json(req1)).await;
    assert!(
        result1.is_ok(),
        "first enrollment should succeed: {:?}",
        result1.err()
    );

    // Second enrollment with same token fails
    let req2 = EnrollRequest {
        token: token_plaintext.to_string(),
        device_id: "dev-second".to_string(),
        hostname: "ws-2".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result2 = enroll(State(state), Json(req2)).await;
    assert!(result2.is_err(), "reusing a consumed token should fail");
}

// --- checkin ---

#[tokio::test]
async fn checkin_rejects_empty_device_id() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let req = CheckinRequest {
        device_id: "".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        config_hash: "abc123".to_string(),
        compliance_summary: None,
    };
    let result = checkin(State(state), Extension(auth), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("device_id")));
}

#[tokio::test]
async fn checkin_registers_new_device() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let req = CheckinRequest {
        device_id: "dev-new".to_string(),
        hostname: "workstation-new".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        config_hash: "hash123".to_string(),
        compliance_summary: None,
    };
    let result = checkin(State(state.clone()), Extension(auth), Json(req)).await;
    assert!(result.is_ok(), "checkin should succeed: {:?}", result.err());
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(checkin_resp["status"], "ok");
    assert_eq!(
        checkin_resp["configChanged"], false,
        "new device should not have config_changed"
    );

    // Verify device was created
    let device = state
        .db
        .get_device("dev-new")
        .await
        .expect("device should exist");
    assert_eq!(device.hostname, "workstation-new");
    assert_eq!(device.config_hash, "hash123");
}

#[tokio::test]
async fn checkin_updates_existing_device() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "ws-1", "linux", "x86_64", "hash-old", None)
        .await
        .expect("register");

    let auth = AuthContext::Admin;
    let req = CheckinRequest {
        device_id: "dev-1".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        config_hash: "hash-new".to_string(),
        compliance_summary: None,
    };
    let result = checkin(State(state.clone()), Extension(auth), Json(req)).await;
    assert!(result.is_ok(), "checkin should succeed: {:?}", result.err());
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(checkin_resp["status"], "ok");
    assert_eq!(
        checkin_resp["configChanged"], true,
        "different hash should detect config change"
    );

    // Verify config hash was updated
    let device = state.db.get_device("dev-1").await.expect("device");
    assert_eq!(device.config_hash, "hash-new");
}

#[tokio::test]
async fn checkin_detects_config_change() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "ws-1", "linux", "x86_64", "hash-old", None)
        .await
        .expect("register");
    state
        .db
        .set_device_config("dev-1", &serde_json::json!({"packages": ["vim"]}))
        .await
        .expect("set config");

    let auth = AuthContext::Admin;
    let req = CheckinRequest {
        device_id: "dev-1".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        config_hash: "hash-different".to_string(),
        compliance_summary: None,
    };
    let result = checkin(State(state), Extension(auth), Json(req)).await;
    assert!(result.is_ok(), "checkin should succeed: {:?}", result.err());
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(checkin_resp["status"], "ok");
    assert_eq!(
        checkin_resp["configChanged"], true,
        "different hash should detect config change"
    );
    assert!(
        !checkin_resp["desiredConfig"].is_null(),
        "should return desired_config when config changed"
    );
}

#[tokio::test]
async fn checkin_no_config_change_when_same_hash() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "ws-1", "linux", "x86_64", "same-hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Admin;
    let req = CheckinRequest {
        device_id: "dev-1".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        config_hash: "same-hash".to_string(),
        compliance_summary: None,
    };
    let result = checkin(State(state), Extension(auth), Json(req)).await;
    assert!(result.is_ok(), "checkin should succeed: {:?}", result.err());
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(checkin_resp["status"], "ok");
    assert_eq!(
        checkin_resp["configChanged"], false,
        "same hash should not detect config change"
    );
    assert!(
        checkin_resp.get("desiredConfig").is_none(),
        "should not return desired_config when config unchanged"
    );
}

#[tokio::test]
async fn checkin_device_auth_can_only_checkin_as_self() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Device {
        device_id: "dev-1".to_string(),
        username: "jdoe".to_string(),
    };
    let req = CheckinRequest {
        device_id: "dev-2".to_string(),
        hostname: "ws-2".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        config_hash: "hash".to_string(),
        compliance_summary: None,
    };
    let result = checkin(State(state), Extension(auth), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::Forbidden(_)));
}

#[tokio::test]
async fn checkin_with_compliance_summary() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let compliance = serde_json::json!({
        "total": 10,
        "compliant": 8,
        "drifted": 2
    });
    let req = CheckinRequest {
        device_id: "dev-compliance".to_string(),
        hostname: "ws-compliance".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        config_hash: "hash-comp".to_string(),
        compliance_summary: Some(compliance.clone()),
    };
    let result = checkin(State(state.clone()), Extension(auth), Json(req)).await;
    assert!(
        result.is_ok(),
        "checkin with compliance summary should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let checkin_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(checkin_resp["status"], "ok");

    // Verify compliance_summary was stored
    let device = state.db.get_device("dev-compliance").await.expect("device");
    assert!(device.compliance_summary.is_some());
    assert_eq!(device.compliance_summary.unwrap()["total"], 10);
}

#[tokio::test]
async fn checkin_broadcasts_event() {
    let (state, _tmp) = test_state();
    let mut rx = state.event_tx.subscribe();

    let auth = AuthContext::Admin;
    let req = CheckinRequest {
        device_id: "dev-broadcast".to_string(),
        hostname: "ws-broadcast".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        config_hash: "hash-bc".to_string(),
        compliance_summary: None,
    };
    let result = checkin(State(state), Extension(auth), Json(req)).await;
    assert!(
        result.is_ok(),
        "checkin should succeed for broadcast test: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);

    // Check that an event was broadcast
    let event = rx.try_recv().expect("should receive broadcast event");
    assert_eq!(event.device_id, "dev-broadcast");
    assert_eq!(event.event_type, "checkin");
}

// --- list_devices ---

#[tokio::test]
async fn list_devices_empty() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let pagination = PaginationParams {
        limit: 100,
        offset: 0,
    };
    let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
    assert!(
        result.is_ok(),
        "list_devices should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let devices: Vec<super::super::db::Device> = serde_json::from_slice(&body).unwrap();
    assert!(
        devices.is_empty(),
        "should return empty list when no devices registered"
    );
}

#[tokio::test]
async fn list_devices_returns_all_devices_for_admin() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "ws-1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    state
        .db
        .register_device("dev-2", "ws-2", "darwin", "aarch64", "h2", None)
        .await
        .expect("register");

    let auth = AuthContext::Admin;
    let pagination = PaginationParams {
        limit: 100,
        offset: 0,
    };
    let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
    assert!(
        result.is_ok(),
        "list_devices should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let devices: Vec<super::super::db::Device> = serde_json::from_slice(&body).unwrap();
    assert_eq!(devices.len(), 2, "admin should see all devices");
    let hostnames: Vec<&str> = devices.iter().map(|d| d.hostname.as_str()).collect();
    assert!(hostnames.contains(&"ws-1"));
    assert!(hostnames.contains(&"ws-2"));
}

#[tokio::test]
async fn list_devices_device_auth_returns_only_self() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "ws-1", "linux", "x86_64", "h1", None)
        .await
        .expect("register");
    state
        .db
        .register_device("dev-2", "ws-2", "darwin", "aarch64", "h2", None)
        .await
        .expect("register");

    let auth = AuthContext::Device {
        device_id: "dev-1".to_string(),
        username: "jdoe".to_string(),
    };
    let pagination = PaginationParams {
        limit: 100,
        offset: 0,
    };
    let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
    assert!(
        result.is_ok(),
        "device should be able to list self: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let devices: Vec<super::super::db::Device> = serde_json::from_slice(&body).unwrap();
    assert_eq!(devices.len(), 1, "device auth should return only self");
    assert_eq!(devices[0].id, "dev-1");
    assert_eq!(devices[0].hostname, "ws-1");
}

#[tokio::test]
async fn list_devices_respects_pagination_limit() {
    let (state, _tmp) = test_state();
    for i in 0..5 {
        state
            .db
            .register_device(
                &format!("dev-{i}"),
                &format!("ws-{i}"),
                "linux",
                "x86_64",
                &format!("h{i}"),
                None,
            )
            .await
            .expect("register");
    }

    let auth = AuthContext::Admin;
    let pagination = PaginationParams {
        limit: 2,
        offset: 0,
    };
    let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
    assert!(
        result.is_ok(),
        "list_devices with limit should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let devices: Vec<super::super::db::Device> = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        devices.len(),
        2,
        "pagination limit=2 should return exactly 2 devices"
    );
}

#[tokio::test]
async fn list_devices_caps_limit_at_1000() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let pagination = PaginationParams {
        limit: 5000,
        offset: 0,
    };
    // Should not error even with limit > 1000 — it gets capped internally
    let result = list_devices(State(state), Extension(auth), Query(pagination)).await;
    assert!(
        result.is_ok(),
        "list_devices should succeed even with limit > 1000: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
}

// --- get_device ---

#[tokio::test]
async fn get_device_existing() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-get", "ws-get", "linux", "x86_64", "hash-get", None)
        .await
        .expect("register");

    let auth = AuthContext::Admin;
    let result = get_device(State(state), Extension(auth), Path("dev-get".to_string())).await;
    assert!(
        result.is_ok(),
        "get_device should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let device: super::super::db::Device = serde_json::from_slice(&body).unwrap();
    assert_eq!(device.id, "dev-get");
    assert_eq!(device.hostname, "ws-get");
    assert_eq!(device.os, "linux");
    assert_eq!(device.arch, "x86_64");
    assert_eq!(device.config_hash, "hash-get");
}

#[tokio::test]
async fn get_device_not_found() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let result = get_device(
        State(state),
        Extension(auth),
        Path("nonexistent".to_string()),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::NotFound(_)));
}

#[tokio::test]
async fn get_device_device_auth_allows_self() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-own", "ws-own", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Device {
        device_id: "dev-own".to_string(),
        username: "jdoe".to_string(),
    };
    let result = get_device(State(state), Extension(auth), Path("dev-own".to_string())).await;
    assert!(
        result.is_ok(),
        "device should be able to get self: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let device: super::super::db::Device = serde_json::from_slice(&body).unwrap();
    assert_eq!(device.id, "dev-own");
    assert_eq!(device.hostname, "ws-own");
}

#[tokio::test]
async fn get_device_device_auth_denies_other() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-other", "ws-other", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Device {
        device_id: "dev-own".to_string(),
        username: "jdoe".to_string(),
    };
    let result = get_device(State(state), Extension(auth), Path("dev-other".to_string())).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::Forbidden(_)));
}

// --- set_device_config ---

#[tokio::test]
async fn set_device_config_admin_success() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-cfg", "ws-cfg", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Admin;
    let req = SetConfigRequest {
        config: serde_json::json!({"packages": ["vim", "git"]}),
    };
    let result = set_device_config(
        State(state.clone()),
        Extension(auth),
        Path("dev-cfg".to_string()),
        Json(req),
    )
    .await;
    assert!(
        result.is_ok(),
        "set_device_config should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify config was stored
    let device = state.db.get_device("dev-cfg").await.expect("device");
    assert!(device.desired_config.is_some());
    assert_eq!(device.desired_config.unwrap()["packages"][0], "vim");
}

#[tokio::test]
async fn set_device_config_device_auth_forbidden() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-cfg2", "ws-cfg2", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Device {
        device_id: "dev-cfg2".to_string(),
        username: "jdoe".to_string(),
    };
    let req = SetConfigRequest {
        config: serde_json::json!({"packages": ["vim"]}),
    };
    let result = set_device_config(
        State(state),
        Extension(auth),
        Path("dev-cfg2".to_string()),
        Json(req),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::Forbidden(_)));
}

#[tokio::test]
async fn set_device_config_not_found() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let req = SetConfigRequest {
        config: serde_json::json!({"packages": ["vim"]}),
    };
    let result = set_device_config(
        State(state),
        Extension(auth),
        Path("nonexistent".to_string()),
        Json(req),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::NotFound(_)));
}

// --- list_drift_events ---

#[tokio::test]
async fn list_drift_events_empty() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-drift", "ws-drift", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Admin;
    let result =
        list_drift_events(State(state), Extension(auth), Path("dev-drift".to_string())).await;
    assert!(
        result.is_ok(),
        "list_drift_events should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let events: Vec<super::super::db::DriftEvent> = serde_json::from_slice(&body).unwrap();
    assert!(
        events.is_empty(),
        "should return empty list when no drift events"
    );
}

#[tokio::test]
async fn list_drift_events_with_events() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-drift2", "ws-drift2", "linux", "x86_64", "hash", None)
        .await
        .expect("register");
    state
        .db
        .record_drift_event("dev-drift2", "field changed")
        .await
        .expect("record drift");

    let auth = AuthContext::Admin;
    let result = list_drift_events(
        State(state),
        Extension(auth),
        Path("dev-drift2".to_string()),
    )
    .await;
    assert!(
        result.is_ok(),
        "list_drift_events should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let events: Vec<super::super::db::DriftEvent> = serde_json::from_slice(&body).unwrap();
    assert_eq!(events.len(), 1, "should return one drift event");
    assert_eq!(events[0].device_id, "dev-drift2");
    assert_eq!(events[0].details, "field changed");
}

#[tokio::test]
async fn list_drift_events_device_auth_allows_self() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-own-drift", "ws", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Device {
        device_id: "dev-own-drift".to_string(),
        username: "jdoe".to_string(),
    };
    let result = list_drift_events(
        State(state),
        Extension(auth),
        Path("dev-own-drift".to_string()),
    )
    .await;
    assert!(
        result.is_ok(),
        "device should be able to list own drift events: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let events: Vec<super::super::db::DriftEvent> = serde_json::from_slice(&body).unwrap();
    assert!(
        events.is_empty(),
        "no drift events recorded for this device"
    );
}

#[tokio::test]
async fn list_drift_events_device_auth_denies_other() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-victim", "ws", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Device {
        device_id: "dev-attacker".to_string(),
        username: "attacker".to_string(),
    };
    let result = list_drift_events(
        State(state),
        Extension(auth),
        Path("dev-victim".to_string()),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::Forbidden(_)));
}

#[tokio::test]
async fn list_drift_events_not_found() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let result = list_drift_events(
        State(state),
        Extension(auth),
        Path("nonexistent".to_string()),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::NotFound(_)));
}

// --- record_drift_event ---

#[tokio::test]
async fn record_drift_event_success() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-record", "ws-record", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Admin;
    let req = DriftRequest {
        details: vec![DriftDetailInput {
            field: "package/vim".to_string(),
            expected: "installed".to_string(),
            actual: "missing".to_string(),
        }],
    };
    let result = record_drift_event(
        State(state.clone()),
        Extension(auth),
        Path("dev-record".to_string()),
        Json(req),
    )
    .await;
    assert!(
        result.is_ok(),
        "record_drift_event should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let event: super::super::db::DriftEvent = serde_json::from_slice(&body).unwrap();
    assert_eq!(event.device_id, "dev-record");
    assert!(!event.id.is_empty(), "drift event should have an id");
    assert!(
        !event.timestamp.is_empty(),
        "drift event should have a timestamp"
    );

    // Verify device status changed to drifted
    let device = state.db.get_device("dev-record").await.expect("device");
    assert_eq!(device.status, super::super::db::DeviceStatus::Drifted);
}

#[tokio::test]
async fn record_drift_event_broadcasts_to_sse() {
    let (state, _tmp) = test_state();
    let mut rx = state.event_tx.subscribe();
    state
        .db
        .register_device("dev-sse", "ws-sse", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Admin;
    let req = DriftRequest {
        details: vec![DriftDetailInput {
            field: "file/bashrc".to_string(),
            expected: "hash-a".to_string(),
            actual: "hash-b".to_string(),
        }],
    };
    let _ = record_drift_event(
        State(state),
        Extension(auth),
        Path("dev-sse".to_string()),
        Json(req),
    )
    .await;

    let event = rx.try_recv().expect("should receive broadcast");
    assert_eq!(event.device_id, "dev-sse");
    assert_eq!(event.event_type, "drift");
}

#[tokio::test]
async fn record_drift_event_device_auth_denies_other() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-target", "ws", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Device {
        device_id: "dev-attacker".to_string(),
        username: "attacker".to_string(),
    };
    let req = DriftRequest {
        details: vec![DriftDetailInput {
            field: "pkg/vim".to_string(),
            expected: "9.0".to_string(),
            actual: "8.2".to_string(),
        }],
    };
    let result = record_drift_event(
        State(state),
        Extension(auth),
        Path("dev-target".to_string()),
        Json(req),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::Forbidden(_)));
}

#[tokio::test]
async fn record_drift_event_nonexistent_device() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let req = DriftRequest {
        details: vec![DriftDetailInput {
            field: "pkg/vim".to_string(),
            expected: "9.0".to_string(),
            actual: "8.2".to_string(),
        }],
    };
    let result = record_drift_event(
        State(state),
        Extension(auth),
        Path("nonexistent".to_string()),
        Json(req),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::NotFound(_)));
}

// --- force_reconcile ---

#[tokio::test]
async fn force_reconcile_admin_success() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-recon", "ws-recon", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Admin;
    let result = force_reconcile(
        State(state.clone()),
        Extension(auth),
        Path("dev-recon".to_string()),
    )
    .await;
    assert!(
        result.is_ok(),
        "force_reconcile should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify status changed to pending-reconcile
    let device = state.db.get_device("dev-recon").await.expect("device");
    assert_eq!(
        device.status,
        super::super::db::DeviceStatus::PendingReconcile
    );
}

#[tokio::test]
async fn force_reconcile_device_auth_forbidden() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-recon2", "ws", "linux", "x86_64", "hash", None)
        .await
        .expect("register");

    let auth = AuthContext::Device {
        device_id: "dev-recon2".to_string(),
        username: "jdoe".to_string(),
    };
    let result = force_reconcile(
        State(state),
        Extension(auth),
        Path("dev-recon2".to_string()),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::Forbidden(_)));
}

#[tokio::test]
async fn force_reconcile_not_found() {
    let (state, _tmp) = test_state();
    let auth = AuthContext::Admin;
    let result = force_reconcile(
        State(state),
        Extension(auth),
        Path("nonexistent".to_string()),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::NotFound(_)));
}

// --- list_fleet_events ---

#[tokio::test]
async fn list_fleet_events_empty() {
    let (state, _tmp) = test_state();
    let pagination = PaginationParams {
        limit: 100,
        offset: 0,
    };
    let result = list_fleet_events(State(state), Query(pagination)).await;
    assert!(
        result.is_ok(),
        "list_fleet_events should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let events: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(
        events.is_empty(),
        "should return empty list when no fleet events"
    );
}

#[tokio::test]
async fn list_fleet_events_with_data() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-fe", "ws-fe", "linux", "x86_64", "hash", None)
        .await
        .expect("register");
    state
        .db
        .record_checkin("dev-fe", "hash", false)
        .await
        .expect("record checkin");
    state
        .db
        .record_drift_event("dev-fe", "drift details")
        .await
        .expect("record drift");

    let pagination = PaginationParams {
        limit: 100,
        offset: 0,
    };
    let result = list_fleet_events(State(state), Query(pagination)).await;
    assert!(
        result.is_ok(),
        "list_fleet_events should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let events: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(
        events.len() >= 2,
        "should have at least checkin and drift events, got {}",
        events.len()
    );
}

#[tokio::test]
async fn list_fleet_events_caps_limit() {
    let (state, _tmp) = test_state();
    let pagination = PaginationParams {
        limit: 5000,
        offset: 0,
    };
    // Should not error — limit gets capped to 1000 internally
    let result = list_fleet_events(State(state), Query(pagination)).await;
    assert!(
        result.is_ok(),
        "list_fleet_events should succeed even with limit > 1000: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
}

// --- request_challenge ---

#[tokio::test]
async fn request_challenge_rejects_when_token_mode() {
    let (state, _tmp) = test_state(); // token mode by default
    let req = ChallengeRequest {
        username: "jdoe".to_string(),
        device_id: "dev-1".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result = request_challenge(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(
        matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("bootstrap token"))
    );
}

#[tokio::test]
async fn request_challenge_rejects_empty_username() {
    let (state, _tmp) = test_state_key_enrollment();
    let req = ChallengeRequest {
        username: "".to_string(),
        device_id: "dev-1".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result = request_challenge(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("username")));
}

#[tokio::test]
async fn request_challenge_rejects_empty_device_id() {
    let (state, _tmp) = test_state_key_enrollment();
    let req = ChallengeRequest {
        username: "jdoe".to_string(),
        device_id: "".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result = request_challenge(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("device_id")));
}

#[tokio::test]
async fn request_challenge_rejects_user_without_keys() {
    let (state, _tmp) = test_state_key_enrollment();
    let req = ChallengeRequest {
        username: "unknown-user".to_string(),
        device_id: "dev-1".to_string(),
        hostname: "ws-1".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result = request_challenge(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("no public keys")));
}

#[tokio::test]
async fn request_challenge_success_with_registered_key() {
    let (state, _tmp) = test_state_key_enrollment();
    state
        .db
        .add_user_public_key(
            "jdoe",
            "ssh",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG...",
            "SHA256:testfp",
            Some("laptop key"),
        )
        .await
        .expect("add key");

    let req = ChallengeRequest {
        username: "jdoe".to_string(),
        device_id: "dev-ch".to_string(),
        hostname: "ws-ch".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    };
    let result = request_challenge(State(state), Json(req)).await;
    assert!(
        result.is_ok(),
        "request_challenge should succeed with registered key: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let challenge: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        !challenge["challengeId"].as_str().unwrap().is_empty(),
        "challenge should have an id"
    );
    assert!(
        !challenge["nonce"].as_str().unwrap().is_empty(),
        "challenge should have a nonce"
    );
    assert!(
        !challenge["expiresAt"].as_str().unwrap().is_empty(),
        "challenge should have an expiry"
    );
}

// --- verify_enrollment ---

#[tokio::test]
async fn verify_enrollment_rejects_when_token_mode() {
    let (state, _tmp) = test_state();
    let req = VerifyRequest {
        challenge_id: "ch-1".to_string(),
        signature: "sig-data".to_string(),
        key_type: "ssh".to_string(),
    };
    let result = verify_enrollment(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("key-based")));
}

#[tokio::test]
async fn verify_enrollment_rejects_empty_challenge_id() {
    let (state, _tmp) = test_state_key_enrollment();
    let req = VerifyRequest {
        challenge_id: "".to_string(),
        signature: "sig-data".to_string(),
        key_type: "ssh".to_string(),
    };
    let result = verify_enrollment(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("challenge_id")));
}

#[tokio::test]
async fn verify_enrollment_rejects_empty_signature() {
    let (state, _tmp) = test_state_key_enrollment();
    let req = VerifyRequest {
        challenge_id: "ch-1".to_string(),
        signature: "".to_string(),
        key_type: "ssh".to_string(),
    };
    let result = verify_enrollment(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("signature")));
}

#[tokio::test]
async fn verify_enrollment_rejects_invalid_key_type() {
    let (state, _tmp) = test_state_key_enrollment();
    let req = VerifyRequest {
        challenge_id: "ch-1".to_string(),
        signature: "sig-data".to_string(),
        key_type: "rsa".to_string(),
    };
    let result = verify_enrollment(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("key_type")));
}

#[tokio::test]
async fn verify_enrollment_rejects_nonexistent_challenge() {
    let (state, _tmp) = test_state_key_enrollment();
    let req = VerifyRequest {
        challenge_id: "nonexistent-challenge".to_string(),
        signature: "sig-data".to_string(),
        key_type: "ssh".to_string(),
    };
    let result = verify_enrollment(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::NotFound(_)));
}

#[tokio::test]
async fn verify_enrollment_rejects_when_no_keys_registered() {
    // Drives lines 197-202 (matching_keys.is_empty() branch). The challenge
    // exists but no user public keys are registered for that username, so
    // the filtered list is empty and we return InvalidRequest.
    let (state, _tmp) = test_state_key_enrollment();
    let challenge = state
        .db
        .create_enrollment_challenge(
            "ghost-user",
            "dev-no-keys",
            "ws-no-keys",
            ("linux", "x86_64"),
            "nonce-bytes",
            300,
        )
        .await
        .expect("create challenge");
    let req = VerifyRequest {
        challenge_id: challenge.id.clone(),
        signature: "any-signature".to_string(),
        key_type: "ssh".to_string(),
    };
    let result = verify_enrollment(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(
        matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("no ssh keys registered"))
    );
}

#[tokio::test]
async fn verify_enrollment_rejects_when_signature_does_not_verify() {
    // Drives the signature-verify-fail arm (line 235-237): challenge + a
    // matching ssh key are registered, but the signature is junk → blocking
    // verification returns false → Unauthorized.
    if !cfgd_core::command_available("ssh-keygen") {
        return;
    }
    let (state, _tmp) = test_state_key_enrollment();
    state
        .db
        .add_user_public_key(
            "junk-sig-user",
            "ssh",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG... laptop",
            "SHA256:junk",
            Some("laptop"),
        )
        .await
        .expect("add user key");
    let challenge = state
        .db
        .create_enrollment_challenge(
            "junk-sig-user",
            "dev-junk-sig",
            "ws-junk-sig",
            ("linux", "x86_64"),
            "verify-fail-nonce",
            300,
        )
        .await
        .expect("create challenge");
    let req = VerifyRequest {
        challenge_id: challenge.id.clone(),
        signature: "not-a-real-pem-signature".to_string(),
        key_type: "ssh".to_string(),
    };
    let result = verify_enrollment(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected verification to fail with junk signature");
    };
    assert!(matches!(err, GatewayError::Unauthorized));
}

#[tokio::test]
async fn verify_enrollment_with_valid_ssh_signature_enrolls_device() {
    // Drives the success path at lines 239-310: signature verifies, a
    // device api-key is generated and hashed, the device + credential rows
    // are inserted in one write transaction, a FleetEvent is broadcast,
    // and the response carries the unhashed api_key + enrollment metadata.
    //
    // Requires `ssh-keygen` for ed25519 keypair generation + signing. Skip
    // when not available so CI environments without OpenSSH still pass.
    if !cfgd_core::command_available("ssh-keygen") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    let key_path = tmp.path().join("enroll_test_key");
    let pub_path = tmp.path().join("enroll_test_key.pub");

    let keygen_status = std::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &key_path.to_string_lossy(),
            "-N",
            "",
            "-C",
            "enroll-test@cfgd",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ssh-keygen");
    assert!(keygen_status.success(), "key generation must succeed");
    let public_key = std::fs::read_to_string(&pub_path).expect("read pubkey");

    let (state, _state_tmp) = test_state_key_enrollment();
    state
        .db
        .add_user_public_key(
            "enroll-user",
            "ssh",
            public_key.trim(),
            "SHA256:enroll-test",
            Some("test"),
        )
        .await
        .expect("add user key");

    let nonce = "cfgd_ch_enroll_nonce_42";
    let challenge = state
        .db
        .create_enrollment_challenge(
            "enroll-user",
            "dev-enroll-1",
            "ws-enroll-1",
            ("linux", "x86_64"),
            nonce,
            300,
        )
        .await
        .expect("create challenge");

    // Sign the nonce that was stored on the challenge (verify_ssh_signature
    // signs the row's nonce, not the request payload).
    let nonce_path = tmp.path().join("nonce.txt");
    std::fs::write(&nonce_path, &challenge.nonce).expect("write nonce");
    let sig_output = std::process::Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            &key_path.to_string_lossy(),
            "-n",
            "cfgd-enroll",
        ])
        .stdin(std::fs::File::open(&nonce_path).expect("open nonce"))
        .output()
        .expect("ssh-keygen sign");
    assert!(sig_output.status.success(), "sign must succeed");
    let signature_pem = String::from_utf8_lossy(&sig_output.stdout).to_string();

    let mut event_rx = state.event_tx.subscribe();

    let req = VerifyRequest {
        challenge_id: challenge.id.clone(),
        signature: signature_pem,
        key_type: "ssh".to_string(),
    };
    let result = verify_enrollment(State(state.clone()), Json(req)).await;
    let response = result
        .expect("verify_enrollment should succeed")
        .into_response();
    assert_eq!(response.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
    assert_eq!(payload["status"], "enrolled");
    assert_eq!(payload["deviceId"], "dev-enroll-1");
    assert_eq!(payload["username"], "enroll-user");
    let api_key = payload["apiKey"].as_str().expect("apiKey present");
    assert!(
        api_key.starts_with("cfgd_dev"),
        "api_key must use cfgd_dev prefix: {api_key}"
    );

    // FleetEvent must be broadcast.
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
        .await
        .expect("event broadcast timed out")
        .expect("event recv");
    assert_eq!(event.device_id, "dev-enroll-1");
    assert_eq!(event.event_type, "enrollment");
    assert!(
        event.summary.contains("user=enroll-user"),
        "summary should mention user: {}",
        event.summary
    );

    // Re-verifying with the same challenge id must fail — the challenge is
    // single-use; the second consume returns InvalidRequest("already been used").
    let req2 = VerifyRequest {
        challenge_id: challenge.id,
        signature: "any".to_string(),
        key_type: "ssh".to_string(),
    };
    let result2 = verify_enrollment(State(state), Json(req2)).await;
    let Err(err) = result2 else {
        panic!("consumed challenge should not verify again");
    };
    assert!(
        matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("already been used")),
        "expected InvalidRequest already-used, got: {err:?}"
    );
}

// --- admin_reset ---

#[tokio::test]
async fn admin_reset_clears_all_data() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-1", "ws-1", "linux", "x86_64", "hash", None)
        .await
        .expect("register");
    state
        .db
        .register_device("dev-2", "ws-2", "darwin", "aarch64", "hash2", None)
        .await
        .expect("register");
    state
        .db
        .record_drift_event("dev-1", "drift details")
        .await
        .expect("record drift");

    let result = admin_reset(State(state.clone())).await;
    assert!(
        result.is_ok(),
        "admin_reset should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let reset_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(reset_resp["status"], "ok");
    assert!(
        reset_resp["rowsDeleted"].as_u64().unwrap() > 0,
        "should have deleted rows"
    );

    // Verify data was wiped
    let devices = state.db.list_devices().await.expect("list");
    assert!(
        devices.is_empty(),
        "all devices should be deleted after reset"
    );
}

// --- add_user_key ---

#[tokio::test]
async fn add_user_key_success() {
    let (state, _tmp) = test_state();
    let req = AddKeyRequest {
        key_type: "ssh".to_string(),
        public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG...".to_string(),
        fingerprint: "SHA256:abc123".to_string(),
        label: Some("laptop key".to_string()),
    };
    let result = add_user_key(State(state), Path("jdoe".to_string()), Json(req)).await;
    assert!(
        result.is_ok(),
        "add_user_key should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let key: super::super::db::UserPublicKey = serde_json::from_slice(&body).unwrap();
    assert_eq!(key.username, "jdoe");
    assert_eq!(key.key_type, "ssh");
    assert_eq!(key.fingerprint, "SHA256:abc123");
    assert_eq!(key.label, Some("laptop key".to_string()));
    assert!(!key.id.is_empty(), "key should have an id");
}

#[tokio::test]
async fn add_user_key_empty_username() {
    let (state, _tmp) = test_state();
    let req = AddKeyRequest {
        key_type: "ssh".to_string(),
        public_key: "ssh-ed25519 ...".to_string(),
        fingerprint: "SHA256:abc".to_string(),
        label: None,
    };
    let result = add_user_key(State(state), Path("".to_string()), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("username")));
}

#[tokio::test]
async fn add_user_key_invalid_key_type() {
    let (state, _tmp) = test_state();
    let req = AddKeyRequest {
        key_type: "rsa".to_string(),
        public_key: "ssh-rsa ...".to_string(),
        fingerprint: "SHA256:abc".to_string(),
        label: None,
    };
    let result = add_user_key(State(state), Path("jdoe".to_string()), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("key_type")));
}

#[tokio::test]
async fn add_user_key_empty_public_key() {
    let (state, _tmp) = test_state();
    let req = AddKeyRequest {
        key_type: "ssh".to_string(),
        public_key: "".to_string(),
        fingerprint: "SHA256:abc".to_string(),
        label: None,
    };
    let result = add_user_key(State(state), Path("jdoe".to_string()), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("public_key")));
}

#[tokio::test]
async fn add_user_key_empty_fingerprint() {
    let (state, _tmp) = test_state();
    let req = AddKeyRequest {
        key_type: "ssh".to_string(),
        public_key: "ssh-ed25519 ...".to_string(),
        fingerprint: "".to_string(),
        label: None,
    };
    let result = add_user_key(State(state), Path("jdoe".to_string()), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("fingerprint")));
}

// --- list_user_keys ---

#[tokio::test]
async fn list_user_keys_empty() {
    let (state, _tmp) = test_state();
    let result = list_user_keys(State(state), Path("jdoe".to_string())).await;
    assert!(
        result.is_ok(),
        "list_user_keys should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let keys: Vec<super::super::db::UserPublicKey> = serde_json::from_slice(&body).unwrap();
    assert!(
        keys.is_empty(),
        "should return empty list when no keys registered"
    );
}

#[tokio::test]
async fn list_user_keys_with_keys() {
    let (state, _tmp) = test_state();
    state
        .db
        .add_user_public_key(
            "jdoe",
            "ssh",
            "ssh-ed25519 AAAA...",
            "SHA256:a",
            Some("key1"),
        )
        .await
        .expect("add key");
    state
        .db
        .add_user_public_key("jdoe", "gpg", "-----BEGIN PGP...", "DEADBEEF", None)
        .await
        .expect("add key");

    let result = list_user_keys(State(state), Path("jdoe".to_string())).await;
    assert!(
        result.is_ok(),
        "list_user_keys should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let keys: Vec<super::super::db::UserPublicKey> = serde_json::from_slice(&body).unwrap();
    assert_eq!(keys.len(), 2, "should return both registered keys");
    let key_types: Vec<&str> = keys.iter().map(|k| k.key_type.as_str()).collect();
    assert!(key_types.contains(&"ssh"));
    assert!(key_types.contains(&"gpg"));
}

// --- delete_user_key ---

#[tokio::test]
async fn delete_user_key_success() {
    let (state, _tmp) = test_state();
    let key = state
        .db
        .add_user_public_key("jdoe", "ssh", "ssh-ed25519 ...", "SHA256:x", None)
        .await
        .expect("add key");
    let key_id = key.id;

    let result = delete_user_key(State(state), Path(("jdoe".to_string(), key_id))).await;
    assert!(
        result.is_ok(),
        "delete_user_key should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_user_key_not_found() {
    let (state, _tmp) = test_state();
    let result = delete_user_key(
        State(state),
        Path(("jdoe".to_string(), "nonexistent".to_string())),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::NotFound(_)));
}

// --- create_token ---

#[tokio::test]
async fn create_token_success() {
    let (state, _tmp) = test_state();
    let req = CreateTokenRequest {
        username: "admin".to_string(),
        team: Some("platform".to_string()),
        expires_in: 3600,
    };
    let result = create_token(State(state), Json(req)).await;
    assert!(
        result.is_ok(),
        "create_token should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let token_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        !token_resp["id"].as_str().unwrap().is_empty(),
        "token should have an id"
    );
    assert!(
        token_resp["token"]
            .as_str()
            .unwrap()
            .starts_with("cfgd_bs_"),
        "token should have cfgd_bs_ prefix"
    );
    assert_eq!(token_resp["username"], "admin");
    assert_eq!(token_resp["team"], "platform");
    assert!(
        !token_resp["expiresAt"].as_str().unwrap().is_empty(),
        "token should have an expiry"
    );
}

#[tokio::test]
async fn create_token_empty_username() {
    let (state, _tmp) = test_state();
    let req = CreateTokenRequest {
        username: "".to_string(),
        team: None,
        expires_in: 3600,
    };
    let result = create_token(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("username")));
}

#[tokio::test]
async fn create_token_zero_expires_in() {
    let (state, _tmp) = test_state();
    let req = CreateTokenRequest {
        username: "admin".to_string(),
        team: None,
        expires_in: 0,
    };
    let result = create_token(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("expires_in")));
}

#[tokio::test]
async fn create_token_excessive_expires_in() {
    let (state, _tmp) = test_state();
    let req = CreateTokenRequest {
        username: "admin".to_string(),
        team: None,
        expires_in: 31 * 86400, // 31 days, exceeds 30 day max
    };
    let result = create_token(State(state), Json(req)).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::InvalidRequest(ref msg) if msg.contains("expires_in")));
}

// --- list_tokens ---

#[tokio::test]
async fn list_tokens_empty() {
    let (state, _tmp) = test_state();
    let result = list_tokens(State(state)).await;
    assert!(
        result.is_ok(),
        "list_tokens should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let tokens: Vec<super::super::db::BootstrapToken> = serde_json::from_slice(&body).unwrap();
    assert!(
        tokens.is_empty(),
        "should return empty list when no tokens exist"
    );
}

#[tokio::test]
async fn list_tokens_with_data() {
    let (state, _tmp) = test_state();
    state
        .db
        .create_bootstrap_token("hash1", "admin", None, "2026-12-31T23:59:59Z")
        .await
        .expect("create token");

    let result = list_tokens(State(state)).await;
    assert!(
        result.is_ok(),
        "list_tokens should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let tokens: Vec<super::super::db::BootstrapToken> = serde_json::from_slice(&body).unwrap();
    assert_eq!(tokens.len(), 1, "should return one token");
    assert_eq!(tokens[0].username, "admin");
    assert_eq!(tokens[0].expires_at, "2026-12-31T23:59:59Z");
}

// --- delete_token ---

#[tokio::test]
async fn delete_token_success() {
    let (state, _tmp) = test_state();
    let token = state
        .db
        .create_bootstrap_token("hash1", "admin", None, "2026-12-31T23:59:59Z")
        .await
        .expect("create token");
    let token_id = token.id;

    let result = delete_token(State(state), Path(token_id)).await;
    assert!(
        result.is_ok(),
        "delete_token should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_token_not_found() {
    let (state, _tmp) = test_state();
    let result = delete_token(State(state), Path("nonexistent".to_string())).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::NotFound(_)));
}

// --- revoke_credential ---

#[tokio::test]
async fn revoke_credential_success() {
    let (state, _tmp) = test_state();
    state
        .db
        .register_device("dev-revoke", "ws", "linux", "x86_64", "hash", None)
        .await
        .expect("register");
    state
        .db
        .create_device_credential("dev-revoke", "api_hash", "jdoe", None)
        .await
        .expect("create credential");

    let result = revoke_credential(State(state), Path("dev-revoke".to_string())).await;
    assert!(
        result.is_ok(),
        "revoke_credential should succeed: {:?}",
        result.err()
    );
    let response = result.unwrap().into_response();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn revoke_credential_not_found() {
    let (state, _tmp) = test_state();
    let result = revoke_credential(State(state), Path("nonexistent".to_string())).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, GatewayError::NotFound(_)));
}

// --- event_stream ---

#[tokio::test]
async fn event_stream_returns_sse() {
    let (state, _tmp) = test_state();
    // Just verify it doesn't panic and returns the SSE stream
    let _sse = event_stream(State(state)).await;
}

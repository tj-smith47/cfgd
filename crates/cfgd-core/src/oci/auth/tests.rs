use super::*;
use serial_test::serial;

// --- Docker config.json parsing ---

#[test]
fn parse_docker_config_auth() {
    let config_json = r#"{
            "auths": {
                "ghcr.io": {
                    "auth": "dXNlcjpwYXNz"
                }
            }
        }"#;
    let config: DockerConfig = serde_json::from_str(config_json).unwrap();
    let auth = resolve_from_docker_auths(&config.auths, "ghcr.io").unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "pass");
}

#[test]
fn parse_docker_config_with_url_key() {
    let config_json = r#"{
            "auths": {
                "https://index.docker.io/v1/": {
                    "auth": "ZG9ja2VyOnNlY3JldA=="
                }
            }
        }"#;
    let config: DockerConfig = serde_json::from_str(config_json).unwrap();
    let auth = resolve_from_docker_auths(&config.auths, "docker.io").unwrap();
    assert_eq!(auth.username, "docker");
    assert_eq!(auth.password, "secret");
}

#[test]
fn parse_docker_config_no_auth() {
    let config_json = r#"{ "auths": {} }"#;
    let config: DockerConfig = serde_json::from_str(config_json).unwrap();
    let auth = resolve_from_docker_auths(&config.auths, "ghcr.io");
    assert!(auth.is_none());
}

#[test]
fn parse_docker_config_cred_helpers() {
    let config_json = r#"{
            "auths": {},
            "credHelpers": {
                "gcr.io": "gcloud",
                "ghcr.io": "gh"
            }
        }"#;
    let config: DockerConfig = serde_json::from_str(config_json).unwrap();
    assert_eq!(config.cred_helpers.get("gcr.io").unwrap(), "gcloud");
    assert_eq!(config.cred_helpers.get("ghcr.io").unwrap(), "gh");
}

// --- Base64 ---

#[test]
fn base64_encode_decode() {
    let original = "user:password";
    let encoded = base64_encode(original.as_bytes());
    let decoded = base64_decode(&encoded).unwrap();
    assert_eq!(String::from_utf8(decoded).unwrap(), original);
}

#[test]
fn base64_decode_known() {
    // "user:pass" → "dXNlcjpwYXNz"
    let decoded = base64_decode("dXNlcjpwYXNz").unwrap();
    assert_eq!(String::from_utf8(decoded).unwrap(), "user:pass");
}

#[test]
fn base64_empty() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_decode("").unwrap(), Vec::<u8>::new());
}

// --- Registry auth resolution ---

#[test]
#[serial]
fn registry_auth_from_env() {
    // Save and restore env
    let old_user = std::env::var("REGISTRY_USERNAME").ok();
    let old_pass = std::env::var("REGISTRY_PASSWORD").ok();

    // SAFETY: test runs single-threaded; no other thread reads these vars.
    unsafe {
        std::env::set_var("REGISTRY_USERNAME", "testuser");
        std::env::set_var("REGISTRY_PASSWORD", "testpass");
    }

    let auth = RegistryAuth::resolve("any-registry.io").unwrap();
    assert_eq!(auth.username, "testuser");
    assert_eq!(auth.password, "testpass");

    // Restore
    unsafe {
        match old_user {
            Some(v) => std::env::set_var("REGISTRY_USERNAME", v),
            None => std::env::remove_var("REGISTRY_USERNAME"),
        }
        match old_pass {
            Some(v) => std::env::set_var("REGISTRY_PASSWORD", v),
            None => std::env::remove_var("REGISTRY_PASSWORD"),
        }
    }
}

// --- Www-Authenticate parsing ---

#[test]
fn extract_auth_params() {
    let header = r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:myorg/mymod:pull""#;
    assert_eq!(
        extract_auth_param(header, "realm"),
        Some("https://ghcr.io/token")
    );
    assert_eq!(extract_auth_param(header, "service"), Some("ghcr.io"));
    assert_eq!(
        extract_auth_param(header, "scope"),
        Some("repository:myorg/mymod:pull")
    );
    assert_eq!(extract_auth_param(header, "nonexistent"), None);
}

// --- decode_docker_auth ---

#[test]
fn decode_docker_auth_valid() {
    // "user:pass" base64 = "dXNlcjpwYXNz"
    let result = decode_docker_auth("dXNlcjpwYXNz");
    assert!(result.is_some());
    let auth = result.unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "pass");
}

#[test]
fn decode_docker_auth_no_colon() {
    let result = decode_docker_auth("bm9jb2xvbg=="); // "nocolon"
    assert!(result.is_none());
}

#[test]
fn decode_docker_auth_empty_password() {
    // "user:" base64 = "dXNlcjo="
    let result = decode_docker_auth("dXNlcjo=");
    assert!(result.is_some());
    let auth = result.unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "");
}

#[test]
fn decode_docker_auth_invalid_base64() {
    let result = decode_docker_auth("!!!invalid!!!");
    assert!(result.is_none());
}

#[test]
fn decode_docker_auth_password_with_colons() {
    // "user:pa:ss:word" base64 = "dXNlcjpwYTpzczp3b3Jk"
    let result = decode_docker_auth("dXNlcjpwYTpzczp3b3Jk");
    assert!(result.is_some());
    let auth = result.unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "pa:ss:word");
}

#[test]
fn decode_docker_auth_empty_username_rejected() {
    // ":password" base64 = "OnBhc3N3b3Jk"
    let result = decode_docker_auth("OnBhc3N3b3Jk");
    assert!(result.is_none());
}

// --- resolve_from_docker_auths ---

#[test]
fn resolve_from_docker_auths_tries_https_prefix() {
    let mut auths = HashMap::new();
    auths.insert(
        "https://registry.example.com".to_string(),
        DockerAuthEntry {
            auth: Some("dXNlcjpwYXNz".to_string()), // user:pass
        },
    );
    let result = resolve_from_docker_auths(&auths, "registry.example.com");
    assert!(result.is_some());
    let auth = result.unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "pass");
}

#[test]
fn resolve_from_docker_auths_tries_v2_suffix() {
    let mut auths = HashMap::new();
    auths.insert(
        "https://myregistry.io/v2/".to_string(),
        DockerAuthEntry {
            auth: Some("dXNlcjpwYXNz".to_string()),
        },
    );
    let result = resolve_from_docker_auths(&auths, "myregistry.io");
    assert!(result.is_some());
    assert_eq!(result.unwrap().username, "user");
}

#[test]
fn resolve_from_docker_auths_no_match() {
    let mut auths = HashMap::new();
    auths.insert(
        "https://other.io".to_string(),
        DockerAuthEntry {
            auth: Some("dXNlcjpwYXNz".to_string()),
        },
    );
    let result = resolve_from_docker_auths(&auths, "registry.example.com");
    assert!(result.is_none());
}

// --- resolve_from_docker_auths edge cases ---

#[test]
fn resolve_from_docker_auths_v1_suffix() {
    let mut auths = HashMap::new();
    auths.insert(
        "https://myregistry.io/v1/".to_string(),
        DockerAuthEntry {
            auth: Some("dXNlcjpwYXNz".to_string()), // user:pass
        },
    );
    let result = resolve_from_docker_auths(&auths, "myregistry.io");
    assert!(result.is_some(), "should match v1 URL suffix");
    let auth = result.unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "pass");
}

#[test]
fn resolve_from_docker_auths_exact_hostname_match() {
    let mut auths = HashMap::new();
    auths.insert(
        "ghcr.io".to_string(),
        DockerAuthEntry {
            auth: Some("dXNlcjpwYXNz".to_string()),
        },
    );
    // Should match directly without needing https:// prefix
    let result = resolve_from_docker_auths(&auths, "ghcr.io");
    assert!(result.is_some(), "should match exact hostname");
    assert_eq!(result.unwrap().username, "user");
}

#[test]
fn resolve_from_docker_auths_docker_io_fallback_to_index() {
    // When querying "docker.io", should also check "index.docker.io" variants
    let mut auths = HashMap::new();
    auths.insert(
        "index.docker.io".to_string(),
        DockerAuthEntry {
            auth: Some("ZG9ja2VyOnNlY3JldA==".to_string()), // docker:secret
        },
    );
    let result = resolve_from_docker_auths(&auths, "docker.io");
    assert!(
        result.is_some(),
        "docker.io should fall back to index.docker.io"
    );
    let auth = result.unwrap();
    assert_eq!(auth.username, "docker");
    assert_eq!(auth.password, "secret");
}

#[test]
fn resolve_from_docker_auths_index_docker_io_fallback() {
    // When querying "index.docker.io", should also check index.docker.io variants
    let mut auths = HashMap::new();
    auths.insert(
        "https://index.docker.io/v1/".to_string(),
        DockerAuthEntry {
            auth: Some("ZG9ja2VyOnNlY3JldA==".to_string()), // docker:secret
        },
    );
    let result = resolve_from_docker_auths(&auths, "index.docker.io");
    assert!(
        result.is_some(),
        "index.docker.io should match https://index.docker.io/v1/ entry"
    );
    assert_eq!(result.unwrap().username, "docker");
}

#[test]
fn resolve_from_docker_auths_entry_with_null_auth_field() {
    let mut auths = HashMap::new();
    auths.insert("ghcr.io".to_string(), DockerAuthEntry { auth: None });
    let result = resolve_from_docker_auths(&auths, "ghcr.io");
    assert!(
        result.is_none(),
        "entry with null auth field should not resolve"
    );
}

#[test]
fn resolve_from_docker_auths_entry_with_invalid_base64() {
    let mut auths = HashMap::new();
    auths.insert(
        "ghcr.io".to_string(),
        DockerAuthEntry {
            auth: Some("not-valid-base64!!!".to_string()),
        },
    );
    let result = resolve_from_docker_auths(&auths, "ghcr.io");
    assert!(
        result.is_none(),
        "entry with invalid base64 should not resolve"
    );
}

#[test]
fn resolve_from_docker_auths_prefers_exact_match_over_url() {
    let mut auths = HashMap::new();
    // Exact match
    auths.insert(
        "ghcr.io".to_string(),
        DockerAuthEntry {
            auth: Some("ZXhhY3Q6bWF0Y2g=".to_string()), // exact:match
        },
    );
    // URL variant
    auths.insert(
        "https://ghcr.io".to_string(),
        DockerAuthEntry {
            auth: Some("dXJsOm1hdGNo".to_string()), // url:match
        },
    );
    let result = resolve_from_docker_auths(&auths, "ghcr.io");
    assert!(result.is_some());
    // The function tries candidates in order: exact, https://, https:///v2/, https:///v1/
    // So exact match should win
    assert_eq!(result.unwrap().username, "exact");
}

// --- get_bearer_token (mockito) ---

#[test]
fn get_bearer_token_parses_www_authenticate() {
    let mut server = mockito::Server::new();

    let www_auth = format!(
        r#"Bearer realm="{}/auth/token",service="registry.example.com",scope="repository:lib/test:pull,push""#,
        server.url()
    );

    server
        .mock(
            "GET",
            mockito::Matcher::Regex(r"/auth/token\?service=.*&scope=.*".to_string()),
        )
        .with_status(200)
        .with_body(r#"{"token":"tok-abc-123"}"#)
        .create();

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();

    let result = get_bearer_token(&agent, &www_auth, None);
    assert!(
        result.is_ok(),
        "get_bearer_token failed: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap(), "tok-abc-123");
}

#[test]
fn get_bearer_token_uses_access_token_field() {
    let mut server = mockito::Server::new();

    let www_auth = format!(r#"Bearer realm="{}/token",service="svc""#, server.url());

    server
        .mock(
            "GET",
            mockito::Matcher::Regex(r"/token\?service=.*".to_string()),
        )
        .with_status(200)
        .with_body(r#"{"access_token":"alt-token-456"}"#)
        .create();

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();

    let result = get_bearer_token(&agent, &www_auth, None);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "alt-token-456");
}

#[test]
fn get_bearer_token_fails_without_realm() {
    let agent = ureq::Agent::config_builder().build().new_agent();
    let result = get_bearer_token(&agent, "Bearer service=\"svc\"", None);
    assert!(result.is_err());
    assert!(
        matches!(result, Err(OciError::AuthFailed { .. })),
        "expected AuthFailed variant"
    );
}

#[test]
fn get_bearer_token_fails_on_non_json_token_response() {
    // 200 OK with malformed body triggers the AuthFailed variant on
    // serde_json::from_str() inside get_bearer_token.
    let mut server = mockito::Server::new();
    let www_auth = format!(r#"Bearer realm="{}/token",service="svc""#, server.url());

    server
        .mock(
            "GET",
            mockito::Matcher::Regex(r"/token\?service=.*".to_string()),
        )
        .with_status(200)
        .with_body("not json at all")
        .create();

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();

    let result = get_bearer_token(&agent, &www_auth, None);
    assert!(matches!(result, Err(OciError::AuthFailed { .. })));
}

#[test]
fn get_bearer_token_sends_basic_auth_when_provided() {
    let mut server = mockito::Server::new();

    let www_auth = format!(r#"Bearer realm="{}/token",service="svc""#, server.url());

    let auth = RegistryAuth {
        username: "user".to_string(),
        password: "pass".to_string(),
    };

    server
        .mock(
            "GET",
            mockito::Matcher::Regex(r"/token\?service=.*".to_string()),
        )
        .match_header(
            "Authorization",
            mockito::Matcher::Regex("Basic .*".to_string()),
        )
        .with_status(200)
        .with_body(r#"{"token":"authed-token"}"#)
        .create();

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();

    let result = get_bearer_token(&agent, &www_auth, Some(&auth));
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "authed-token");
}

// --- docker_config_path ---

#[test]
#[serial]
fn docker_config_path_uses_env() {
    let prev = std::env::var("DOCKER_CONFIG").ok();

    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("DOCKER_CONFIG", tmp.path().to_str().unwrap());
    }

    let path = docker_config_path();
    assert_eq!(path, tmp.path().join("config.json"));

    unsafe {
        match prev {
            Some(v) => std::env::set_var("DOCKER_CONFIG", v),
            None => std::env::remove_var("DOCKER_CONFIG"),
        }
    }
}

// --- DockerConfig deserialization ---

#[test]
fn docker_config_empty_json() {
    let config: DockerConfig = serde_json::from_str("{}").unwrap();
    assert!(config.auths.is_empty());
    assert!(config.cred_helpers.is_empty());
}

#[test]
fn docker_config_with_only_cred_helpers() {
    let json = r#"{"credHelpers":{"gcr.io":"gcloud"}}"#;
    let config: DockerConfig = serde_json::from_str(json).unwrap();
    assert!(config.auths.is_empty());
    assert_eq!(config.cred_helpers.len(), 1);
    assert_eq!(config.cred_helpers.get("gcr.io").unwrap(), "gcloud");
}

// --- extract_auth_param edge cases ---

#[test]
fn extract_auth_param_empty_value() {
    let header = r#"Bearer realm="",service="svc""#;
    assert_eq!(extract_auth_param(header, "realm"), Some(""));
}

#[test]
fn extract_auth_param_no_quotes() {
    let header = "Bearer realm_value=noquotes";
    assert_eq!(extract_auth_param(header, "realm"), None);
}

#[test]
fn extract_auth_param_url_with_special_chars() {
    let header = r#"Bearer realm="https://auth.example.com/token?foo=bar",service="svc""#;
    let realm = extract_auth_param(header, "realm").unwrap();
    assert_eq!(realm, "https://auth.example.com/token?foo=bar");
}

// --- Docker config parsing: additional coverage ---

#[test]
fn docker_config_with_empty_auth_field() {
    let config_json = r#"{
            "auths": {
                "ghcr.io": {
                    "auth": ""
                }
            }
        }"#;
    let config: DockerConfig = serde_json::from_str(config_json).unwrap();
    // Empty auth string should decode to empty Vec, which has no colon, so None
    let auth = resolve_from_docker_auths(&config.auths, "ghcr.io");
    assert!(auth.is_none(), "empty auth field should not resolve");
}

#[test]
fn docker_config_index_docker_io_fallback() {
    let config_json = r#"{
            "auths": {
                "index.docker.io": {
                    "auth": "dXNlcjpwYXNz"
                }
            }
        }"#;
    let config: DockerConfig = serde_json::from_str(config_json).unwrap();
    // Querying for "docker.io" should fall back to checking index.docker.io
    let auth = resolve_from_docker_auths(&config.auths, "docker.io");
    assert!(
        auth.is_some(),
        "docker.io should fall back to index.docker.io"
    );
    assert_eq!(auth.unwrap().username, "user");
}

#[test]
fn docker_config_v1_suffix_match() {
    let config_json = r#"{
            "auths": {
                "https://ghcr.io/v1/": {
                    "auth": "dXNlcjpwYXNz"
                }
            }
        }"#;
    let config: DockerConfig = serde_json::from_str(config_json).unwrap();
    let auth = resolve_from_docker_auths(&config.auths, "ghcr.io");
    assert!(auth.is_some(), "should match https://ghcr.io/v1/");
}

// --- base64 edge cases ---

#[test]
fn base64_encode_single_byte() {
    let encoded = base64_encode(b"a");
    let decoded = base64_decode(&encoded).unwrap();
    assert_eq!(decoded, b"a");
}

#[test]
fn base64_encode_two_bytes() {
    let encoded = base64_encode(b"ab");
    let decoded = base64_decode(&encoded).unwrap();
    assert_eq!(decoded, b"ab");
}

#[test]
fn base64_encode_three_bytes() {
    // Three bytes = no padding needed
    let encoded = base64_encode(b"abc");
    assert!(!encoded.contains('='));
    let decoded = base64_decode(&encoded).unwrap();
    assert_eq!(decoded, b"abc");
}

#[test]
fn base64_encode_binary_data() {
    let data: Vec<u8> = (0..=255).collect();
    let encoded = base64_encode(&data);
    let decoded = base64_decode(&encoded).unwrap();
    assert_eq!(decoded, data);
}

#[test]
fn base64_decode_with_whitespace() {
    // base64_decode trims whitespace
    let decoded = base64_decode("  dXNlcjpwYXNz  ").unwrap();
    assert_eq!(String::from_utf8(decoded).unwrap(), "user:pass");
}

#[test]
fn base64_decode_invalid_length() {
    // Non-multiple-of-4 length should fail
    assert!(base64_decode("abc").is_none());
    assert!(base64_decode("abcde").is_none());
}

#[test]
fn base64_decode_invalid_chars() {
    // Invalid base64 characters should fail
    assert!(base64_decode("ab~d").is_none());
}

// --- RegistryAuth basic_auth_header ---

#[test]
fn registry_auth_basic_auth_header_format() {
    let auth = RegistryAuth {
        username: "testuser".to_string(),
        password: "testpass".to_string(),
    };
    let header = auth.basic_auth_header();
    assert!(header.starts_with("Basic "));
    // "testuser:testpass" base64 = "dGVzdHVzZXI6dGVzdHBhc3M="
    let encoded_part = header.strip_prefix("Basic ").unwrap();
    let decoded = base64_decode(encoded_part).unwrap();
    let decoded_str = String::from_utf8(decoded).unwrap();
    assert_eq!(decoded_str, "testuser:testpass");
}

#[test]
fn registry_auth_basic_auth_header_special_chars() {
    let auth = RegistryAuth {
        username: "user".to_string(),
        password: "p@ss:w0rd!".to_string(),
    };
    let header = auth.basic_auth_header();
    let encoded_part = header.strip_prefix("Basic ").unwrap();
    let decoded = base64_decode(encoded_part).unwrap();
    let decoded_str = String::from_utf8(decoded).unwrap();
    assert_eq!(decoded_str, "user:p@ss:w0rd!");
}

// --- base64 padding edge cases ---

#[test]
fn base64_encode_padding_one_byte() {
    // "a" -> "YQ==" (requires 2 padding chars)
    let encoded = base64_encode(b"a");
    assert_eq!(encoded, "YQ==");
    let decoded = base64_decode(&encoded).unwrap();
    assert_eq!(decoded, b"a");
}

#[test]
fn base64_encode_padding_two_bytes() {
    // "ab" -> "YWI=" (requires 1 padding char)
    let encoded = base64_encode(b"ab");
    assert_eq!(encoded, "YWI=");
    let decoded = base64_decode(&encoded).unwrap();
    assert_eq!(decoded, b"ab");
}

#[test]
fn base64_encode_no_padding_three_bytes() {
    // "abc" -> "YWJj" (no padding needed)
    let encoded = base64_encode(b"abc");
    assert_eq!(encoded, "YWJj");
    let decoded = base64_decode(&encoded).unwrap();
    assert_eq!(decoded, b"abc");
}

#[test]
fn base64_decode_invalid_length_short() {
    // Not a multiple of 4
    let result = base64_decode("abc");
    assert!(result.is_none(), "invalid length should fail");
}

#[test]
fn base64_decode_invalid_chars_special() {
    let result = base64_decode("!!!!");
    assert!(result.is_none(), "invalid base64 characters should fail");
}

#[test]
fn base64_roundtrip_binary_data() {
    let data: Vec<u8> = (0..=255).collect();
    let encoded = base64_encode(&data);
    let decoded = base64_decode(&encoded).unwrap();
    assert_eq!(decoded, data);
}

// --- RegistryAuth basic_auth_header (additional) ---

#[test]
fn registry_auth_basic_header_format() {
    let auth = RegistryAuth {
        username: "myuser".to_string(),
        password: "mypass".to_string(),
    };
    let header = auth.basic_auth_header();
    assert!(header.starts_with("Basic "), "should start with 'Basic '");

    // Decode and verify
    let encoded = header.strip_prefix("Basic ").unwrap();
    let decoded = base64_decode(encoded).unwrap();
    let cred = String::from_utf8(decoded).unwrap();
    assert_eq!(cred, "myuser:mypass");
}

// --- extract_auth_param edge cases ---

#[test]
fn extract_auth_param_missing_param() {
    let header = r#"Bearer realm="https://auth.example.com/token""#;
    assert!(extract_auth_param(header, "service").is_none());
    assert!(extract_auth_param(header, "scope").is_none());
    assert!(extract_auth_param(header, "realm").is_some());
}

#[test]
fn extract_auth_param_empty_value_parsed() {
    let header = r#"Bearer realm="",service="svc""#;
    assert_eq!(extract_auth_param(header, "realm"), Some(""));
    assert_eq!(extract_auth_param(header, "service"), Some("svc"));
}

// --- decode_docker_auth: password with special chars ---

#[test]
fn decode_docker_auth_with_token_password() {
    // Simulate a PAT token as password: "ghp_abc123xyz"
    let input = "user:ghp_abc123xyz";
    let encoded = base64_encode(input.as_bytes());
    let result = decode_docker_auth(&encoded);
    assert!(result.is_some());
    let auth = result.unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "ghp_abc123xyz");
}

// --- RegistryAuth::resolve: file-based path coverage ---

#[test]
#[serial]
fn registry_auth_resolve_reads_docker_config_file() {
    use crate::test_helpers::EnvVarGuard;
    // Unset env vars so the file-based path is the only one that can match.
    let _user = EnvVarGuard::unset("REGISTRY_USERNAME");
    let _pass = EnvVarGuard::unset("REGISTRY_PASSWORD");
    // Point DOCKER_CONFIG at a fresh temp dir with a config.json.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("config.json"),
        r#"{"auths":{"my.reg.example":{"auth":"dXNlcjpwYXNz"}}}"#,
    )
    .unwrap();
    let _dc = EnvVarGuard::set("DOCKER_CONFIG", tmp.path().to_str().unwrap());

    let auth = RegistryAuth::resolve("my.reg.example").expect("file-based resolve must succeed");
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "pass");
}

#[test]
#[serial]
fn registry_auth_resolve_returns_none_when_no_match_in_config() {
    use crate::test_helpers::EnvVarGuard;
    let _user = EnvVarGuard::unset("REGISTRY_USERNAME");
    let _pass = EnvVarGuard::unset("REGISTRY_PASSWORD");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("config.json"),
        r#"{"auths":{"other.example":{"auth":"dXNlcjpwYXNz"}}}"#,
    )
    .unwrap();
    let _dc = EnvVarGuard::set("DOCKER_CONFIG", tmp.path().to_str().unwrap());

    let result = RegistryAuth::resolve("missing.example");
    assert!(
        result.is_none(),
        "no matching entry must return None, got: {:?}",
        result.map(|a| a.username)
    );
}

#[test]
#[serial]
fn registry_auth_resolve_returns_none_when_env_vars_empty() {
    use crate::test_helpers::EnvVarGuard;
    // Both vars present but empty must not satisfy the resolution.
    let _user = EnvVarGuard::set("REGISTRY_USERNAME", "");
    let _pass = EnvVarGuard::set("REGISTRY_PASSWORD", "");
    // Also point DOCKER_CONFIG at an empty dir so the file path doesn't fire.
    let tmp = tempfile::tempdir().unwrap();
    let _dc = EnvVarGuard::set("DOCKER_CONFIG", tmp.path().to_str().unwrap());

    let result = RegistryAuth::resolve("any.example");
    assert!(
        result.is_none(),
        "empty env vars + no config must return None"
    );
}

#[test]
#[serial]
fn registry_auth_resolve_credential_helper_branch_invokes_helper() {
    use crate::test_helpers::EnvVarGuard;
    let _user = EnvVarGuard::unset("REGISTRY_USERNAME");
    let _pass = EnvVarGuard::unset("REGISTRY_PASSWORD");
    // Set up DOCKER_CONFIG with a credHelpers entry that points to a non-existent
    // binary. The helper-spawn fails, .ok() returns None inside the closure, and
    // the resolve falls through to None.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("config.json"),
        r#"{"auths":{},"credHelpers":{"helper.example":"cfgd-test-no-such-helper-binary"}}"#,
    )
    .unwrap();
    let _dc = EnvVarGuard::set("DOCKER_CONFIG", tmp.path().to_str().unwrap());

    let result = RegistryAuth::resolve("helper.example");
    // Helper binary doesn't exist → spawn fails → None.
    assert!(result.is_none());
}

#[test]
fn get_bearer_token_response_with_no_token_returns_auth_failed() {
    let mut server = mockito::Server::new();
    let www_auth = format!(r#"Bearer realm="{}/token",service="svc""#, server.url());

    // Valid JSON but neither `token` nor `access_token` populated.
    server
        .mock(
            "GET",
            mockito::Matcher::Regex(r"/token\?service=.*".to_string()),
        )
        .with_status(200)
        .with_body(r#"{"other":"field"}"#)
        .create();

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();
    let result = get_bearer_token(&agent, &www_auth, None);
    assert!(matches!(result, Err(OciError::AuthFailed { .. })));
    let msg = format!("{:?}", result.err().unwrap());
    assert!(msg.contains("no token"), "must mention no token: {msg}");
}

#[test]
fn get_bearer_token_handles_failed_http_call() {
    // The realm URL points to a non-listening port so the call fails outright.
    let www_auth = r#"Bearer realm="http://127.0.0.1:1/token",service="svc""#;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(500)))
        .build()
        .new_agent();
    let result = get_bearer_token(&agent, www_auth, None);
    assert!(matches!(result, Err(OciError::AuthFailed { .. })));
}

// --- resolve_from_credential_helper (docker-credential-<helper> shim) ---
//
// `resolve_from_credential_helper` shells out to `docker-credential-<name>`,
// writes the registry to its stdin, and parses `{"Username":..,"Secret":..}`
// from stdout. These drive the real spawn/stdin/parse path via a fake helper
// binary installed at the front of PATH. `#[serial]` because PATH mutation is
// process-global.

#[cfg(unix)]
#[test]
#[serial]
fn credential_helper_parses_capitalized_username_secret() {
    use crate::test_helpers::install_named_path_shim;
    // Docker credential helpers emit capitalized keys; serde aliases map them.
    let (_dir, _path) = install_named_path_shim(
        "docker-credential-cfgdtesthelper",
        0,
        r#"{"ServerURL":"ghcr.io","Username":"helperuser","Secret":"helpersecret"}"#,
        "",
    );
    let auth = resolve_from_credential_helper("cfgdtesthelper", "ghcr.io")
        .expect("helper output must parse into credentials");
    assert_eq!(auth.username, "helperuser");
    assert_eq!(auth.password, "helpersecret");
}

#[cfg(unix)]
#[test]
#[serial]
fn credential_helper_parses_lowercase_username_secret() {
    use crate::test_helpers::install_named_path_shim;
    // The lowercase form (no alias needed) must also parse.
    let (_dir, _path) = install_named_path_shim(
        "docker-credential-cfgdtesthelper",
        0,
        r#"{"username":"lc-user","secret":"lc-secret"}"#,
        "",
    );
    let auth = resolve_from_credential_helper("cfgdtesthelper", "registry.example")
        .expect("lowercase helper output must parse");
    assert_eq!(auth.username, "lc-user");
    assert_eq!(auth.password, "lc-secret");
}

#[cfg(unix)]
#[test]
#[serial]
fn credential_helper_nonzero_exit_returns_none() {
    use crate::test_helpers::install_named_path_shim;
    // A helper exiting non-zero (e.g. "credentials not found") must yield None.
    let (_dir, _path) = install_named_path_shim(
        "docker-credential-cfgdtesthelper",
        1,
        "",
        "credentials not found",
    );
    let result = resolve_from_credential_helper("cfgdtesthelper", "ghcr.io");
    assert!(result.is_none(), "non-zero helper exit must return None");
}

#[cfg(unix)]
#[test]
#[serial]
fn credential_helper_empty_username_returns_none() {
    use crate::test_helpers::install_named_path_shim;
    // Helper succeeds but returns an empty username — must be rejected.
    let (_dir, _path) = install_named_path_shim(
        "docker-credential-cfgdtesthelper",
        0,
        r#"{"Username":"","Secret":"some-secret"}"#,
        "",
    );
    let result = resolve_from_credential_helper("cfgdtesthelper", "ghcr.io");
    assert!(result.is_none(), "empty username must return None");
}

#[cfg(unix)]
#[test]
#[serial]
fn credential_helper_invalid_json_returns_none() {
    use crate::test_helpers::install_named_path_shim;
    // Helper exits 0 but emits non-JSON — serde parse fails → None.
    let (_dir, _path) =
        install_named_path_shim("docker-credential-cfgdtesthelper", 0, "not json output", "");
    let result = resolve_from_credential_helper("cfgdtesthelper", "ghcr.io");
    assert!(
        result.is_none(),
        "invalid JSON from helper must return None"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn resolve_uses_cred_helper_when_config_names_it() {
    use crate::test_helpers::{EnvVarGuard, install_named_path_shim};
    // End-to-end: env empty, docker config.json has no direct auth but names a
    // credHelper for the registry; resolution must spawn the helper and return
    // its credentials. This exercises the success arm of `resolve`'s cred-helper
    // branch (the path that is otherwise only hit on a real machine).
    let _user = EnvVarGuard::unset("REGISTRY_USERNAME");
    let _pass = EnvVarGuard::unset("REGISTRY_PASSWORD");

    let (_dir, _path) = install_named_path_shim(
        "docker-credential-cfgdtesthelper",
        0,
        r#"{"Username":"viahelper","Secret":"viahelper-secret"}"#,
        "",
    );

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("config.json"),
        r#"{"auths":{},"credHelpers":{"helper.registry.example":"cfgdtesthelper"}}"#,
    )
    .unwrap();
    let _dc = EnvVarGuard::set("DOCKER_CONFIG", tmp.path().to_str().unwrap());

    let auth = RegistryAuth::resolve("helper.registry.example")
        .expect("resolve must use the named credential helper");
    assert_eq!(auth.username, "viahelper");
    assert_eq!(auth.password, "viahelper-secret");
}

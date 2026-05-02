// Registry authentication: Docker config.json, env vars, credential helpers,
// bearer token exchange, and base64 encoding for HTTP Basic auth.

use std::collections::HashMap;

use serde::Deserialize;

use crate::errors::OciError;

/// Credentials for authenticating to an OCI registry.
#[derive(Debug, Clone)]
pub struct RegistryAuth {
    pub username: String,
    pub password: String,
}

/// Docker config.json structure (subset).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DockerConfig {
    #[serde(default)]
    pub(super) auths: HashMap<String, DockerAuthEntry>,
    #[serde(default)]
    pub(super) cred_helpers: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct DockerAuthEntry {
    pub(super) auth: Option<String>,
}

impl RegistryAuth {
    /// Resolve credentials for the given registry hostname.
    ///
    /// Tries, in order:
    /// 1. `REGISTRY_USERNAME` / `REGISTRY_PASSWORD` environment variables
    /// 2. Docker config.json (`~/.docker/config.json`) — base64 auth field
    /// 3. Docker credential helpers (`docker-credential-<helper>`)
    pub fn resolve(registry: &str) -> Option<Self> {
        // 1. Environment variables
        if let (Ok(user), Ok(pass)) = (
            std::env::var("REGISTRY_USERNAME"),
            std::env::var("REGISTRY_PASSWORD"),
        ) && !user.is_empty()
            && !pass.is_empty()
        {
            return Some(RegistryAuth {
                username: user,
                password: pass,
            });
        }

        // 2. Docker config.json
        let config_path = docker_config_path();
        if let Ok(contents) = std::fs::read_to_string(&config_path)
            && let Ok(config) = serde_json::from_str::<DockerConfig>(&contents)
        {
            // Try direct auth entry
            if let Some(auth) = resolve_from_docker_auths(&config.auths, registry) {
                return Some(auth);
            }

            // Try credential helper
            if let Some(helper) = config.cred_helpers.get(registry)
                && let Some(auth) = resolve_from_credential_helper(helper, registry)
            {
                return Some(auth);
            }
        }

        None
    }

    /// Returns the HTTP Basic auth header value.
    pub(super) fn basic_auth_header(&self) -> String {
        format!(
            "Basic {}",
            base64_encode(format!("{}:{}", self.username, self.password).as_bytes())
        )
    }
}

pub(super) fn docker_config_path() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("DOCKER_CONFIG") {
        return std::path::PathBuf::from(dir).join("config.json");
    }
    crate::expand_tilde(std::path::Path::new("~/.docker/config.json"))
}

/// Try to find credentials in the Docker config auths map.
/// Keys can be full URLs like `https://ghcr.io` or just hostnames like `ghcr.io`.
pub(super) fn resolve_from_docker_auths(
    auths: &HashMap<String, DockerAuthEntry>,
    registry: &str,
) -> Option<RegistryAuth> {
    // Try exact match and common variants
    let candidates = [
        registry.to_string(),
        format!("https://{}", registry),
        format!("https://{}/v2/", registry),
        format!("https://{}/v1/", registry),
    ];

    for candidate in &candidates {
        if let Some(entry) = auths.get(candidate)
            && let Some(ref auth_b64) = entry.auth
            && let Some(cred) = decode_docker_auth(auth_b64)
        {
            return Some(cred);
        }
    }

    // For docker.io, also check index.docker.io
    if registry == "docker.io" || registry == "index.docker.io" {
        let alt_candidates = [
            "https://index.docker.io/v1/".to_string(),
            "index.docker.io".to_string(),
        ];
        for candidate in &alt_candidates {
            if let Some(entry) = auths.get(candidate)
                && let Some(ref auth_b64) = entry.auth
                && let Some(cred) = decode_docker_auth(auth_b64)
            {
                return Some(cred);
            }
        }
    }

    None
}

/// Decode a base64 `user:password` auth string from Docker config.
pub(super) fn decode_docker_auth(auth_b64: &str) -> Option<RegistryAuth> {
    let decoded = base64_decode(auth_b64)?;
    let decoded_str = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded_str.split_once(':')?;
    if user.is_empty() {
        return None;
    }
    Some(RegistryAuth {
        username: user.to_string(),
        password: pass.to_string(),
    })
}

/// Run a Docker credential helper to get credentials.
fn resolve_from_credential_helper(helper_name: &str, registry: &str) -> Option<RegistryAuth> {
    let helper_bin = format!("docker-credential-{}", helper_name);
    let output = std::process::Command::new(&helper_bin)
        .arg("get")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(registry.as_bytes()).ok();
            }
            drop(child.stdin.take()); // Close stdin so helper can proceed
            let start = std::time::Instant::now();
            let timeout = std::time::Duration::from_secs(10);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return child.wait_with_output().ok(),
                    Ok(None) if start.elapsed() >= timeout => {
                        let _ = child.kill();
                        let _ = child.wait();
                        tracing::warn!(helper = %helper_bin, "credential helper timed out after {}s", timeout.as_secs());
                        return None;
                    }
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                    Err(_) => return None,
                }
            }
        })?;

    if !output.status.success() {
        return None;
    }

    #[derive(Deserialize)]
    struct CredHelperOutput {
        #[serde(alias = "Username")]
        username: String,
        #[serde(alias = "Secret")]
        secret: String,
    }

    let parsed: CredHelperOutput = serde_json::from_slice(&output.stdout).ok()?;
    if parsed.username.is_empty() {
        return None;
    }
    Some(RegistryAuth {
        username: parsed.username,
        password: parsed.secret,
    })
}

/// Attempt to get a bearer token for the given registry+repository scope.
/// Registries return a 401 with a Www-Authenticate header pointing to a token endpoint.
pub(super) fn get_bearer_token(
    agent: &ureq::Agent,
    www_authenticate: &str,
    auth: Option<&RegistryAuth>,
) -> Result<String, OciError> {
    // Parse: Bearer realm="...",service="...",scope="..."
    let realm = extract_auth_param(www_authenticate, "realm");
    let service = extract_auth_param(www_authenticate, "service");
    let scope = extract_auth_param(www_authenticate, "scope");

    let realm = realm.ok_or_else(|| OciError::AuthFailed {
        registry: String::new(),
        message: format!("missing realm in Www-Authenticate header: {www_authenticate}"),
    })?;

    let mut url = realm.to_string();
    let mut params = Vec::new();
    if let Some(svc) = service {
        params.push(format!("service={}", svc));
    }
    if let Some(sc) = scope {
        params.push(format!("scope={}", sc));
    }
    if !params.is_empty() {
        url = format!("{}?{}", url, params.join("&"));
    }

    let mut req = agent.get(&url);
    if let Some(cred) = auth {
        req = req.set("Authorization", &cred.basic_auth_header());
    }

    let resp = req.call().map_err(|e| OciError::AuthFailed {
        registry: String::new(),
        message: format!("token request failed: {e}"),
    })?;

    #[derive(Deserialize)]
    struct TokenResponse {
        token: Option<String>,
        access_token: Option<String>,
    }

    let body_str = resp.into_string().map_err(|e| OciError::AuthFailed {
        registry: String::new(),
        message: format!("cannot read token response body: {e}"),
    })?;
    let body: TokenResponse =
        serde_json::from_str(&body_str).map_err(|e| OciError::AuthFailed {
            registry: String::new(),
            message: format!("invalid token response JSON: {e}"),
        })?;

    body.token
        .or(body.access_token)
        .ok_or_else(|| OciError::AuthFailed {
            registry: String::new(),
            message: "no token in response".to_string(),
        })
}

pub(super) fn extract_auth_param<'a>(header: &'a str, param: &str) -> Option<&'a str> {
    let search = format!("{param}=\"");
    let start = header.find(&search)?;
    let value_start = start + search.len();
    let end = header[value_start..].find('"')?;
    Some(&header[value_start..value_start + end])
}

// ---------------------------------------------------------------------------
// Base64 helpers (no external dependency needed for this)
// ---------------------------------------------------------------------------

pub(super) fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

pub(super) fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn char_val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    }

    let input = input.trim();
    if input.is_empty() {
        return Some(Vec::new());
    }

    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }

    let mut result = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let a = char_val(chunk[0])?;
        let b = char_val(chunk[1])?;
        let c = char_val(chunk[2])?;
        let d = char_val(chunk[3])?;

        let triple = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | (d as u32);

        result.push(((triple >> 16) & 0xFF) as u8);
        if chunk[2] != b'=' {
            result.push(((triple >> 8) & 0xFF) as u8);
        }
        if chunk[3] != b'=' {
            result.push((triple & 0xFF) as u8);
        }
    }

    Some(result)
}

#[cfg(test)]
mod tests {
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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = get_bearer_token(&agent, &www_auth, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "alt-token-456");
    }

    #[test]
    fn get_bearer_token_fails_without_realm() {
        let agent = ureq::AgentBuilder::new().build();
        let result = get_bearer_token(&agent, "Bearer service=\"svc\"", None);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("missing realm"),
            "expected missing realm error, got: {err_msg}"
        );
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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

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
}

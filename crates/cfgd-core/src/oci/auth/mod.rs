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
        req = req.header("Authorization", &cred.basic_auth_header());
    }

    let mut resp = req.call().map_err(|e| OciError::AuthFailed {
        registry: String::new(),
        message: format!("token request failed: {e}"),
    })?;

    #[derive(Deserialize)]
    struct TokenResponse {
        token: Option<String>,
        access_token: Option<String>,
    }

    let body_str = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| OciError::AuthFailed {
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
mod tests;

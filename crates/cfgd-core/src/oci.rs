// OCI artifact push/pull for cfgd modules.
//
// Implements a minimal OCI Distribution Spec client using ureq (sync HTTP).
// Supports pushing/pulling module archives with custom media types,
// registry authentication via Docker config.json, credential helpers, and env vars.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::errors::OciError;
use crate::output::Printer;

// ---------------------------------------------------------------------------
// Media type constants
// ---------------------------------------------------------------------------

pub const MEDIA_TYPE_MODULE_CONFIG: &str = "application/vnd.cfgd.module.config.v1+json";
pub const MEDIA_TYPE_MODULE_LAYER: &str = "application/vnd.cfgd.module.layer.v1.tar+gzip";

/// OCI image manifest v2 media type.
const MEDIA_TYPE_OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";

// ---------------------------------------------------------------------------
// OCI Reference
// ---------------------------------------------------------------------------

/// A parsed OCI artifact reference: `[registry/]repository[:tag|@digest]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciReference {
    /// Registry hostname (e.g. `ghcr.io`, `docker.io`). Defaults to `docker.io`.
    pub registry: String,
    /// Repository path (e.g. `myorg/mymodule`).
    pub repository: String,
    /// Tag or digest. Defaults to `latest` if neither specified.
    pub reference: ReferenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceKind {
    Tag(String),
    Digest(String),
}

impl std::fmt::Display for OciReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reference {
            ReferenceKind::Tag(tag) => {
                write!(f, "{}/{}:{}", self.registry, self.repository, tag)
            }
            ReferenceKind::Digest(digest) => {
                write!(f, "{}/{}@{}", self.registry, self.repository, digest)
            }
        }
    }
}

impl OciReference {
    /// Parse an OCI reference string.
    ///
    /// Accepted formats:
    /// - `registry.example.com/repo/name:tag`
    /// - `registry.example.com/repo/name@sha256:abc...`
    /// - `repo/name:tag` (defaults to `docker.io`)
    /// - `repo/name` (defaults to `docker.io`, tag `latest`)
    /// - `localhost:5000/repo:tag`
    pub fn parse(reference: &str) -> Result<Self, OciError> {
        if reference.is_empty()
            || reference
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(OciError::InvalidReference {
                reference: reference.to_string(),
            });
        }

        // Split off digest first (@sha256:...)
        let (name_part, ref_kind) = if let Some((name, digest)) = reference.split_once('@') {
            (name, ReferenceKind::Digest(digest.to_string()))
        } else if let Some((name, tag)) = reference.rsplit_once(':') {
            // Be careful not to split on port numbers. A port number is preceded
            // by the registry hostname (no slashes after the colon before the next
            // slash). We check: if the part after ':' contains '/' it's not a tag.
            // Also, if the name part has no '/' at all, and the tag looks numeric,
            // it might be a port — but that would make the reference invalid without
            // a repo path. We handle by checking if 'tag' looks like a port (all digits)
            // AND the name_part has no slash.
            if tag.chars().all(|c| c.is_ascii_digit()) && !name.contains('/') {
                // Looks like host:port with no repo — invalid
                return Err(OciError::InvalidReference {
                    reference: reference.to_string(),
                });
            }
            // If the text before ':' ends with a slash-less segment that contains '.',
            // and the text after ':' contains '/', then it's registry:port/repo/name
            if tag.contains('/') {
                // The colon was a port separator, not a tag separator.
                // Re-parse without splitting on this colon.
                (reference, ReferenceKind::Tag("latest".to_string()))
            } else {
                (name, ReferenceKind::Tag(tag.to_string()))
            }
        } else {
            (reference, ReferenceKind::Tag("latest".to_string()))
        };

        // Now split name_part into registry and repository.
        // Convention: if the first path component contains a dot or colon, or is
        // "localhost", it's a registry hostname. Otherwise, default to docker.io.
        let parts: Vec<&str> = name_part.splitn(2, '/').collect();
        let (registry, repository) = if parts.len() == 1 {
            // Just a name like "ubuntu" — docker.io/library/<name>
            ("docker.io".to_string(), format!("library/{}", parts[0]))
        } else {
            let first = parts[0];
            let rest = parts[1];
            if first.contains('.') || first.contains(':') || first == "localhost" {
                (first.to_string(), rest.to_string())
            } else {
                // e.g. "myorg/myrepo" — default registry
                ("docker.io".to_string(), name_part.to_string())
            }
        };

        if repository.is_empty() {
            return Err(OciError::InvalidReference {
                reference: reference.to_string(),
            });
        }

        Ok(OciReference {
            registry,
            repository,
            reference: ref_kind,
        })
    }

    /// The tag string (or digest) used in API paths.
    pub fn reference_str(&self) -> &str {
        match &self.reference {
            ReferenceKind::Tag(t) => t,
            ReferenceKind::Digest(d) => d,
        }
    }

    /// Base API URL for this registry.
    fn api_base(&self) -> String {
        let scheme = if self.registry == "localhost"
            || self.registry.starts_with("localhost:")
            || self.registry.starts_with("127.0.0.1")
            || is_insecure_registry(&self.registry)
        {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{}/v2", self.registry)
    }
}

/// Check if a registry is listed in `OCI_INSECURE_REGISTRIES` (comma-separated).
fn is_insecure_registry(registry: &str) -> bool {
    std::env::var("OCI_INSECURE_REGISTRIES")
        .unwrap_or_default()
        .split(',')
        .any(|r| r.trim() == registry)
}

// ---------------------------------------------------------------------------
// Registry Authentication
// ---------------------------------------------------------------------------

/// Credentials for authenticating to an OCI registry.
#[derive(Debug, Clone)]
pub struct RegistryAuth {
    pub username: String,
    pub password: String,
}

/// Docker config.json structure (subset).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DockerConfig {
    #[serde(default)]
    auths: HashMap<String, DockerAuthEntry>,
    #[serde(default)]
    cred_helpers: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct DockerAuthEntry {
    auth: Option<String>,
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
    fn basic_auth_header(&self) -> String {
        format!(
            "Basic {}",
            base64_encode(format!("{}:{}", self.username, self.password).as_bytes())
        )
    }
}

fn docker_config_path() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("DOCKER_CONFIG") {
        return std::path::PathBuf::from(dir).join("config.json");
    }
    crate::expand_tilde(std::path::Path::new("~/.docker/config.json"))
}

/// Try to find credentials in the Docker config auths map.
/// Keys can be full URLs like `https://ghcr.io` or just hostnames like `ghcr.io`.
fn resolve_from_docker_auths(
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
fn decode_docker_auth(auth_b64: &str) -> Option<RegistryAuth> {
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

// ---------------------------------------------------------------------------
// OCI Manifest types (OCI Image Manifest v1)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciManifest {
    schema_version: u32,
    media_type: String,
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    annotations: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciDescriptor {
    media_type: String,
    digest: String,
    size: u64,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    annotations: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Internal HTTP helpers
// ---------------------------------------------------------------------------

/// Attempt to get a bearer token for the given registry+repository scope.
/// Registries return a 401 with a Www-Authenticate header pointing to a token endpoint.
fn get_bearer_token(
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

fn extract_auth_param<'a>(header: &'a str, param: &str) -> Option<&'a str> {
    let search = format!("{param}=\"");
    let start = header.find(&search)?;
    let value_start = start + search.len();
    let end = header[value_start..].find('"')?;
    Some(&header[value_start..value_start + end])
}

/// Make an authenticated request. Handles 401 → token exchange flow.
fn authenticated_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    auth: Option<&RegistryAuth>,
    accept: Option<&str>,
    content_type: Option<&str>,
    body: Option<&[u8]>,
) -> Result<ureq::Response, OciError> {
    // First attempt — may get 401
    let mut req = match method {
        "GET" => agent.get(url),
        "PUT" => agent.put(url),
        "POST" => agent.post(url),
        "HEAD" => agent.head(url),
        "PATCH" => agent.request("PATCH", url),
        _ => agent.get(url),
    };

    if let Some(ct) = content_type {
        req = req.set("Content-Type", ct);
    }
    if let Some(acc) = accept {
        req = req.set("Accept", acc);
    }
    if let Some(cred) = auth {
        req = req.set("Authorization", &cred.basic_auth_header());
    }

    let result = if let Some(b) = body {
        req.send_bytes(b)
    } else {
        req.call()
    };

    match result {
        Ok(resp) => Ok(resp),
        Err(ureq::Error::Status(401, resp)) => {
            // Get the Www-Authenticate header and try token auth
            let www_auth = resp
                .header("Www-Authenticate")
                .or_else(|| resp.header("www-authenticate"))
                .unwrap_or("")
                .to_string();

            if www_auth.is_empty() || !www_auth.contains("Bearer") {
                return Err(OciError::AuthFailed {
                    registry: url.to_string(),
                    message: "401 with no Bearer challenge".to_string(),
                });
            }

            let token = get_bearer_token(agent, &www_auth, auth)?;

            // Retry with bearer token
            let mut req2 = match method {
                "GET" => agent.get(url),
                "PUT" => agent.put(url),
                "POST" => agent.post(url),
                "HEAD" => agent.head(url),
                "PATCH" => agent.request("PATCH", url),
                _ => agent.get(url),
            };

            if let Some(ct) = content_type {
                req2 = req2.set("Content-Type", ct);
            }
            if let Some(acc) = accept {
                req2 = req2.set("Accept", acc);
            }
            req2 = req2.set("Authorization", &format!("Bearer {}", token));

            let result2 = if let Some(b) = body {
                req2.send_bytes(b)
            } else {
                req2.call()
            };

            result2.map_err(|e| OciError::RequestFailed {
                message: format!("{e}"),
            })
        }
        Err(ureq::Error::Status(code, _)) => Err(OciError::RequestFailed {
            message: format!("HTTP {code} from {url}"),
        }),
        Err(e) => Err(OciError::RequestFailed {
            message: format!("{e}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Blob helpers
// ---------------------------------------------------------------------------

fn sha256_digest(data: &[u8]) -> String {
    format!("sha256:{}", crate::sha256_hex(data))
}

/// Upload a blob to the registry via the monolithic upload flow.
/// POST /v2/{name}/blobs/uploads/ → PUT with digest.
fn upload_blob(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    data: &[u8],
    content_type: &str,
) -> Result<String, OciError> {
    let digest = sha256_digest(data);

    // Check if blob already exists (HEAD)
    let head_url = format!(
        "{}/{}/blobs/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        digest
    );
    let head_result = authenticated_request(agent, "HEAD", &head_url, auth, None, None, None);
    if head_result.is_ok() {
        tracing::debug!(digest = %digest, "blob already exists, skipping upload");
        return Ok(digest);
    }

    // Start upload
    let upload_url = format!(
        "{}/{}/blobs/uploads/",
        oci_ref.api_base(),
        oci_ref.repository,
    );

    let resp = authenticated_request(agent, "POST", &upload_url, auth, None, None, Some(&[]))
        .map_err(|e| OciError::BlobUploadFailed {
            digest: digest.clone(),
            message: format!("upload initiation failed: {e}"),
        })?;

    let location = resp
        .header("Location")
        .ok_or_else(|| OciError::BlobUploadFailed {
            digest: digest.clone(),
            message: "no Location header in upload response".to_string(),
        })?
        .to_string();

    // Complete upload with PUT — monolithic upload
    let sep = if location.contains('?') { "&" } else { "?" };
    let put_url = format!("{location}{sep}digest={digest}");

    // For the PUT, we need to handle the case where location is a relative URL
    let put_url = if put_url.starts_with("http://") || put_url.starts_with("https://") {
        put_url
    } else {
        format!(
            "{}{}",
            oci_ref
                .api_base()
                .trim_end_matches("/v2")
                .trim_end_matches("/v2/"),
            put_url
        )
    };

    authenticated_request(
        agent,
        "PUT",
        &put_url,
        auth,
        None,
        Some(content_type),
        Some(data),
    )
    .map_err(|e| OciError::BlobUploadFailed {
        digest: digest.clone(),
        message: format!("blob PUT failed: {e}"),
    })?;

    tracing::debug!(digest = %digest, size = data.len(), "blob uploaded");
    Ok(digest)
}

// ---------------------------------------------------------------------------
// Archive helpers
// ---------------------------------------------------------------------------

/// Create a tar.gz archive of a directory's contents.
pub fn create_tar_gz(dir: &Path) -> Result<Vec<u8>, OciError> {
    let buf = Vec::new();
    let encoder = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);

    // Add directory contents relative to dir
    add_dir_to_tar(&mut archive, dir, dir)?;

    let encoder = archive.into_inner().map_err(|e| OciError::ArchiveError {
        message: format!("tar finalization failed: {e}"),
    })?;
    let compressed = encoder.finish().map_err(|e| OciError::ArchiveError {
        message: format!("gzip finalization failed: {e}"),
    })?;
    Ok(compressed)
}

fn add_dir_to_tar<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    dir: &Path,
    root: &Path,
) -> Result<(), OciError> {
    let entries = std::fs::read_dir(dir).map_err(|e| OciError::ArchiveError {
        message: format!("cannot read directory {}: {e}", dir.display()),
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| OciError::ArchiveError {
            message: format!("directory entry error: {e}"),
        })?;
        let file_type = entry.file_type().map_err(|e| OciError::ArchiveError {
            message: format!("file type error: {e}"),
        })?;
        // Skip symlinks — prevents including content from outside the module directory
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|e| OciError::ArchiveError {
                message: format!("path prefix error: {e}"),
            })?;

        if file_type.is_dir() {
            archive
                .append_dir(relative, &path)
                .map_err(|e| OciError::ArchiveError {
                    message: format!("tar append dir failed: {e}"),
                })?;
            add_dir_to_tar(archive, &path, root)?;
        } else {
            archive
                .append_path_with_name(&path, relative)
                .map_err(|e| OciError::ArchiveError {
                    message: format!("tar append file failed: {e}"),
                })?;
        }
    }
    Ok(())
}

/// Extract a tar.gz archive into a directory.
pub fn extract_tar_gz(data: &[u8], output_dir: &Path) -> Result<(), OciError> {
    let decoder = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);

    std::fs::create_dir_all(output_dir).map_err(|e| OciError::ArchiveError {
        message: format!("cannot create output directory: {e}"),
    })?;

    // Extract entries individually to validate against symlink attacks.
    // The tar crate rejects `..` and absolute paths by default (since 0.4.26),
    // but symlinks can still point outside the output directory.
    let canonical_output = output_dir
        .canonicalize()
        .map_err(|e| OciError::ArchiveError {
            message: format!("cannot canonicalize output directory: {e}"),
        })?;

    for entry in archive.entries().map_err(|e| OciError::ArchiveError {
        message: format!("tar iteration failed: {e}"),
    })? {
        let mut entry = entry.map_err(|e| OciError::ArchiveError {
            message: format!("tar entry read failed: {e}"),
        })?;

        // Skip symlinks — prevents symlink-based path traversal
        if entry.header().entry_type().is_symlink() || entry.header().entry_type().is_hard_link() {
            let path = entry.path().unwrap_or_default();
            tracing::warn!(path = %path.display(), "skipping symlink/hardlink in OCI archive");
            continue;
        }

        entry
            .unpack_in(&canonical_output)
            .map_err(|e| OciError::ArchiveError {
                message: format!("tar entry extraction failed: {e}"),
            })?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Push
// ---------------------------------------------------------------------------

/// Push a module directory as an OCI artifact.
///
/// Reads `module.yaml` from `dir`, serializes it as the config blob, and
/// tars+gzips the directory contents as a single layer. Pushes to the
/// registry specified by `artifact_ref`.
///
/// Returns the pushed manifest digest.
pub fn push_module(
    dir: &Path,
    artifact_ref: &str,
    platform: Option<&str>,
    printer: Option<&Printer>,
) -> Result<String, OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(300))
        .build();
    let spinner = printer.map(|p| p.spinner(&format!("Pushing module to {artifact_ref}...")));
    let (digest, _size) = push_module_inner(&agent, dir, &oci_ref, auth.as_ref(), platform)?;
    if let Some(s) = spinner {
        s.finish_and_clear();
    }
    Ok(digest)
}

/// Inner push logic shared by single-platform and multi-platform push.
/// Returns (manifest_digest, manifest_size_bytes).
fn push_module_inner(
    agent: &ureq::Agent,
    dir: &Path,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    platform: Option<&str>,
) -> Result<(String, u64), OciError> {
    // Read module.yaml
    let module_yaml_path = dir.join("module.yaml");
    if !module_yaml_path.exists() {
        return Err(OciError::ModuleYamlNotFound {
            dir: dir.to_path_buf(),
        });
    }
    let module_yaml = std::fs::read_to_string(&module_yaml_path)?;

    // Serialize config blob as JSON (module.yaml content wrapped in JSON)
    let config_blob = serde_json::to_vec(&serde_json::json!({
        "moduleYaml": module_yaml,
    }))?;

    // Create layer archive
    let layer_data = create_tar_gz(dir)?;

    // Build platform annotation
    let platform_str = platform.map(String::from).unwrap_or_else(current_platform);

    // Upload config blob
    let config_digest = upload_blob(agent, oci_ref, auth, &config_blob, MEDIA_TYPE_MODULE_CONFIG)?;

    // Upload layer blob
    let layer_digest = upload_blob(agent, oci_ref, auth, &layer_data, MEDIA_TYPE_MODULE_LAYER)?;

    // Build manifest
    let mut annotations = HashMap::new();
    annotations.insert("cfgd.io/platform".to_string(), platform_str);
    annotations.insert(
        "org.opencontainers.image.created".to_string(),
        crate::utc_now_iso8601(),
    );

    let manifest = OciManifest {
        schema_version: 2,
        media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
        config: OciDescriptor {
            media_type: MEDIA_TYPE_MODULE_CONFIG.to_string(),
            digest: config_digest,
            size: config_blob.len() as u64,
            annotations: HashMap::new(),
        },
        layers: vec![OciDescriptor {
            media_type: MEDIA_TYPE_MODULE_LAYER.to_string(),
            digest: layer_digest,
            size: layer_data.len() as u64,
            annotations: HashMap::new(),
        }],
        annotations,
    };

    let manifest_json = serde_json::to_vec(&manifest)?;

    // Push manifest
    let manifest_url = format!(
        "{}/{}/manifests/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        oci_ref.reference_str(),
    );

    authenticated_request(
        agent,
        "PUT",
        &manifest_url,
        auth,
        None,
        Some(MEDIA_TYPE_OCI_MANIFEST),
        Some(&manifest_json),
    )
    .map_err(|e| OciError::ManifestPushFailed {
        message: format!("{e}"),
    })?;

    let manifest_size = manifest_json.len() as u64;
    let manifest_digest = sha256_digest(&manifest_json);
    tracing::info!(
        reference = %oci_ref,
        digest = %manifest_digest,
        "module pushed"
    );

    Ok((manifest_digest, manifest_size))
}

// ---------------------------------------------------------------------------
// Multi-platform index
// ---------------------------------------------------------------------------

const MEDIA_TYPE_OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciIndex {
    schema_version: u32,
    media_type: String,
    manifests: Vec<OciPlatformManifest>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciPlatformManifest {
    media_type: String,
    digest: String,
    size: u64,
    platform: OciPlatform,
}

#[derive(Debug, Serialize, Deserialize)]
struct OciPlatform {
    os: String,
    architecture: String,
}

/// Map Rust arch names to OCI architecture names.
pub fn rust_arch_to_oci(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "arm",
        "s390x" => "s390x",
        "powerpc64" => "ppc64le",
        other => other,
    }
}

/// Return the current platform in OCI format (os/arch).
pub fn current_platform() -> String {
    format!(
        "{}/{}",
        std::env::consts::OS,
        rust_arch_to_oci(std::env::consts::ARCH)
    )
}

/// Parse "os/arch" (e.g. "linux/amd64") into (os, arch).
pub fn parse_platform_target(target: &str) -> Result<(&str, &str), OciError> {
    target.split_once('/').ok_or_else(|| OciError::BuildError {
        message: format!(
            "invalid platform target '{target}' — expected os/arch (e.g. linux/amd64)"
        ),
    })
}

/// Push a module for multiple platforms, creating an OCI index (manifest list).
///
/// Each `builds` entry is `(build_dir, platform)` where platform is "os/arch".
/// Pushes each platform-specific manifest, then pushes the index.
pub fn push_module_multiplatform(
    builds: &[(&Path, &str)],
    artifact_ref: &str,
    printer: Option<&Printer>,
) -> Result<String, OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(300))
        .build();

    let spinner = printer.map(|p| {
        p.spinner(&format!(
            "Pushing multi-platform module to {artifact_ref}..."
        ))
    });

    let mut platform_manifests = Vec::new();

    for (dir, platform) in builds {
        let (os, arch) = parse_platform_target(platform)?;

        // Push each platform as its own tagged manifest
        let platform_tag = format!("{}-{}", oci_ref.reference_str(), platform.replace('/', "-"));
        let platform_ref = OciReference {
            registry: oci_ref.registry.clone(),
            repository: oci_ref.repository.clone(),
            reference: ReferenceKind::Tag(platform_tag),
        };

        let (digest, size) =
            push_module_inner(&agent, dir, &platform_ref, auth.as_ref(), Some(platform))?;

        platform_manifests.push(OciPlatformManifest {
            media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
            digest,
            size,
            platform: OciPlatform {
                os: os.to_string(),
                architecture: arch.to_string(),
            },
        });
    }

    // Build and push the index
    let index = OciIndex {
        schema_version: 2,
        media_type: MEDIA_TYPE_OCI_INDEX.to_string(),
        manifests: platform_manifests,
    };
    let index_json = serde_json::to_vec(&index)?;

    let index_url = format!(
        "{}/{}/manifests/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        oci_ref.reference_str(),
    );

    authenticated_request(
        &agent,
        "PUT",
        &index_url,
        auth.as_ref(),
        None,
        Some(MEDIA_TYPE_OCI_INDEX),
        Some(&index_json),
    )
    .map_err(|e| OciError::ManifestPushFailed {
        message: format!("index push failed: {e}"),
    })?;

    let index_digest = sha256_digest(&index_json);

    if let Some(s) = spinner {
        s.finish_and_clear();
    }

    tracing::info!(
        reference = %oci_ref,
        digest = %index_digest,
        platforms = builds.len(),
        "multi-platform module pushed"
    );

    Ok(index_digest)
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

/// Detect which container runtime is available (docker or podman).
pub fn detect_container_runtime() -> Option<&'static str> {
    if crate::command_available("docker") {
        Some("docker")
    } else if crate::command_available("podman") {
        Some("podman")
    } else {
        None
    }
}

/// Detect the package manager install command based on the base image name.
fn detect_pkg_install_cmd(base_image: &str) -> &'static str {
    let lower = base_image.to_ascii_lowercase();
    if lower.starts_with("alpine") || lower.contains("/alpine") {
        "apk add --no-cache"
    } else if lower.starts_with("fedora")
        || lower.contains("/fedora")
        || lower.starts_with("rockylinux")
        || lower.contains("/rockylinux")
        || lower.starts_with("almalinux")
        || lower.contains("/almalinux")
    {
        "dnf install -y"
    } else if lower.starts_with("centos") || lower.contains("/centos") {
        "yum install -y"
    } else if lower.starts_with("archlinux") || lower.contains("/archlinux") {
        "pacman -Sy --noconfirm"
    } else {
        // Debian, Ubuntu, and default
        "apt-get update && apt-get install -y"
    }
}

/// Generate a Dockerfile for building a module in an isolated container.
fn build_dockerfile(base_image: &str, packages: &[&str]) -> String {
    let mut lines = vec![format!("FROM {base_image}")];
    if !packages.is_empty() {
        let pkg_list = packages.join(" ");
        let install_cmd = detect_pkg_install_cmd(base_image);
        if install_cmd.starts_with("apt-get") {
            lines.push(format!(
                "RUN {install_cmd} {pkg_list} && rm -rf /var/lib/apt/lists/*"
            ));
        } else {
            lines.push(format!("RUN {install_cmd} {pkg_list}"));
        }
    }
    lines.push("WORKDIR /build".to_string());
    lines.push("COPY . /build/".to_string());
    lines.join("\n")
}

/// Build a module directory into an OCI-ready artifact using a container.
///
/// Creates a Dockerfile, builds a container image, copies out the installed
/// files, and packages them as a tar.gz layer ready for `push_module()`.
///
/// Returns the path to the build output directory.
pub fn build_module(
    dir: &Path,
    target_platform: Option<&str>,
    base_image: Option<&str>,
) -> Result<std::path::PathBuf, OciError> {
    let module_yaml_path = dir.join("module.yaml");
    if !module_yaml_path.exists() {
        return Err(OciError::ModuleYamlNotFound {
            dir: dir.to_path_buf(),
        });
    }

    let runtime = detect_container_runtime().ok_or(OciError::ToolNotFound {
        tool: "docker or podman".to_string(),
    })?;

    let module_yaml = std::fs::read_to_string(&module_yaml_path)?;
    let module_doc =
        crate::config::parse_module(&module_yaml).map_err(|e| OciError::BuildError {
            message: format!("invalid module.yaml: {e}"),
        })?;

    // Extract package names from module spec
    let pkg_names: Vec<String> = module_doc
        .spec
        .packages
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let packages: Vec<&str> = pkg_names.iter().map(|s| s.as_str()).collect();

    let base = base_image.unwrap_or("ubuntu:22.04");
    let dockerfile_content = build_dockerfile(base, &packages);

    // Copy module directory into temp build context, then write Dockerfile
    // (write after copy so a user's Dockerfile doesn't overwrite the generated one)
    let build_dir = tempfile::tempdir().map_err(|e| OciError::BuildError {
        message: format!("cannot create temp dir: {e}"),
    })?;
    crate::copy_dir_recursive(dir, build_dir.path())?;
    std::fs::write(build_dir.path().join("Dockerfile"), &dockerfile_content)?;

    // Build the container image
    let tag = format!(
        "cfgd-build-{}:{}",
        module_doc.metadata.name,
        std::process::id(),
    );
    let container_name = format!(
        "cfgd-build-{}-{}",
        std::process::id(),
        crate::utc_now_iso8601().replace([':', '-', 'T', 'Z'], ""),
    );

    let mut build_cmd = std::process::Command::new(runtime);
    build_cmd.arg("build").arg("-t").arg(&tag);

    if let Some(platform) = target_platform {
        build_cmd.arg("--platform").arg(platform);
    }

    build_cmd
        .arg("-f")
        .arg(build_dir.path().join("Dockerfile"))
        .arg(build_dir.path());

    let build_output = build_cmd.output().map_err(|e| OciError::BuildError {
        message: format!("{runtime} build failed: {e}"),
    })?;

    if !build_output.status.success() {
        return Err(OciError::BuildError {
            message: format!(
                "{runtime} build failed:\n{}",
                crate::stderr_lossy_trimmed(&build_output)
            ),
        });
    }

    // Create container and copy build output
    let output_dir = tempfile::tempdir().map_err(|e| OciError::BuildError {
        message: format!("cannot create output dir: {e}"),
    })?;

    let create_output = std::process::Command::new(runtime)
        .args(["create", "--name", &container_name, &tag])
        .output()
        .map_err(|e| OciError::BuildError {
            message: format!("container create failed: {e}"),
        })?;

    if !create_output.status.success() {
        return Err(OciError::BuildError {
            message: format!(
                "container create failed: {}",
                crate::stderr_lossy_trimmed(&create_output)
            ),
        });
    }

    // Copy /build directory out of the container
    let cp_output = std::process::Command::new(runtime)
        .args([
            "cp",
            &format!("{container_name}:/build/."),
            &output_dir.path().display().to_string(),
        ])
        .output()
        .map_err(|e| OciError::BuildError {
            message: format!("container cp failed: {e}"),
        })?;

    // Cleanup container and image (best effort)
    let _ = std::process::Command::new(runtime)
        .args(["rm", "-f", &container_name])
        .output();
    let _ = std::process::Command::new(runtime)
        .args(["rmi", "-f", &tag])
        .output();

    if !cp_output.status.success() {
        return Err(OciError::BuildError {
            message: format!(
                "container cp failed: {}",
                crate::stderr_lossy_trimmed(&cp_output)
            ),
        });
    }

    let out = output_dir.path().to_path_buf();
    let _keep = output_dir.keep();
    tracing::info!(output = %out.display(), "module built");
    Ok(out)
}

// ---------------------------------------------------------------------------
// Signing (cosign)
// ---------------------------------------------------------------------------

/// Sign an OCI artifact with cosign.
///
/// If `key_path` is Some, uses `cosign sign --key <path>`.
/// If `key_path` is None, uses keyless signing (Fulcio/Rekor via OIDC).
pub fn sign_artifact(artifact_ref: &str, key_path: Option<&str>) -> Result<(), OciError> {
    if !crate::command_available("cosign") {
        return Err(OciError::ToolNotFound {
            tool: "cosign".to_string(),
        });
    }

    let mut cmd = std::process::Command::new("cosign");
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

    if !crate::command_available("cosign") {
        return Err(OciError::ToolNotFound {
            tool: "cosign".to_string(),
        });
    }

    let mut cmd = std::process::Command::new("cosign");
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
                "sha256": digest.strip_prefix("sha256:").unwrap_or(digest),
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
    if !crate::command_available("cosign") {
        return Err(OciError::ToolNotFound {
            tool: "cosign".to_string(),
        });
    }

    let mut cmd = std::process::Command::new("cosign");
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

    if !crate::command_available("cosign") {
        return Err(OciError::ToolNotFound {
            tool: "cosign".to_string(),
        });
    }

    let mut cmd = std::process::Command::new("cosign");
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

// ---------------------------------------------------------------------------
// Pull
// ---------------------------------------------------------------------------

/// Pull a module from an OCI registry and extract it to `output_dir`.
///
/// If `require_signature` is true, checks for a cosign signature tag
/// (`<tag>.sig` or `sha256-<hash>.sig`) and returns an error if not found.
pub fn pull_module(
    artifact_ref: &str,
    output_dir: &Path,
    require_signature: bool,
    printer: Option<&Printer>,
) -> Result<(), OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(300))
        .build();

    let spinner = printer.map(|p| p.spinner(&format!("Pulling module from {artifact_ref}...")));

    // If signature required, check for cosign signature tag
    if require_signature {
        check_signature_exists(&agent, &oci_ref, auth.as_ref())?;
    }

    // Pull manifest
    let manifest_url = format!(
        "{}/{}/manifests/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        oci_ref.reference_str(),
    );

    let resp = authenticated_request(
        &agent,
        "GET",
        &manifest_url,
        auth.as_ref(),
        Some(MEDIA_TYPE_OCI_MANIFEST),
        None,
        None,
    )
    .map_err(|e| OciError::ManifestNotFound {
        reference: format!("{}: {e}", oci_ref),
    })?;

    let manifest_body = resp.into_string().map_err(|e| OciError::RequestFailed {
        message: format!("cannot read manifest body: {e}"),
    })?;
    let manifest: OciManifest =
        serde_json::from_str(&manifest_body).map_err(|e| OciError::RequestFailed {
            message: format!("invalid manifest JSON: {e}"),
        })?;

    // Find our layer
    let layer = manifest
        .layers
        .first()
        .ok_or_else(|| OciError::RequestFailed {
            message: "manifest has no layers".to_string(),
        })?;

    // Download layer blob
    let blob_url = format!(
        "{}/{}/blobs/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        layer.digest,
    );

    let resp = authenticated_request(
        &agent,
        "GET",
        &blob_url,
        auth.as_ref(),
        Some("application/octet-stream"),
        None,
        None,
    )
    .map_err(|e| OciError::BlobNotFound {
        digest: format!("{}: {e}", layer.digest),
    })?;

    // Read blob data (cap at 512 MB to prevent OOM from malicious manifests)
    const MAX_BLOB_SIZE: u64 = 512 * 1024 * 1024;
    if layer.size > MAX_BLOB_SIZE {
        return Err(OciError::RequestFailed {
            message: format!(
                "layer size {} exceeds maximum allowed size ({} bytes)",
                layer.size, MAX_BLOB_SIZE
            ),
        });
    }
    let mut blob_data = Vec::with_capacity(layer.size as usize);
    resp.into_reader()
        .take(MAX_BLOB_SIZE + 1024)
        .read_to_end(&mut blob_data)?;

    // Verify digest
    let actual_digest = sha256_digest(&blob_data);
    if actual_digest != layer.digest {
        return Err(OciError::RequestFailed {
            message: format!(
                "layer digest mismatch: expected {}, got {}",
                layer.digest, actual_digest
            ),
        });
    }

    // Extract
    extract_tar_gz(&blob_data, output_dir)?;

    if let Some(s) = spinner {
        s.finish_and_clear();
    }

    tracing::info!(
        reference = %oci_ref,
        output = %output_dir.display(),
        "module pulled"
    );

    Ok(())
}

/// Check if a cosign-style signature exists for the given reference.
/// Cosign stores signatures at tag `<tag>.sig` or `sha256-<hash>.sig`.
fn check_signature_exists(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
) -> Result<(), OciError> {
    let sig_tag = match &oci_ref.reference {
        ReferenceKind::Tag(tag) => format!("{tag}.sig"),
        ReferenceKind::Digest(digest) => {
            // sha256:abc... → sha256-abc....sig
            digest.replace(':', "-") + ".sig"
        }
    };

    let sig_url = format!(
        "{}/{}/manifests/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        sig_tag,
    );

    let result = authenticated_request(agent, "HEAD", &sig_url, auth, None, None, None);

    match result {
        Ok(_) => Ok(()),
        Err(_) => Err(OciError::SignatureRequired {
            reference: oci_ref.to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Base64 helpers (no external dependency needed for this)
// ---------------------------------------------------------------------------

fn base64_encode(data: &[u8]) -> String {
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

fn base64_decode(input: &str) -> Option<Vec<u8>> {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- OCI Reference parsing ---

    #[test]
    fn parse_full_reference_with_tag() {
        let r = OciReference::parse("ghcr.io/myorg/mymodule:v1.0.0").unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "myorg/mymodule");
        assert_eq!(r.reference, ReferenceKind::Tag("v1.0.0".to_string()));
    }

    #[test]
    fn parse_reference_with_digest() {
        let r = OciReference::parse(
            "ghcr.io/myorg/mymodule@sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
        )
        .unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "myorg/mymodule");
        assert_eq!(
            r.reference,
            ReferenceKind::Digest(
                "sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
                    .to_string()
            )
        );
    }

    #[test]
    fn parse_reference_default_tag() {
        let r = OciReference::parse("ghcr.io/myorg/mymodule").unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "myorg/mymodule");
        assert_eq!(r.reference, ReferenceKind::Tag("latest".to_string()));
    }

    #[test]
    fn parse_reference_default_registry() {
        let r = OciReference::parse("myorg/mymodule:v2").unwrap();
        assert_eq!(r.registry, "docker.io");
        assert_eq!(r.repository, "myorg/mymodule");
        assert_eq!(r.reference, ReferenceKind::Tag("v2".to_string()));
    }

    #[test]
    fn parse_reference_docker_library() {
        let r = OciReference::parse("ubuntu").unwrap();
        assert_eq!(r.registry, "docker.io");
        assert_eq!(r.repository, "library/ubuntu");
        assert_eq!(r.reference, ReferenceKind::Tag("latest".to_string()));
    }

    #[test]
    fn parse_reference_localhost() {
        let r = OciReference::parse("localhost:5000/mymodule:dev").unwrap();
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.repository, "mymodule");
        assert_eq!(r.reference, ReferenceKind::Tag("dev".to_string()));
    }

    #[test]
    fn parse_reference_nested_repo() {
        let r = OciReference::parse("registry.example.com/a/b/c:latest").unwrap();
        assert_eq!(r.registry, "registry.example.com");
        assert_eq!(r.repository, "a/b/c");
        assert_eq!(r.reference, ReferenceKind::Tag("latest".to_string()));
    }

    #[test]
    fn parse_empty_reference_fails() {
        assert!(OciReference::parse("").is_err());
    }

    #[test]
    fn reference_display() {
        let r = OciReference {
            registry: "ghcr.io".to_string(),
            repository: "myorg/mymod".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };
        assert_eq!(r.to_string(), "ghcr.io/myorg/mymod:v1");

        let r2 = OciReference {
            registry: "ghcr.io".to_string(),
            repository: "myorg/mymod".to_string(),
            reference: ReferenceKind::Digest("sha256:abc".to_string()),
        };
        assert_eq!(r2.to_string(), "ghcr.io/myorg/mymod@sha256:abc");
    }

    #[test]
    fn api_base_https() {
        let r = OciReference::parse("ghcr.io/test/repo:v1").unwrap();
        assert_eq!(r.api_base(), "https://ghcr.io/v2");
    }

    #[test]
    fn api_base_localhost_http() {
        let r = OciReference::parse("localhost:5000/test:v1").unwrap();
        assert!(r.api_base().starts_with("http://"));
    }

    // --- Config blob round-trip ---

    #[test]
    fn config_blob_round_trip() {
        let module_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test\nspec:\n  packages:\n    - name: curl\n";
        let config_blob = serde_json::to_vec(&serde_json::json!({
            "moduleYaml": module_yaml,
        }))
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&config_blob).unwrap();
        assert_eq!(parsed["moduleYaml"].as_str().unwrap(), module_yaml);
    }

    // --- tar+gzip round-trip ---

    #[test]
    fn tar_gz_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();

        // Create test files
        std::fs::write(dir_path.join("module.yaml"), "name: test\n").unwrap();
        std::fs::create_dir(dir_path.join("files")).unwrap();
        std::fs::write(
            dir_path.join("files/config.toml"),
            "[section]\nkey = \"value\"\n",
        )
        .unwrap();

        // Create archive
        let archive = create_tar_gz(dir_path).unwrap();
        assert!(!archive.is_empty());

        // Extract to new directory
        let out_dir = tempfile::tempdir().unwrap();
        extract_tar_gz(&archive, out_dir.path()).unwrap();

        // Verify contents
        let module_yaml = std::fs::read_to_string(out_dir.path().join("module.yaml")).unwrap();
        assert_eq!(module_yaml, "name: test\n");

        let config = std::fs::read_to_string(out_dir.path().join("files/config.toml")).unwrap();
        assert_eq!(config, "[section]\nkey = \"value\"\n");
    }

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

    // --- SHA256 ---

    #[test]
    fn sha256_digest_known() {
        let digest = sha256_digest(b"hello");
        assert!(digest.starts_with("sha256:"));
        assert_eq!(digest.len(), 7 + 64); // "sha256:" + 64 hex chars
    }

    // --- OCI manifest serialization ---

    #[test]
    fn oci_manifest_serialization() {
        let manifest = OciManifest {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
            config: OciDescriptor {
                media_type: MEDIA_TYPE_MODULE_CONFIG.to_string(),
                digest: "sha256:abc123".to_string(),
                size: 100,
                annotations: HashMap::new(),
            },
            layers: vec![OciDescriptor {
                media_type: MEDIA_TYPE_MODULE_LAYER.to_string(),
                digest: "sha256:def456".to_string(),
                size: 2048,
                annotations: HashMap::new(),
            }],
            annotations: HashMap::new(),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("schemaVersion"));
        assert!(json.contains(MEDIA_TYPE_MODULE_CONFIG));
        assert!(json.contains(MEDIA_TYPE_MODULE_LAYER));

        // Round-trip
        let parsed: OciManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.schema_version, 2);
        assert_eq!(parsed.config.digest, "sha256:abc123");
        assert_eq!(parsed.layers.len(), 1);
        assert_eq!(parsed.layers[0].digest, "sha256:def456");
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

    // --- Build ---

    #[test]
    fn build_module_rejects_missing_module_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let result = build_module(dir.path(), None, None);
        assert!(matches!(result, Err(OciError::ModuleYamlNotFound { .. })));
    }

    #[test]
    fn detect_container_runtime_returns_option() {
        let rt = detect_container_runtime();
        if let Some(name) = rt {
            assert!(name == "docker" || name == "podman");
        }
    }

    #[test]
    fn generate_build_dockerfile_content() {
        let dockerfile = build_dockerfile("ubuntu:22.04", &["curl", "wget"]);
        assert!(dockerfile.contains("FROM ubuntu:22.04"));
        assert!(dockerfile.contains("curl"));
        assert!(dockerfile.contains("wget"));
        assert!(dockerfile.contains("WORKDIR /build"));
    }

    #[test]
    fn generate_build_dockerfile_no_packages() {
        let dockerfile = build_dockerfile("alpine:3.18", &[]);
        assert!(dockerfile.contains("FROM alpine:3.18"));
        assert!(!dockerfile.contains("apt-get"));
    }

    // --- Multi-platform ---

    #[test]
    fn oci_index_manifest_serialization() {
        let index = OciIndex {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_INDEX.to_string(),
            manifests: vec![
                OciPlatformManifest {
                    media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
                    digest: "sha256:abc".to_string(),
                    size: 1024,
                    platform: OciPlatform {
                        os: "linux".to_string(),
                        architecture: "amd64".to_string(),
                    },
                },
                OciPlatformManifest {
                    media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
                    digest: "sha256:def".to_string(),
                    size: 2048,
                    platform: OciPlatform {
                        os: "linux".to_string(),
                        architecture: "arm64".to_string(),
                    },
                },
            ],
        };
        let json = serde_json::to_string(&index).unwrap();
        assert!(json.contains("schemaVersion"));
        assert!(json.contains("amd64"));
        assert!(json.contains("arm64"));

        let parsed: OciIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.manifests.len(), 2);
    }

    #[test]
    fn parse_platform_target_valid() {
        let (os, arch) = parse_platform_target("linux/amd64").unwrap();
        assert_eq!(os, "linux");
        assert_eq!(arch, "amd64");
    }

    #[test]
    fn parse_platform_target_invalid() {
        assert!(parse_platform_target("invalid").is_err());
    }

    // --- Dockerfile generation ---

    #[test]
    fn build_dockerfile_debian_default() {
        let df = build_dockerfile("ubuntu:22.04", &["curl", "jq"]);
        assert!(df.contains("FROM ubuntu:22.04"));
        assert!(df.contains("apt-get"));
        assert!(df.contains("curl jq"));
        assert!(df.contains("rm -rf /var/lib/apt/lists"));
    }

    #[test]
    fn build_dockerfile_alpine() {
        let df = build_dockerfile("alpine:3.18", &["curl"]);
        assert!(df.contains("apk add --no-cache"));
        assert!(!df.contains("apt-get"));
    }

    #[test]
    fn build_dockerfile_fedora() {
        let df = build_dockerfile("fedora:39", &["strace"]);
        assert!(df.contains("dnf install -y"));
    }

    #[test]
    fn build_dockerfile_no_packages() {
        let df = build_dockerfile("ubuntu:22.04", &[]);
        assert!(!df.contains("RUN"));
        assert!(df.contains("WORKDIR /build"));
    }

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

    #[test]
    fn insecure_registry_env_var() {
        // Save and restore env var to avoid affecting other tests
        let prev = std::env::var("OCI_INSECURE_REGISTRIES").ok();

        // SAFETY: test is single-threaded for this env var; save/restore brackets usage
        unsafe {
            std::env::set_var(
                "OCI_INSECURE_REGISTRIES",
                "myregistry:5000,other.local:8080",
            );
        }
        assert!(is_insecure_registry("myregistry:5000"));
        assert!(is_insecure_registry("other.local:8080"));
        assert!(!is_insecure_registry("ghcr.io"));
        assert!(!is_insecure_registry("myregistry:5001"));

        // Verify api_base uses HTTP for insecure registries
        let r = OciReference::parse("myregistry:5000/test/mod:v1").unwrap();
        assert!(r.api_base().starts_with("http://"));

        let r2 = OciReference::parse("ghcr.io/test/mod:v1").unwrap();
        assert!(r2.api_base().starts_with("https://"));

        // Restore
        unsafe {
            match prev {
                Some(v) => std::env::set_var("OCI_INSECURE_REGISTRIES", v),
                None => std::env::remove_var("OCI_INSECURE_REGISTRIES"),
            }
        }
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

    // --- rust_arch_to_oci ---

    #[test]
    fn rust_arch_to_oci_known() {
        assert_eq!(rust_arch_to_oci("x86_64"), "amd64");
        assert_eq!(rust_arch_to_oci("aarch64"), "arm64");
        assert_eq!(rust_arch_to_oci("arm"), "arm");
        assert_eq!(rust_arch_to_oci("s390x"), "s390x");
        assert_eq!(rust_arch_to_oci("powerpc64"), "ppc64le");
    }

    #[test]
    fn rust_arch_to_oci_unknown_passes_through() {
        assert_eq!(rust_arch_to_oci("mips64"), "mips64");
        assert_eq!(rust_arch_to_oci("riscv64"), "riscv64");
    }

    // --- detect_pkg_install_cmd ---

    #[test]
    fn detect_pkg_install_cmd_centos() {
        assert_eq!(detect_pkg_install_cmd("centos:8"), "yum install -y");
    }

    #[test]
    fn detect_pkg_install_cmd_archlinux() {
        assert_eq!(
            detect_pkg_install_cmd("archlinux:latest"),
            "pacman -Sy --noconfirm"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_unknown_defaults_to_apt() {
        let cmd = detect_pkg_install_cmd("someunknownimage:latest");
        assert!(
            cmd.contains("apt-get"),
            "unknown image should default to apt-get, got: {cmd}"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_rockylinux() {
        assert_eq!(detect_pkg_install_cmd("rockylinux:9"), "dnf install -y");
    }

    #[test]
    fn detect_pkg_install_cmd_almalinux() {
        assert_eq!(detect_pkg_install_cmd("almalinux:8"), "dnf install -y");
    }

    #[test]
    fn detect_pkg_install_cmd_fedora() {
        assert_eq!(detect_pkg_install_cmd("fedora:40"), "dnf install -y");
    }

    #[test]
    fn detect_pkg_install_cmd_alpine_with_registry() {
        assert_eq!(
            detect_pkg_install_cmd("docker.io/library/alpine:3.19"),
            "apk add --no-cache"
        );
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

    // --- current_platform ---

    #[test]
    fn current_platform_returns_valid_format() {
        let platform = current_platform();
        assert!(
            platform.contains('/'),
            "platform should be os/arch format: {platform}"
        );
        let parts: Vec<&str> = platform.split('/').collect();
        assert_eq!(parts.len(), 2);
        assert!(!parts[0].is_empty());
        assert!(!parts[1].is_empty());
    }

    // --- parse_platform_target edge cases ---

    #[test]
    fn parse_platform_target_three_parts_gives_arch_with_slash() {
        // split_once('/') on "linux/amd64/extra" gives ("linux", "amd64/extra")
        let result = parse_platform_target("linux/amd64/extra");
        assert!(result.is_ok());
        let (os, arch) = result.unwrap();
        assert_eq!(os, "linux");
        assert_eq!(arch, "amd64/extra");
    }

    #[test]
    fn parse_platform_target_no_slash_fails() {
        let result = parse_platform_target("linuxamd64");
        assert!(
            matches!(result, Err(OciError::BuildError { .. })),
            "expected BuildError, got: {result:?}"
        );
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("invalid platform target"),
            "expected 'invalid platform target' message, got: {err_msg}"
        );
    }

    // --- sha256_digest ---

    #[test]
    fn sha256_digest_known_empty() {
        let digest = sha256_digest(b"");
        assert!(digest.starts_with("sha256:"));
        assert_eq!(digest.len(), 7 + 64); // "sha256:" + 64 hex chars
        assert_eq!(
            digest,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_digest_known_hello() {
        let digest = sha256_digest(b"hello");
        // SHA256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            digest,
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
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

    // --- mockito-based HTTP integration tests ---

    /// Helper: extract host:port from a mockito server URL for use as OCI registry.
    fn registry_from_url(url: &str) -> String {
        url.trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_string()
    }

    /// Helper: create a module directory with a valid module.yaml for push tests.
    fn create_test_module_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("README.md"), "# Test module\n").unwrap();
        dir
    }

    #[test]
    fn upload_blob_posts_then_puts() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry: registry.clone(),
            repository: "test/mod".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        let data = b"hello blob data";
        let expected_digest = sha256_digest(data);

        // HEAD check returns 404 (blob doesn't exist)
        let head_mock = server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .create();

        // POST to initiate upload -> 202 with Location
        let upload_location = format!("{}/v2/test/mod/blobs/uploads/some-uuid", server.url());
        let post_mock = server
            .mock("POST", "/v2/test/mod/blobs/uploads/")
            .with_status(202)
            .with_header("Location", &upload_location)
            .create();

        // PUT to complete upload
        let put_mock = server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/test/mod/blobs/uploads/some-uuid\?digest=sha256:.*".to_string(),
                ),
            )
            .with_status(201)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = upload_blob(&agent, &oci_ref, None, data, "application/octet-stream");
        assert!(result.is_ok(), "upload_blob failed: {:?}", result.err());
        assert_eq!(result.unwrap(), expected_digest);

        head_mock.assert();
        post_mock.assert();
        put_mock.assert();
    }

    #[test]
    fn upload_blob_skips_when_already_exists() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/mod".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        let data = b"existing blob";
        let expected_digest = sha256_digest(data);

        // HEAD check returns 200 (blob exists)
        let head_mock = server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
            )
            .with_status(200)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = upload_blob(&agent, &oci_ref, None, data, "application/octet-stream");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), expected_digest);

        head_mock.assert();
        // POST and PUT should NOT have been called
    }

    #[test]
    fn upload_blob_handles_relative_location() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/mod".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        let data = b"blob with relative location";

        // HEAD returns 404
        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .create();

        // POST returns relative Location
        server
            .mock("POST", "/v2/test/mod/blobs/uploads/")
            .with_status(202)
            .with_header("Location", "/v2/test/mod/blobs/uploads/rel-uuid")
            .create();

        // PUT with relative path (should be made absolute)
        let put_mock = server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/test/mod/blobs/uploads/rel-uuid\?digest=sha256:.*".to_string(),
                ),
            )
            .with_status(201)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = upload_blob(&agent, &oci_ref, None, data, "application/octet-stream");
        assert!(
            result.is_ok(),
            "upload_blob with relative location failed: {:?}",
            result.err()
        );
        put_mock.assert();
    }

    #[test]
    fn push_module_inner_uploads_blobs_and_manifest() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/pushmod".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        let module_dir = create_test_module_dir();

        // Mock blob HEAD (not found) for config + layer
        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/test/pushmod/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .expect_at_least(2)
            .create();

        // Mock blob upload POST (config + layer)
        let upload_location = format!("{}/v2/test/pushmod/blobs/uploads/upload-id", server.url());
        server
            .mock("POST", "/v2/test/pushmod/blobs/uploads/")
            .with_status(202)
            .with_header("Location", &upload_location)
            .expect_at_least(2)
            .create();

        // Mock blob upload PUT (config + layer)
        server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/test/pushmod/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
                ),
            )
            .with_status(201)
            .expect_at_least(2)
            .create();

        // Mock manifest PUT
        let manifest_mock = server
            .mock("PUT", "/v2/test/pushmod/manifests/v1")
            .with_status(201)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = push_module_inner(
            &agent,
            module_dir.path(),
            &oci_ref,
            None,
            Some("linux/amd64"),
        );
        assert!(
            result.is_ok(),
            "push_module_inner failed: {:?}",
            result.err()
        );

        let (digest, size) = result.unwrap();
        assert!(digest.starts_with("sha256:"));
        assert!(size > 0);
        manifest_mock.assert();
    }

    #[test]
    fn push_module_inner_rejects_missing_module_yaml() {
        let dir = tempfile::tempdir().unwrap();
        // No module.yaml

        let oci_ref = OciReference {
            registry: "localhost:9999".to_string(),
            repository: "test/mod".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        let agent = ureq::AgentBuilder::new().build();
        let result = push_module_inner(&agent, dir.path(), &oci_ref, None, None);
        assert!(matches!(result, Err(OciError::ModuleYamlNotFound { .. })));
    }

    #[test]
    fn pull_module_downloads_and_verifies_digest() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        // Create a layer tarball from a temp module dir
        let src_dir = create_test_module_dir();
        let layer_data = create_tar_gz(src_dir.path()).unwrap();
        let layer_digest = sha256_digest(&layer_data);

        // Build a manifest referencing this layer
        let config_blob = serde_json::to_vec(&serde_json::json!({
            "moduleYaml": "name: test",
        }))
        .unwrap();
        let config_digest = sha256_digest(&config_blob);

        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_TYPE_OCI_MANIFEST,
            "config": {
                "mediaType": MEDIA_TYPE_MODULE_CONFIG,
                "digest": config_digest,
                "size": config_blob.len(),
            },
            "layers": [{
                "mediaType": MEDIA_TYPE_MODULE_LAYER,
                "digest": layer_digest,
                "size": layer_data.len(),
            }],
        });

        // Mock manifest GET
        server
            .mock("GET", "/v2/test/pullmod/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_body(serde_json::to_string(&manifest).unwrap())
            .create();

        // Mock layer blob GET
        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/v2/test/pullmod/blobs/sha256:.*".to_string()),
            )
            .with_status(200)
            .with_body(layer_data)
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/pullmod:v1", registry);
        let result = pull_module(&artifact_ref, output_dir.path(), false, None);
        assert!(result.is_ok(), "pull_module failed: {:?}", result.err());

        // Verify extracted files
        assert!(output_dir.path().join("module.yaml").exists());
        assert!(output_dir.path().join("README.md").exists());
    }

    #[test]
    fn pull_module_detects_digest_mismatch() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let real_layer_data = b"real layer content";
        // Use a fake digest that does NOT match the real data
        let fake_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_TYPE_OCI_MANIFEST,
            "config": {
                "mediaType": MEDIA_TYPE_MODULE_CONFIG,
                "digest": "sha256:cfgcfg",
                "size": 10,
            },
            "layers": [{
                "mediaType": MEDIA_TYPE_MODULE_LAYER,
                "digest": fake_digest,
                "size": real_layer_data.len(),
            }],
        });

        server
            .mock("GET", "/v2/test/badmod/manifests/v1")
            .with_status(200)
            .with_body(serde_json::to_string(&manifest).unwrap())
            .create();

        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/v2/test/badmod/blobs/sha256:.*".to_string()),
            )
            .with_status(200)
            .with_body(real_layer_data.as_slice())
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/badmod:v1", registry);
        let result = pull_module(&artifact_ref, output_dir.path(), false, None);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("digest mismatch"),
            "expected digest mismatch error, got: {err_msg}"
        );
    }

    #[test]
    fn pull_module_checks_signature_when_required() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        // No signature tag exists
        server
            .mock("HEAD", "/v2/test/sigmod/manifests/v1.sig")
            .with_status(404)
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/sigmod:v1", registry);
        let result = pull_module(&artifact_ref, output_dir.path(), true, None);
        assert!(result.is_err());
        assert!(
            matches!(result, Err(OciError::SignatureRequired { .. })),
            "expected SignatureRequired, got: {:?}",
            result.err()
        );
    }

    #[test]
    fn authenticated_request_handles_401_token_exchange() {
        let mut server = mockito::Server::new();

        let token_url = format!("{}/token", server.url());
        let www_auth = format!(
            r#"Bearer realm="{token_url}",service="test.io",scope="repository:test/repo:pull""#,
        );

        // First request returns 401 with Www-Authenticate
        server
            .mock("GET", "/v2/test/repo/manifests/v1")
            .with_status(401)
            .with_header("Www-Authenticate", &www_auth)
            .expect(1)
            .create();

        // Token endpoint returns a bearer token
        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/token\?service=.*&scope=.*".to_string()),
            )
            .with_status(200)
            .with_body(r#"{"token":"my-bearer-token"}"#)
            .create();

        // Retry with bearer token succeeds
        server
            .mock("GET", "/v2/test/repo/manifests/v1")
            .match_header("Authorization", "Bearer my-bearer-token")
            .with_status(200)
            .with_body("manifest content")
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let url = format!("{}/v2/test/repo/manifests/v1", server.url());
        let result = authenticated_request(&agent, "GET", &url, None, None, None, None);
        assert!(
            result.is_ok(),
            "authenticated_request failed: {:?}",
            result.err()
        );
    }

    #[test]
    fn authenticated_request_fails_on_401_without_bearer() {
        let mut server = mockito::Server::new();

        // 401 without Bearer challenge
        server
            .mock("GET", "/v2/test/repo/manifests/v1")
            .with_status(401)
            .with_header("Www-Authenticate", "Basic realm=\"test\"")
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let url = format!("{}/v2/test/repo/manifests/v1", server.url());
        let result = authenticated_request(&agent, "GET", &url, None, None, None, None);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("no Bearer challenge"),
            "expected no Bearer challenge error, got: {err_msg}"
        );
    }

    #[test]
    fn authenticated_request_passes_basic_auth() {
        let mut server = mockito::Server::new();

        let auth = RegistryAuth {
            username: "user".to_string(),
            password: "pass".to_string(),
        };

        server
            .mock("GET", "/v2/test/repo/tags/list")
            .match_header(
                "Authorization",
                mockito::Matcher::Regex("Basic .*".to_string()),
            )
            .with_status(200)
            .with_body("{}")
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let url = format!("{}/v2/test/repo/tags/list", server.url());
        let result = authenticated_request(&agent, "GET", &url, Some(&auth), None, None, None);
        let resp = result.expect("authenticated request should succeed with basic auth");
        assert_eq!(resp.status(), 200);
    }

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

    #[test]
    fn check_signature_exists_ok_when_present() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/sigexist".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        server
            .mock("HEAD", "/v2/test/sigexist/manifests/v1.sig")
            .with_status(200)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = check_signature_exists(&agent, &oci_ref, None);
        assert_eq!(result.unwrap(), (), "signature check should return Ok(())");
    }

    #[test]
    fn check_signature_exists_fails_when_missing() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/nosig".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        server
            .mock("HEAD", "/v2/test/nosig/manifests/v1.sig")
            .with_status(404)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = check_signature_exists(&agent, &oci_ref, None);
        assert!(matches!(result, Err(OciError::SignatureRequired { .. })));
    }

    #[test]
    fn check_signature_exists_digest_reference() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/digsig".to_string(),
            reference: ReferenceKind::Digest("sha256:abc123".to_string()),
        };

        // For digest refs, sig tag is "sha256-abc123.sig"
        server
            .mock("HEAD", "/v2/test/digsig/manifests/sha256-abc123.sig")
            .with_status(200)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = check_signature_exists(&agent, &oci_ref, None);
        assert_eq!(
            result.unwrap(),
            (),
            "digest-referenced signature check should return Ok(())"
        );
    }

    #[test]
    fn authenticated_request_sends_body_with_put() {
        let mut server = mockito::Server::new();

        let body_content = b"test manifest content";
        server
            .mock("PUT", "/v2/test/repo/manifests/v1")
            .match_header("Content-Type", "application/vnd.oci.image.manifest.v1+json")
            .match_body(mockito::Matcher::Any)
            .with_status(201)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let url = format!("{}/v2/test/repo/manifests/v1", server.url());
        let result = authenticated_request(
            &agent,
            "PUT",
            &url,
            None,
            None,
            Some("application/vnd.oci.image.manifest.v1+json"),
            Some(body_content),
        );
        let resp = result.expect("PUT with body should succeed");
        assert_eq!(resp.status(), 201);
    }

    #[test]
    fn authenticated_request_returns_error_on_500() {
        let mut server = mockito::Server::new();

        server
            .mock("GET", "/v2/test/repo/tags/list")
            .with_status(500)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let url = format!("{}/v2/test/repo/tags/list", server.url());
        let result = authenticated_request(&agent, "GET", &url, None, None, None, None);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("HTTP 500"),
            "expected HTTP 500 error, got: {err_msg}"
        );
    }

    // --- tar_gz symlink handling ---

    #[test]
    fn tar_gz_round_trip_via_create_and_extract() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("module");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("module.yaml"), "name: test").unwrap();

        let archive = create_tar_gz(&subdir).unwrap();
        assert!(!archive.is_empty());

        let output = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&archive, output.path());
        assert!(result.is_ok());
        assert!(output.path().join("module.yaml").exists());
        let content = std::fs::read_to_string(output.path().join("module.yaml")).unwrap();
        assert_eq!(content, "name: test");
    }

    // --- extract_tar_gz path traversal protection ---

    #[test]
    fn extract_tar_gz_prevents_path_traversal_via_dotdot() {
        // Build a tar.gz archive with an entry whose path contains ".."
        // We must write the raw tar bytes to bypass the tar crate's own
        // set_path safety checks (which also reject "..")
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);

            let data = b"malicious content";
            let mut header = tar::Header::new_gnu();
            // Set a benign path first, then overwrite the raw bytes
            header.set_path("placeholder.txt").unwrap();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);

            // Overwrite the path field in the raw header with "../escaped.txt"
            let path_bytes = b"../escaped.txt";
            header.as_mut_bytes()[..path_bytes.len()].copy_from_slice(path_bytes);
            header.as_mut_bytes()[path_bytes.len()] = 0; // null-terminate
            header.set_cksum();

            archive.append(&header, &data[..]).unwrap();

            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        // extract_tar_gz uses unpack_in which silently skips entries with ".."
        // (returns Ok(false) for unsafe paths). The key safety guarantee is
        // that the file is NOT created outside the output directory.
        let _ = extract_tar_gz(&buf, output.path());

        // Verify the malicious file was NOT created outside the output directory
        let parent = output.path().parent().unwrap();
        assert!(
            !parent.join("escaped.txt").exists(),
            "path traversal file should not have been created outside output dir"
        );

        // Also verify nothing was created inside the output directory with this name
        assert!(
            !output.path().join("escaped.txt").exists(),
            "traversal entry should not have been extracted"
        );
    }

    #[test]
    fn extract_tar_gz_skips_symlinks() {
        // Build a tar.gz archive containing a symlink entry
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);

            // Add a regular file first
            let data = b"normal file content";
            let mut header = tar::Header::new_gnu();
            header.set_path("normal.txt").unwrap();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append(&header, &data[..]).unwrap();

            // Add a symlink pointing outside the output directory
            let mut sym_header = tar::Header::new_gnu();
            sym_header.set_path("evil_link").unwrap();
            sym_header.set_entry_type(tar::EntryType::Symlink);
            sym_header.set_link_name("/etc/passwd").unwrap();
            sym_header.set_size(0);
            sym_header.set_mode(0o777);
            sym_header.set_cksum();
            archive.append(&sym_header, &[][..]).unwrap();

            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&buf, output.path());
        assert!(
            result.is_ok(),
            "extraction should succeed, skipping symlinks"
        );

        // The regular file should exist
        let normal_content = std::fs::read_to_string(output.path().join("normal.txt")).unwrap();
        assert_eq!(normal_content, "normal file content");

        // The symlink should NOT have been created
        assert!(
            !output.path().join("evil_link").exists(),
            "symlink entry should have been skipped during extraction"
        );
    }

    #[test]
    fn extract_tar_gz_skips_hardlinks() {
        // Build a tar.gz archive containing a hardlink entry
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);

            // Add a regular file
            let data = b"target content";
            let mut header = tar::Header::new_gnu();
            header.set_path("target.txt").unwrap();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append(&header, &data[..]).unwrap();

            // Add a hardlink
            let mut link_header = tar::Header::new_gnu();
            link_header.set_path("hardlink.txt").unwrap();
            link_header.set_entry_type(tar::EntryType::Link);
            link_header.set_link_name("target.txt").unwrap();
            link_header.set_size(0);
            link_header.set_mode(0o644);
            link_header.set_cksum();
            archive.append(&link_header, &[][..]).unwrap();

            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&buf, output.path());
        assert!(
            result.is_ok(),
            "extraction should succeed, skipping hardlinks"
        );

        // Regular file should exist
        assert!(output.path().join("target.txt").exists());
        let content = std::fs::read_to_string(output.path().join("target.txt")).unwrap();
        assert_eq!(content, "target content");

        // Hardlink should NOT have been created (it gets skipped)
        assert!(
            !output.path().join("hardlink.txt").exists(),
            "hardlink entry should have been skipped during extraction"
        );
    }

    #[test]
    fn extract_tar_gz_extracts_multiple_files_preserving_directories() {
        // Build a tar.gz archive with nested structure
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);

            // Add a directory entry
            let mut dir_header = tar::Header::new_gnu();
            dir_header.set_path("subdir/").unwrap();
            dir_header.set_entry_type(tar::EntryType::Directory);
            dir_header.set_size(0);
            dir_header.set_mode(0o755);
            dir_header.set_cksum();
            archive.append(&dir_header, &[][..]).unwrap();

            // Add files in subdirectory
            let data1 = b"file one";
            let mut h1 = tar::Header::new_gnu();
            h1.set_path("subdir/one.txt").unwrap();
            h1.set_size(data1.len() as u64);
            h1.set_mode(0o644);
            h1.set_cksum();
            archive.append(&h1, &data1[..]).unwrap();

            let data2 = b"file two";
            let mut h2 = tar::Header::new_gnu();
            h2.set_path("subdir/two.txt").unwrap();
            h2.set_size(data2.len() as u64);
            h2.set_mode(0o644);
            h2.set_cksum();
            archive.append(&h2, &data2[..]).unwrap();

            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        extract_tar_gz(&buf, output.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(output.path().join("subdir/one.txt")).unwrap(),
            "file one"
        );
        assert_eq!(
            std::fs::read_to_string(output.path().join("subdir/two.txt")).unwrap(),
            "file two"
        );
    }

    #[test]
    fn extract_tar_gz_empty_archive() {
        // Build a tar.gz with no entries
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let archive = tar::Builder::new(encoder);
            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&buf, output.path());
        assert!(result.is_ok(), "empty archive should extract successfully");
        // Output directory should exist but be empty
        assert!(output.path().exists());
        let entries: Vec<_> = std::fs::read_dir(output.path()).unwrap().collect();
        assert_eq!(entries.len(), 0, "output directory should be empty");
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

    // --- build_dockerfile additional coverage ---

    #[test]
    fn build_dockerfile_centos() {
        let df = build_dockerfile("centos:8", &["vim", "git"]);
        assert!(df.contains("FROM centos:8"));
        assert!(df.contains("yum install -y"));
        assert!(df.contains("vim git"));
        assert!(!df.contains("apt-get"));
        assert!(!df.contains("rm -rf /var/lib/apt/lists"));
    }

    #[test]
    fn build_dockerfile_archlinux() {
        let df = build_dockerfile("archlinux:latest", &["base-devel"]);
        assert!(df.contains("FROM archlinux:latest"));
        assert!(df.contains("pacman -Sy --noconfirm"));
        assert!(df.contains("base-devel"));
    }

    #[test]
    fn build_dockerfile_rockylinux() {
        let df = build_dockerfile("rockylinux:9", &["httpd"]);
        assert!(df.contains("FROM rockylinux:9"));
        assert!(df.contains("dnf install -y"));
        assert!(df.contains("httpd"));
    }

    #[test]
    fn build_dockerfile_almalinux() {
        let df = build_dockerfile("almalinux:8", &["nginx"]);
        assert!(df.contains("FROM almalinux:8"));
        assert!(df.contains("dnf install -y"));
    }

    #[test]
    fn build_dockerfile_debian_cleans_apt_lists() {
        let df = build_dockerfile("debian:bookworm", &["curl"]);
        assert!(df.contains("FROM debian:bookworm"));
        assert!(df.contains("apt-get update && apt-get install -y"));
        assert!(
            df.contains("rm -rf /var/lib/apt/lists"),
            "debian-based images should clean apt lists"
        );
    }

    #[test]
    fn build_dockerfile_always_includes_workdir_and_copy() {
        // Even with no packages, WORKDIR and COPY lines should be present
        let df = build_dockerfile("scratch", &[]);
        assert!(df.contains("WORKDIR /build"));
        assert!(df.contains("COPY . /build/"));
        assert!(!df.contains("RUN"));
    }

    #[test]
    fn build_dockerfile_registry_prefixed_alpine() {
        // Images with registry prefix like "docker.io/library/alpine" should still be detected
        let df = build_dockerfile("docker.io/library/alpine:3.19", &["curl"]);
        assert!(df.contains("apk add --no-cache"));
        assert!(!df.contains("apt-get"));
    }

    #[test]
    fn build_dockerfile_multiple_packages() {
        let df = build_dockerfile("ubuntu:22.04", &["curl", "jq", "vim", "git"]);
        assert!(df.contains("curl jq vim git"));
    }

    // --- OCI index manifest structure ---

    #[test]
    fn oci_index_serializes_with_correct_field_names() {
        let index = OciIndex {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_INDEX.to_string(),
            manifests: vec![OciPlatformManifest {
                media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
                digest: "sha256:abc123".to_string(),
                size: 1024,
                platform: OciPlatform {
                    os: "linux".to_string(),
                    architecture: "amd64".to_string(),
                },
            }],
        };

        let json_str = serde_json::to_string_pretty(&index).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        // Verify camelCase field names (from serde rename_all)
        assert_eq!(parsed["schemaVersion"], 2);
        assert_eq!(
            parsed["mediaType"],
            "application/vnd.oci.image.index.v1+json"
        );
        assert!(parsed["manifests"].is_array());
        assert_eq!(parsed["manifests"][0]["size"], 1024);
        assert_eq!(parsed["manifests"][0]["digest"], "sha256:abc123");
        assert_eq!(parsed["manifests"][0]["platform"]["os"], "linux");
        assert_eq!(parsed["manifests"][0]["platform"]["architecture"], "amd64");
    }

    #[test]
    fn oci_index_roundtrips_multiple_platforms() {
        let index = OciIndex {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_INDEX.to_string(),
            manifests: vec![
                OciPlatformManifest {
                    media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
                    digest: "sha256:aaa".to_string(),
                    size: 100,
                    platform: OciPlatform {
                        os: "linux".to_string(),
                        architecture: "amd64".to_string(),
                    },
                },
                OciPlatformManifest {
                    media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
                    digest: "sha256:bbb".to_string(),
                    size: 200,
                    platform: OciPlatform {
                        os: "linux".to_string(),
                        architecture: "arm64".to_string(),
                    },
                },
                OciPlatformManifest {
                    media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
                    digest: "sha256:ccc".to_string(),
                    size: 300,
                    platform: OciPlatform {
                        os: "darwin".to_string(),
                        architecture: "arm64".to_string(),
                    },
                },
            ],
        };

        let json_bytes = serde_json::to_vec(&index).unwrap();
        let roundtripped: OciIndex = serde_json::from_slice(&json_bytes).unwrap();

        assert_eq!(roundtripped.schema_version, 2);
        assert_eq!(roundtripped.manifests.len(), 3);
        assert_eq!(roundtripped.manifests[0].platform.os, "linux");
        assert_eq!(roundtripped.manifests[0].platform.architecture, "amd64");
        assert_eq!(roundtripped.manifests[0].digest, "sha256:aaa");
        assert_eq!(roundtripped.manifests[0].size, 100);
        assert_eq!(roundtripped.manifests[1].platform.architecture, "arm64");
        assert_eq!(roundtripped.manifests[1].digest, "sha256:bbb");
        assert_eq!(roundtripped.manifests[2].platform.os, "darwin");
        assert_eq!(roundtripped.manifests[2].digest, "sha256:ccc");
        assert_eq!(roundtripped.manifests[2].size, 300);
    }

    #[test]
    fn oci_index_empty_manifests_list() {
        let index = OciIndex {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_INDEX.to_string(),
            manifests: vec![],
        };

        let json_str = serde_json::to_string(&index).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["manifests"].as_array().unwrap().len(), 0);

        let roundtripped: OciIndex = serde_json::from_str(&json_str).unwrap();
        assert_eq!(roundtripped.manifests.len(), 0);
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

    // --- OciReference::parse edge cases ---

    #[test]
    fn parse_reference_with_whitespace_fails() {
        assert!(OciReference::parse("ghcr.io/my repo:v1").is_err());
    }

    #[test]
    fn parse_reference_with_control_chars_fails() {
        assert!(OciReference::parse("ghcr.io/repo\x00:v1").is_err());
    }

    #[test]
    fn parse_reference_host_colon_port_only_fails() {
        // "host:5000" without a repo path is invalid (looks like port with no repo)
        assert!(OciReference::parse("host:5000").is_err());
    }

    #[test]
    fn parse_reference_localhost_no_port() {
        let r = OciReference::parse("localhost/myrepo:v1").unwrap();
        assert_eq!(r.registry, "localhost");
        assert_eq!(r.repository, "myrepo");
        assert_eq!(r.reference, ReferenceKind::Tag("v1".to_string()));
    }

    #[test]
    fn parse_reference_registry_with_port_and_nested_repo() {
        let r = OciReference::parse("myregistry.io:5000/org/repo:v2").unwrap();
        assert_eq!(r.registry, "myregistry.io:5000");
        assert_eq!(r.repository, "org/repo");
        assert_eq!(r.reference, ReferenceKind::Tag("v2".to_string()));
    }

    #[test]
    fn parse_reference_127_0_0_1() {
        let r = OciReference::parse("127.0.0.1:5000/test:dev").unwrap();
        assert_eq!(r.registry, "127.0.0.1:5000");
        assert_eq!(r.repository, "test");
        // api_base should use http for 127.0.0.1
        assert!(r.api_base().starts_with("http://"));
    }

    #[test]
    fn reference_str_tag() {
        let r = OciReference {
            registry: "ghcr.io".to_string(),
            repository: "test/mod".to_string(),
            reference: ReferenceKind::Tag("v1.0".to_string()),
        };
        assert_eq!(r.reference_str(), "v1.0");
    }

    #[test]
    fn reference_str_digest() {
        let r = OciReference {
            registry: "ghcr.io".to_string(),
            repository: "test/mod".to_string(),
            reference: ReferenceKind::Digest("sha256:abc".to_string()),
        };
        assert_eq!(r.reference_str(), "sha256:abc");
    }

    // --- docker_config_path ---

    #[test]
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

    // --- create_tar_gz: symlinks skipped ---

    #[test]
    #[cfg(unix)]
    fn create_tar_gz_skips_symlinks_in_source_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "content").unwrap();
        // Create a symlink
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link")).unwrap();

        let archive = create_tar_gz(dir.path()).unwrap();

        // Extract and verify only the regular file exists
        let out = tempfile::tempdir().unwrap();
        extract_tar_gz(&archive, out.path()).unwrap();

        assert!(out.path().join("file.txt").exists());
        // The symlink should not have been included in the archive
        assert!(
            !out.path().join("link").exists(),
            "symlinks should be skipped during archive creation"
        );
    }

    // --- OciManifest with annotations ---

    #[test]
    fn oci_manifest_with_annotations_round_trips() {
        let mut annotations = HashMap::new();
        annotations.insert("cfgd.io/platform".to_string(), "linux/amd64".to_string());
        annotations.insert(
            "org.opencontainers.image.created".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );

        let manifest = OciManifest {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
            config: OciDescriptor {
                media_type: MEDIA_TYPE_MODULE_CONFIG.to_string(),
                digest: "sha256:cfg123".to_string(),
                size: 50,
                annotations: HashMap::new(),
            },
            layers: vec![OciDescriptor {
                media_type: MEDIA_TYPE_MODULE_LAYER.to_string(),
                digest: "sha256:layer123".to_string(),
                size: 1024,
                annotations: HashMap::new(),
            }],
            annotations,
        };

        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let parsed: OciManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.annotations.len(), 2);
        assert_eq!(
            parsed.annotations.get("cfgd.io/platform").unwrap(),
            "linux/amd64"
        );
        assert_eq!(
            parsed
                .annotations
                .get("org.opencontainers.image.created")
                .unwrap(),
            "2026-01-01T00:00:00Z"
        );
    }

    #[test]
    fn oci_manifest_empty_annotations_skipped_in_json() {
        let manifest = OciManifest {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
            config: OciDescriptor {
                media_type: MEDIA_TYPE_MODULE_CONFIG.to_string(),
                digest: "sha256:cfg".to_string(),
                size: 10,
                annotations: HashMap::new(),
            },
            layers: vec![],
            annotations: HashMap::new(),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        // Empty HashMaps have skip_serializing_if = "HashMap::is_empty"
        assert!(
            !json.contains("annotations"),
            "empty annotations should be skipped in serialization"
        );
    }

    // --- OciDescriptor with annotations ---

    #[test]
    fn oci_descriptor_with_annotations_round_trips() {
        let mut anns = HashMap::new();
        anns.insert(
            "org.opencontainers.image.title".to_string(),
            "my-module".to_string(),
        );

        let desc = OciDescriptor {
            media_type: MEDIA_TYPE_MODULE_LAYER.to_string(),
            digest: "sha256:abc".to_string(),
            size: 512,
            annotations: anns,
        };

        let json = serde_json::to_string(&desc).unwrap();
        let parsed: OciDescriptor = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.size, 512);
        assert_eq!(
            parsed
                .annotations
                .get("org.opencontainers.image.title")
                .unwrap(),
            "my-module"
        );
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

    // --- detect_pkg_install_cmd: container registry-prefixed images ---

    #[test]
    fn detect_pkg_install_cmd_registry_prefixed_fedora() {
        assert_eq!(
            detect_pkg_install_cmd("registry.example.com/fedora:39"),
            "dnf install -y"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_registry_prefixed_centos() {
        assert_eq!(
            detect_pkg_install_cmd("quay.io/centos/centos:stream8"),
            "yum install -y"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_registry_prefixed_archlinux() {
        assert_eq!(
            detect_pkg_install_cmd("docker.io/library/archlinux:base"),
            "pacman -Sy --noconfirm"
        );
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

    // --- create_tar_gz with nested directories ---

    #[test]
    fn create_tar_gz_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("deep.txt"), "nested content").unwrap();
        std::fs::write(dir.path().join("root.txt"), "top level").unwrap();

        let archive = create_tar_gz(dir.path()).unwrap();

        let out = tempfile::tempdir().unwrap();
        extract_tar_gz(&archive, out.path()).unwrap();

        let root_content = std::fs::read_to_string(out.path().join("root.txt")).unwrap();
        assert_eq!(root_content, "top level");

        let deep_content = std::fs::read_to_string(out.path().join("a/b/c/deep.txt")).unwrap();
        assert_eq!(deep_content, "nested content");
    }

    // --- create_tar_gz empty directory ---

    #[test]
    fn create_tar_gz_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let archive = create_tar_gz(dir.path()).unwrap();
        assert!(
            !archive.is_empty(),
            "even empty dir should produce a valid gzip stream"
        );

        let out = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&archive, out.path());
        assert!(result.is_ok());
    }

    // --- is_insecure_registry: no env var set ---

    #[test]
    fn is_insecure_registry_false_when_env_not_set() {
        // With the env var not containing our test registry, it should be false
        // (we cannot safely unset env vars in parallel tests, but the default
        // is empty which means no registry is insecure)
        assert!(!is_insecure_registry("totally-not-insecure.example.com"));
    }

    // --- OciReference Display for various formats ---

    #[test]
    fn oci_reference_display_tag() {
        let r = OciReference {
            registry: "registry.example.com".to_string(),
            repository: "org/repo".to_string(),
            reference: ReferenceKind::Tag("v1.2.3".to_string()),
        };
        assert_eq!(format!("{}", r), "registry.example.com/org/repo:v1.2.3");
    }

    #[test]
    fn oci_reference_display_digest() {
        let r = OciReference {
            registry: "ghcr.io".to_string(),
            repository: "myorg/mymod".to_string(),
            reference: ReferenceKind::Digest("sha256:abcdef1234".to_string()),
        };
        assert_eq!(format!("{}", r), "ghcr.io/myorg/mymod@sha256:abcdef1234");
    }

    // --- media type constants ---

    #[test]
    fn media_type_constants_are_correct() {
        assert_eq!(
            MEDIA_TYPE_MODULE_CONFIG,
            "application/vnd.cfgd.module.config.v1+json"
        );
        assert_eq!(
            MEDIA_TYPE_MODULE_LAYER,
            "application/vnd.cfgd.module.layer.v1.tar+gzip"
        );
        assert_eq!(
            MEDIA_TYPE_OCI_MANIFEST,
            "application/vnd.oci.image.manifest.v1+json"
        );
        assert_eq!(
            MEDIA_TYPE_OCI_INDEX,
            "application/vnd.oci.image.index.v1+json"
        );
    }

    // --- sha256_digest determinism ---

    #[test]
    fn sha256_digest_deterministic() {
        let data = b"test data for determinism check";
        let d1 = sha256_digest(data);
        let d2 = sha256_digest(data);
        assert_eq!(d1, d2);
    }

    #[test]
    fn sha256_digest_different_inputs_different_outputs() {
        let d1 = sha256_digest(b"input one");
        let d2 = sha256_digest(b"input two");
        assert_ne!(d1, d2);
    }

    // --- build_dockerfile: comprehensive generation ---

    #[test]
    fn build_dockerfile_ubuntu_with_packages_cleans_apt_lists() {
        let df = build_dockerfile("ubuntu:24.04", &["git", "curl", "make"]);
        assert_eq!(df.lines().count(), 4, "FROM + RUN + WORKDIR + COPY");
        assert!(df.starts_with("FROM ubuntu:24.04"));
        assert!(df.contains("apt-get update && apt-get install -y git curl make"));
        assert!(df.contains("rm -rf /var/lib/apt/lists/*"));
        assert!(df.contains("WORKDIR /build"));
        assert!(df.contains("COPY . /build/"));
    }

    #[test]
    fn build_dockerfile_debian_uses_apt() {
        let df = build_dockerfile("debian:bookworm", &["vim"]);
        assert!(df.contains("apt-get"), "debian should use apt-get");
        assert!(df.contains("rm -rf /var/lib/apt/lists/*"));
    }

    #[test]
    fn build_dockerfile_alpine_uses_apk() {
        let df = build_dockerfile("alpine:3.20", &["bash", "coreutils"]);
        assert!(df.contains("apk add --no-cache bash coreutils"));
        assert!(!df.contains("apt-get"));
        assert!(!df.contains("rm -rf"));
    }

    #[test]
    fn build_dockerfile_fedora_uses_dnf() {
        let df = build_dockerfile("fedora:41", &["gcc", "gdb"]);
        assert!(df.contains("dnf install -y gcc gdb"));
    }

    #[test]
    fn build_dockerfile_rockylinux_uses_dnf() {
        let df = build_dockerfile("rockylinux:9.3", &["python3"]);
        assert!(df.contains("dnf install -y python3"));
    }

    #[test]
    fn build_dockerfile_almalinux_uses_dnf() {
        let df = build_dockerfile("almalinux:9", &["wget"]);
        assert!(df.contains("dnf install -y wget"));
    }

    #[test]
    fn build_dockerfile_centos_uses_yum() {
        let df = build_dockerfile("centos:7", &["nmap"]);
        assert!(df.contains("yum install -y nmap"));
    }

    #[test]
    fn build_dockerfile_archlinux_uses_pacman() {
        let df = build_dockerfile("archlinux:base", &["neovim"]);
        assert!(df.contains("pacman -Sy --noconfirm neovim"));
    }

    #[test]
    fn build_dockerfile_registry_prefixed_alpine_custom() {
        let df = build_dockerfile("ghcr.io/custom/alpine:edge", &["jq"]);
        assert!(df.contains("apk add --no-cache jq"));
        assert!(df.starts_with("FROM ghcr.io/custom/alpine:edge"));
    }

    #[test]
    fn build_dockerfile_registry_prefixed_fedora() {
        let df = build_dockerfile("quay.io/fedora/fedora:40", &["strace"]);
        assert!(df.contains("dnf install -y strace"));
    }

    #[test]
    fn build_dockerfile_empty_packages_has_no_run() {
        let df = build_dockerfile("scratch", &[]);
        assert_eq!(df.lines().count(), 3, "FROM + WORKDIR + COPY, no RUN");
        assert!(!df.contains("RUN"));
    }

    #[test]
    fn build_dockerfile_single_package() {
        let df = build_dockerfile("ubuntu:22.04", &["curl"]);
        assert!(df.contains("curl"));
        // Should only have one RUN line
        let run_count = df.lines().filter(|l| l.starts_with("RUN")).count();
        assert_eq!(run_count, 1);
    }

    // --- detect_pkg_install_cmd: all mappings ---

    #[test]
    fn detect_pkg_install_cmd_alpine() {
        assert_eq!(detect_pkg_install_cmd("alpine:3.19"), "apk add --no-cache");
    }

    #[test]
    fn detect_pkg_install_cmd_ubuntu() {
        let cmd = detect_pkg_install_cmd("ubuntu:22.04");
        assert!(
            cmd.starts_with("apt-get"),
            "ubuntu should use apt-get: {cmd}"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_debian() {
        let cmd = detect_pkg_install_cmd("debian:12");
        assert!(
            cmd.starts_with("apt-get"),
            "debian should use apt-get: {cmd}"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_case_insensitive() {
        // The function lowercases the image name
        assert_eq!(detect_pkg_install_cmd("ALPINE:3.19"), "apk add --no-cache");
        assert_eq!(detect_pkg_install_cmd("Fedora:39"), "dnf install -y");
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

    // --- OCI manifest construction ---

    #[test]
    fn oci_manifest_round_trip_with_annotations() {
        let mut annotations = HashMap::new();
        annotations.insert("cfgd.io/platform".to_string(), "linux/amd64".to_string());
        annotations.insert(
            "org.opencontainers.image.created".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );

        let manifest = OciManifest {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
            config: OciDescriptor {
                media_type: MEDIA_TYPE_MODULE_CONFIG.to_string(),
                digest: "sha256:configdigest123".to_string(),
                size: 512,
                annotations: HashMap::new(),
            },
            layers: vec![OciDescriptor {
                media_type: MEDIA_TYPE_MODULE_LAYER.to_string(),
                digest: "sha256:layer1digest".to_string(),
                size: 4096,
                annotations: HashMap::new(),
            }],
            annotations: annotations.clone(),
        };

        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let parsed: OciManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.schema_version, 2);
        assert_eq!(parsed.media_type, MEDIA_TYPE_OCI_MANIFEST);
        assert_eq!(parsed.config.size, 512);
        assert_eq!(parsed.layers.len(), 1);
        assert_eq!(parsed.layers[0].size, 4096);
        assert_eq!(parsed.annotations.len(), 2);
        assert_eq!(
            parsed.annotations.get("cfgd.io/platform").unwrap(),
            "linux/amd64"
        );
    }

    #[test]
    fn oci_manifest_camel_case_keys() {
        let manifest = OciManifest {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
            config: OciDescriptor {
                media_type: MEDIA_TYPE_MODULE_CONFIG.to_string(),
                digest: "sha256:abc".to_string(),
                size: 100,
                annotations: HashMap::new(),
            },
            layers: vec![],
            annotations: HashMap::new(),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        assert!(
            json.contains("\"schemaVersion\""),
            "should use camelCase: {json}"
        );
        assert!(
            json.contains("\"mediaType\""),
            "should use camelCase: {json}"
        );
        assert!(
            !json.contains("\"schema_version\""),
            "should not use snake_case: {json}"
        );
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

    // --- RegistryAuth basic_auth_header ---

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

    // --- OCI reference edge cases ---

    #[test]
    fn parse_reference_whitespace_rejected() {
        assert!(OciReference::parse("ghcr.io/repo name:v1").is_err());
        assert!(OciReference::parse("ghcr.io/repo\tname:v1").is_err());
    }

    #[test]
    fn parse_reference_host_port_no_repo_rejected() {
        // "localhost:5000" without a repo path and with a numeric "tag" = port
        let result = OciReference::parse("localhost:5000");
        // This should be treated as host:port with no repo
        assert!(
            result.is_err(),
            "bare host:port without repo should be rejected, got: {result:?}"
        );
    }

    #[test]
    fn reference_str_returns_tag_or_digest() {
        let tag_ref = OciReference {
            registry: "ghcr.io".to_string(),
            repository: "test".to_string(),
            reference: ReferenceKind::Tag("v1.0.0".to_string()),
        };
        assert_eq!(tag_ref.reference_str(), "v1.0.0");

        let digest_ref = OciReference {
            registry: "ghcr.io".to_string(),
            repository: "test".to_string(),
            reference: ReferenceKind::Digest("sha256:abc".to_string()),
        };
        assert_eq!(digest_ref.reference_str(), "sha256:abc");
    }

    // --- tar gz with symlinks skipped ---

    #[test]
    fn create_tar_gz_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "real content").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("real.txt", dir.path().join("link.txt")).unwrap();

        let archive = create_tar_gz(dir.path()).unwrap();
        assert!(!archive.is_empty());

        // Extract and verify symlink was skipped
        let out = tempfile::tempdir().unwrap();
        extract_tar_gz(&archive, out.path()).unwrap();
        assert!(out.path().join("real.txt").exists());
        // Symlink should NOT be in the extracted output
        assert!(
            !out.path().join("link.txt").exists(),
            "symlinks should be skipped in create_tar_gz"
        );
    }

    // --- extract_tar_gz rejects symlinks ---

    #[test]
    fn extract_tar_gz_skips_symlinks_in_archive() {
        // Build a tar.gz that contains a symlink entry
        use flate2::write::GzEncoder;

        let buf = Vec::new();
        let encoder = GzEncoder::new(buf, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);

        // Add a normal file
        let content = b"normal file";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "normal.txt", &content[..])
            .unwrap();

        // Add a symlink entry
        let mut sym_header = tar::Header::new_gnu();
        sym_header.set_entry_type(tar::EntryType::Symlink);
        sym_header.set_size(0);
        sym_header.set_mode(0o777);
        sym_header.set_cksum();
        archive
            .append_link(&mut sym_header, "evil-link", "/etc/passwd")
            .unwrap();

        let encoder = archive.into_inner().unwrap();
        let compressed = encoder.finish().unwrap();

        let out = tempfile::tempdir().unwrap();
        extract_tar_gz(&compressed, out.path()).unwrap();

        assert!(out.path().join("normal.txt").exists());
        assert!(
            !out.path().join("evil-link").exists(),
            "symlinks should be skipped during extraction"
        );
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

    // --- is_insecure_registry ---

    #[test]
    fn is_insecure_registry_without_env_var() {
        // When OCI_INSECURE_REGISTRIES is not set (or empty), nothing is insecure
        let prev = std::env::var("OCI_INSECURE_REGISTRIES").ok();
        unsafe {
            std::env::remove_var("OCI_INSECURE_REGISTRIES");
        }
        assert!(!is_insecure_registry("localhost:5000"));
        assert!(!is_insecure_registry("ghcr.io"));

        // Restore
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("OCI_INSECURE_REGISTRIES", v);
            }
        }
    }

    // --- OCI index round-trip with platform details ---

    #[test]
    fn oci_index_camel_case_and_round_trip() {
        let index = OciIndex {
            schema_version: 2,
            media_type: MEDIA_TYPE_OCI_INDEX.to_string(),
            manifests: vec![OciPlatformManifest {
                media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
                digest: "sha256:abc".to_string(),
                size: 100,
                platform: OciPlatform {
                    os: "linux".to_string(),
                    architecture: "amd64".to_string(),
                },
            }],
        };

        let json = serde_json::to_string(&index).unwrap();
        assert!(json.contains("\"schemaVersion\""));
        assert!(json.contains("\"mediaType\""));
        assert!(!json.contains("\"schema_version\""));

        let parsed: OciIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.manifests[0].platform.os, "linux");
        assert_eq!(parsed.manifests[0].platform.architecture, "amd64");
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

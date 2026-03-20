// OCI artifact push/pull for cfgd modules.
//
// Implements a minimal OCI Distribution Spec client using ureq (sync HTTP).
// Supports pushing/pulling module archives with custom media types,
// registry authentication via Docker config.json, credential helpers, and env vars.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::OciError;

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
        {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{}/v2", self.registry)
    }
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
        use std::io::Write;
        let mut buf = Vec::new();
        write!(buf, "{}:{}", self.username, self.password).ok();
        format!("Basic {}", base64_encode(&buf))
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
            child.wait_with_output().ok()
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
    format!("sha256:{:x}", Sha256::digest(data))
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
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|e| OciError::ArchiveError {
                message: format!("path prefix error: {e}"),
            })?;

        if path.is_dir() {
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

    archive
        .unpack(output_dir)
        .map_err(|e| OciError::ArchiveError {
            message: format!("tar extraction failed: {e}"),
        })?;

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
) -> Result<String, OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = ureq::Agent::new();
    let (digest, _size) = push_module_inner(&agent, dir, &oci_ref, auth.as_ref(), platform)?;
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
    let platform_str = platform
        .map(String::from)
        .unwrap_or_else(current_platform);

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
    target
        .split_once('/')
        .ok_or_else(|| OciError::BuildError {
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
) -> Result<String, OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = ureq::Agent::new();

    let mut platform_manifests = Vec::new();

    for (dir, platform) in builds {
        let (os, arch) = parse_platform_target(platform)?;

        // Push each platform as its own tagged manifest
        let platform_tag = format!(
            "{}-{}",
            oci_ref.reference_str(),
            platform.replace('/', "-")
        );
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

/// Generate a Dockerfile for building a module in an isolated container.
fn build_dockerfile(base_image: &str, packages: &[&str]) -> String {
    let mut lines = vec![format!("FROM {base_image}")];
    if !packages.is_empty() {
        let pkg_list = packages.join(" ");
        lines.push(format!(
            "RUN apt-get update && apt-get install -y {pkg_list} && rm -rf /var/lib/apt/lists/*"
        ));
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
    let module_doc = crate::config::parse_module(&module_yaml).map_err(|e| OciError::BuildError {
        message: format!("invalid module.yaml: {e}"),
    })?;

    // Extract package names from module spec
    let pkg_names: Vec<String> = module_doc.spec.packages.iter().map(|p| p.name.clone()).collect();
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
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        return Err(OciError::BuildError {
            message: format!("{runtime} build failed:\n{stderr}"),
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
                String::from_utf8_lossy(&create_output.stderr)
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
                String::from_utf8_lossy(&cp_output.stderr)
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OciError::SigningError {
            message: format!("cosign sign failed: {stderr}"),
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OciError::VerificationFailed {
            reference: artifact_ref.to_string(),
            message: format!("cosign verify failed: {stderr}"),
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OciError::AttestationError {
            message: format!("cosign attest failed: {stderr}"),
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OciError::AttestationError {
            message: format!("attestation verification failed: {stderr}"),
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
) -> Result<(), OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = ureq::Agent::new();

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

    // Read blob data
    let mut blob_data = Vec::with_capacity(layer.size as usize);
    resp.into_reader()
        .take(layer.size + 1024) // small buffer for safety
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
            &VerifyOptions { key: None, identity: None, issuer: None },
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
            &VerifyOptions { key: Some("cosign.pub"), identity: None, issuer: None },
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
            &VerifyOptions { key: None, identity: None, issuer: None },
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
            &VerifyOptions { key: Some("cosign.pub"), identity: None, issuer: None },
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
}

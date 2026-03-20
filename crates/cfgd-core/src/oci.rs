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
        if reference.is_empty() {
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
        .unwrap_or_else(|| format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH));

    let agent = ureq::Agent::new();

    // Upload config blob
    let config_digest = upload_blob(
        &agent,
        &oci_ref,
        auth.as_ref(),
        &config_blob,
        MEDIA_TYPE_MODULE_CONFIG,
    )?;

    // Upload layer blob
    let layer_digest = upload_blob(
        &agent,
        &oci_ref,
        auth.as_ref(),
        &layer_data,
        MEDIA_TYPE_MODULE_LAYER,
    )?;

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
        &agent,
        "PUT",
        &manifest_url,
        auth.as_ref(),
        None,
        Some(MEDIA_TYPE_OCI_MANIFEST),
        Some(&manifest_json),
    )
    .map_err(|e| OciError::ManifestPushFailed {
        message: format!("{e}"),
    })?;

    let manifest_digest = sha256_digest(&manifest_json);
    tracing::info!(
        reference = %oci_ref,
        digest = %manifest_digest,
        "module pushed"
    );

    Ok(manifest_digest)
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
}

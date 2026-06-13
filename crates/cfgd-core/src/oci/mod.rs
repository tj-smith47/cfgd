// OCI artifact push/pull for cfgd modules.
//
// Implements a minimal OCI Distribution Spec client using ureq (sync HTTP).
// Supports pushing/pulling module archives with custom media types,
// registry authentication via Docker config.json, credential helpers, and env vars.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::errors::OciError;

mod archive;
mod auth;
mod build;
mod pack;
mod pull;
mod push;
mod sign;
mod transport;

pub use archive::{create_tar_gz, create_tar_gz_with_diff_id, extract_tar_gz};
pub use auth::RegistryAuth;
pub use build::{build_module, detect_container_runtime};
pub use pack::{PackOptions, PackOutcome, pack_image};
pub use pull::{SignaturePolicy, pull_module};
pub use push::{
    current_platform, parse_platform_target, push_module, push_module_multiplatform,
    rust_arch_to_oci,
};
pub use sign::{
    VerifyOptions, attach_attestation, generate_slsa_provenance, sign_artifact, verify_attestation,
    verify_signature,
};

// ---------------------------------------------------------------------------
// Media type constants
// ---------------------------------------------------------------------------

pub const MEDIA_TYPE_MODULE_CONFIG: &str = "application/vnd.cfgd.module.config.v1+json";
pub const MEDIA_TYPE_MODULE_LAYER: &str = "application/vnd.cfgd.module.layer.v1.tar+gzip";

/// OCI image manifest v2 media type.
pub(super) const MEDIA_TYPE_OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";

/// Standard OCI image config media type (the JSON blob whose digest becomes the manifest's config
/// descriptor). Use this when building a standard mountable image (e.g. `cfgd image pack`), as
/// opposed to the cfgd-custom `vnd.cfgd.module.config.*` type used for module artifacts.
pub const MEDIA_TYPE_OCI_IMAGE_CONFIG: &str = "application/vnd.oci.image.config.v1+json";

/// Standard OCI gzipped-layer media type. Identifies a layer as a gzip-compressed tar archive
/// conforming to the OCI Image Layer spec, making the image mountable as a Kubernetes
/// `volume.image` — as opposed to the cfgd-custom `vnd.cfgd.module.layer.*` type.
pub const MEDIA_TYPE_OCI_IMAGE_LAYER: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// OCI image index (multi-platform manifest list) media type. A base image may be
/// served as an index whose `manifests` array points at per-platform image manifests.
pub(super) const MEDIA_TYPE_OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";

/// Docker manifest-list media type — the Docker v2 equivalent of an OCI image index.
/// Registries serving Docker-format multi-platform images use this type.
pub(super) const MEDIA_TYPE_DOCKER_MANIFEST_LIST: &str =
    "application/vnd.docker.distribution.manifest.list.v2+json";

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
    pub(super) fn api_base(&self) -> String {
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
// OCI Manifest types (OCI Image Manifest v1)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OciManifest {
    pub(super) schema_version: u32,
    pub(super) media_type: String,
    pub(super) config: OciDescriptor,
    pub(super) layers: Vec<OciDescriptor>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(super) annotations: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OciDescriptor {
    pub(super) media_type: String,
    pub(super) digest: String,
    pub(super) size: u64,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(super) annotations: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// OCI Image Index types (multi-platform manifest list)
// ---------------------------------------------------------------------------

/// A parsed OCI image index / Docker manifest list. Only the `manifests` array
/// is consumed — enough to select the per-platform image manifest a base ref
/// resolves to when layering a new config layer on top of it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OciImageIndex {
    pub(super) manifests: Vec<OciIndexEntry>,
}

/// One entry in an image index: the digest of a platform-specific manifest plus
/// the platform it targets (absent for non-image entries like attestations).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OciIndexEntry {
    pub(super) digest: String,
    #[serde(default)]
    pub(super) platform: Option<OciPlatform>,
}

/// The `platform` object of an index entry. `os`/`architecture` are matched
/// against the resolved pack target to pick the right base manifest.
#[derive(Debug, Deserialize)]
pub(super) struct OciPlatform {
    pub(super) os: String,
    pub(super) architecture: String,
}

// ---------------------------------------------------------------------------
// OCI Image Config types (image-config JSON blob, per OCI Image Layout Spec)
// ---------------------------------------------------------------------------

/// OCI image config blob — the JSON whose digest populates the manifest's `config` descriptor.
///
/// Top-level keys are **lowercase/snake_case** per the OCI Image Config Spec
/// (`architecture`, `os`, `created`, `config`, `rootfs`). Do NOT apply
/// `rename_all = "camelCase"` here; the field names already match the required JSON keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageConfig {
    pub architecture: String,
    pub os: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ImageRuntimeConfig>,
    pub rootfs: RootFs,
}

/// The `rootfs` section of an OCI image config.
///
/// Uses snake_case keys (`type`, `diff_ids`) as specified by the OCI Image Config Spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootFs {
    /// Always `"layers"` for a standard OCI image.
    #[serde(rename = "type")]
    pub fs_type: String,
    /// Layer diff IDs (`sha256:<hex>` of the uncompressed tar). Serializes as `"diff_ids"`.
    pub diff_ids: Vec<String>,
}

/// The inner runtime `config` object within an OCI image config blob.
///
/// Keys are **PascalCase** per the OCI Runtime Config Spec (`Entrypoint`, `Cmd`, `Env`, etc.).
/// Each field carries an explicit `rename` so the Rust `snake_case` field names map correctly.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageRuntimeConfig {
    #[serde(rename = "Entrypoint", skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(rename = "Cmd", skip_serializing_if = "Option::is_none")]
    pub cmd: Option<Vec<String>>,
    #[serde(rename = "Env", skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    #[serde(rename = "WorkingDir", skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(rename = "User", skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(rename = "Labels", skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::BTreeMap<String, String>>,
}

// ---------------------------------------------------------------------------
// Test helpers shared across submodule test blocks.
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(super) mod test_helpers {
    /// Helper: extract host:port from a mockito server URL for use as OCI registry.
    pub(crate) fn registry_from_url(url: &str) -> String {
        url.trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_string()
    }

    /// Helper: create a module directory with a valid module.yaml for push tests.
    pub(crate) fn create_test_module_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("README.md"), "# Test module\n").unwrap();
        dir
    }
}

// ---------------------------------------------------------------------------
// Cross-cutting tests for top-level OCI types and the public surface.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

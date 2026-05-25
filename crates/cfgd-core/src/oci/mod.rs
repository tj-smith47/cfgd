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
mod pull;
mod push;
mod sign;
mod transport;

pub use archive::{create_tar_gz, extract_tar_gz};
pub use auth::RegistryAuth;
pub use build::{build_module, detect_container_runtime};
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

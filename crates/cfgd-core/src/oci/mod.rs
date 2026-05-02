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
pub use pull::pull_module;
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
mod tests {
    use super::*;
    use crate::sha256_digest;
    use serial_test::serial;

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

    // --- is_insecure_registry env var ---

    #[test]
    #[serial]
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

    // --- OciManifest with annotations ---

    #[test]
    fn oci_manifest_with_annotations_round_trips() {
        let mut annotations = HashMap::new();
        annotations.insert(
            crate::OCI_ANNOTATION_PLATFORM.to_string(),
            "linux/amd64".to_string(),
        );
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
            parsed
                .annotations
                .get(crate::OCI_ANNOTATION_PLATFORM)
                .unwrap(),
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
            push::MEDIA_TYPE_OCI_INDEX,
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

    // --- OCI manifest construction ---

    #[test]
    fn oci_manifest_round_trip_with_annotations() {
        let mut annotations = HashMap::new();
        annotations.insert(
            crate::OCI_ANNOTATION_PLATFORM.to_string(),
            "linux/amd64".to_string(),
        );
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
            parsed
                .annotations
                .get(crate::OCI_ANNOTATION_PLATFORM)
                .unwrap(),
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

    // --- is_insecure_registry ---

    #[test]
    #[serial]
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
}

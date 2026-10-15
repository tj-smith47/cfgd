use super::*;
use crate::sha256_digest;
use serde_json::Value;
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
            "sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890".to_string()
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
            annotations: Annotations::new(),
        },
        layers: vec![OciDescriptor {
            media_type: MEDIA_TYPE_MODULE_LAYER.to_string(),
            digest: "sha256:def456".to_string(),
            size: 2048,
            annotations: Annotations::new(),
        }],
        annotations: Annotations::new(),
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
    let prev = std::env::var(crate::oci::OCI_INSECURE_REGISTRIES_ENV).ok();

    // SAFETY: test is single-threaded for this env var; save/restore brackets usage
    unsafe {
        std::env::set_var(
            crate::oci::OCI_INSECURE_REGISTRIES_ENV,
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
            Some(v) => std::env::set_var(crate::oci::OCI_INSECURE_REGISTRIES_ENV, v),
            None => std::env::remove_var(crate::oci::OCI_INSECURE_REGISTRIES_ENV),
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
    let result = OciReference::parse("ghcr.io/my repo:v1");
    assert!(matches!(result, Err(OciError::InvalidReference { .. })));
}

#[test]
fn parse_reference_with_control_chars_fails() {
    let result = OciReference::parse("ghcr.io/repo\x00:v1");
    assert!(matches!(result, Err(OciError::InvalidReference { .. })));
}

#[test]
fn parse_reference_host_colon_port_only_fails() {
    // "host:5000" without a repo path is invalid (looks like port with no repo)
    let result = OciReference::parse("host:5000");
    assert!(matches!(result, Err(OciError::InvalidReference { .. })));
}

#[test]
fn parse_empty_reference_yields_invalid_reference_variant() {
    let result = OciReference::parse("");
    assert!(matches!(result, Err(OciError::InvalidReference { .. })));
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
    let mut annotations = Annotations::new();
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
            annotations: Annotations::new(),
        },
        layers: vec![OciDescriptor {
            media_type: MEDIA_TYPE_MODULE_LAYER.to_string(),
            digest: "sha256:layer123".to_string(),
            size: 1024,
            annotations: Annotations::new(),
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
            annotations: Annotations::new(),
        },
        layers: vec![],
        annotations: Annotations::new(),
    };

    let json = serde_json::to_string(&manifest).unwrap();
    // Empty annotation maps have skip_serializing_if = "Annotations::is_empty"
    assert!(
        !json.contains("annotations"),
        "empty annotations should be skipped in serialization"
    );
}

// --- OciDescriptor with annotations ---

#[test]
fn oci_descriptor_with_annotations_round_trips() {
    let mut anns = Annotations::new();
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
    // (env vars cannot safely be unset in parallel tests, but the default
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

// --- OCI manifest construction ---

#[test]
fn oci_manifest_round_trip_with_annotations() {
    let mut annotations = Annotations::new();
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
            annotations: Annotations::new(),
        },
        layers: vec![OciDescriptor {
            media_type: MEDIA_TYPE_MODULE_LAYER.to_string(),
            digest: "sha256:layer1digest".to_string(),
            size: 4096,
            annotations: Annotations::new(),
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
            annotations: Annotations::new(),
        },
        layers: vec![],
        annotations: Annotations::new(),
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
    let prev = std::env::var(crate::oci::OCI_INSECURE_REGISTRIES_ENV).ok();
    unsafe {
        std::env::remove_var(crate::oci::OCI_INSECURE_REGISTRIES_ENV);
    }
    assert!(!is_insecure_registry("localhost:5000"));
    assert!(!is_insecure_registry("ghcr.io"));

    // Restore
    unsafe {
        if let Some(v) = prev {
            std::env::set_var(crate::oci::OCI_INSECURE_REGISTRIES_ENV, v);
        }
    }
}

#[cfg(all(unix, feature = "test-helpers"))]
mod bridge {
    use crate::oci::archive::create_tar_gz;
    use crate::oci::pull::{PullChecks, pull_module};
    use crate::oci::push::push_module;
    use crate::oci::test_helpers::{create_test_module_dir, registry_from_url};
    use crate::oci::{MEDIA_TYPE_MODULE_CONFIG, MEDIA_TYPE_MODULE_LAYER, MEDIA_TYPE_OCI_MANIFEST};
    use crate::output::test_capture::{assert_snapshot_at, strip_ansi, strip_spinner_duration};
    use crate::output::{Doc, Printer, Role};
    use crate::sha256_digest;

    fn snapshot_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/oci/snapshots")
    }

    fn assert_snapshot(name: &str, actual: &str) {
        assert_snapshot_at(&snapshot_dir(), name, actual);
    }

    fn normalize_mock_url(s: &str, server_url: &str, registry: &str) -> String {
        s.replace(server_url, "<MOCK_URL>")
            .replace(registry, "<MOCK_REGISTRY>")
    }

    #[derive(serde::Serialize)]
    struct OciPullSummary {
        artifact: String,
        signature_verified: bool,
    }

    #[derive(serde::Serialize)]
    struct OciPushSummary {
        artifact: String,
        digest: String,
    }

    #[test]
    #[serial_test::serial]
    fn snapshot_oci_pull_clean() {
        let _registry_env = crate::test_helpers::isolate_registry_host_env();
        let mut server = mockito::Server::new();
        let server_url = server.url();
        let registry = registry_from_url(&server_url);

        let src_dir = create_test_module_dir();
        let layer_data = create_tar_gz(src_dir.path()).unwrap();
        let layer_digest = sha256_digest(&layer_data);

        let config_blob = serde_json::to_vec(&serde_json::json!({
            "moduleYaml": "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\n",
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

        server
            .mock("GET", "/v2/test/bridge-pull/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_body(serde_json::to_string(&manifest).unwrap())
            .create();

        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/v2/test/bridge-pull/blobs/sha256:.*".to_string()),
            )
            .with_status(200)
            .with_body(layer_data)
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/bridge-pull:v1", registry);

        let (printer, cap) = Printer::for_test_doc();
        let outcome = pull_module(
            &artifact_ref,
            output_dir.path(),
            PullChecks::default(),
            None,
            Some(&printer),
        )
        .unwrap();

        let summary = OciPullSummary {
            artifact: "<MOCK_URL>/test/bridge-pull:v1".to_string(),
            signature_verified: false,
        };
        let doc = Doc::new()
            .status(Role::Ok, "module pulled successfully")
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        let raw = strip_ansi(&cap.human());
        let url_normalized =
            normalize_mock_url(&raw, &server_url, &registry).replace(&outcome.digest, "<DIGEST>");
        let captured = strip_spinner_duration(url_normalized);

        assert!(
            captured.contains("\n\n"),
            "oci_pull_clean missing blank line at seam:\n{captured}"
        );
        assert!(
            !captured.contains("\n\n\n"),
            "oci_pull_clean has duplicate blank line:\n{captured}"
        );

        assert_snapshot("oci_pull_clean.txt", &captured);
    }

    #[test]
    #[serial_test::serial]
    fn snapshot_oci_push_clean() {
        let _registry_env = crate::test_helpers::isolate_registry_host_env();
        let mut server = mockito::Server::new();
        let server_url = server.url();
        let registry = registry_from_url(&server_url);

        let module_dir = create_test_module_dir();

        // Mock blob HEAD (not found) for config + layer
        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/test/bridge-push/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .expect_at_least(2)
            .create();

        let upload_location = format!("{}/v2/test/bridge-push/blobs/uploads/upload-id", server_url);
        server
            .mock("POST", "/v2/test/bridge-push/blobs/uploads/")
            .with_status(202)
            .with_header("Location", &upload_location)
            .expect_at_least(2)
            .create();

        server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/test/bridge-push/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
                ),
            )
            .with_status(201)
            .expect_at_least(2)
            .create();

        server
            .mock("GET", "/v2/test/bridge-push/manifests/v1")
            .with_status(404)
            .create();
        server
            .mock(
                "PUT",
                mockito::Matcher::Regex(r"^/v2/test/bridge-push/manifests/v1-".to_string()),
            )
            .with_status(201)
            .create();
        server
            .mock("PUT", "/v2/test/bridge-push/manifests/v1")
            .with_status(201)
            .create();

        let artifact_ref = format!("{}/test/bridge-push:v1", registry);

        let (printer, cap) = Printer::for_test_doc();
        let outcome = push_module(module_dir.path(), &artifact_ref, None, Some(&printer)).unwrap();

        let summary = OciPushSummary {
            artifact: "<MOCK_URL>/test/bridge-push:v1".to_string(),
            digest: "<DIGEST>".to_string(),
        };
        let doc = Doc::new()
            .status(Role::Ok, "module pushed successfully")
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        let raw = strip_ansi(&cap.human());
        let url_normalized = normalize_mock_url(&raw, &server_url, &registry);
        // Also normalize the actual digest (sha256:<hex>) since it's
        // content-derived, and the resolved platform since it is this host's.
        let digest_normalized = url_normalized
            .replace(&outcome.digest, "<DIGEST>")
            .replace(&outcome.platform, "<PLATFORM>");
        let captured = strip_spinner_duration(digest_normalized);

        assert!(
            captured.contains("\n\n"),
            "oci_push_clean missing blank line at seam:\n{captured}"
        );
        assert!(
            !captured.contains("\n\n\n"),
            "oci_push_clean has duplicate blank line:\n{captured}"
        );

        assert_snapshot("oci_push_clean.txt", &captured);
    }
}

// --- ImageConfig / RootFs / ImageRuntimeConfig serialization ---

#[test]
fn image_config_serializes_with_correct_oci_keys() {
    let cfg = ImageConfig {
        architecture: "amd64".to_string(),
        os: "linux".to_string(),
        variant: None,
        created: Some("2026-01-01T00:00:00Z".to_string()),
        config: Some(ImageRuntimeConfig {
            entrypoint: Some(vec!["/app/server".to_string()]),
            env: Some(vec!["PATH=/app/bin".to_string()]),
            ..Default::default()
        }),
        rootfs: RootFs {
            fs_type: "layers".to_string(),
            diff_ids: vec!["sha256:abc".to_string()],
        },
    };

    let val: Value = serde_json::to_value(&cfg).unwrap();

    // Top-level keys are lowercase/snake_case.
    assert_eq!(val["architecture"], "amd64");
    assert_eq!(val["os"], "linux");
    assert!(val.get("rootfs").is_some());

    // rootfs uses snake_case: "type" and "diff_ids" (NOT "diffIds").
    let rootfs = &val["rootfs"];
    assert_eq!(rootfs["type"], "layers");
    let diff_ids = rootfs["diff_ids"].as_array().unwrap();
    assert_eq!(diff_ids.len(), 1);
    assert_eq!(diff_ids[0], "sha256:abc");
    assert!(
        rootfs.get("diffIds").is_none(),
        "diff_ids must not camelCase to diffIds"
    );

    // Inner config object uses PascalCase keys.
    let config = &val["config"];
    assert!(config.get("Entrypoint").is_some());
    let entrypoint = config["Entrypoint"].as_array().unwrap();
    assert_eq!(entrypoint[0], "/app/server");
    assert!(config.get("Env").is_some());
    let env = config["Env"].as_array().unwrap();
    assert_eq!(env[0], "PATH=/app/bin");

    // None fields are absent from the JSON.
    assert!(config.get("Cmd").is_none(), "None Cmd must be absent");
    assert!(config.get("User").is_none(), "None User must be absent");

    // Round-trip: deserialize back and verify structural equality.
    let round_tripped: ImageConfig = serde_json::from_value(val).unwrap();
    assert_eq!(round_tripped.architecture, "amd64");
    assert_eq!(round_tripped.os, "linux");
    assert_eq!(round_tripped.rootfs.fs_type, "layers");
    assert_eq!(round_tripped.rootfs.diff_ids, vec!["sha256:abc"]);
    let rt_config = round_tripped.config.unwrap();
    assert_eq!(rt_config.entrypoint.unwrap(), vec!["/app/server"]);
    assert_eq!(rt_config.env.unwrap(), vec!["PATH=/app/bin"]);
    assert!(rt_config.cmd.is_none());
    assert!(rt_config.user.is_none());
}

/// An item compiled only into test builds, which ships nothing.
fn test_gated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .meta
                .require_list()
                .is_ok_and(|list| crate::test_helpers::cfg_requires_test(&list.tokens.to_string()))
    })
}

/// Only a registry's own `404` is "not found": `transport::get_present` is the
/// one function in the workspace's production code that builds
/// `ManifestNotFound` or `BlobNotFound`, in any spelling (`OciError::`,
/// `Self::`, a glob import, inside a macro); importing either under another
/// name counts as a construction. A construction anywhere else could
/// answer "missing" for an unreachable registry, a `500` or a refused
/// credential. Each production file is parsed whole with its test-gated items
/// skipped, so a file that does not parse fails the walk, and the expected map
/// names the file and function holding the constructions, so a renamed or
/// unread file fails by name.
#[test]
fn only_get_present_builds_a_not_found_error() {
    use std::collections::BTreeMap;
    use syn::visit::Visit;

    const VARIANTS: [&str; 2] = ["ManifestNotFound", "BlobNotFound"];

    struct Builds {
        file: String,
        function: Vec<String>,
        found: BTreeMap<String, usize>,
    }
    impl Builds {
        fn record(&mut self) {
            let function = self.function.last().map_or("<no fn>", String::as_str);
            *self
                .found
                .entry(format!("{} in fn {function}", self.file))
                .or_default() += 1;
        }
        fn scan_tokens(&mut self, tokens: proc_macro2::TokenStream) {
            let mut previous_named_a_variant = false;
            for tree in tokens {
                if let proc_macro2::TokenTree::Group(group) = &tree {
                    if previous_named_a_variant
                        && group.delimiter() == proc_macro2::Delimiter::Brace
                    {
                        self.record();
                    }
                    self.scan_tokens(group.stream());
                }
                previous_named_a_variant = matches!(
                    &tree,
                    proc_macro2::TokenTree::Ident(ident) if VARIANTS.iter().any(|v| ident == v)
                );
            }
        }
    }
    impl<'ast> Visit<'ast> for Builds {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            if !test_gated(&item.attrs) {
                syn::visit::visit_item_mod(self, item);
            }
        }
        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            if !test_gated(&item.attrs) {
                syn::visit::visit_item_impl(self, item);
            }
        }
        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            if test_gated(&item.attrs) {
                return;
            }
            self.function.push(item.sig.ident.to_string());
            syn::visit::visit_item_fn(self, item);
            self.function.pop();
        }
        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            if test_gated(&item.attrs) {
                return;
            }
            self.function.push(item.sig.ident.to_string());
            syn::visit::visit_impl_item_fn(self, item);
            self.function.pop();
        }
        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            self.function.push(item.sig.ident.to_string());
            syn::visit::visit_trait_item_fn(self, item);
            self.function.pop();
        }
        fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
            if expr
                .path
                .segments
                .last()
                .is_some_and(|segment| VARIANTS.iter().any(|v| segment.ident == v))
            {
                self.record();
            }
            syn::visit::visit_expr_struct(self, expr);
        }
        fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
            // A variant imported under another name is constructed under that
            // name, where neither check above can see it.
            if VARIANTS.iter().any(|v| rename.ident == v) {
                self.record();
            }
        }
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            self.scan_tokens(mac.tokens.clone());
        }
    }

    let root = crate::test_helpers::workspace_root();
    let mut builds = Builds {
        file: String::new(),
        function: Vec::new(),
        found: BTreeMap::new(),
    };
    for (_, files) in
        crate::test_helpers::production_sources_per_root(crate::test_helpers::WORKSPACE_CRATES)
    {
        // A file holding tests alone has an empty production view.
        for (path, _) in files
            .iter()
            .filter(|(_, production)| !production.is_empty())
        {
            let rel = path.strip_prefix(&root).unwrap_or(path);
            builds.file = rel.to_string_lossy().replace('\\', "/");
            let body = crate::test_helpers::walked_file_body(path);
            let file = syn::parse_file(&body)
                .unwrap_or_else(|e| panic!("{}: does not parse: {e}", builds.file));
            builds.visit_file(&file);
        }
    }
    proc_macro2::extra::invalidate_current_thread_spans();

    let expected = BTreeMap::from([(
        "crates/cfgd-core/src/oci/transport.rs in fn get_present".to_string(),
        2,
    )]);
    let stray: Vec<_> = builds
        .found
        .iter()
        .filter(|(at, _)| !expected.contains_key(*at))
        .collect();
    assert!(stray.is_empty(), "built outside get_present: {stray:?}");
    assert_eq!(
        builds.found, expected,
        "get_present no longer builds each variant once"
    );
}

/// Every environment variable a registry request reads is named on
/// [`registry_request_env`], and every name there is read.
///
/// The list is what `cfgd_bin()` removes from every spawn and what
/// `test_helpers::isolate_registry_host_env` unsets for an in-process test, so
/// a read the list does not name is a host input that reaches a test's
/// requests, and a name the list holds but nothing reads is a scrub that
/// guards nothing. Each production file under `oci/`, and `http.rs`, which
/// builds the agent every request goes through, is parsed whole with its
/// test-gated items skipped, so a file that does not parse fails the walk. A
/// read is a call to `var`, `var_os`, `vars`, `vars_os` or `env_or`; its
/// argument must be a path to a `const` declared in a walked file, whose value
/// is read off the same parse, and `vars`/`vars_os` take no name to check.
/// ureq reads the proxy variables itself, so the one call to
/// `Proxy::try_from_env` counts as reading every name on
/// [`crate::http::PROXY_ENV`]; `proxy_from_env_reads_every_variable_proxy_env_lists`
/// holds that list to what ureq reads. The container-runtime seams `build.rs`
/// reads through `tool_cmd` are `CFGD_*` names, which `cfgd_bin()` already
/// removes as a class.
#[test]
fn every_variable_the_registry_client_reads_is_a_registry_host_variable() {
    use std::collections::{BTreeMap, BTreeSet};
    use syn::visit::Visit;

    const READERS: [&str; 5] = ["var", "var_os", "vars", "vars_os", "env_or"];
    const PROXY_READER: [&str; 2] = ["Proxy", "try_from_env"];

    #[derive(Default)]
    struct Reads {
        file: String,
        consts: BTreeMap<String, String>,
        named: Vec<(String, String)>,
        unnamed: Vec<String>,
        proxy_reads: Vec<String>,
    }
    impl<'ast> Visit<'ast> for Reads {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            if !test_gated(&item.attrs) {
                syn::visit::visit_item_mod(self, item);
            }
        }
        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            if !test_gated(&item.attrs) {
                syn::visit::visit_item_impl(self, item);
            }
        }
        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            if !test_gated(&item.attrs) {
                syn::visit::visit_item_fn(self, item);
            }
        }
        fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(value),
                ..
            }) = &*item.expr
            {
                self.consts.insert(item.ident.to_string(), value.value());
            }
            syn::visit::visit_item_const(self, item);
        }
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(func) = &*call.func {
                let segments: Vec<String> = func
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect();
                if segments.ends_with(&PROXY_READER.map(String::from)) {
                    self.proxy_reads.push(self.file.clone());
                }
            }
            if let syn::Expr::Path(func) = &*call.func
                && let Some(reader) = func.path.segments.last()
                && READERS.iter().any(|r| reader.ident == r)
            {
                match call.args.first() {
                    Some(syn::Expr::Path(arg)) if arg.path.segments.last().is_some() => {
                        let name = arg.path.segments.last().map(|s| s.ident.to_string());
                        self.named
                            .push((self.file.clone(), name.unwrap_or_default()));
                    }
                    other => self.unnamed.push(format!(
                        "{}: {}({})",
                        self.file,
                        reader.ident,
                        match other {
                            Some(syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(literal),
                                ..
                            })) => format!("{:?}", literal.value()),
                            Some(_) => "<expression>".to_string(),
                            None => String::new(),
                        }
                    )),
                }
            }
            syn::visit::visit_expr_call(self, call);
        }
    }

    let root = crate::test_helpers::workspace_root();
    let oci_dir = root.join("crates/cfgd-core/src/oci");
    let http_file = root.join("crates/cfgd-core/src/http.rs");
    let mut reads = Reads::default();
    let mut files = 0usize;
    for (_, sources) in
        crate::test_helpers::production_sources_per_root(crate::test_helpers::WORKSPACE_CRATES)
    {
        for (path, _) in sources.iter().filter(|(path, production)| {
            (path.starts_with(&oci_dir) || *path == http_file) && !production.is_empty()
        }) {
            files += 1;
            let rel = path.strip_prefix(&root).unwrap_or(path);
            reads.file = rel.to_string_lossy().replace('\\', "/");
            let body = crate::test_helpers::walked_file_body(path);
            let file = syn::parse_file(&body)
                .unwrap_or_else(|e| panic!("{}: does not parse: {e}", reads.file));
            reads.visit_file(&file);
        }
    }
    proc_macro2::extra::invalidate_current_thread_spans();
    // 10 = archive, auth, build, mod, pack, pull, push, sign and transport,
    // and http.rs.
    assert!(
        files >= 10,
        "the walk read {files} production files under {} and {}; it has gone blind",
        oci_dir.display(),
        http_file.display()
    );
    assert_eq!(
        reads.proxy_reads,
        ["crates/cfgd-core/src/http.rs"],
        "the agent's proxy is read through the one `Proxy::try_from_env` call in http.rs"
    );

    assert!(
        reads.unnamed.is_empty(),
        "a registry request's read names its variable through a const on registry_request_env: {:?}",
        reads.unnamed
    );
    let listed: BTreeSet<&str> = registry_request_env().collect();
    let mut read: BTreeSet<&str> = reads
        .proxy_reads
        .iter()
        .flat_map(|_| crate::http::PROXY_ENV)
        .collect();
    for name in &read {
        assert!(
            listed.contains(name),
            "http.rs: the agent reads {name}, which registry_request_env does not list"
        );
    }
    for (file, ident) in &reads.named {
        let value = reads.consts.get(ident).unwrap_or_else(|| {
            panic!("{file}: reads `{ident}`, which names no string const in a walked file")
        });
        assert!(
            listed.contains(value.as_str()),
            "{file}: reads {value}, which registry_request_env does not list"
        );
        read.insert(value.as_str());
    }
    assert_eq!(
        read, listed,
        "registry_request_env lists a variable no registry request reads"
    );
}

/// Every registry host variable is spelled once, by its `*_ENV` const in
/// `oci/mod.rs`; a test setting, unsetting or reading one names the const, so
/// a renamed variable leaves no test guarding the old name.
#[test]
fn every_registry_host_variable_is_spelled_by_its_const_alone() {
    let root = crate::test_helpers::workspace_root();
    let home = root.join("crates/cfgd-core/src/oci/mod.rs");
    let mut files = 0usize;
    let mut offenders = Vec::new();
    for path in crate::test_helpers::rust_sources_under(&root.join("crates")) {
        files += 1;
        if path == home {
            continue;
        }
        // unfloored-slice-ok: a spelling anywhere, tests included, is the subject.
        let body = crate::test_helpers::walked_file_body(&path);
        let code = crate::test_helpers::blank_comments(&body);
        for name in REGISTRY_HOST_ENV {
            let literal = format!("\"{name}\"");
            for (i, line) in code.lines().enumerate() {
                if line.contains(&literal) {
                    offenders.push(format!("{}:{}: {literal}", path.display(), i + 1));
                }
            }
        }
    }
    assert!(
        files >= 500,
        "the walk read {files} files; it has gone blind"
    );
    assert!(
        offenders.is_empty(),
        "a registry host variable is spelled as a literal; name its `*_ENV` const:\n{}",
        offenders.join("\n")
    );
}

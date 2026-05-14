use super::*;
use crate::oci::test_helpers::{create_test_module_dir, registry_from_url};

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

// --- push_module_inner (mockito) ---

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

// --- push_module (top-level wrapper) ---

#[test]
fn push_module_top_level_parses_ref_and_returns_manifest_digest() {
    // Drives the wrapper at push/mod.rs:28-43: parse the artifact_ref string,
    // resolve auth (which is None for this 127.0.0.1 mock registry), build
    // the http agent, then delegate to push_module_inner. Asserts the
    // returned digest is the sha256 of the manifest JSON.
    let mut server = mockito::Server::new();
    let registry = registry_from_url(&server.url());
    let module_dir = create_test_module_dir();

    server
        .mock(
            "HEAD",
            mockito::Matcher::Regex(r"/v2/test/wrapped/blobs/sha256:.*".to_string()),
        )
        .with_status(404)
        .expect_at_least(2)
        .create();
    let upload_location = format!("{}/v2/test/wrapped/blobs/uploads/upload-id", server.url());
    server
        .mock("POST", "/v2/test/wrapped/blobs/uploads/")
        .with_status(202)
        .with_header("Location", &upload_location)
        .expect_at_least(2)
        .create();
    server
        .mock(
            "PUT",
            mockito::Matcher::Regex(
                r"/v2/test/wrapped/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
            ),
        )
        .with_status(201)
        .expect_at_least(2)
        .create();
    let manifest_mock = server
        .mock("PUT", "/v2/test/wrapped/manifests/wrap-tag")
        .with_status(201)
        .create();

    let artifact_ref = format!("{}/test/wrapped:wrap-tag", registry);
    let result = push_module(
        module_dir.path(),
        &artifact_ref,
        Some("linux/amd64"),
        None, // No printer — exercises the spinner=None branch
    );
    assert!(
        result.is_ok(),
        "push_module wrapper should succeed: {:?}",
        result.err()
    );
    let digest = result.unwrap();
    assert!(
        digest.starts_with("sha256:"),
        "manifest digest must be sha256-prefixed: {digest}"
    );
    manifest_mock.assert();
}

#[test]
fn push_module_top_level_propagates_invalid_reference_err() {
    // OciReference::parse rejects the empty string — the wrapper must surface
    // the parse Err before doing any I/O. Tempdir is a placeholder; nothing
    // is dialed.
    let module_dir = create_test_module_dir();
    let result = push_module(module_dir.path(), "", None, None);
    assert!(
        result.is_err(),
        "empty artifact_ref must error before any blob upload"
    );
}

// --- push_module_multiplatform (mockito) ---

#[test]
fn push_module_multiplatform_pushes_index_with_per_platform_manifests() {
    // Drives push_module_multiplatform at push/mod.rs:203-287: iterate two
    // (build_dir, platform) pairs, push each as its own platform-tagged
    // manifest via push_module_inner, then push the OCI index manifest list
    // under the original tag. Asserts the index PUT fires and the returned
    // digest is sha256-prefixed.
    let mut server = mockito::Server::new();
    let registry = registry_from_url(&server.url());
    let amd64_dir = create_test_module_dir();
    let arm64_dir = create_test_module_dir();

    // Per-manifest blobs: HEAD 404 + POST 202 + PUT 201 — same shape as
    // the single-platform path, but with `expect_at_least(4)` because
    // each of the two platforms uploads two blobs (config + layer).
    server
        .mock(
            "HEAD",
            mockito::Matcher::Regex(r"/v2/test/multi/blobs/sha256:.*".to_string()),
        )
        .with_status(404)
        .expect_at_least(4)
        .create();
    let upload_location = format!("{}/v2/test/multi/blobs/uploads/upload-id", server.url());
    server
        .mock("POST", "/v2/test/multi/blobs/uploads/")
        .with_status(202)
        .with_header("Location", &upload_location)
        .expect_at_least(4)
        .create();
    server
        .mock(
            "PUT",
            mockito::Matcher::Regex(
                r"/v2/test/multi/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
            ),
        )
        .with_status(201)
        .expect_at_least(4)
        .create();

    // Per-platform manifest PUTs (one per build, named `<tag>-<platform-with-dash>`).
    server
        .mock("PUT", "/v2/test/multi/manifests/multi-tag-linux-amd64")
        .with_status(201)
        .create();
    server
        .mock("PUT", "/v2/test/multi/manifests/multi-tag-linux-arm64")
        .with_status(201)
        .create();
    // Index manifest PUT (the original tag).
    let index_mock = server
        .mock("PUT", "/v2/test/multi/manifests/multi-tag")
        .with_status(201)
        .create();

    let artifact_ref = format!("{}/test/multi:multi-tag", registry);
    let builds: Vec<(&std::path::Path, &str)> = vec![
        (amd64_dir.path(), "linux/amd64"),
        (arm64_dir.path(), "linux/arm64"),
    ];
    let result = push_module_multiplatform(&builds, &artifact_ref, None);
    assert!(
        result.is_ok(),
        "multiplatform push should succeed: {:?}",
        result.err()
    );
    let index_digest = result.unwrap();
    assert!(
        index_digest.starts_with("sha256:"),
        "index digest must be sha256-prefixed: {index_digest}"
    );
    index_mock.assert();
}

#[test]
fn push_module_multiplatform_propagates_invalid_platform_target_err() {
    // A "linuxamd64" entry has no `/` so parse_platform_target returns
    // BuildError. The Err must surface from push_module_multiplatform
    // before any blob is uploaded.
    let module_dir = create_test_module_dir();
    let builds: Vec<(&std::path::Path, &str)> = vec![(module_dir.path(), "linuxamd64")];
    let result = push_module_multiplatform(&builds, "127.0.0.1:9999/test/m:t", None);
    assert!(
        matches!(result, Err(OciError::BuildError { .. })),
        "expected BuildError, got: {result:?}"
    );
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

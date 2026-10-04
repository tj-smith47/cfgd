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

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();

    let result = push_module_inner(&agent, module_dir.path(), &oci_ref, None, "linux/amd64");
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

/// The manifest `cfgd module push` actually PUTs carries its annotations in
/// one key order.
///
/// The pins beside `build_image_manifest` judge the packed-image builders;
/// this one judges the module push, which builds its own `OciManifest` inline
/// and is the path whose two runs a second apart produced two digests. The
/// mock only matches a body whose annotation object spells
/// `cfgd.io/platform` before `org.opencontainers.image.created` (the sorted
/// order; the function inserts them the other way round), so a manifest
/// that serialized them the other way never reaches this mock and
/// `manifest_mock.assert()` reports it.
#[test]
fn push_module_inner_writes_its_manifest_annotations_in_sorted_key_order() {
    let mut server = mockito::Server::new();
    let registry = registry_from_url(&server.url());

    let oci_ref = OciReference {
        registry,
        repository: "test/ordered".to_string(),
        reference: ReferenceKind::Tag("v1".to_string()),
    };

    let module_dir = create_test_module_dir();

    server
        .mock(
            "HEAD",
            mockito::Matcher::Regex(r"/v2/test/ordered/blobs/sha256:.*".to_string()),
        )
        .with_status(404)
        .expect_at_least(2)
        .create();
    let upload_location = format!("{}/v2/test/ordered/blobs/uploads/upload-id", server.url());
    server
        .mock("POST", "/v2/test/ordered/blobs/uploads/")
        .with_status(202)
        .with_header("Location", &upload_location)
        .expect_at_least(2)
        .create();
    server
        .mock(
            "PUT",
            mockito::Matcher::Regex(
                r"/v2/test/ordered/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
            ),
        )
        .with_status(201)
        .expect_at_least(2)
        .create();

    // `created` is the wall clock, so it is matched as "any string"; the two
    // keys' ORDER and the object's end are what the pattern pins.
    let ordered = format!(
        r#""annotations":\{{"{platform}":"linux/amd64","{created}":"[^"]+"\}}"#,
        platform = crate::OCI_ANNOTATION_PLATFORM.replace('.', r"\."),
        created = crate::OCI_ANNOTATION_CREATED.replace('.', r"\."),
    );
    let manifest_mock = server
        .mock("PUT", "/v2/test/ordered/manifests/v1")
        .with_status(201)
        .match_body(mockito::Matcher::Regex(ordered))
        .create();

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();

    push_module_inner(&agent, module_dir.path(), &oci_ref, None, "linux/amd64")
        .expect("the ordered push must succeed");
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

    let agent = ureq::Agent::config_builder().build().new_agent();
    let result = push_module_inner(&agent, dir.path(), &oci_ref, None, "linux/amd64");
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
    server
        .mock("PUT", "/v2/test/wrapped/manifests/wrap-tag-linux-amd64")
        .with_status(201)
        .create();
    server
        .mock("GET", "/v2/test/wrapped/manifests/wrap-tag")
        .with_status(404)
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
    let outcome = result.unwrap();
    assert!(
        outcome.digest.starts_with("sha256:"),
        "manifest digest must be sha256-prefixed: {}",
        outcome.digest
    );
    assert_eq!(
        outcome.platform, "linux/amd64",
        "the outcome reports the platform the push annotated the manifest with"
    );
    manifest_mock.assert();
}

/// With no `--platform`, the annotation written and the platform reported are
/// one resolution of this host — the caller has no second derivation to make.
#[test]
fn push_module_with_no_platform_reports_the_host_platform_it_annotated() {
    let mut server = mockito::Server::new();
    let registry = registry_from_url(&server.url());
    let module_dir = create_test_module_dir();

    server
        .mock(
            "HEAD",
            mockito::Matcher::Regex(r"/v2/test/defaulted/blobs/sha256:.*".to_string()),
        )
        .with_status(404)
        .expect_at_least(2)
        .create();
    let upload_location = format!("{}/v2/test/defaulted/blobs/uploads/upload-id", server.url());
    server
        .mock("POST", "/v2/test/defaulted/blobs/uploads/")
        .with_status(202)
        .with_header("Location", &upload_location)
        .expect_at_least(2)
        .create();
    server
        .mock(
            "PUT",
            mockito::Matcher::Regex(
                r"/v2/test/defaulted/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
            ),
        )
        .with_status(201)
        .expect_at_least(2)
        .create();
    // Every push reads the tag first and tags its manifest per platform.
    server
        .mock("GET", "/v2/test/defaulted/manifests/v1")
        .with_status(404)
        .create();
    server
        .mock(
            "PUT",
            mockito::Matcher::Regex(r"^/v2/test/defaulted/manifests/v1-".to_string()),
        )
        .with_status(201)
        .create();
    let manifest_mock = server
        .mock("PUT", "/v2/test/defaulted/manifests/v1")
        .with_status(201)
        .match_body(mockito::Matcher::PartialJsonString(format!(
            r#"{{"annotations":{{"{}":"{}"}}}}"#,
            crate::OCI_ANNOTATION_PLATFORM,
            crate::oci::current_platform()
        )))
        .create();

    let artifact_ref = format!("{}/test/defaulted:v1", registry);
    let outcome = push_module(module_dir.path(), &artifact_ref, None, None)
        .expect("defaulted push must succeed");
    assert_eq!(
        outcome.platform,
        crate::oci::current_platform(),
        "a push given no platform reports the host it resolved"
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

#[test]
fn push_module_registry_failure_finishes_spinner_as_fail() {
    // Drives the error arm added at push/mod.rs:44-51: when push_module_inner
    // errors, the spinner must close with Role::Fail rather than being
    // dropped (which would render as an unmarked Info line instead).
    let mut server = mockito::Server::new();
    let registry = registry_from_url(&server.url());
    let module_dir = create_test_module_dir();

    server
        .mock(
            "HEAD",
            mockito::Matcher::Regex(r"/v2/test/failpush/blobs/sha256:.*".to_string()),
        )
        .with_status(500)
        .create();
    server
        .mock("POST", "/v2/test/failpush/blobs/uploads/")
        .with_status(500)
        .with_body(r#"{"errors":[{"code":"DENIED","message":"quota exceeded"}]}"#)
        .create();

    let (printer, buf) = crate::output::Printer::for_test();
    let artifact_ref = format!("{}/test/failpush:v1", registry);
    let result = push_module(
        module_dir.path(),
        &artifact_ref,
        Some("linux/amd64"),
        Some(&printer),
    );
    assert!(result.is_err(), "500 from registry must surface as Err");
    printer.flush();

    let rendered = crate::test_helpers::captured_text(&buf);
    assert!(
        rendered.contains("Failed to push module"),
        "spinner must finish_fail with a push-failure subject, got: {rendered}"
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

    // Per-platform manifest PUTs (one per build, named `<tag>-<platform-with-dash>`),
    // each answering the digest of the bytes it took and recording it.
    let platform_digests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    for platform in ["linux-amd64", "linux-arm64"] {
        let recorded = std::sync::Arc::clone(&platform_digests);
        server
            .mock(
                "PUT",
                format!("/v2/test/multi/manifests/multi-tag-{platform}").as_str(),
            )
            .with_status(201)
            .with_header_from_request("Docker-Content-Digest", move |req| {
                let digest = crate::sha256_digest(req.body().expect("manifest body"));
                recorded.lock().expect("digests lock").push(digest.clone());
                digest
            })
            .create();
    }
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
    let outcome = result.unwrap();
    assert!(
        outcome.index_digest.starts_with("sha256:"),
        "index digest must be sha256-prefixed: {outcome:?}"
    );
    let platform_digests = platform_digests.lock().expect("digests lock").clone();
    assert_ne!(
        platform_digests[0], platform_digests[1],
        "the two manifests differ"
    );
    assert_eq!(
        outcome.manifest_digests, platform_digests,
        "each platform's manifest digest, in build order"
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

#[test]
fn push_module_multiplatform_index_failure_finishes_spinner_as_fail() {
    // Same finish_fail contract as push_module, but for the index-push
    // failure branch of push_module_multiplatform (mod.rs:242-250): the
    // per-platform manifests succeed but the index PUT 500s.
    let mut server = mockito::Server::new();
    let registry = registry_from_url(&server.url());
    let amd64_dir = create_test_module_dir();

    server
        .mock(
            "HEAD",
            mockito::Matcher::Regex(r"/v2/test/failmulti/blobs/sha256:.*".to_string()),
        )
        .with_status(404)
        .expect_at_least(2)
        .create();
    let upload_location = format!("{}/v2/test/failmulti/blobs/uploads/upload-id", server.url());
    server
        .mock("POST", "/v2/test/failmulti/blobs/uploads/")
        .with_status(202)
        .with_header("Location", &upload_location)
        .expect_at_least(2)
        .create();
    server
        .mock(
            "PUT",
            mockito::Matcher::Regex(
                r"/v2/test/failmulti/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
            ),
        )
        .with_status(201)
        .expect_at_least(2)
        .create();
    server
        .mock("PUT", "/v2/test/failmulti/manifests/multi-tag-linux-amd64")
        .with_status(201)
        .create();
    server
        .mock("PUT", "/v2/test/failmulti/manifests/multi-tag")
        .with_status(500)
        .with_body(r#"{"errors":[{"code":"DENIED","message":"insufficient storage"}]}"#)
        .create();

    let (printer, buf) = crate::output::Printer::for_test();
    let artifact_ref = format!("{}/test/failmulti:multi-tag", registry);
    let builds: Vec<(&std::path::Path, &str)> = vec![(amd64_dir.path(), "linux/amd64")];
    let result = push_module_multiplatform(&builds, &artifact_ref, Some(&printer));
    assert!(result.is_err(), "index 500 must surface as Err");
    printer.flush();

    let rendered = crate::test_helpers::captured_text(&buf);
    assert!(
        rendered.contains("Failed to push multi-platform module"),
        "spinner must finish_fail with a push-failure subject, got: {rendered}"
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

// --- push_module --platform: accumulating platforms under one tag ---

const MANIFEST_PUT: &str = "application/vnd.oci.image.manifest.v1+json";
const INDEX_PUT: &str = "application/vnd.oci.image.index.v1+json";

/// A manifest an earlier push left at a tag, annotated with `platform` when
/// one is given.
fn earlier_manifest(platform: Option<&str>) -> serde_json::Value {
    let mut doc = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": MEDIA_TYPE_OCI_MANIFEST,
        "config": { "mediaType": MEDIA_TYPE_MODULE_CONFIG, "digest": "sha256:c0", "size": 1 },
        "layers": [],
    });
    if let Some(p) = platform {
        doc["annotations"] = serde_json::json!({ crate::OCI_ANNOTATION_PLATFORM: p });
    }
    doc
}

fn json_of(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("stored bytes are JSON")
}

#[test]
fn platform_push_to_an_absent_tag_puts_the_one_manifest_at_the_tag() {
    let store = crate::oci::test_helpers::ManifestStore::new("test/acc");
    let module_dir = create_test_module_dir();

    let outcome = push_module(
        module_dir.path(),
        &store.artifact("v1"),
        Some("linux/amd64"),
        None,
    )
    .expect("push to an absent tag");

    assert_eq!(
        store.requests(),
        vec![
            "GET v1".to_string(),
            format!("PUT v1-linux-amd64 {MANIFEST_PUT}"),
            format!("PUT v1 {MANIFEST_PUT}"),
        ]
    );
    let at_tag = store.stored("v1");
    assert_eq!(
        at_tag,
        store.stored("v1-linux-amd64"),
        "the tag holds the platform manifest's own bytes"
    );
    assert_eq!(outcome.digest, crate::sha256_digest(&at_tag));
    assert_eq!(outcome.index_digest, None);
}

#[test]
fn platform_push_beside_another_platforms_manifest_puts_an_index_of_both() {
    let store = crate::oci::test_helpers::ManifestStore::new("test/acc");
    let earlier = store.seed("v1", &earlier_manifest(Some("linux/amd64")));
    let module_dir = create_test_module_dir();

    let outcome = push_module(
        module_dir.path(),
        &store.artifact("v1"),
        Some("linux/arm64"),
        None,
    )
    .expect("push beside another platform");

    assert_eq!(
        store.requests(),
        vec![
            "GET v1".to_string(),
            format!("PUT v1-linux-arm64 {MANIFEST_PUT}"),
            format!("PUT v1 {INDEX_PUT}"),
        ]
    );
    let index_bytes = store.stored("v1");
    let new_manifest = store.stored("v1-linux-arm64");
    assert_eq!(
        json_of(&index_bytes),
        serde_json::json!({
            "schemaVersion": 2,
            "mediaType": INDEX_PUT,
            "manifests": [
                {
                    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                    "digest": crate::sha256_digest(&earlier),
                    "size": earlier.len(),
                    "platform": { "os": "linux", "architecture": "amd64" },
                },
                {
                    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                    "digest": crate::sha256_digest(&new_manifest),
                    "size": new_manifest.len(),
                    "platform": { "os": "linux", "architecture": "arm64" },
                },
            ],
        })
    );
    assert_eq!(outcome.digest, crate::sha256_digest(&new_manifest));
    assert_eq!(
        outcome.index_digest,
        Some(crate::sha256_digest(&index_bytes))
    );
}

#[test]
fn platform_push_over_the_same_platforms_manifest_replaces_it() {
    let store = crate::oci::test_helpers::ManifestStore::new("test/acc");
    store.seed("v1", &earlier_manifest(Some("linux/arm64")));
    let module_dir = create_test_module_dir();

    let outcome = push_module(
        module_dir.path(),
        &store.artifact("v1"),
        Some("linux/arm64"),
        None,
    )
    .expect("push over the same platform");

    assert_eq!(
        store.requests(),
        vec![
            "GET v1".to_string(),
            format!("PUT v1-linux-arm64 {MANIFEST_PUT}"),
            format!("PUT v1 {MANIFEST_PUT}"),
        ]
    );
    assert_eq!(store.stored("v1"), store.stored("v1-linux-arm64"));
    assert_eq!(outcome.index_digest, None);
}

#[test]
fn platform_push_to_an_index_replaces_its_platforms_entry_in_place() {
    let store = crate::oci::test_helpers::ManifestStore::new("test/acc");
    let attestation = serde_json::json!({
        "mediaType": MEDIA_TYPE_OCI_MANIFEST,
        "digest": "sha256:a77",
        "size": 9,
        "platform": { "os": "unknown", "architecture": "unknown" },
        "annotations": { "vnd.docker.reference.type": "attestation-manifest" },
    });
    store.seed(
        "v1",
        &serde_json::json!({
            "schemaVersion": 2,
            "mediaType": INDEX_PUT,
            "manifests": [
                {
                    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                    "digest": "sha256:a1",
                    "size": 1,
                    "platform": { "os": "linux", "architecture": "amd64" },
                },
                {
                    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                    "digest": "sha256:old",
                    "size": 2,
                    "platform": { "os": "linux", "architecture": "arm64" },
                },
                attestation,
            ],
        }),
    );
    let module_dir = create_test_module_dir();

    let outcome = push_module(
        module_dir.path(),
        &store.artifact("v1"),
        Some("linux/arm64"),
        None,
    )
    .expect("push into an index");

    assert_eq!(
        store.requests(),
        vec![
            "GET v1".to_string(),
            format!("PUT v1-linux-arm64 {MANIFEST_PUT}"),
            format!("PUT v1 {INDEX_PUT}"),
        ]
    );
    let index_bytes = store.stored("v1");
    let new_manifest = store.stored("v1-linux-arm64");
    let index = json_of(&index_bytes);
    assert_eq!(index["mediaType"], INDEX_PUT);
    assert_eq!(
        index["manifests"],
        serde_json::json!([
            {
                "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                "digest": "sha256:a1",
                "size": 1,
                "platform": { "os": "linux", "architecture": "amd64" },
            },
            {
                "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                "digest": crate::sha256_digest(&new_manifest),
                "size": new_manifest.len(),
                "platform": { "os": "linux", "architecture": "arm64" },
            },
            attestation,
        ])
    );
    assert_eq!(
        outcome.index_digest,
        Some(crate::sha256_digest(&index_bytes))
    );
}

#[test]
fn platform_push_to_an_index_without_its_platform_appends_an_entry() {
    let store = crate::oci::test_helpers::ManifestStore::new("test/acc");
    let amd64 = serde_json::json!({
        "mediaType": MEDIA_TYPE_OCI_MANIFEST,
        "digest": "sha256:a1",
        "size": 1,
        "platform": { "os": "linux", "architecture": "amd64" },
    });
    store.seed(
        "v1",
        &serde_json::json!({ "schemaVersion": 2, "mediaType": INDEX_PUT, "manifests": [amd64] }),
    );
    let module_dir = create_test_module_dir();

    push_module(
        module_dir.path(),
        &store.artifact("v1"),
        Some("linux/s390x"),
        None,
    )
    .expect("push a new platform into an index");

    assert_eq!(
        store.requests().last().map(String::as_str),
        Some(format!("PUT v1 {INDEX_PUT}").as_str())
    );
    let new_manifest = store.stored("v1-linux-s390x");
    assert_eq!(
        json_of(&store.stored("v1"))["manifests"],
        serde_json::json!([
            amd64,
            {
                "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                "digest": crate::sha256_digest(&new_manifest),
                "size": new_manifest.len(),
                "platform": { "os": "linux", "architecture": "s390x" },
            },
        ])
    );
}

#[test]
fn platform_push_beside_a_manifest_naming_no_platform_is_refused_and_leaves_the_tag() {
    let store = crate::oci::test_helpers::ManifestStore::new("test/acc");
    let earlier = store.seed("v1", &earlier_manifest(None));
    let module_dir = create_test_module_dir();

    let err = push_module(
        module_dir.path(),
        &store.artifact("v1"),
        Some("linux/arm64"),
        None,
    )
    .err()
    .expect("an unlabelled manifest cannot join an index");

    match &err {
        OciError::TagPlatformUnknown {
            reference,
            annotation,
        } => {
            assert_eq!(reference, &store.artifact("v1"));
            assert_eq!(annotation, crate::OCI_ANNOTATION_PLATFORM);
        }
        other => panic!("expected TagPlatformUnknown, got {other:?}"),
    }
    assert_eq!(
        store.requests(),
        vec!["GET v1".to_string()],
        "a refused join reads the tag and writes nothing"
    );
    assert!(
        store.blob_digests().is_empty(),
        "the refusal comes before any blob upload"
    );
    assert_eq!(store.stored("v1"), earlier, "the tag is left as it was");
}

#[test]
fn platform_push_to_a_digest_reference_is_refused_before_any_request() {
    let store = crate::oci::test_helpers::ManifestStore::new("test/acc");
    let module_dir = create_test_module_dir();
    let artifact = store.artifact("sha256:0123456789abcdef");

    let err = push_module(module_dir.path(), &artifact, Some("linux/arm64"), None)
        .err()
        .expect("a digest cannot be re-pointed");

    assert!(
        matches!(&err, OciError::PushToDigest { reference } if *reference == artifact),
        "expected PushToDigest naming {artifact}, got {err:?}"
    );
    assert!(store.requests().is_empty(), "{:?}", store.requests());
}

/// A push given no platform names this host, and joins the tag like any
/// other platform: both verbs behave the same.
#[test]
fn push_without_a_platform_joins_as_the_host_platform() {
    let store = crate::oci::test_helpers::ManifestStore::new("test/acc");
    let earlier = store.seed("v1", &earlier_manifest(Some("plan9/mips")));
    let module_dir = create_test_module_dir();
    let host = crate::oci::current_platform();
    let host_tag = format!("v1-{}", host.replace('/', "-"));

    let outcome = push_module(module_dir.path(), &store.artifact("v1"), None, None)
        .expect("push with no platform");

    assert_eq!(
        store.requests(),
        vec![
            "GET v1".to_string(),
            format!("PUT {host_tag} {MANIFEST_PUT}"),
            format!("PUT v1 {INDEX_PUT}"),
        ]
    );
    let index_bytes = store.stored("v1");
    let platforms: Vec<String> = json_of(&index_bytes)["manifests"]
        .as_array()
        .expect("an index")
        .iter()
        .map(|e| {
            format!(
                "{}/{}",
                e["platform"]["os"].as_str().unwrap(),
                e["platform"]["architecture"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(platforms, vec!["plan9/mips".to_string(), host]);
    assert_eq!(
        json_of(&index_bytes)["manifests"][0]["digest"],
        crate::sha256_digest(&earlier)
    );
    assert_eq!(
        outcome.index_digest,
        Some(crate::sha256_digest(&index_bytes))
    );
}

#[test]
fn the_push_row_names_the_index_only_when_one_was_written() {
    let store = crate::oci::test_helpers::ManifestStore::new("test/acc");
    let module_dir = create_test_module_dir();
    let (printer, cap) = crate::output::Printer::for_test_doc();

    let first = push_module(
        module_dir.path(),
        &store.artifact("v1"),
        Some("linux/amd64"),
        Some(&printer),
    )
    .expect("first platform");
    let second = push_module(
        module_dir.path(),
        &store.artifact("v1"),
        Some("linux/arm64"),
        Some(&printer),
    )
    .expect("second platform");
    drop(printer);

    let rendered = cap.human();
    let index = second.index_digest.expect("the second push wrote an index");
    let first_row = rendered
        .lines()
        .find(|l| l.contains(&first.digest))
        .unwrap_or_else(|| panic!("no row names {}: {rendered}", first.digest));
    assert!(
        first_row.contains("(linux/amd64)") && !first_row.contains("index"),
        "{first_row}"
    );
    assert!(
        rendered.contains(&format!("{} (linux/arm64), index {index}", second.digest)),
        "{rendered}"
    );
}

/// Blob-upload mocks for `repo`, each expected `uploads` times.
fn blob_upload_mocks(
    server: &mut mockito::ServerGuard,
    repo: &str,
    uploads: usize,
) -> Vec<mockito::Mock> {
    let location = format!("{}/v2/{repo}/blobs/uploads/upload-id", server.url());
    vec![
        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(format!(r"^/v2/{repo}/blobs/sha256:")),
            )
            .with_status(404)
            .expect(uploads)
            .create(),
        server
            .mock("POST", format!("/v2/{repo}/blobs/uploads/").as_str())
            .with_status(202)
            .with_header("Location", &location)
            .expect(uploads)
            .create(),
        server
            .mock(
                "PUT",
                mockito::Matcher::Regex(format!(r"^/v2/{repo}/blobs/uploads/upload-id\?digest=")),
            )
            .with_status(201)
            .expect(uploads)
            .create(),
    ]
}

/// A tag read failing with `status` stops the push before any blob or
/// manifest is written.
fn assert_tag_read_failure_writes_nothing(status: usize) {
    let mut server = mockito::Server::new();
    let registry = registry_from_url(&server.url());
    let repo = "test/tagfail";
    let blobs = blob_upload_mocks(&mut server, repo, 0);
    let tag_read = server
        .mock("GET", format!("/v2/{repo}/manifests/v1").as_str())
        .with_status(status)
        .expect(1)
        .create();
    let manifest_put = server
        .mock(
            "PUT",
            mockito::Matcher::Regex(format!(r"^/v2/{repo}/manifests/")),
        )
        .with_status(201)
        .expect(0)
        .create();

    let dir = create_test_module_dir();
    let result = push_module(
        dir.path(),
        &format!("{registry}/{repo}:v1"),
        Some("linux/amd64"),
        None,
    );
    assert!(
        result.is_err(),
        "a {status} reading the tag must fail the push"
    );
    tag_read.assert();
    manifest_put.assert();
    for mock in blobs {
        mock.assert();
    }
}

#[test]
fn push_fails_without_writing_when_the_tag_read_is_a_server_error() {
    assert_tag_read_failure_writes_nothing(500);
}

#[test]
fn push_fails_without_writing_when_the_tag_read_is_forbidden() {
    assert_tag_read_failure_writes_nothing(403);
}

#[test]
fn push_answers_a_bearer_challenge_on_the_tag_read_and_takes_an_absent_tag() {
    let mut server = mockito::Server::new();
    let registry = registry_from_url(&server.url());
    let repo = "test/tagauth";
    let blobs = blob_upload_mocks(&mut server, repo, 2);
    let challenge = format!(
        r#"Bearer realm="{}/auth/token",service="registry.test",scope="repository:{repo}:pull,push""#,
        server.url()
    );
    let token = server
        .mock("GET", mockito::Matcher::Regex(r"^/auth/token".to_string()))
        .with_status(200)
        .with_body(r#"{"token":"tok"}"#)
        .expect(1)
        .create();
    let unauthorized = server
        .mock("GET", format!("/v2/{repo}/manifests/v1").as_str())
        .match_header("authorization", mockito::Matcher::Missing)
        .with_status(401)
        .with_header("Www-Authenticate", &challenge)
        .expect(1)
        .create();
    let absent = server
        .mock("GET", format!("/v2/{repo}/manifests/v1").as_str())
        .match_header("authorization", "Bearer tok")
        .with_status(404)
        .expect(1)
        .create();
    server
        .mock(
            "PUT",
            mockito::Matcher::Regex(format!(r"^/v2/{repo}/manifests/v1-")),
        )
        .with_status(201)
        .create();
    let tag_put = server
        .mock("PUT", format!("/v2/{repo}/manifests/v1").as_str())
        .match_header("content-type", MEDIA_TYPE_OCI_MANIFEST)
        .with_status(201)
        .expect(1)
        .create();

    let dir = create_test_module_dir();
    let outcome = push_module(
        dir.path(),
        &format!("{registry}/{repo}:v1"),
        Some("linux/amd64"),
        None,
    )
    .expect("an absent tag behind a challenge is pushed");
    assert_eq!(outcome.index_digest, None);
    token.assert();
    unauthorized.assert();
    absent.assert();
    tag_put.assert();
    for mock in blobs {
        mock.assert();
    }
}

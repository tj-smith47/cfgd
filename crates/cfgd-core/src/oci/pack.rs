// Pack a host directory into a standard, containerd-mountable OCI image and
// push it to a registry. Intended for `cfgd image pack` — unlike push_module,
// which uses cfgd-custom media types, pack_image emits the standard OCI image
// config / layer media types so the result is mountable as a Kubernetes
// volume.image.

use std::collections::HashMap;
use std::path::Path;

use crate::errors::OciError;
use crate::output::Printer;
use crate::sha256_digest;

use super::archive::create_tar_gz_with_diff_id;
use super::auth::RegistryAuth;
use super::transport::{authenticated_request, resolve_pushed_digest, upload_blob};
use super::{
    ImageConfig, ImageRuntimeConfig, MEDIA_TYPE_OCI_IMAGE_CONFIG, MEDIA_TYPE_OCI_IMAGE_LAYER,
    MEDIA_TYPE_OCI_MANIFEST, OciDescriptor, OciManifest, OciReference, RootFs,
};

/// User-supplied overrides for the packed image's runtime config and metadata.
///
/// Every field is optional with a sane default; an empty `PackOptions` yields a
/// valid scratch image whose only purpose is to be mounted as a volume.image.
#[derive(Debug, Default, Clone)]
pub struct PackOptions {
    pub entrypoint: Option<Vec<String>>,
    pub cmd: Option<Vec<String>>,
    /// `"KEY=VALUE"` entries forwarded to the image's runtime config.
    pub env: Vec<String>,
    pub working_dir: Option<String>,
    pub user: Option<String>,
    pub labels: std::collections::BTreeMap<String, String>,
    /// OCI manifest annotations merged with the auto-generated
    /// `org.opencontainers.image.created` entry.
    pub annotations: std::collections::BTreeMap<String, String>,
    /// Target platform in `"os/arch"` form (e.g. `"linux/amd64"`). Defaults to
    /// the current host platform when absent.
    pub platform: Option<String>,
}

// ---------------------------------------------------------------------------
// Public builder helpers — extracted so tests can assert on the pure data
// without needing a full HTTP round-trip.
// ---------------------------------------------------------------------------

/// Build an [`ImageConfig`] from `PackOptions` and the layer's diff-id.
///
/// Extracted for unit-testability: the returned struct's fields can be
/// asserted directly without standing up a mock registry.
pub(super) fn build_image_config(
    opts: &PackOptions,
    diff_id: String,
    os: &str,
    arch: &str,
) -> ImageConfig {
    // Runtime config is Some only when at least one field is populated.
    let runtime_config = {
        let has_any = opts.entrypoint.is_some()
            || opts.cmd.is_some()
            || !opts.env.is_empty()
            || opts.working_dir.is_some()
            || opts.user.is_some()
            || !opts.labels.is_empty();

        if has_any {
            Some(ImageRuntimeConfig {
                entrypoint: opts.entrypoint.clone(),
                cmd: opts.cmd.clone(),
                env: if opts.env.is_empty() {
                    None
                } else {
                    Some(opts.env.clone())
                },
                working_dir: opts.working_dir.clone(),
                user: opts.user.clone(),
                labels: if opts.labels.is_empty() {
                    None
                } else {
                    Some(opts.labels.clone())
                },
            })
        } else {
            None
        }
    };

    ImageConfig {
        architecture: arch.to_string(),
        os: os.to_string(),
        created: Some(crate::utc_now_iso8601()),
        config: runtime_config,
        rootfs: RootFs {
            fs_type: "layers".to_string(),
            diff_ids: vec![diff_id],
        },
    }
}

/// Build an [`OciManifest`] from pre-uploaded descriptors.
///
/// The `org.opencontainers.image.created` annotation is always injected;
/// caller-supplied `annotations` from `PackOptions` are merged on top.
pub(super) fn build_image_manifest(
    config_digest: String,
    config_size: u64,
    layer_digest: String,
    layer_size: u64,
    opts: &PackOptions,
) -> OciManifest {
    let mut annotations: HashMap<String, String> = opts
        .annotations
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    annotations
        .entry("org.opencontainers.image.created".to_string())
        .or_insert_with(crate::utc_now_iso8601);

    OciManifest {
        schema_version: 2,
        media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
        config: OciDescriptor {
            media_type: MEDIA_TYPE_OCI_IMAGE_CONFIG.to_string(),
            digest: config_digest,
            size: config_size,
            annotations: HashMap::new(),
        },
        layers: vec![OciDescriptor {
            media_type: MEDIA_TYPE_OCI_IMAGE_LAYER.to_string(),
            digest: layer_digest,
            size: layer_size,
            annotations: HashMap::new(),
        }],
        annotations,
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// The result of a successful [`pack_image`] call.
///
/// Carries the pushed manifest digest and the resolved platform so callers
/// report ground truth rather than re-deriving the platform independently.
pub struct PackOutcome {
    /// OCI manifest digest (`"sha256:..."`).
    pub digest: String,
    /// Resolved platform in `"os/arch"` form (e.g. `"linux/amd64"`).
    pub platform: String,
}

/// Pack `dir` into a standard OCI image and push it to `artifact_ref`.
///
/// The produced image uses standard OCI media types, making it mountable as a
/// Kubernetes `volume.image` via containerd's image volume support. Returns
/// a [`PackOutcome`] containing the pushed manifest digest and the resolved
/// platform string so callers report ground truth.
pub fn pack_image(
    dir: &Path,
    artifact_ref: &str,
    opts: &PackOptions,
    printer: Option<&Printer>,
) -> Result<PackOutcome, OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = crate::http::http_agent(crate::http::HTTP_OCI_TIMEOUT);

    let spinner = printer.map(|p| p.spinner(format!("Packing image to {artifact_ref}...")));

    // Resolve platform — split opts.platform or fall back to host platform.
    let (os, arch) = resolve_platform(opts)?;

    // Build the layer archive and compute its diff_id (sha256 of uncompressed tar).
    let (layer_gz, diff_id) = create_tar_gz_with_diff_id(dir)?;
    let layer_digest = sha256_digest(&layer_gz);
    let layer_size = layer_gz.len() as u64;

    // Build and serialize the image config blob.
    let image_config = build_image_config(opts, diff_id, &os, &arch);
    let config_blob = serde_json::to_vec(&image_config)?;
    let config_digest = sha256_digest(&config_blob);
    let config_size = config_blob.len() as u64;

    // Upload both blobs (HEAD-exists check is inside upload_blob).
    upload_blob(
        &agent,
        &oci_ref,
        auth.as_ref(),
        &config_blob,
        MEDIA_TYPE_OCI_IMAGE_CONFIG,
    )?;
    upload_blob(
        &agent,
        &oci_ref,
        auth.as_ref(),
        &layer_gz,
        MEDIA_TYPE_OCI_IMAGE_LAYER,
    )?;

    // Build and push the manifest.
    let manifest = build_image_manifest(config_digest, config_size, layer_digest, layer_size, opts);
    let manifest_json = serde_json::to_vec(&manifest)?;

    let manifest_url = format!(
        "{}/{}/manifests/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        oci_ref.reference_str(),
    );

    let manifest_resp = authenticated_request(
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

    let manifest_digest = resolve_pushed_digest(&manifest_resp, &manifest_json);

    if let Some(s) = spinner {
        let _ = s.finish_ok(format!("Packed image to {artifact_ref}"));
    }

    tracing::info!(
        reference = %oci_ref,
        digest = %manifest_digest,
        "image packed and pushed"
    );

    Ok(PackOutcome {
        digest: manifest_digest,
        platform: format!("{os}/{arch}"),
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve the (os, arch) pair from `PackOptions.platform` or the host.
fn resolve_platform(opts: &PackOptions) -> Result<(String, String), OciError> {
    match &opts.platform {
        Some(p) => {
            let (os, arch) = super::parse_platform_target(p)?;
            Ok((os.to_string(), arch.to_string()))
        }
        None => {
            let os = std::env::consts::OS.to_string();
            let arch = super::rust_arch_to_oci(std::env::consts::ARCH).to_string();
            Ok((os, arch))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::test_helpers::registry_from_url;

    /// Create a temp directory with a couple of regular files for pack tests.
    fn create_test_pack_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yaml"), "key: value\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Test image\n").unwrap();
        dir
    }

    // -----------------------------------------------------------------------
    // Builder unit tests — assert on pure data, no HTTP involved.
    // -----------------------------------------------------------------------

    #[test]
    fn build_image_config_uses_standard_media_types_in_rootfs() {
        let diff_id = "sha256:aabbcc".to_string();
        let config = build_image_config(&PackOptions::default(), diff_id.clone(), "linux", "amd64");
        assert_eq!(config.os, "linux");
        assert_eq!(config.architecture, "amd64");
        assert_eq!(config.rootfs.fs_type, "layers");
        assert_eq!(config.rootfs.diff_ids, vec![diff_id]);
        assert!(config.created.is_some());
    }

    #[test]
    fn build_image_config_runtime_config_populated_from_opts() {
        let opts = PackOptions {
            entrypoint: Some(vec!["/app/server".into()]),
            env: vec!["PATH=/app/bin".into()],
            ..Default::default()
        };
        let config = build_image_config(&opts, "sha256:xx".into(), "linux", "amd64");
        let rc = config.config.expect("runtime config should be Some");
        assert_eq!(rc.entrypoint, Some(vec!["/app/server".into()]));
        assert_eq!(rc.env, Some(vec!["PATH=/app/bin".into()]));
        assert!(rc.cmd.is_none());
    }

    #[test]
    fn build_image_config_no_runtime_config_when_opts_empty() {
        let config = build_image_config(
            &PackOptions::default(),
            "sha256:xx".into(),
            "linux",
            "amd64",
        );
        assert!(
            config.config.is_none(),
            "runtime config must be None when no opts are set"
        );
    }

    #[test]
    fn build_image_manifest_uses_standard_media_types() {
        let manifest = build_image_manifest(
            "sha256:cfg".into(),
            64,
            "sha256:lyr".into(),
            1024,
            &PackOptions::default(),
        );
        assert_eq!(manifest.schema_version, 2);
        assert_eq!(manifest.media_type, MEDIA_TYPE_OCI_MANIFEST);
        assert_eq!(manifest.config.media_type, MEDIA_TYPE_OCI_IMAGE_CONFIG);
        assert_eq!(manifest.layers[0].media_type, MEDIA_TYPE_OCI_IMAGE_LAYER);
        assert!(
            manifest
                .annotations
                .contains_key("org.opencontainers.image.created"),
            "created annotation must be present"
        );
    }

    #[test]
    fn build_image_manifest_merges_caller_annotations() {
        let mut opts = PackOptions::default();
        opts.annotations.insert("my.org/env".into(), "prod".into());
        let manifest = build_image_manifest("sha256:c".into(), 1, "sha256:l".into(), 2, &opts);
        assert_eq!(
            manifest.annotations.get("my.org/env").map(String::as_str),
            Some("prod")
        );
        assert!(
            manifest
                .annotations
                .contains_key("org.opencontainers.image.created")
        );
    }

    #[test]
    fn build_image_manifest_config_digest_matches_input() {
        let config_digest = "sha256:deadbeef".to_string();
        let manifest = build_image_manifest(
            config_digest.clone(),
            42,
            "sha256:layer".into(),
            100,
            &PackOptions::default(),
        );
        assert_eq!(manifest.config.digest, config_digest);
        assert_eq!(manifest.config.size, 42);
        assert_eq!(manifest.layers[0].size, 100);
    }

    // -----------------------------------------------------------------------
    // Round-trip serialization — config and manifest must be valid JSON with
    // the correct OCI field names.
    // -----------------------------------------------------------------------

    #[test]
    fn image_config_serializes_with_correct_field_names() {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert(
            "org.opencontainers.image.title".to_string(),
            "demo".to_string(),
        );
        let opts = PackOptions {
            entrypoint: Some(vec!["/bin/sh".into()]),
            cmd: Some(vec!["-c".into(), "echo hi".into()]),
            env: vec!["HOME=/root".into()],
            working_dir: Some("/work".into()),
            user: Some("1000:1000".into()),
            labels,
            ..Default::default()
        };
        let config = build_image_config(&opts, "sha256:xx".into(), "linux", "arm64");
        let json: serde_json::Value = serde_json::to_value(&config).unwrap();
        assert_eq!(json["os"], "linux");
        assert_eq!(json["architecture"], "arm64");
        assert!(json["rootfs"]["diff_ids"].is_array());
        // All six runtime-config keys must serialize PascalCase per the OCI
        // Runtime Config Spec — guard every renamed field, not just two.
        assert!(json["config"]["Entrypoint"].is_array());
        assert!(json["config"]["Cmd"].is_array());
        assert!(json["config"]["Env"].is_array());
        assert_eq!(json["config"]["WorkingDir"], "/work");
        assert_eq!(json["config"]["User"], "1000:1000");
        assert_eq!(
            json["config"]["Labels"]["org.opencontainers.image.title"],
            "demo"
        );
    }

    #[test]
    fn image_manifest_serializes_camelcase() {
        let manifest = build_image_manifest(
            "sha256:c".into(),
            10,
            "sha256:l".into(),
            20,
            &PackOptions::default(),
        );
        let json: serde_json::Value = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["schemaVersion"], 2);
        assert!(json["mediaType"].is_string());
        assert!(json["config"]["mediaType"].is_string());
        assert!(json["layers"][0]["mediaType"].is_string());
    }

    // -----------------------------------------------------------------------
    // Integration: full push via mockito server.
    // -----------------------------------------------------------------------

    #[test]
    fn pack_image_pushes_standard_oci_image() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());
        let dir = create_test_pack_dir();

        // HEAD 404 for both blobs (config + layer).
        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/myorg/myimage/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .expect_at_least(2)
            .create();

        // POST to initiate blob upload.
        let upload_location = format!("{}/v2/myorg/myimage/blobs/uploads/upload-id", server.url());
        server
            .mock("POST", "/v2/myorg/myimage/blobs/uploads/")
            .with_status(202)
            .with_header("Location", &upload_location)
            .expect_at_least(2)
            .create();

        // PUT to complete each blob upload.
        server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/myorg/myimage/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
                ),
            )
            .with_status(201)
            .expect_at_least(2)
            .create();

        // Manifest PUT.
        let manifest_mock = server
            .mock("PUT", "/v2/myorg/myimage/manifests/v1")
            .with_status(201)
            .create();

        let artifact_ref = format!("{registry}/myorg/myimage:v1");
        let opts = PackOptions {
            entrypoint: Some(vec!["/app/server".into()]),
            env: vec!["PATH=/app/bin".into()],
            ..Default::default()
        };

        let outcome = pack_image(dir.path(), &artifact_ref, &opts, None);
        assert!(outcome.is_ok(), "pack_image failed: {:?}", outcome.err());

        let outcome = outcome.unwrap();
        assert!(
            outcome.digest.starts_with("sha256:"),
            "returned digest must be sha256-prefixed: {}",
            outcome.digest,
        );
        assert!(
            outcome.platform.contains('/'),
            "platform must be 'os/arch' form: {}",
            outcome.platform,
        );
        manifest_mock.assert();
    }

    #[test]
    fn pack_image_propagates_invalid_reference_error() {
        let dir = create_test_pack_dir();
        let result = pack_image(dir.path(), "", &PackOptions::default(), None);
        assert!(
            result.is_err(),
            "empty artifact_ref must error before any I/O"
        );
    }

    #[test]
    fn pack_image_descriptor_digests_match_blob_bytes() {
        // Cross-check: the config descriptor digest in the manifest must equal
        // sha256_digest of the serialized config blob, and the layer descriptor
        // digest must equal sha256_digest of the gzipped layer bytes.
        // This fails if someone ever hashes one serialization and uploads another.
        use crate::oci::archive::create_tar_gz_with_diff_id;
        use crate::sha256_digest;

        let dir = create_test_pack_dir();
        let opts = PackOptions {
            entrypoint: Some(vec!["/bin/sh".into()]),
            env: vec!["HOME=/root".into()],
            platform: Some("linux/amd64".into()),
            ..Default::default()
        };

        // Reproduce the same derivation as pack_image's internals.
        let (layer_gz, diff_id) = create_tar_gz_with_diff_id(dir.path()).unwrap();
        let expected_layer_digest = sha256_digest(&layer_gz);

        let (os, arch) = resolve_platform(&opts).unwrap();
        let image_config = build_image_config(&opts, diff_id, &os, &arch);
        let config_blob = serde_json::to_vec(&image_config).unwrap();
        let expected_config_digest = sha256_digest(&config_blob);

        // Now ask build_image_manifest to construct the manifest with those digests.
        let manifest = build_image_manifest(
            expected_config_digest.clone(),
            config_blob.len() as u64,
            expected_layer_digest.clone(),
            layer_gz.len() as u64,
            &opts,
        );

        // The manifest descriptor digests must equal the digests we computed from
        // the actual bytes — proving the manifest won't reference a blob it doesn't have.
        assert_eq!(
            manifest.config.digest, expected_config_digest,
            "manifest config.digest must equal sha256_digest(config_blob)"
        );
        assert_eq!(
            manifest.layers[0].digest, expected_layer_digest,
            "manifest layers[0].digest must equal sha256_digest(layer_gz)"
        );

        // Both must be well-formed sha256: digests.
        assert!(
            manifest.config.digest.starts_with("sha256:") && manifest.config.digest.len() == 71,
            "config digest must be sha256:<64-hex>: {}",
            manifest.config.digest,
        );
        assert!(
            manifest.layers[0].digest.starts_with("sha256:")
                && manifest.layers[0].digest.len() == 71,
            "layer digest must be sha256:<64-hex>: {}",
            manifest.layers[0].digest,
        );

        // Media types must be the standard OCI ones.
        assert_eq!(manifest.config.media_type, MEDIA_TYPE_OCI_IMAGE_CONFIG);
        assert_eq!(manifest.layers[0].media_type, MEDIA_TYPE_OCI_IMAGE_LAYER);
    }

    #[test]
    fn pack_image_wire_bytes_hash_to_manifest_descriptor_digests() {
        // Strongest double-hash proof: capture the ACTUAL bytes pushed over the
        // wire for each blob PUT and for the manifest PUT, then assert that
        // sha256(captured config body) == config.digest in the pushed manifest
        // and sha256(captured layer body) == layers[0].digest. Unlike the
        // re-derivation test above, this inspects what was really sent, so it
        // catches a regression where the hashed bytes diverge from the uploaded
        // bytes (e.g. config serialized twice with different field order).
        use std::sync::{Arc, Mutex};

        use crate::sha256_digest;

        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());
        let dir = create_test_pack_dir();

        // Captured blob-PUT bodies keyed by the digest in the PUT query string.
        let blob_bodies: Arc<Mutex<HashMap<String, Vec<u8>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        // Captured manifest-PUT body.
        let manifest_body: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));

        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/myorg/myimage/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .expect_at_least(2)
            .create();

        let upload_location = format!("{}/v2/myorg/myimage/blobs/uploads/upload-id", server.url());
        server
            .mock("POST", "/v2/myorg/myimage/blobs/uploads/")
            .with_status(202)
            .with_header("Location", &upload_location)
            .expect_at_least(2)
            .create();

        // Blob PUT: record body under the digest carried in the query string so
        // we can later match it against the config/layer descriptors.
        let blob_capture = Arc::clone(&blob_bodies);
        server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/myorg/myimage/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
                ),
            )
            .match_request(move |req| {
                let digest = req
                    .path_and_query()
                    .split("digest=")
                    .nth(1)
                    .unwrap_or("")
                    .to_string();
                if let Ok(body) = req.body() {
                    blob_capture
                        .lock()
                        .expect("blob capture lock")
                        .insert(digest, body.clone());
                }
                true
            })
            .with_status(201)
            .expect_at_least(2)
            .create();

        // Manifest PUT: record the exact JSON bytes sent.
        let manifest_capture = Arc::clone(&manifest_body);
        server
            .mock("PUT", "/v2/myorg/myimage/manifests/v1")
            .match_request(move |req| {
                if let Ok(body) = req.body() {
                    *manifest_capture.lock().expect("manifest capture lock") = Some(body.clone());
                }
                true
            })
            .with_status(201)
            .create();

        let artifact_ref = format!("{registry}/myorg/myimage:v1");
        let opts = PackOptions {
            entrypoint: Some(vec!["/app/server".into()]),
            env: vec!["PATH=/app/bin".into()],
            platform: Some("linux/amd64".into()),
            ..Default::default()
        };

        let outcome = pack_image(dir.path(), &artifact_ref, &opts, None);
        assert!(outcome.is_ok(), "pack_image failed: {:?}", outcome.err());

        // Parse the manifest that was actually pushed.
        let manifest_bytes = manifest_body
            .lock()
            .expect("manifest lock")
            .clone()
            .expect("manifest PUT must have been captured");
        let manifest: OciManifest =
            serde_json::from_slice(&manifest_bytes).expect("captured manifest must parse");

        let captured = blob_bodies.lock().expect("blob lock");

        // The config blob the registry received must hash to the config
        // descriptor digest in the pushed manifest.
        let config_body = captured
            .get(&manifest.config.digest)
            .expect("a blob PUT must carry the config digest from the manifest");
        assert_eq!(
            sha256_digest(config_body),
            manifest.config.digest,
            "sha256 of the uploaded config bytes must equal manifest config.digest"
        );
        assert_eq!(
            config_body.len() as u64,
            manifest.config.size,
            "uploaded config byte length must equal manifest config.size"
        );

        // Same for the layer blob.
        let layer_body = captured
            .get(&manifest.layers[0].digest)
            .expect("a blob PUT must carry the layer digest from the manifest");
        assert_eq!(
            sha256_digest(layer_body),
            manifest.layers[0].digest,
            "sha256 of the uploaded layer bytes must equal manifest layers[0].digest"
        );
        assert_eq!(
            layer_body.len() as u64,
            manifest.layers[0].size,
            "uploaded layer byte length must equal manifest layers[0].size"
        );
    }

    #[test]
    fn pack_image_manifest_push_500_returns_manifest_push_failed() {
        // Blob uploads succeed; the manifest PUT returns 500. pack_image must
        // surface that as OciError::ManifestPushFailed, not a generic error.
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());
        let dir = create_test_pack_dir();

        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/myorg/myimage/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .expect_at_least(2)
            .create();

        let upload_location = format!("{}/v2/myorg/myimage/blobs/uploads/upload-id", server.url());
        server
            .mock("POST", "/v2/myorg/myimage/blobs/uploads/")
            .with_status(202)
            .with_header("Location", &upload_location)
            .expect_at_least(2)
            .create();

        server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/myorg/myimage/blobs/uploads/upload-id\?digest=sha256:.*".to_string(),
                ),
            )
            .with_status(201)
            .expect_at_least(2)
            .create();

        // Manifest PUT fails server-side.
        server
            .mock("PUT", "/v2/myorg/myimage/manifests/v1")
            .with_status(500)
            .create();

        let artifact_ref = format!("{registry}/myorg/myimage:v1");
        let result = pack_image(dir.path(), &artifact_ref, &PackOptions::default(), None);

        let err = result.err().expect("manifest PUT 500 must yield Err");
        assert!(
            matches!(err, OciError::ManifestPushFailed { .. }),
            "manifest PUT 500 must yield ManifestPushFailed, got: {err:?}"
        );
    }

    #[test]
    fn pack_image_blob_upload_500_returns_blob_upload_failed() {
        // The first blob upload (config) fails its POST initiation with 500.
        // upload_blob maps that to OciError::BlobUploadFailed, which pack_image
        // propagates without ever reaching the manifest PUT.
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());
        let dir = create_test_pack_dir();

        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/myorg/myimage/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .expect_at_least(1)
            .create();

        // POST to initiate upload fails server-side.
        server
            .mock("POST", "/v2/myorg/myimage/blobs/uploads/")
            .with_status(500)
            .expect_at_least(1)
            .create();

        let artifact_ref = format!("{registry}/myorg/myimage:v1");
        let result = pack_image(dir.path(), &artifact_ref, &PackOptions::default(), None);

        let err = result.err().expect("blob POST 500 must yield Err");
        assert!(
            matches!(err, OciError::BlobUploadFailed { .. }),
            "blob POST 500 must yield BlobUploadFailed, got: {err:?}"
        );
    }

    #[test]
    fn pack_image_manifest_uses_standard_media_types() {
        // Pure builder test: serialize the manifest and assert the media type
        // strings are the canonical OCI ones (not cfgd-custom ones).
        let manifest = build_image_manifest(
            "sha256:config-digest".into(),
            512,
            "sha256:layer-digest".into(),
            4096,
            &PackOptions::default(),
        );

        assert_eq!(
            manifest.config.media_type, "application/vnd.oci.image.config.v1+json",
            "config mediaType must be the standard OCI image config type"
        );
        assert_eq!(
            manifest.layers[0].media_type, "application/vnd.oci.image.layer.v1.tar+gzip",
            "layer mediaType must be the standard OCI gzipped layer type"
        );
        assert!(
            !manifest.layers.is_empty(),
            "manifest must have at least one layer"
        );
        // Verify diff_ids would be populated (tested via config builder).
        let config = build_image_config(
            &PackOptions::default(),
            "sha256:diff-id".into(),
            "linux",
            "amd64",
        );
        assert!(
            !config.rootfs.diff_ids.is_empty(),
            "rootfs.diff_ids must be non-empty"
        );
        assert_eq!(config.rootfs.diff_ids[0], "sha256:diff-id");
    }

    #[test]
    fn resolve_platform_uses_host_when_none() {
        let opts = PackOptions::default();
        let (os, arch) = resolve_platform(&opts).unwrap();
        assert!(!os.is_empty());
        assert!(!arch.is_empty());
    }

    #[test]
    fn resolve_platform_parses_explicit_platform() {
        let opts = PackOptions {
            platform: Some("linux/arm64".into()),
            ..Default::default()
        };
        let (os, arch) = resolve_platform(&opts).unwrap();
        assert_eq!(os, "linux");
        assert_eq!(arch, "arm64");
    }

    #[test]
    fn resolve_platform_rejects_invalid_platform() {
        let opts = PackOptions {
            platform: Some("linuxarm64".into()),
            ..Default::default()
        };
        assert!(resolve_platform(&opts).is_err());
    }
}

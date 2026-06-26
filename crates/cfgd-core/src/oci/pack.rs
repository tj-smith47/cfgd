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
use super::transport::{
    authenticated_request, ensure_blob_present, fetch_blob, resolve_pushed_digest, upload_blob,
};
use super::{
    ImageConfig, ImageRuntimeConfig, MEDIA_TYPE_DOCKER_MANIFEST_LIST, MEDIA_TYPE_OCI_IMAGE_CONFIG,
    MEDIA_TYPE_OCI_IMAGE_LAYER, MEDIA_TYPE_OCI_INDEX, MEDIA_TYPE_OCI_MANIFEST, OciDescriptor,
    OciImageIndex, OciManifest, OciReference, RootFs,
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
    /// Base image reference (e.g. `"ghcr.io/org/app:v1"`) to layer the packed
    /// directory on top of. When set, the produced image is `base + new layer`:
    /// the base's layers/config are preserved and the packed directory becomes
    /// the final layer. When absent, a single-layer scratch image is produced.
    pub base: Option<String>,
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

/// Build the layered [`ImageConfig`] when packing on top of a base image.
///
/// The result is `base config` with:
/// - `created` refreshed to now,
/// - the new layer's `diff_id` **appended last** to `rootfs.diff_ids` (so the
///   diff-id order matches the manifest's layer order, base-first/new-last),
/// - the runtime config merged: `opts` overrides base for
///   entrypoint/cmd/working_dir/user; `env` is base ++ opts; `labels` are base
///   merged with opts (opts wins on key conflict).
///
/// Architecture/os are kept from the base. Callers must validate that the base
/// platform equals the resolved pack target before invoking this.
pub(super) fn build_layered_image_config(
    base: &ImageConfig,
    diff_id: String,
    opts: &PackOptions,
) -> ImageConfig {
    let mut diff_ids = base.rootfs.diff_ids.clone();
    diff_ids.push(diff_id);

    let base_rc = base.config.clone().unwrap_or_default();

    let entrypoint = opts.entrypoint.clone().or(base_rc.entrypoint);
    let cmd = opts.cmd.clone().or(base_rc.cmd);
    let working_dir = opts.working_dir.clone().or(base_rc.working_dir);
    let user = opts.user.clone().or(base_rc.user);

    let env = {
        let mut merged = base_rc.env.unwrap_or_default();
        merged.extend(opts.env.iter().cloned());
        if merged.is_empty() {
            None
        } else {
            Some(merged)
        }
    };

    let labels = {
        let mut merged = base_rc.labels.unwrap_or_default();
        for (k, v) in &opts.labels {
            merged.insert(k.clone(), v.clone());
        }
        if merged.is_empty() {
            None
        } else {
            Some(merged)
        }
    };

    let has_any = entrypoint.is_some()
        || cmd.is_some()
        || env.is_some()
        || working_dir.is_some()
        || user.is_some()
        || labels.is_some();

    let runtime_config = if has_any {
        Some(ImageRuntimeConfig {
            entrypoint,
            cmd,
            env,
            working_dir,
            user,
            labels,
        })
    } else {
        None
    };

    ImageConfig {
        architecture: base.architecture.clone(),
        os: base.os.clone(),
        created: Some(crate::utc_now_iso8601()),
        config: runtime_config,
        rootfs: RootFs {
            fs_type: base.rootfs.fs_type.clone(),
            diff_ids,
        },
    }
}

/// Build the layered [`OciManifest`]: the base manifest's layer descriptors
/// (order/media-type/size/digest preserved) followed by the new layer
/// descriptor, with the config descriptor pointing at the freshly-uploaded
/// layered config. Annotation behavior matches [`build_image_manifest`].
pub(super) fn build_layered_manifest(
    base_layers: &[OciDescriptor],
    config_digest: String,
    config_size: u64,
    new_layer_digest: String,
    new_layer_size: u64,
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

    let mut layers: Vec<OciDescriptor> = base_layers
        .iter()
        .map(|d| OciDescriptor {
            media_type: d.media_type.clone(),
            digest: d.digest.clone(),
            size: d.size,
            annotations: HashMap::new(),
        })
        .collect();
    layers.push(OciDescriptor {
        media_type: MEDIA_TYPE_OCI_IMAGE_LAYER.to_string(),
        digest: new_layer_digest,
        size: new_layer_size,
        annotations: HashMap::new(),
    });

    OciManifest {
        schema_version: 2,
        media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
        config: OciDescriptor {
            media_type: MEDIA_TYPE_OCI_IMAGE_CONFIG.to_string(),
            digest: config_digest,
            size: config_size,
            annotations: HashMap::new(),
        },
        layers,
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

    let manifest = if let Some(base_ref) = opts.base.as_deref() {
        layer_onto_base(
            &agent,
            base_ref,
            &oci_ref,
            auth.as_ref(),
            &os,
            &arch,
            &layer_gz,
            layer_digest,
            layer_size,
            diff_id,
            opts,
        )?
    } else {
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

        build_image_manifest(config_digest, config_size, layer_digest, layer_size, opts)
    };

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

/// Combined Accept header advertising every base-doc media type we can parse:
/// OCI image manifest + OCI index + Docker manifest list.
fn base_accept_header() -> String {
    format!("{MEDIA_TYPE_OCI_MANIFEST}, {MEDIA_TYPE_OCI_INDEX}, {MEDIA_TYPE_DOCKER_MANIFEST_LIST}")
}

/// GET a manifest document by reference (tag or digest) from `base`.
fn get_base_doc(
    agent: &ureq::Agent,
    base: &OciReference,
    auth: Option<&RegistryAuth>,
    reference: &str,
) -> Result<String, OciError> {
    let url = format!(
        "{}/{}/manifests/{}",
        base.api_base(),
        base.repository,
        reference
    );
    let resp = authenticated_request(
        agent,
        "GET",
        &url,
        auth,
        Some(&base_accept_header()),
        None,
        None,
    )
    .map_err(|e| OciError::ManifestNotFound {
        reference: format!("{base}: {e}"),
    })?;
    resp.into_body()
        .read_to_string()
        .map_err(|e| OciError::RequestFailed {
            message: format!("cannot read base manifest body: {e}"),
        })
}

/// Resolve the base reference to a concrete single-platform [`OciManifest`],
/// following an image index / manifest list when necessary to select the entry
/// matching `(os, arch)`.
fn resolve_base_manifest(
    agent: &ureq::Agent,
    base: &OciReference,
    auth: Option<&RegistryAuth>,
    os: &str,
    arch: &str,
) -> Result<OciManifest, OciError> {
    let body = get_base_doc(agent, base, auth, base.reference_str())?;

    // An index has a `manifests` array (and an index `mediaType`); an image
    // manifest has `config` + `layers`. Branch on the presence of `manifests`
    // so we tolerate registries that omit/abbreviate `mediaType`.
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| OciError::RequestFailed {
            message: format!("invalid base manifest JSON: {e}"),
        })?;

    let is_index = value
        .get("mediaType")
        .and_then(|m| m.as_str())
        .map(|m| m == MEDIA_TYPE_OCI_INDEX || m == MEDIA_TYPE_DOCKER_MANIFEST_LIST)
        .unwrap_or(false)
        || value.get("manifests").is_some();

    if is_index {
        let index: OciImageIndex =
            serde_json::from_str(&body).map_err(|e| OciError::RequestFailed {
                message: format!("invalid base image index JSON: {e}"),
            })?;
        let entry = index
            .manifests
            .iter()
            .find(|m| {
                m.platform
                    .as_ref()
                    .map(|p| p.os == os && p.architecture == arch)
                    .unwrap_or(false)
            })
            .ok_or_else(|| OciError::RequestFailed {
                message: format!("base image index has no manifest for {os}/{arch}"),
            })?;
        let manifest_body = get_base_doc(agent, base, auth, &entry.digest)?;
        serde_json::from_str(&manifest_body).map_err(|e| OciError::RequestFailed {
            message: format!("invalid base platform manifest JSON: {e}"),
        })
    } else {
        serde_json::from_str(&body).map_err(|e| OciError::RequestFailed {
            message: format!("invalid base manifest JSON: {e}"),
        })
    }
}

/// Pack `layer_gz` as a new top layer on top of the resolved `base_ref` image,
/// uploading everything into the target `oci_ref` repository and returning the
/// layered manifest ready to PUT.
///
/// Steps: resolve base manifest (following an index for `(os, arch)`) → fetch +
/// parse base config → assert base platform matches the resolved target → build
/// the layered config (base config + appended diff-id + merged runtime config) →
/// ensure every base layer blob is present in the target repo → upload the new
/// config + new layer → assemble the layered manifest.
#[allow(clippy::too_many_arguments)]
fn layer_onto_base(
    agent: &ureq::Agent,
    base_ref: &str,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    os: &str,
    arch: &str,
    layer_gz: &[u8],
    layer_digest: String,
    layer_size: u64,
    diff_id: String,
    opts: &PackOptions,
) -> Result<OciManifest, OciError> {
    let base = OciReference::parse(base_ref)?;
    let base_auth = RegistryAuth::resolve(&base.registry);

    let base_manifest = resolve_base_manifest(agent, &base, base_auth.as_ref(), os, arch)?;

    // Fetch + parse the base image config.
    let base_config_bytes = fetch_blob(
        agent,
        &base,
        base_auth.as_ref(),
        &base_manifest.config.digest,
    )?;
    let base_config: ImageConfig =
        serde_json::from_slice(&base_config_bytes).map_err(|e| OciError::RequestFailed {
            message: format!("invalid base image config JSON: {e}"),
        })?;

    // The base config's platform must equal the resolved target. If the caller
    // pinned an explicit --platform that conflicts with the base, that is a
    // correctness error: the new layer would claim a platform the base layers
    // were not built for.
    if base_config.os != os || base_config.architecture != arch {
        return Err(OciError::RequestFailed {
            message: format!(
                "base image platform {}/{} does not match requested {os}/{arch}",
                base_config.os, base_config.architecture
            ),
        });
    }

    // Build the layered config from the base.
    let layered_config = build_layered_image_config(&base_config, diff_id, opts);
    let config_blob = serde_json::to_vec(&layered_config)?;
    let config_digest = sha256_digest(&config_blob);
    let config_size = config_blob.len() as u64;

    // Ensure every base layer blob exists in the target repository (mount or copy).
    for base_layer in &base_manifest.layers {
        ensure_blob_present(
            agent,
            &base,
            oci_ref,
            base_auth.as_ref(),
            auth,
            &base_layer.digest,
            &base_layer.media_type,
        )?;
    }

    // Upload the new layered config + the new layer to the target.
    upload_blob(
        agent,
        oci_ref,
        auth,
        &config_blob,
        MEDIA_TYPE_OCI_IMAGE_CONFIG,
    )?;
    upload_blob(agent, oci_ref, auth, layer_gz, MEDIA_TYPE_OCI_IMAGE_LAYER)?;

    Ok(build_layered_manifest(
        &base_manifest.layers,
        config_digest,
        config_size,
        layer_digest,
        layer_size,
        opts,
    ))
}

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

    // -----------------------------------------------------------------------
    // Layered (base) path — pure builder unit tests.
    // -----------------------------------------------------------------------

    /// Construct a base ImageConfig with two diff_ids and explicit runtime env
    /// + labels + entrypoint, for use in layered-builder tests.
    fn base_config_with_two_layers() -> ImageConfig {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert("base.only".to_string(), "keep".to_string());
        labels.insert("shared".to_string(), "base-value".to_string());
        ImageConfig {
            architecture: "amd64".to_string(),
            os: "linux".to_string(),
            created: Some("2020-01-01T00:00:00Z".to_string()),
            config: Some(ImageRuntimeConfig {
                entrypoint: Some(vec!["/base/entry".into()]),
                cmd: Some(vec!["base-cmd".into()]),
                env: Some(vec!["BASE_ENV=1".into()]),
                working_dir: Some("/base".into()),
                user: Some("base-user".into()),
                labels: Some(labels),
            }),
            rootfs: RootFs {
                fs_type: "layers".to_string(),
                diff_ids: vec!["sha256:base-a".to_string(), "sha256:base-b".to_string()],
            },
        }
    }

    #[test]
    fn build_layered_image_config_appends_diff_id_last() {
        let base = base_config_with_two_layers();
        let result =
            build_layered_image_config(&base, "sha256:new-layer".into(), &PackOptions::default());
        // Exactly three diff_ids, base two first (in order), new one last.
        assert_eq!(
            result.rootfs.diff_ids,
            vec![
                "sha256:base-a".to_string(),
                "sha256:base-b".to_string(),
                "sha256:new-layer".to_string(),
            ],
            "diff_ids must be base-first, new-last"
        );
        // Platform inherited from base.
        assert_eq!(result.os, "linux");
        assert_eq!(result.architecture, "amd64");
        // created must be refreshed (not the base's frozen 2020 timestamp).
        assert_ne!(result.created.as_deref(), Some("2020-01-01T00:00:00Z"));
        assert!(result.created.is_some());
    }

    #[test]
    fn build_layered_image_config_merges_env_and_labels_with_opts_winning() {
        let base = base_config_with_two_layers();
        let mut opt_labels = std::collections::BTreeMap::new();
        opt_labels.insert("shared".to_string(), "opts-value".to_string());
        opt_labels.insert("opts.only".to_string(), "added".to_string());
        let opts = PackOptions {
            entrypoint: Some(vec!["/opts/entry".into()]),
            env: vec!["OPTS_ENV=2".into()],
            labels: opt_labels,
            ..Default::default()
        };
        let result = build_layered_image_config(&base, "sha256:new".into(), &opts);
        let rc = result.config.expect("runtime config must be Some");

        // env = base ++ opts, in that order.
        assert_eq!(
            rc.env,
            Some(vec!["BASE_ENV=1".to_string(), "OPTS_ENV=2".to_string()]),
            "env must be base entries followed by opts entries"
        );

        // entrypoint overridden by opts.
        assert_eq!(rc.entrypoint, Some(vec!["/opts/entry".to_string()]));
        // cmd untouched (opts had none) → base value.
        assert_eq!(rc.cmd, Some(vec!["base-cmd".to_string()]));

        // labels merged: base-only kept, shared overridden by opts, opts-only added.
        let labels = rc.labels.expect("labels must be Some");
        assert_eq!(labels.get("base.only").map(String::as_str), Some("keep"));
        assert_eq!(
            labels.get("shared").map(String::as_str),
            Some("opts-value"),
            "opts must win on label key conflict"
        );
        assert_eq!(labels.get("opts.only").map(String::as_str), Some("added"));
    }

    #[test]
    fn build_layered_manifest_appends_new_layer_last() {
        let base_layers = vec![
            OciDescriptor {
                media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_string(),
                digest: "sha256:base-layer-1".to_string(),
                size: 111,
                annotations: HashMap::new(),
            },
            OciDescriptor {
                media_type: "application/vnd.docker.image.rootfs.diff.tar.gzip".to_string(),
                digest: "sha256:base-layer-2".to_string(),
                size: 222,
                annotations: HashMap::new(),
            },
        ];
        let manifest = build_layered_manifest(
            &base_layers,
            "sha256:new-config".into(),
            64,
            "sha256:new-layer".into(),
            333,
            &PackOptions::default(),
        );

        // base_layers + 1.
        assert_eq!(manifest.layers.len(), 3, "must be base layers plus one");
        // Base layers preserved in order with original media types + sizes + digests.
        assert_eq!(manifest.layers[0].digest, "sha256:base-layer-1");
        assert_eq!(manifest.layers[0].size, 111);
        assert_eq!(
            manifest.layers[1].media_type, "application/vnd.docker.image.rootfs.diff.tar.gzip",
            "non-OCI base layer media type must be preserved verbatim"
        );
        assert_eq!(manifest.layers[1].digest, "sha256:base-layer-2");
        // New layer is LAST and uses the standard OCI layer media type.
        assert_eq!(manifest.layers[2].digest, "sha256:new-layer");
        assert_eq!(manifest.layers[2].size, 333);
        assert_eq!(manifest.layers[2].media_type, MEDIA_TYPE_OCI_IMAGE_LAYER);
        // Config descriptor points at the new layered config.
        assert_eq!(manifest.config.digest, "sha256:new-config");
        assert_eq!(manifest.config.size, 64);
        assert_eq!(manifest.config.media_type, MEDIA_TYPE_OCI_IMAGE_CONFIG);
        // created annotation injected.
        assert!(
            manifest
                .annotations
                .contains_key("org.opencontainers.image.created")
        );
    }

    // -----------------------------------------------------------------------
    // Layered (base) path — mockito integration tests.
    // -----------------------------------------------------------------------

    /// Serialize a base ImageConfig with one diff_id for a given platform.
    fn base_image_config_json(os: &str, arch: &str, diff_id: &str) -> Vec<u8> {
        let cfg = ImageConfig {
            architecture: arch.to_string(),
            os: os.to_string(),
            created: Some("2021-06-01T00:00:00Z".to_string()),
            config: None,
            rootfs: RootFs {
                fs_type: "layers".to_string(),
                diff_ids: vec![diff_id.to_string()],
            },
        };
        serde_json::to_vec(&cfg).expect("serialize base config")
    }

    #[test]
    fn pack_image_base_path_layers_onto_base_via_mount() {
        use std::sync::{Arc, Mutex};

        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());
        let dir = create_test_pack_dir();

        // --- Base image artifacts (served from /v2/base/img) ---
        let base_layer_digest =
            "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        let base_config_blob = base_image_config_json("linux", "amd64", "sha256:base-diff");
        let base_config_digest = sha256_digest(&base_config_blob);

        let base_manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_TYPE_OCI_MANIFEST,
            "config": {
                "mediaType": MEDIA_TYPE_OCI_IMAGE_CONFIG,
                "digest": base_config_digest,
                "size": base_config_blob.len(),
            },
            "layers": [{
                "mediaType": MEDIA_TYPE_OCI_IMAGE_LAYER,
                "digest": base_layer_digest,
                "size": 4096u64,
            }],
        });

        // Base manifest GET.
        server
            .mock("GET", "/v2/base/img/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_body(serde_json::to_string(&base_manifest).unwrap())
            .create();
        // Base config blob GET.
        server
            .mock(
                "GET",
                format!("/v2/base/img/blobs/{base_config_digest}").as_str(),
            )
            .with_status(200)
            .with_body(base_config_blob.clone())
            .create();

        // --- Target repo (/v2/myorg/myimage) ---
        // Base layer blob HEAD on target → 404 (not yet present).
        server
            .mock(
                "HEAD",
                format!("/v2/myorg/myimage/blobs/{base_layer_digest}").as_str(),
            )
            .with_status(404)
            .create();
        // Same-registry cross-repo mount of the base layer → 201 (mounted).
        let mount_mock = server
            .mock(
                "POST",
                mockito::Matcher::Regex(
                    r"/v2/myorg/myimage/blobs/uploads/\?mount=sha256:.*&from=base/img".to_string(),
                ),
            )
            .with_status(201)
            .create();

        // New config + new layer: HEAD 404 then POST upload-init then PUT.
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

        // Capture the final manifest PUT body.
        let manifest_body: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
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
            .with_header(
                "Docker-Content-Digest",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .create();

        let artifact_ref = format!("{registry}/myorg/myimage:v1");
        let opts = PackOptions {
            platform: Some("linux/amd64".into()),
            base: Some(format!("{registry}/base/img:v1")),
            ..Default::default()
        };

        let outcome = pack_image(dir.path(), &artifact_ref, &opts, None);
        assert!(outcome.is_ok(), "pack_image failed: {:?}", outcome.err());

        mount_mock.assert();

        // Inspect the pushed manifest.
        let bytes = manifest_body
            .lock()
            .expect("manifest lock")
            .clone()
            .expect("manifest PUT must have been captured");
        let manifest: OciManifest =
            serde_json::from_slice(&bytes).expect("captured manifest must parse");

        // base_layers (1) + new layer (1) == 2 layers.
        assert_eq!(
            manifest.layers.len(),
            2,
            "layered manifest must carry base layers + new layer"
        );
        // First layer is the preserved base layer.
        assert_eq!(manifest.layers[0].digest, base_layer_digest);
        // Last layer is the new OCI layer.
        assert_eq!(
            manifest.layers[1].media_type, MEDIA_TYPE_OCI_IMAGE_LAYER,
            "the appended (last) layer must use the OCI image layer media type"
        );
        // Config descriptor must NOT equal the base config digest (it is the new
        // layered config with an appended diff_id).
        assert_ne!(
            manifest.config.digest, base_config_digest,
            "layered config must differ from the base config"
        );
    }

    #[test]
    fn pack_image_base_index_selects_matching_platform_manifest() {
        use std::sync::{Arc, Mutex};

        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());
        let dir = create_test_pack_dir();

        // Index with two platform entries. The arm64 entry's manifest digest is
        // the one we expect pack_image to GET next.
        let amd64_manifest_digest =
            "sha256:aaaa000000000000000000000000000000000000000000000000000000000000";
        let arm64_manifest_digest =
            "sha256:bbbb000000000000000000000000000000000000000000000000000000000000";

        let index = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_TYPE_OCI_INDEX,
            "manifests": [
                {
                    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                    "digest": amd64_manifest_digest,
                    "size": 100u64,
                    "platform": {"os": "linux", "architecture": "amd64"},
                },
                {
                    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                    "digest": arm64_manifest_digest,
                    "size": 100u64,
                    "platform": {"os": "linux", "architecture": "arm64"},
                },
            ],
        });

        // arm64 platform manifest + its config.
        let base_layer_digest =
            "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        let base_config_blob = base_image_config_json("linux", "arm64", "sha256:arm-diff");
        let base_config_digest = sha256_digest(&base_config_blob);
        let arm64_manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_TYPE_OCI_MANIFEST,
            "config": {
                "mediaType": MEDIA_TYPE_OCI_IMAGE_CONFIG,
                "digest": base_config_digest,
                "size": base_config_blob.len(),
            },
            "layers": [{
                "mediaType": MEDIA_TYPE_OCI_IMAGE_LAYER,
                "digest": base_layer_digest,
                "size": 4096u64,
            }],
        });

        // GET index by tag.
        server
            .mock("GET", "/v2/base/multi/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_INDEX)
            .with_body(serde_json::to_string(&index).unwrap())
            .create();

        // The arm64 platform manifest must be the one fetched next. Capture
        // whether the arm64 digest path was requested (and assert the amd64 one
        // is NOT) by giving each digest path its own mock with an expect count.
        let arm64_hit: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let arm64_flag = Arc::clone(&arm64_hit);
        server
            .mock(
                "GET",
                format!("/v2/base/multi/manifests/{arm64_manifest_digest}").as_str(),
            )
            .match_request(move |_req| {
                *arm64_flag.lock().expect("arm64 flag lock") = true;
                true
            })
            .with_status(200)
            .with_body(serde_json::to_string(&arm64_manifest).unwrap())
            .create();
        // amd64 manifest path must never be requested (resolved platform is arm64).
        let amd64_mock = server
            .mock(
                "GET",
                format!("/v2/base/multi/manifests/{amd64_manifest_digest}").as_str(),
            )
            .with_status(200)
            .with_body("{}")
            .expect(0)
            .create();

        // Base config blob GET.
        server
            .mock(
                "GET",
                format!("/v2/base/multi/blobs/{base_config_digest}").as_str(),
            )
            .with_status(200)
            .with_body(base_config_blob.clone())
            .create();

        // Target repo: base layer HEAD 404 then mount 201, new blobs upload, manifest PUT.
        server
            .mock(
                "HEAD",
                format!("/v2/myorg/myimage/blobs/{base_layer_digest}").as_str(),
            )
            .with_status(404)
            .create();
        server
            .mock(
                "POST",
                mockito::Matcher::Regex(
                    r"/v2/myorg/myimage/blobs/uploads/\?mount=sha256:.*&from=base/multi"
                        .to_string(),
                ),
            )
            .with_status(201)
            .create();
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
        server
            .mock("PUT", "/v2/myorg/myimage/manifests/v1")
            .with_status(201)
            .create();

        let artifact_ref = format!("{registry}/myorg/myimage:v1");
        let opts = PackOptions {
            platform: Some("linux/arm64".into()),
            base: Some(format!("{registry}/base/multi:v1")),
            ..Default::default()
        };

        let outcome = pack_image(dir.path(), &artifact_ref, &opts, None);
        assert!(outcome.is_ok(), "pack_image failed: {:?}", outcome.err());

        assert!(
            *arm64_hit.lock().expect("arm64 flag lock"),
            "the arm64 platform manifest digest must have been fetched"
        );
        amd64_mock.assert();
    }

    #[test]
    fn pack_image_base_index_no_matching_platform_errors() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());
        let dir = create_test_pack_dir();

        // Index has only an amd64 entry; we request linux/arm64.
        let index = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_TYPE_OCI_INDEX,
            "manifests": [{
                "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                "digest": "sha256:cccc000000000000000000000000000000000000000000000000000000000000",
                "size": 100u64,
                "platform": {"os": "linux", "architecture": "amd64"},
            }],
        });

        server
            .mock("GET", "/v2/base/noplat/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_INDEX)
            .with_body(serde_json::to_string(&index).unwrap())
            .create();

        let artifact_ref = format!("{registry}/myorg/myimage:v1");
        let opts = PackOptions {
            platform: Some("linux/arm64".into()),
            base: Some(format!("{registry}/base/noplat:v1")),
            ..Default::default()
        };

        let result = pack_image(dir.path(), &artifact_ref, &opts, None);
        let err = result
            .err()
            .expect("missing platform in index must yield Err");
        let msg = format!("{err}");
        assert!(
            matches!(err, OciError::RequestFailed { .. }),
            "expected RequestFailed, got: {err:?}"
        );
        assert!(
            msg.contains("no manifest for linux/arm64"),
            "error must name the missing platform: {msg}"
        );
    }
}

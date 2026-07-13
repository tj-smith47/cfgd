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
        .entry(crate::OCI_ANNOTATION_CREATED.to_string())
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
        .entry(crate::OCI_ANNOTATION_CREATED.to_string())
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
mod tests;

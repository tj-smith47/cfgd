// Push: single-platform module push, multi-platform OCI index push,
// platform-target parsing and Rust→OCI arch mapping.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::errors::OciError;
use crate::output::{Printer, collapse_to_subject_line};

use super::archive::create_tar_gz;
use super::auth::RegistryAuth;
use super::pull::{ManifestDocument, read_manifest_document};
use super::transport::{
    authenticated_request, authenticated_request_if_present, resolve_pushed_digest, upload_blob,
};
use super::{
    Annotations, MEDIA_TYPE_MODULE_CONFIG, MEDIA_TYPE_MODULE_LAYER, MEDIA_TYPE_OCI_INDEX,
    MEDIA_TYPE_OCI_MANIFEST, OciDescriptor, OciManifest, OciReference, ReferenceKind,
};

/// The result of a successful [`push_module`] call.
///
/// Carries the pushed manifest digest and the resolved platform so callers
/// report ground truth rather than re-deriving the platform independently —
/// the same shape [`super::PackOutcome`] carries, for the same reason.
pub struct PushOutcome {
    /// OCI manifest digest (`"sha256:..."`).
    pub digest: String,
    /// Resolved platform in `"os/arch"` form (e.g. `"linux/amd64"`), as stamped
    /// into the manifest's `OCI_ANNOTATION_PLATFORM`.
    pub platform: String,
    /// Digest of the OCI index written at the tag when this push joined its
    /// platform to the others the tag already listed; `None` when the tag now
    /// holds this platform's manifest alone.
    pub index_digest: Option<String>,
}

/// Push a module directory as an OCI artifact.
///
/// Reads `module.yaml` from `dir`, serializes it as the config blob, and
/// tars+gzips the directory contents as a single layer. Pushes to the
/// registry specified by `artifact_ref`.
///
/// With an explicit `platform` and a tag reference, the push accumulates
/// platforms under that tag: the manifest is also tagged `<tag>-<os>-<arch>`,
/// and a tag already holding another platform's manifest, or an index of
/// several, becomes an index listing every platform with this one replacing
/// any earlier push of the same platform. A digest reference cannot be
/// re-pointed, so an explicit platform with one is refused.
///
/// Returns a [`PushOutcome`] carrying the pushed manifest digest, the
/// platform this push resolved and annotated the manifest with, and the index
/// digest when an index was written.
pub fn push_module(
    dir: &Path,
    artifact_ref: &str,
    platform: Option<&str>,
    printer: Option<&Printer>,
) -> Result<PushOutcome, OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = crate::http::http_agent(crate::http::HTTP_OCI_TIMEOUT);
    let spinner = printer.map(|p| p.spinner(format!("Pushing module to {artifact_ref}")));
    let resolved_platform = resolve_platform(platform);
    let pushed = match platform {
        Some(_) => push_platform_to_tag(&agent, dir, &oci_ref, auth.as_ref(), &resolved_platform),
        None => push_module_inner(&agent, dir, &oci_ref, auth.as_ref(), &resolved_platform)
            .map(|(digest, _size)| (digest, None)),
    };
    match pushed {
        Ok((digest, index_digest)) => {
            // The running message names the reference because the wait is the
            // only thing on screen; the settled line does not, because every
            // caller has already headed the run with the same reference. The
            // digest and the resolved platform are the row's detail: the facts
            // the push PRODUCED, so they belong to the row that produced them
            // rather than to a kv row wedged between this verdict and the
            // signing verdict after it.
            if let Some(s) = spinner {
                let _ = s
                    .finish_ok("Pushed module")
                    .detail(super::artifact_row_detail(
                        &digest,
                        &resolved_platform,
                        index_digest.as_deref(),
                    ));
            }
            Ok(PushOutcome {
                digest,
                platform: resolved_platform,
                index_digest,
            })
        }
        Err(e) => {
            if let Some(s) = spinner {
                let _ = s
                    .finish_fail(format!("Failed to push module to {artifact_ref}"))
                    .detail(collapse_to_subject_line(&e));
            }
            Err(e)
        }
    }
}

/// The platform this push annotates the manifest with: what the caller asked
/// for, or this host. The ONE place the default is applied, so the annotation
/// written and the platform reported are the same string by construction.
fn resolve_platform(platform: Option<&str>) -> String {
    platform.map(String::from).unwrap_or_else(current_platform)
}

/// Inner push logic shared by single-platform and multi-platform push.
/// Returns (manifest_digest, manifest_size_bytes).
pub(super) fn push_module_inner(
    agent: &ureq::Agent,
    dir: &Path,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    platform: &str,
) -> Result<(String, u64), OciError> {
    let manifest_json = upload_module_manifest(agent, dir, oci_ref, auth, platform)?;
    let manifest_digest = put_manifest(agent, oci_ref, auth, &manifest_json)?;
    Ok((manifest_digest, manifest_json.len() as u64))
}

/// Upload a module directory's config and layer blobs and return the bytes of
/// the image manifest naming them, annotated with `platform`. The manifest is
/// returned unsent so one set of bytes can be PUT under more than one tag.
fn upload_module_manifest(
    agent: &ureq::Agent,
    dir: &Path,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    platform: &str,
) -> Result<Vec<u8>, OciError> {
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

    // Upload config blob
    let config_digest = upload_blob(agent, oci_ref, auth, &config_blob, MEDIA_TYPE_MODULE_CONFIG)?;

    // Upload layer blob
    let layer_digest = upload_blob(agent, oci_ref, auth, &layer_data, MEDIA_TYPE_MODULE_LAYER)?;

    // Build manifest
    let mut annotations = Annotations::new();
    annotations.insert(
        crate::OCI_ANNOTATION_PLATFORM.to_string(),
        platform.to_string(),
    );
    annotations.insert(
        crate::OCI_ANNOTATION_CREATED.to_string(),
        crate::utc_now_iso8601(),
    );

    let manifest = OciManifest {
        schema_version: 2,
        media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
        config: OciDescriptor {
            media_type: MEDIA_TYPE_MODULE_CONFIG.to_string(),
            digest: config_digest,
            size: config_blob.len() as u64,
            annotations: Annotations::new(),
        },
        layers: vec![OciDescriptor {
            media_type: MEDIA_TYPE_MODULE_LAYER.to_string(),
            digest: layer_digest,
            size: layer_data.len() as u64,
            annotations: Annotations::new(),
        }],
        annotations,
    };

    Ok(serde_json::to_vec(&manifest)?)
}

/// PUT an image manifest at `oci_ref`'s reference and return the digest the
/// registry addresses it by.
fn put_manifest(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    manifest_json: &[u8],
) -> Result<String, OciError> {
    let resp = authenticated_request(
        agent,
        "PUT",
        &manifest_url(oci_ref),
        auth,
        None,
        Some(MEDIA_TYPE_OCI_MANIFEST),
        Some(manifest_json),
    )
    .map_err(|e| OciError::ManifestPushFailed {
        message: format!("{e}"),
    })?;
    let digest = resolve_pushed_digest(&resp, manifest_json);
    tracing::debug!(reference = %oci_ref, digest = %digest, "module pushed");
    Ok(digest)
}

/// PUT an OCI index at `oci_ref`'s reference and return its digest. The one
/// writer of an index for both [`push_module`] and [`push_module_multiplatform`].
fn put_index(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    index_json: &[u8],
) -> Result<String, OciError> {
    let resp = authenticated_request(
        agent,
        "PUT",
        &manifest_url(oci_ref),
        auth,
        None,
        Some(MEDIA_TYPE_OCI_INDEX),
        Some(index_json),
    )
    .map_err(|e| OciError::ManifestPushFailed {
        message: format!("index push failed: {e}"),
    })?;
    Ok(resolve_pushed_digest(&resp, index_json))
}

fn manifest_url(oci_ref: &OciReference) -> String {
    format!(
        "{}/{}/manifests/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        oci_ref.reference_str(),
    )
}

/// `oci_ref`'s repository under the per-platform tag `<tag>-<os>-<arch>`,
/// where each platform's manifest stays addressable beside the index.
fn platform_tagged(oci_ref: &OciReference, platform: &str) -> OciReference {
    OciReference {
        registry: oci_ref.registry.clone(),
        repository: oci_ref.repository.clone(),
        reference: ReferenceKind::Tag(format!(
            "{}-{}",
            oci_ref.reference_str(),
            platform.replace('/', "-")
        )),
    }
}

/// Push one platform's manifest to a tag that may already list others.
/// Returns the manifest digest and, when the tag now holds an index, the
/// index digest.
fn push_platform_to_tag(
    agent: &ureq::Agent,
    dir: &Path,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    platform: &str,
) -> Result<(String, Option<String>), OciError> {
    if matches!(oci_ref.reference, ReferenceKind::Digest(_)) {
        return Err(OciError::PlatformPushToDigest {
            reference: oci_ref.to_string(),
        });
    }
    let (os, arch) = parse_platform_target(platform)?;
    let manifest_json = upload_module_manifest(agent, dir, oci_ref, auth, platform)?;
    let digest = put_manifest(
        agent,
        &platform_tagged(oci_ref, platform),
        auth,
        &manifest_json,
    )?;
    let entry = OciPlatformManifest {
        media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
        digest,
        size: manifest_json.len() as u64,
        platform: OciPlatform {
            os: os.to_string(),
            architecture: arch.to_string(),
        },
    };

    let existing = authenticated_request_if_present(
        agent,
        "GET",
        &manifest_url(oci_ref),
        auth,
        Some(&super::manifest_accept()),
    )?
    .map(read_manifest_document)
    .transpose()?;
    let index = match existing {
        Some(existing) => index_joining(existing, &entry, oci_ref, platform)?,
        None => None,
    };

    let index_digest = match index {
        Some(index_json) => Some(put_index(agent, oci_ref, auth, &index_json)?),
        None => {
            put_manifest(agent, oci_ref, auth, &manifest_json)?;
            None
        }
    };
    Ok((entry.digest, index_digest))
}

/// The index bytes the tag should hold once `entry` joins what it holds now,
/// or `None` when the tag should hold `entry`'s manifest alone (it held a
/// manifest for the same platform).
///
/// An existing index is edited as JSON so entries and fields cfgd never
/// writes (attestation entries, annotations, a platform `variant`) survive.
fn index_joining(
    existing: ManifestDocument,
    entry: &OciPlatformManifest,
    oci_ref: &OciReference,
    platform: &str,
) -> Result<Option<Vec<u8>>, OciError> {
    let ManifestDocument {
        digest,
        size,
        mut doc,
    } = existing;

    if let Some(entries) = doc.get_mut("manifests").and_then(|m| m.as_array_mut()) {
        let same_platform = |e: &serde_json::Value| {
            e.get("platform").is_some_and(|p| {
                p.get("os").and_then(|v| v.as_str()) == Some(entry.platform.os.as_str())
                    && p.get("architecture").and_then(|v| v.as_str())
                        == Some(entry.platform.architecture.as_str())
            })
        };
        let new_entry = serde_json::to_value(entry)?;
        match entries.iter_mut().find(|e| same_platform(e)) {
            Some(slot) => *slot = new_entry,
            None => entries.push(new_entry),
        }
        doc["mediaType"] = serde_json::Value::from(MEDIA_TYPE_OCI_INDEX);
        return Ok(Some(serde_json::to_vec(&doc)?));
    }

    let Some(existing_platform) = doc
        .get("annotations")
        .and_then(|a| a.get(crate::OCI_ANNOTATION_PLATFORM))
        .and_then(|p| p.as_str())
    else {
        return Err(OciError::TagPlatformUnknown {
            reference: oci_ref.to_string(),
            annotation: crate::OCI_ANNOTATION_PLATFORM.to_string(),
        });
    };
    if existing_platform == platform {
        return Ok(None);
    }
    let (os, arch) = parse_platform_target(existing_platform)?;
    let media_type = doc
        .get("mediaType")
        .and_then(|m| m.as_str())
        .unwrap_or(MEDIA_TYPE_OCI_MANIFEST);
    let index = OciIndex {
        schema_version: 2,
        media_type: MEDIA_TYPE_OCI_INDEX.to_string(),
        manifests: vec![
            OciPlatformManifest {
                media_type: media_type.to_string(),
                digest,
                size,
                platform: OciPlatform {
                    os: os.to_string(),
                    architecture: arch.to_string(),
                },
            },
            entry.clone(),
        ],
    };
    Ok(Some(serde_json::to_vec(&index)?))
}

// ---------------------------------------------------------------------------
// Multi-platform index
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OciIndex {
    pub(super) schema_version: u32,
    pub(super) media_type: String,
    pub(super) manifests: Vec<OciPlatformManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OciPlatformManifest {
    pub(super) media_type: String,
    pub(super) digest: String,
    pub(super) size: u64,
    pub(super) platform: OciPlatform,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OciPlatform {
    pub(super) os: String,
    pub(super) architecture: String,
}

/// Map Rust arch names to OCI architecture names.
pub fn rust_arch_to_oci(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "arm",
        "s390x" => "s390x",
        "powerpc64" => "ppc64le",
        other => other,
    }
}

/// Return the current platform in OCI format (os/arch).
pub fn current_platform() -> String {
    format!(
        "{}/{}",
        std::env::consts::OS,
        rust_arch_to_oci(std::env::consts::ARCH)
    )
}

/// Parse "os/arch" (e.g. "linux/amd64") into (os, arch).
pub fn parse_platform_target(target: &str) -> Result<(&str, &str), OciError> {
    target.split_once('/').ok_or_else(|| OciError::BuildError {
        message: format!(
            "invalid platform target '{target}' — expected os/arch (e.g. linux/amd64)"
        ),
    })
}

/// Push a module for multiple platforms, creating an OCI index (manifest list).
///
/// Each `builds` entry is `(build_dir, platform)` where platform is "os/arch".
/// Pushes each platform-specific manifest, then pushes the index.
///
/// Returns the index digest alone, unlike [`push_module`]: every platform here
/// was named by the caller, so there is no defaulted value the caller would
/// otherwise have to re-derive, and an index spans platforms rather than
/// resolving one.
pub fn push_module_multiplatform(
    builds: &[(&Path, &str)],
    artifact_ref: &str,
    printer: Option<&Printer>,
) -> Result<String, OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = crate::http::http_agent(crate::http::HTTP_OCI_TIMEOUT);

    let spinner =
        printer.map(|p| p.spinner(format!("Pushing multi-platform module to {artifact_ref}")));

    let result = push_multiplatform_manifests_and_index(&agent, builds, &oci_ref, auth.as_ref());

    match &result {
        Ok(index_digest) => {
            if let Some(s) = spinner {
                let _ = s
                    .finish_ok("Pushed multi-platform module")
                    .detail(index_digest.clone());
            }
            tracing::debug!(
                reference = %oci_ref,
                digest = %index_digest,
                platforms = builds.len(),
                "multi-platform module pushed"
            );
        }
        Err(e) => {
            if let Some(s) = spinner {
                let _ = s
                    .finish_fail(format!(
                        "Failed to push multi-platform module to {artifact_ref}"
                    ))
                    .detail(collapse_to_subject_line(e));
            }
        }
    }

    result
}

/// Push each platform's manifest, then the OCI index tying them together.
/// Factored out of [`push_module_multiplatform`] so every fallible step runs
/// under one `Result` the caller can pattern-match once to drive the spinner.
fn push_multiplatform_manifests_and_index(
    agent: &ureq::Agent,
    builds: &[(&Path, &str)],
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
) -> Result<String, OciError> {
    let mut platform_manifests = Vec::new();

    for (dir, platform) in builds {
        let (os, arch) = parse_platform_target(platform)?;

        let (digest, size) = push_module_inner(
            agent,
            dir,
            &platform_tagged(oci_ref, platform),
            auth,
            platform,
        )?;

        platform_manifests.push(OciPlatformManifest {
            media_type: MEDIA_TYPE_OCI_MANIFEST.to_string(),
            digest,
            size,
            platform: OciPlatform {
                os: os.to_string(),
                architecture: arch.to_string(),
            },
        });
    }

    // Build and push the index
    let index = OciIndex {
        schema_version: 2,
        media_type: MEDIA_TYPE_OCI_INDEX.to_string(),
        manifests: platform_manifests,
    };
    put_index(agent, oci_ref, auth, &serde_json::to_vec(&index)?)
}

#[cfg(test)]
mod tests;

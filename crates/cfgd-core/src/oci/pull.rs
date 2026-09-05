// Pull: download OCI module artifact, verify layer digest, extract to disk.
// Optional cosign signature verification (real cryptographic check).

use std::io::Read;
use std::path::Path;

use crate::PathDisplayExt;
use crate::errors::OciError;
use crate::output::{Printer, collapse_to_subject_line};
use crate::sha256_digest;

use super::archive::extract_tar_gz;
use super::auth::RegistryAuth;
use super::sign::{VerifyOptions, verify_signature};
use super::transport::{authenticated_request, response_digest};
use super::{MEDIA_TYPE_OCI_MANIFEST, OciManifest, OciReference};

/// Policy for verifying a module artifact's cosign signature during pull.
///
/// - `None` — skip signature verification entirely (default).
/// - `RequireKey { path }` — fail unless `cosign verify --key <path>` succeeds.
/// - `RequireKeyless { identity, issuer }` — fail unless keyless verification
///   matches the supplied certificate identity / OIDC issuer constraints.
#[derive(Debug, Clone)]
pub enum SignaturePolicy<'a> {
    None,
    RequireKey {
        path: &'a str,
    },
    RequireKeyless {
        identity: Option<&'a str>,
        issuer: Option<&'a str>,
    },
}

impl SignaturePolicy<'_> {
    fn requires_signature(&self) -> bool {
        !matches!(self, SignaturePolicy::None)
    }
}

/// Pull a module from an OCI registry and extract it to `output_dir`.
///
/// `signature_policy` controls cryptographic signature verification:
/// - `SignaturePolicy::None` — no verification (default).
/// - `SignaturePolicy::RequireKey { path }` — run real `cosign verify --key`,
///   fail the pull if it does not succeed.
/// - `SignaturePolicy::RequireKeyless { identity, issuer }` — run real
///   keyless verification with the supplied constraints, fail the pull if it
///   does not succeed.
///
/// Prior to v0.4.0 this took a `bool` and only checked for the *presence* of
/// a signature manifest (HEAD on `<tag>.sig`) — a TOFU sentinel an attacker
/// who could push to the registry could trivially satisfy. The current API
/// requires callers to supply the verifying key (or identity/issuer) so the
/// trust decision is explicit and cryptographically enforced.
pub fn pull_module(
    artifact_ref: &str,
    output_dir: &Path,
    signature_policy: SignaturePolicy<'_>,
    printer: Option<&Printer>,
) -> Result<(), OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = crate::http::http_agent(crate::http::HTTP_OCI_TIMEOUT);

    let spinner = printer.map(|p| p.spinner(format!("Pulling module from {artifact_ref}")));

    match pull_module_inner(
        &agent,
        &oci_ref,
        auth.as_ref(),
        output_dir,
        &signature_policy,
        artifact_ref,
    ) {
        Ok(()) => {
            // Settled without the reference: the caller's header block names
            // it, and the running message above already carried it while the
            // wait was the only thing on screen.
            if let Some(s) = spinner {
                let _ = s.finish_ok("Pulled module");
            }
            tracing::debug!(
                reference = %oci_ref,
                output = %output_dir.posix(),
                "module pulled"
            );
            Ok(())
        }
        Err(e) => {
            if let Some(s) = spinner {
                let _ = s
                    .finish_fail(format!("Failed to pull module from {artifact_ref}"))
                    .detail(collapse_to_subject_line(&e));
            }
            Err(e)
        }
    }
}

/// What an already-pushed artifact says about itself, read straight off the
/// manifest documents beside it — no blob is downloaded, every answer here
/// being a label rather than content.
///
/// The two halves travel together because they come from one read: the
/// subject's manifest carries the platforms AND resolves the digest the
/// attestation tag is named after, so asking for both costs one request more
/// than asking for either.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactFacts {
    /// The `os/arch` pairs the artifact declares, in the order it declares them.
    pub platforms: Vec<String>,
    /// The attestation types cosign has attached to it, named in the
    /// vocabulary `cosign verify-attestation --type` takes.
    pub attestations: Vec<String>,
}

/// Read an artifact's declared facts from its registry.
///
/// A registry that answers the subject manifest is reachable, so only that
/// read is fallible here: everything after it is a label the artifact either
/// carries or does not.
pub fn artifact_facts(artifact_ref: &str) -> Result<ArtifactFacts, OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = crate::http::http_agent(crate::http::HTTP_OCI_TIMEOUT);

    let (digest, doc) =
        fetch_manifest_document(&agent, &oci_ref, auth.as_ref(), oci_ref.reference_str()).map_err(
            |e| OciError::ManifestNotFound {
                reference: format!("{oci_ref}: {e}"),
            },
        )?;

    Ok(ArtifactFacts {
        platforms: declared_platforms(&doc),
        attestations: attached_attestations(&agent, &oci_ref, auth.as_ref(), &digest),
    })
}

/// GET one manifest, answering both the digest the registry addresses it by
/// and its parsed document.
///
/// The digest comes from the registry's own `Docker-Content-Digest` header
/// whenever it sends one, because a registry that re-canonicalizes a manifest
/// stores it under a digest the received bytes do not hash to — and the
/// cosign tag derived from it has to be the one cosign pushed.
fn fetch_manifest_document(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    reference: &str,
) -> Result<(String, serde_json::Value), OciError> {
    let url = format!(
        "{}/{}/manifests/{reference}",
        oci_ref.api_base(),
        oci_ref.repository,
    );
    let accept = format!(
        "{MEDIA_TYPE_OCI_MANIFEST}, {}, {}",
        super::MEDIA_TYPE_OCI_INDEX,
        super::MEDIA_TYPE_DOCKER_MANIFEST_LIST
    );
    let resp = authenticated_request(agent, "GET", &url, auth, Some(&accept), None, None)?;
    let header_digest = response_digest(&resp);
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| OciError::RequestFailed {
            message: format!("cannot read manifest body: {e}"),
        })?;
    let doc: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| OciError::RequestFailed {
            message: format!("invalid manifest JSON: {e}"),
        })?;
    Ok((
        header_digest.unwrap_or_else(|| sha256_digest(body.as_bytes())),
        doc,
    ))
}

/// The platforms a manifest document declares.
///
/// The two shapes [`super::push_module`] and [`super::push_module_multiplatform`]
/// write are both read here: an index names an `os`/`architecture` pair per
/// entry, and a single-platform manifest carries the whole `os/arch` string in
/// its [`crate::OCI_ANNOTATION_PLATFORM`] annotation. An artifact declaring
/// neither answers an empty list rather than an error — a manifest a third
/// party pushed is a legitimate artifact that simply says nothing about its
/// platform.
fn declared_platforms(doc: &serde_json::Value) -> Vec<String> {
    // Branch on the presence of `manifests` rather than on `mediaType`, so a
    // registry that omits or abbreviates the type is still read correctly —
    // the same test `oci::pack` applies to a base image.
    if let Some(entries) = doc.get("manifests").and_then(|m| m.as_array()) {
        // An index lists its entries in the order the pusher wrote them, which
        // is the order the column reads best in, so duplicates are dropped by
        // first sighting rather than by sorting.
        let mut seen = std::collections::HashSet::new();
        return entries
            .iter()
            .filter_map(|entry| {
                let platform = entry.get("platform")?;
                let os = platform.get("os")?.as_str()?;
                let arch = platform.get("architecture")?.as_str()?;
                Some(format!("{os}/{arch}"))
            })
            .filter(|p| seen.insert(p.clone()))
            .collect();
    }

    doc.get("annotations")
        .and_then(|a| a.get(crate::OCI_ANNOTATION_PLATFORM))
        .and_then(|p| p.as_str())
        .map(|p| vec![p.to_string()])
        .unwrap_or_default()
}

/// The attestation types cosign has attached to the artifact at `digest`.
///
/// `cosign attest` pushes its DSSE envelopes as an ordinary manifest tagged
/// `sha256-<hex>.att` beside the subject, one layer per attestation, each
/// annotated with the predicate type it carries. An artifact nobody attested
/// has no such tag and the registry says so — which is an answer, not a
/// failure, and the reason this half is infallible: the subject manifest was
/// already fetched, so the registry has proven itself reachable and readable.
fn attached_attestations(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    digest: &str,
) -> Vec<String> {
    let url = format!(
        "{}/{}/manifests/{}.att",
        oci_ref.api_base(),
        oci_ref.repository,
        digest.replace(':', "-"),
    );
    let Ok(resp) = authenticated_request(
        agent,
        "GET",
        &url,
        auth,
        Some(MEDIA_TYPE_OCI_MANIFEST),
        None,
        None,
    ) else {
        return Vec::new();
    };
    let Ok(body) = resp.into_body().read_to_string() else {
        return Vec::new();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&body) else {
        return Vec::new();
    };
    let Some(layers) = doc.get("layers").and_then(|l| l.as_array()) else {
        return Vec::new();
    };

    // Two attestations of one type are one type; first sighting keeps the
    // order cosign attached them in, oldest first.
    let mut seen = std::collections::HashSet::new();
    layers
        .iter()
        .filter_map(|layer| {
            let predicate = layer.get("annotations")?.get("predicateType")?.as_str()?;
            Some(super::sign::attestation_type_name(predicate))
        })
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// The fallible half of [`pull_module`]: every step from signature
/// verification through extraction runs under one `Result` the caller
/// matches once, rather than an early `?` abandoning the spinner mid-pull.
fn pull_module_inner(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    output_dir: &Path,
    signature_policy: &SignaturePolicy<'_>,
    artifact_ref: &str,
) -> Result<(), OciError> {
    if signature_policy.requires_signature() {
        let opts = match signature_policy {
            SignaturePolicy::None => unreachable!("guarded by requires_signature()"),
            SignaturePolicy::RequireKey { path } => VerifyOptions {
                key: Some(path),
                identity: None,
                issuer: None,
            },
            SignaturePolicy::RequireKeyless { identity, issuer } => VerifyOptions {
                key: None,
                identity: *identity,
                issuer: *issuer,
            },
        };
        verify_signature(artifact_ref, &opts)?;
    }

    // Pull manifest
    let manifest_url = format!(
        "{}/{}/manifests/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        oci_ref.reference_str(),
    );

    let resp = authenticated_request(
        agent,
        "GET",
        &manifest_url,
        auth,
        Some(MEDIA_TYPE_OCI_MANIFEST),
        None,
        None,
    )
    .map_err(|e| OciError::ManifestNotFound {
        reference: format!("{}: {e}", oci_ref),
    })?;

    let manifest_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| OciError::RequestFailed {
            message: format!("cannot read manifest body: {e}"),
        })?;
    let manifest: OciManifest =
        serde_json::from_str(&manifest_body).map_err(|e| OciError::RequestFailed {
            message: format!("invalid manifest JSON: {e}"),
        })?;

    // Find our layer
    let layer = manifest
        .layers
        .first()
        .ok_or_else(|| OciError::RequestFailed {
            message: "manifest has no layers".to_string(),
        })?;

    // Download layer blob
    let blob_url = format!(
        "{}/{}/blobs/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        layer.digest,
    );

    let resp = authenticated_request(
        agent,
        "GET",
        &blob_url,
        auth,
        Some("application/octet-stream"),
        None,
        None,
    )
    .map_err(|e| OciError::BlobNotFound {
        digest: format!("{}: {e}", layer.digest),
    })?;

    // Read blob data (cap at 512 MB to prevent OOM from malicious manifests)
    const MAX_BLOB_SIZE: u64 = 512 * 1024 * 1024;
    if layer.size > MAX_BLOB_SIZE {
        return Err(OciError::RequestFailed {
            message: format!(
                "layer size {} exceeds maximum allowed size ({} bytes)",
                layer.size, MAX_BLOB_SIZE
            ),
        });
    }
    let mut blob_data = Vec::with_capacity(layer.size as usize);
    resp.into_body()
        .into_reader()
        .take(MAX_BLOB_SIZE + 1024)
        .read_to_end(&mut blob_data)?;

    // Verify digest
    let actual_digest = sha256_digest(&blob_data);
    if actual_digest != layer.digest {
        return Err(OciError::RequestFailed {
            message: format!(
                "layer digest mismatch: expected {}, got {}",
                layer.digest, actual_digest
            ),
        });
    }

    // Extract
    extract_tar_gz(&blob_data, output_dir)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::archive::create_tar_gz;
    use crate::oci::test_helpers::{create_test_module_dir, registry_from_url};
    use crate::oci::{MEDIA_TYPE_MODULE_CONFIG, MEDIA_TYPE_MODULE_LAYER};

    #[test]
    fn artifact_facts_read_the_annotation_of_a_single_platform_artifact() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        server
            .mock("GET", "/v2/test/onemod/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_body(
                serde_json::json!({
                    "schemaVersion": 2,
                    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                    "annotations": { crate::OCI_ANNOTATION_PLATFORM: "linux/amd64" },
                })
                .to_string(),
            )
            .create();

        let facts = artifact_facts(&format!("{registry}/test/onemod:v1")).unwrap();
        assert_eq!(facts.platforms, vec!["linux/amd64".to_string()]);
    }

    #[test]
    fn artifact_facts_read_every_index_entry_in_order() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        server
            .mock("GET", "/v2/test/multimod/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", crate::oci::MEDIA_TYPE_OCI_INDEX)
            .with_body(
                serde_json::json!({
                    "schemaVersion": 2,
                    "mediaType": crate::oci::MEDIA_TYPE_OCI_INDEX,
                    "manifests": [
                        { "platform": { "os": "linux", "architecture": "arm64" } },
                        { "platform": { "os": "linux", "architecture": "amd64" } },
                        // A duplicate entry (an attestation manifest re-stating
                        // its subject's platform) names no second platform.
                        { "platform": { "os": "linux", "architecture": "arm64" } },
                        // An entry with no platform block contributes nothing.
                        { "digest": "sha256:deadbeef" },
                    ],
                })
                .to_string(),
            )
            .create();

        let facts = artifact_facts(&format!("{registry}/test/multimod:v1")).unwrap();
        assert_eq!(
            facts.platforms,
            vec!["linux/arm64".to_string(), "linux/amd64".to_string()]
        );
    }

    #[test]
    fn artifact_facts_of_an_artifact_declaring_no_platform_are_empty() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        server
            .mock("GET", "/v2/test/plainmod/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_body(serde_json::json!({ "schemaVersion": 2, "layers": [] }).to_string())
            .create();

        let facts = artifact_facts(&format!("{registry}/test/plainmod:v1")).unwrap();
        assert!(facts.platforms.is_empty());
    }

    #[test]
    fn artifact_facts_name_each_attestation_in_the_vocabulary_cosign_verifies_by() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        // The registry addresses the manifest by a digest of its own, which is
        // the one the attestation tag is named after — hashing the received body
        // would look for a tag cosign never pushed.
        server
            .mock("GET", "/v2/test/attmod/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_header("Docker-Content-Digest", "sha256:feedface")
            .with_body(serde_json::json!({ "schemaVersion": 2, "layers": [] }).to_string())
            .create();

        let att = server
            .mock("GET", "/v2/test/attmod/manifests/sha256-feedface.att")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_body(
                serde_json::json!({
                    "schemaVersion": 2,
                    "layers": [
                        { "annotations": { "predicateType": "https://slsa.dev/provenance/v1" } },
                        // A second envelope of the same type names no second type.
                        { "annotations": { "predicateType": "https://slsa.dev/provenance/v1" } },
                        // A predicate cosign has no short name for is reported verbatim.
                        { "annotations": { "predicateType": "https://example.test/audit/v1" } },
                        // A layer annotating nothing contributes nothing.
                        { "digest": "sha256:deadbeef" },
                    ],
                })
                .to_string(),
            )
            .create();

        let facts = artifact_facts(&format!("{registry}/test/attmod:v1")).unwrap();
        att.assert();
        assert_eq!(
            facts.attestations,
            vec![
                "slsaprovenance1".to_string(),
                "https://example.test/audit/v1".to_string(),
            ]
        );
    }

    #[test]
    fn artifact_facts_of_an_unattested_artifact_name_no_attestation() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        // No `.att` tag is mocked: an artifact nobody attested has none, and
        // the registry's refusal to serve it is the answer "none".
        server
            .mock("GET", "/v2/test/baremod/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_header("Docker-Content-Digest", "sha256:c0ffee")
            .with_body(serde_json::json!({ "schemaVersion": 2, "layers": [] }).to_string())
            .create();

        let facts = artifact_facts(&format!("{registry}/test/baremod:v1")).unwrap();
        assert!(facts.attestations.is_empty());
    }

    #[test]
    fn pull_module_downloads_and_verifies_digest() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        // Create a layer tarball from a temp module dir
        let src_dir = create_test_module_dir();
        let layer_data = create_tar_gz(src_dir.path()).unwrap();
        let layer_digest = sha256_digest(&layer_data);

        // Build a manifest referencing this layer
        let config_blob = serde_json::to_vec(&serde_json::json!({
            "moduleYaml": "name: test",
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

        // Mock manifest GET
        server
            .mock("GET", "/v2/test/pullmod/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_body(serde_json::to_string(&manifest).unwrap())
            .create();

        // Mock layer blob GET
        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/v2/test/pullmod/blobs/sha256:.*".to_string()),
            )
            .with_status(200)
            .with_body(layer_data)
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/pullmod:v1", registry);
        let result = pull_module(
            &artifact_ref,
            output_dir.path(),
            SignaturePolicy::None,
            None,
        );
        assert!(result.is_ok(), "pull_module failed: {:?}", result.err());

        // Verify extracted files
        assert!(output_dir.path().join("module.yaml").exists());
        assert!(output_dir.path().join("README.md").exists());
    }

    #[test]
    fn pull_module_detects_digest_mismatch() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let real_layer_data = b"real layer content";
        // Use a fake digest that does NOT match the real data
        let fake_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_TYPE_OCI_MANIFEST,
            "config": {
                "mediaType": MEDIA_TYPE_MODULE_CONFIG,
                "digest": "sha256:cfgcfg",
                "size": 10,
            },
            "layers": [{
                "mediaType": MEDIA_TYPE_MODULE_LAYER,
                "digest": fake_digest,
                "size": real_layer_data.len(),
            }],
        });

        server
            .mock("GET", "/v2/test/badmod/manifests/v1")
            .with_status(200)
            .with_body(serde_json::to_string(&manifest).unwrap())
            .create();

        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/v2/test/badmod/blobs/sha256:.*".to_string()),
            )
            .with_status(200)
            .with_body(real_layer_data.as_slice())
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/badmod:v1", registry);
        let result = pull_module(
            &artifact_ref,
            output_dir.path(),
            SignaturePolicy::None,
            None,
        );
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("digest mismatch"),
            "expected digest mismatch error, got: {err_msg}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn pull_module_with_require_key_fails_when_cosign_verify_rejects() {
        use crate::test_helpers::CosignTestShim;
        let _shim = CosignTestShim::builder()
            .with_exit(1)
            .with_stderr("cosign error: signature does not match")
            .install();

        let server = mockito::Server::new();
        let registry = registry_from_url(&server.url());
        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/sigfail:v1", registry);

        let key_dir = tempfile::tempdir().unwrap();
        let key_path = key_dir.path().join("cosign.pub");
        std::fs::write(&key_path, "fake-public-key").unwrap();
        let key_path_str = key_path.to_str().unwrap();

        let policy = SignaturePolicy::RequireKey { path: key_path_str };
        let result = pull_module(&artifact_ref, output_dir.path(), policy, None);
        assert!(result.is_err());
        assert!(
            matches!(result, Err(OciError::VerificationFailed { .. })),
            "expected VerificationFailed, got: {:?}",
            result.err()
        );
    }

    #[test]
    #[serial_test::serial]
    fn pull_module_with_require_key_proceeds_when_cosign_verify_succeeds() {
        use crate::test_helpers::CosignTestShim;
        let _shim = CosignTestShim::builder().with_exit(0).install();

        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let src_dir = create_test_module_dir();
        let layer_data = create_tar_gz(src_dir.path()).unwrap();
        let layer_digest = sha256_digest(&layer_data);
        let config_blob =
            serde_json::to_vec(&serde_json::json!({"moduleYaml": "name: t"})).unwrap();
        let config_digest = sha256_digest(&config_blob);
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_TYPE_OCI_MANIFEST,
            "config": {"mediaType": MEDIA_TYPE_MODULE_CONFIG, "digest": config_digest, "size": config_blob.len()},
            "layers": [{"mediaType": MEDIA_TYPE_MODULE_LAYER, "digest": layer_digest, "size": layer_data.len()}],
        });
        server
            .mock("GET", "/v2/test/sigok/manifests/v1")
            .with_status(200)
            .with_body(serde_json::to_string(&manifest).unwrap())
            .create();
        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/v2/test/sigok/blobs/sha256:.*".to_string()),
            )
            .with_status(200)
            .with_body(layer_data)
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/sigok:v1", registry);
        let key_dir = tempfile::tempdir().unwrap();
        let key_path = key_dir.path().join("cosign.pub");
        std::fs::write(&key_path, "fake-public-key").unwrap();
        let key_path_str = key_path.to_str().unwrap();

        let policy = SignaturePolicy::RequireKey { path: key_path_str };
        let result = pull_module(&artifact_ref, output_dir.path(), policy, None);
        assert!(result.is_ok(), "pull_module failed: {:?}", result.err());
    }

    #[test]
    fn signature_policy_requires_signature_predicate() {
        assert!(!SignaturePolicy::None.requires_signature());
        assert!(SignaturePolicy::RequireKey { path: "k" }.requires_signature());
        assert!(
            SignaturePolicy::RequireKeyless {
                identity: Some("u@example"),
                issuer: None,
            }
            .requires_signature()
        );
    }

    #[test]
    fn pull_module_returns_manifest_not_found_on_404() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        // Manifest endpoint returns 404 → maps to ManifestNotFound
        server
            .mock("GET", "/v2/test/missingmod/manifests/v1")
            .with_status(404)
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/missingmod:v1", registry);
        let result = pull_module(
            &artifact_ref,
            output_dir.path(),
            SignaturePolicy::None,
            None,
        );
        assert!(matches!(result, Err(OciError::ManifestNotFound { .. })));
    }

    /// Representative of the "inner-fn" shape (the other
    /// site is `oci/pack.rs`, same reasoning). `pull_module` used to create
    /// its spinner and then run every fallible step under its own early `?`,
    /// so a manifest 404 abandoned an already-running spinner — Drop then
    /// settled it as an unwanted "(interrupted)" line nobody asked for. Every
    /// step now runs inside `pull_module_inner`, matched exactly once, so the
    /// spinner is always settled by `finish_fail` and never by Drop.
    #[test]
    fn pull_module_failure_settles_via_finish_fail_not_drop() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        server
            .mock("GET", "/v2/test/missingmod/manifests/v1")
            .with_status(404)
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/missingmod:v1", registry);

        let (printer, buf) = crate::output::Printer::for_test_live_scrollback();
        let result = pull_module(
            &artifact_ref,
            output_dir.path(),
            SignaturePolicy::None,
            Some(&printer),
        );
        drop(printer);

        assert!(matches!(result, Err(OciError::ManifestNotFound { .. })));
        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("Failed to pull module"),
            "the finish_fail line must be committed: {out}"
        );
        assert_eq!(
            out.matches("Failed to pull module").count(),
            1,
            "the failure must settle exactly once, never twice: {out}"
        );
        assert!(
            !out.contains("(interrupted)"),
            "a spinner settled by finish_fail must never also settle via Drop: {out}"
        );
    }

    #[test]
    fn pull_module_returns_blob_not_found_when_layer_missing() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        // Manifest succeeds but references a layer the registry won't serve.
        let fake_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_TYPE_OCI_MANIFEST,
            "config": {
                "mediaType": MEDIA_TYPE_MODULE_CONFIG,
                "digest": "sha256:cfgcfg",
                "size": 10,
            },
            "layers": [{
                "mediaType": MEDIA_TYPE_MODULE_LAYER,
                "digest": fake_digest,
                "size": 16,
            }],
        });

        server
            .mock("GET", "/v2/test/noblob/manifests/v1")
            .with_status(200)
            .with_body(serde_json::to_string(&manifest).unwrap())
            .create();

        // Blob fetch returns 404 → maps to BlobNotFound
        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/v2/test/noblob/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/noblob:v1", registry);
        let result = pull_module(
            &artifact_ref,
            output_dir.path(),
            SignaturePolicy::None,
            None,
        );
        assert!(matches!(result, Err(OciError::BlobNotFound { .. })));
    }

    #[test]
    fn pull_module_returns_request_failed_on_invalid_manifest_json() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        // Manifest GET succeeds (200) but body is unparseable → RequestFailed
        server
            .mock("GET", "/v2/test/badjson/manifests/v1")
            .with_status(200)
            .with_body("not valid json")
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/badjson:v1", registry);
        let result = pull_module(
            &artifact_ref,
            output_dir.path(),
            SignaturePolicy::None,
            None,
        );
        assert!(matches!(result, Err(OciError::RequestFailed { .. })));
    }
}

// Pull: download OCI module artifact, verify layer digest, extract to disk.
// Optional cosign signature presence check (HEAD on `<tag>.sig`).

use std::io::Read;
use std::path::Path;

use crate::errors::OciError;
use crate::output::Printer;
use crate::sha256_digest;

use super::archive::extract_tar_gz;
use super::auth::RegistryAuth;
use super::transport::authenticated_request;
use super::{MEDIA_TYPE_OCI_MANIFEST, OciManifest, OciReference, ReferenceKind};

/// Pull a module from an OCI registry and extract it to `output_dir`.
///
/// If `require_signature` is true, checks for a cosign signature tag
/// (`<tag>.sig` or `sha256-<hash>.sig`) and returns an error if not found.
pub fn pull_module(
    artifact_ref: &str,
    output_dir: &Path,
    require_signature: bool,
    printer: Option<&Printer>,
) -> Result<(), OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = crate::http::http_agent(crate::http::HTTP_OCI_TIMEOUT);

    let spinner = printer.map(|p| p.spinner(format!("Pulling module from {artifact_ref}...")));

    // If signature required, check for cosign signature tag
    if require_signature {
        check_signature_exists(&agent, &oci_ref, auth.as_ref())?;
    }

    // Pull manifest
    let manifest_url = format!(
        "{}/{}/manifests/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        oci_ref.reference_str(),
    );

    let resp = authenticated_request(
        &agent,
        "GET",
        &manifest_url,
        auth.as_ref(),
        Some(MEDIA_TYPE_OCI_MANIFEST),
        None,
        None,
    )
    .map_err(|e| OciError::ManifestNotFound {
        reference: format!("{}: {e}", oci_ref),
    })?;

    let manifest_body = resp.into_string().map_err(|e| OciError::RequestFailed {
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
        &agent,
        "GET",
        &blob_url,
        auth.as_ref(),
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
    resp.into_reader()
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

    if let Some(s) = spinner {
        let _ = s.finish_ok(format!("Pulled module from {artifact_ref}"));
    }

    tracing::info!(
        reference = %oci_ref,
        output = %output_dir.display(),
        "module pulled"
    );

    Ok(())
}

/// Check if a cosign-style signature exists for the given reference.
/// Cosign stores signatures at tag `<tag>.sig` or `sha256-<hash>.sig`.
fn check_signature_exists(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
) -> Result<(), OciError> {
    let sig_tag = match &oci_ref.reference {
        ReferenceKind::Tag(tag) => format!("{tag}.sig"),
        ReferenceKind::Digest(digest) => {
            // sha256:abc... → sha256-abc....sig
            digest.replace(':', "-") + ".sig"
        }
    };

    let sig_url = format!(
        "{}/{}/manifests/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        sig_tag,
    );

    let result = authenticated_request(agent, "HEAD", &sig_url, auth, None, None, None);

    match result {
        Ok(_) => Ok(()),
        Err(_) => Err(OciError::SignatureRequired {
            reference: oci_ref.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::archive::create_tar_gz;
    use crate::oci::test_helpers::{create_test_module_dir, registry_from_url};
    use crate::oci::{MEDIA_TYPE_MODULE_CONFIG, MEDIA_TYPE_MODULE_LAYER};

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
        let result = pull_module(&artifact_ref, output_dir.path(), false, None);
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
        let result = pull_module(&artifact_ref, output_dir.path(), false, None);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("digest mismatch"),
            "expected digest mismatch error, got: {err_msg}"
        );
    }

    #[test]
    fn pull_module_checks_signature_when_required() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        // No signature tag exists
        server
            .mock("HEAD", "/v2/test/sigmod/manifests/v1.sig")
            .with_status(404)
            .create();

        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/sigmod:v1", registry);
        let result = pull_module(&artifact_ref, output_dir.path(), true, None);
        assert!(result.is_err());
        assert!(
            matches!(result, Err(OciError::SignatureRequired { .. })),
            "expected SignatureRequired, got: {:?}",
            result.err()
        );
    }

    #[test]
    fn check_signature_exists_ok_when_present() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/sigexist".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        server
            .mock("HEAD", "/v2/test/sigexist/manifests/v1.sig")
            .with_status(200)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = check_signature_exists(&agent, &oci_ref, None);
        assert_eq!(result.unwrap(), (), "signature check should return Ok(())");
    }

    #[test]
    fn check_signature_exists_fails_when_missing() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/nosig".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        server
            .mock("HEAD", "/v2/test/nosig/manifests/v1.sig")
            .with_status(404)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = check_signature_exists(&agent, &oci_ref, None);
        assert!(matches!(result, Err(OciError::SignatureRequired { .. })));
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
        let result = pull_module(&artifact_ref, output_dir.path(), false, None);
        assert!(matches!(result, Err(OciError::ManifestNotFound { .. })));
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
        let result = pull_module(&artifact_ref, output_dir.path(), false, None);
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
        let result = pull_module(&artifact_ref, output_dir.path(), false, None);
        assert!(matches!(result, Err(OciError::RequestFailed { .. })));
    }

    #[test]
    fn check_signature_exists_digest_reference() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/digsig".to_string(),
            reference: ReferenceKind::Digest("sha256:abc123".to_string()),
        };

        // For digest refs, sig tag is "sha256-abc123.sig"
        server
            .mock("HEAD", "/v2/test/digsig/manifests/sha256-abc123.sig")
            .with_status(200)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let result = check_signature_exists(&agent, &oci_ref, None);
        assert_eq!(
            result.unwrap(),
            (),
            "digest-referenced signature check should return Ok(())"
        );
    }
}

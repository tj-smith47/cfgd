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
use super::sign::{VerifyOptions, verify_attestation, verify_signature};
use super::transport::{authenticated_request, response_digest};
use super::{MEDIA_TYPE_OCI_MANIFEST, OciManifest, OciReference};

/// The result of a successful [`pull_module`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullOutcome {
    /// Digest of the image manifest whose layer was extracted (`"sha256:..."`).
    pub digest: String,
    /// Digest of the index the reference resolved to, when it resolved to one
    /// and `digest` is the entry picked out of it.
    pub index_digest: Option<String>,
}

/// Policy for verifying a module artifact's cosign signature during pull.
///
/// - `None` — skip signature verification entirely (default).
/// - `RequireKey { path }` — fail unless `cosign verify --key <path>` succeeds.
/// - `RequireKeyless { identity, issuer }` — fail unless keyless verification
///   matches the supplied certificate identity / OIDC issuer constraints.
#[derive(Debug, Clone, Copy, Default)]
pub enum SignaturePolicy<'a> {
    #[default]
    None,
    RequireKey {
        path: &'a str,
    },
    RequireKeyless {
        identity: Option<&'a str>,
        issuer: Option<&'a str>,
    },
}

/// The cosign checks [`pull_module`] runs before it extracts anything. Both
/// run against the digest the reference resolved to on the one read the pull
/// takes from, so what was verified is what gets extracted even if the tag
/// moves mid-pull.
#[derive(Debug, Clone, Copy, Default)]
pub struct PullChecks<'a> {
    /// The signature the artifact must carry.
    pub signature: SignaturePolicy<'a>,
    /// An attestation the artifact must carry: its predicate type (in the
    /// vocabulary `cosign verify-attestation --type` takes) and the options
    /// cosign verifies it with.
    pub attestation: Option<(&'a str, VerifyOptions<'a>)>,
}

/// Pull a module from an OCI registry and extract it to `output_dir`.
///
/// `checks.signature` controls cryptographic signature verification:
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
///
/// `checks.attestation` adds a `cosign verify-attestation` of that predicate
/// type. A signature failure is [`OciError::VerificationFailed`] and an
/// attestation failure [`OciError::AttestationError`], whatever stopped cosign.
///
/// A reference that resolves to an index pulls the entry for `platform`
/// (this host when `None`); one that resolves to a single manifest pulls it
/// whatever platform it names.
pub fn pull_module(
    artifact_ref: &str,
    output_dir: &Path,
    checks: PullChecks<'_>,
    platform: Option<&str>,
    printer: Option<&Printer>,
) -> Result<PullOutcome, OciError> {
    let oci_ref = OciReference::parse(artifact_ref)?;
    let auth = RegistryAuth::resolve(&oci_ref.registry);
    let agent = crate::http::http_agent(crate::http::HTTP_OCI_TIMEOUT);

    let spinner = printer.map(|p| p.spinner(format!("Pulling module from {artifact_ref}")));

    match pull_module_inner(
        &agent,
        &oci_ref,
        auth.as_ref(),
        output_dir,
        &checks,
        &platform.map_or_else(super::current_platform, String::from),
    ) {
        Ok((outcome, pulled_platform)) => {
            // Settled without the reference: the caller's header block names
            // it, and the running message above already carried it while the
            // wait was the only thing on screen.
            if let Some(s) = spinner {
                let detail = match pulled_platform.as_deref() {
                    Some(p) => super::artifact_row_detail(
                        &outcome.digest,
                        p,
                        outcome.index_digest.as_deref(),
                    ),
                    None => outcome.digest.clone(),
                };
                let _ = s.finish_ok("Pulled module").detail(detail);
            }
            tracing::debug!(
                reference = %oci_ref,
                output = %output_dir.posix(),
                "module pulled"
            );
            Ok(outcome)
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
    let resp = authenticated_request(
        agent,
        "GET",
        &url,
        auth,
        Some(&super::manifest_accept()),
        None,
        None,
    )?;
    let ManifestDocument { digest, doc, .. } = read_manifest_document(resp)?;
    Ok((digest, doc))
}

/// A manifest document as the registry serves it.
pub(super) struct ManifestDocument {
    /// The digest the registry addresses it by (see [`fetch_manifest_document`]).
    pub(super) digest: String,
    /// The byte length of the served body, which a descriptor pointing at it
    /// declares as its `size`.
    pub(super) size: u64,
    /// The sha256 of the served body itself, which a manifest fetched by
    /// digest must equal: the registry's header is its own claim.
    pub(super) content_digest: String,
    pub(super) doc: serde_json::Value,
}

/// Read a manifest GET's body into a [`ManifestDocument`].
pub(super) fn read_manifest_document(
    resp: ureq::http::Response<ureq::Body>,
) -> Result<ManifestDocument, OciError> {
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
    let content_digest = sha256_digest(body.as_bytes());
    Ok(ManifestDocument {
        digest: header_digest.unwrap_or_else(|| content_digest.clone()),
        size: body.len() as u64,
        content_digest,
        doc,
    })
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

/// The fallible half of [`pull_module`]: every step from the tag read
/// through extraction runs under one `Result` the caller matches once, rather
/// than an early `?` abandoning the spinner mid-pull. Answers the outcome and
/// the platform the pulled manifest is for, when it names one.
fn pull_module_inner(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    output_dir: &Path,
    checks: &PullChecks<'_>,
    platform: &str,
) -> Result<(PullOutcome, Option<String>), OciError> {
    let (os, architecture) = super::parse_platform_target(platform)?;
    let wanted = super::push::OciPlatform {
        os: os.to_string(),
        architecture: architecture.to_string(),
    };
    let top = fetch_pull_document(agent, oci_ref, auth, oci_ref.reference_str(), oci_ref)?;
    // The checks and the extraction must both be about the bytes read: a
    // header naming a signed digest over a body naming other layers would
    // otherwise pass cosign and extract the unsigned body.
    if top.digest != top.content_digest {
        return Err(OciError::RequestFailed {
            message: format!(
                "{oci_ref} answered with digest {} in its Docker-Content-Digest header, but the \
                 manifest it served hashes to {}",
                top.digest, top.content_digest
            ),
        });
    }
    run_checks(checks, &oci_ref.at_digest(&top.content_digest).to_string())?;
    let (manifest, outcome, pulled_platform) =
        select_platform_manifest(agent, oci_ref, auth, top, &wanted)?;

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

    Ok((outcome, pulled_platform))
}

/// Run `checks` against `subject`, typing each failure by the check it came
/// from whatever stopped cosign, so a caller tells the two apart by variant.
fn run_checks(checks: &PullChecks<'_>, subject: &str) -> Result<(), OciError> {
    let opts = match checks.signature {
        SignaturePolicy::None => None,
        SignaturePolicy::RequireKey { path } => Some(VerifyOptions {
            key: Some(path),
            identity: None,
            issuer: None,
        }),
        SignaturePolicy::RequireKeyless { identity, issuer } => Some(VerifyOptions {
            key: None,
            identity,
            issuer,
        }),
    };
    if let Some(opts) = opts {
        verify_signature(subject, &opts).map_err(|e| match e {
            OciError::VerificationFailed { .. } => e,
            other => OciError::VerificationFailed {
                reference: subject.to_string(),
                message: other.to_string(),
            },
        })?;
    }
    if let Some((predicate_type, opts)) = &checks.attestation {
        verify_attestation(subject, predicate_type, opts).map_err(|e| match e {
            OciError::AttestationError { .. } => e,
            other => OciError::AttestationError {
                message: other.to_string(),
            },
        })?;
    }
    Ok(())
}

/// GET a manifest document for a pull, naming `name` when it is missing.
fn fetch_pull_document(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    reference: &str,
    name: &OciReference,
) -> Result<ManifestDocument, OciError> {
    let url = format!(
        "{}/{}/manifests/{reference}",
        oci_ref.api_base(),
        oci_ref.repository,
    );
    authenticated_request(
        agent,
        "GET",
        &url,
        auth,
        Some(&super::manifest_accept()),
        None,
        None,
    )
    .map_err(|e| OciError::ManifestNotFound {
        reference: format!("{name}: {e}"),
    })
    .and_then(read_manifest_document)
}

/// The image manifest to pull out of `top`, the document the reference
/// resolved to: `top` itself, or its entry for `wanted` fetched by digest.
/// Answers the digests that name it and the platform it is for, when it names one.
fn select_platform_manifest(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    top: ManifestDocument,
    wanted: &super::push::OciPlatform,
) -> Result<(OciManifest, PullOutcome, Option<String>), OciError> {
    let Some(entries) = top.doc.get("manifests").and_then(|m| m.as_array()) else {
        let annotated = top
            .doc
            .get("annotations")
            .and_then(|a| a.get(crate::OCI_ANNOTATION_PLATFORM))
            .and_then(|p| p.as_str())
            .map(String::from);
        let outcome = PullOutcome {
            digest: top.content_digest,
            index_digest: None,
        };
        return Ok((parse_image_manifest(top.doc)?, outcome, annotated));
    };

    let platform = format!("{}/{}", wanted.os, wanted.architecture);
    let Some(entry_digest) = entries
        .iter()
        .find(|e| super::push::entry_platform_is(e, wanted))
        .and_then(|e| e.get("digest"))
        .and_then(|d| d.as_str())
    else {
        return Err(OciError::PlatformNotInIndex {
            reference: oci_ref.to_string(),
            platform,
            available: declared_platforms(&top.doc),
        });
    };

    let picked = fetch_pull_document(
        agent,
        oci_ref,
        auth,
        entry_digest,
        &oci_ref.at_digest(entry_digest),
    )?;
    if picked.content_digest != entry_digest {
        return Err(OciError::RequestFailed {
            message: format!(
                "manifest digest mismatch for {platform} in {oci_ref}: the index names \
                 {entry_digest}, the registry served {}",
                picked.content_digest
            ),
        });
    }
    let outcome = PullOutcome {
        digest: entry_digest.to_string(),
        index_digest: Some(top.content_digest),
    };
    Ok((parse_image_manifest(picked.doc)?, outcome, Some(platform)))
}

fn parse_image_manifest(doc: serde_json::Value) -> Result<OciManifest, OciError> {
    serde_json::from_value(doc).map_err(|e| OciError::RequestFailed {
        message: format!("invalid manifest JSON: {e}"),
    })
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
    fn two_platform_pushes_to_one_tag_read_back_as_both_platforms_in_push_order() {
        let store = crate::oci::test_helpers::ManifestStore::new("test/roundtrip");
        let amd64 = create_test_module_dir();
        let arm64 = create_test_module_dir();
        let artifact = store.artifact("v1");

        crate::oci::push_module(amd64.path(), &artifact, Some("linux/amd64"), None)
            .expect("push amd64");
        crate::oci::push_module(arm64.path(), &artifact, Some("linux/arm64"), None)
            .expect("push arm64");

        let facts = artifact_facts(&artifact).unwrap();
        assert_eq!(
            facts.platforms,
            vec!["linux/amd64".to_string(), "linux/arm64".to_string()]
        );
    }

    /// A module directory whose README says which build it is, so an
    /// extraction shows which platform's layer was pulled.
    fn module_dir_marked(mark: &str) -> tempfile::TempDir {
        let dir = create_test_module_dir();
        std::fs::write(dir.path().join("README.md"), mark).unwrap();
        dir
    }

    fn pulled_mark(
        artifact: &str,
        platform: Option<&str>,
    ) -> Result<(String, PullOutcome), OciError> {
        let out = tempfile::tempdir().unwrap();
        let outcome = pull_module(artifact, out.path(), PullChecks::default(), platform, None)?;
        let mark = std::fs::read_to_string(out.path().join("README.md")).unwrap();
        Ok((mark, outcome))
    }

    #[test]
    fn pull_of_an_index_extracts_the_entry_for_the_requested_platform() {
        let store = crate::oci::test_helpers::ManifestStore::new("test/pullidx");
        let artifact = store.artifact("v1");
        let amd = module_dir_marked("amd64 build");
        let arm = module_dir_marked("arm64 build");
        let amd_push =
            crate::oci::push_module(amd.path(), &artifact, Some("linux/amd64"), None).unwrap();
        let arm_push =
            crate::oci::push_module(arm.path(), &artifact, Some("linux/arm64"), None).unwrap();
        let index = arm_push
            .index_digest
            .clone()
            .expect("the second push wrote an index");

        let (mark, outcome) = pulled_mark(&artifact, Some("linux/arm64")).unwrap();
        assert_eq!(mark, "arm64 build");
        assert_eq!(
            outcome,
            PullOutcome {
                digest: arm_push.digest,
                index_digest: Some(index.clone()),
            }
        );

        let (mark, outcome) = pulled_mark(&artifact, Some("linux/amd64")).unwrap();
        assert_eq!(
            mark, "amd64 build",
            "the platform asked for overrides the host"
        );
        assert_eq!(outcome.digest, amd_push.digest);
        assert_eq!(outcome.index_digest, Some(index));
    }

    #[test]
    fn pull_of_an_index_with_no_platform_takes_the_hosts_entry() {
        let store = crate::oci::test_helpers::ManifestStore::new("test/pullhost");
        let artifact = store.artifact("v1");
        let other = module_dir_marked("other build");
        let host = module_dir_marked("host build");
        crate::oci::push_module(other.path(), &artifact, Some("plan9/mips"), None).unwrap();
        crate::oci::push_module(host.path(), &artifact, None, None).unwrap();

        let (mark, _) = pulled_mark(&artifact, None).unwrap();
        assert_eq!(mark, "host build");
    }

    #[test]
    fn pull_of_an_index_without_the_platform_names_what_it_holds() {
        let store = crate::oci::test_helpers::ManifestStore::new("test/pullnone");
        let artifact = store.artifact("v1");
        for platform in ["linux/amd64", "linux/arm64"] {
            let dir = module_dir_marked(platform);
            crate::oci::push_module(dir.path(), &artifact, Some(platform), None).unwrap();
        }

        let err = pulled_mark(&artifact, Some("linux/s390x")).expect_err("no entry for s390x");
        match &err {
            OciError::PlatformNotInIndex {
                reference,
                platform,
                available,
            } => {
                assert_eq!(reference, &artifact);
                assert_eq!(platform, "linux/s390x");
                assert_eq!(available, &["linux/amd64", "linux/arm64"]);
            }
            other => panic!("expected PlatformNotInIndex, got {other:?}"),
        }
        assert!(
            err.to_string()
                .contains("it holds linux/amd64, linux/arm64"),
            "{err}"
        );
    }

    #[test]
    fn pull_of_a_single_manifest_takes_it_whatever_platform_is_asked() {
        let store = crate::oci::test_helpers::ManifestStore::new("test/pullone");
        let artifact = store.artifact("v1");
        let dir = module_dir_marked("only build");
        let pushed =
            crate::oci::push_module(dir.path(), &artifact, Some("linux/amd64"), None).unwrap();

        let (mark, outcome) = pulled_mark(&artifact, Some("linux/arm64")).unwrap();
        assert_eq!(mark, "only build");
        assert_eq!(
            outcome,
            PullOutcome {
                digest: pushed.digest,
                index_digest: None,
            }
        );
    }

    #[test]
    fn pull_refuses_a_malformed_platform_before_reading_the_tag() {
        let store = crate::oci::test_helpers::ManifestStore::new("test/pullbad");
        let artifact = store.artifact("v1");
        let dir = module_dir_marked("only build");
        crate::oci::push_module(dir.path(), &artifact, Some("linux/amd64"), None).unwrap();
        let before = store.requests().len();

        let err =
            pulled_mark(&artifact, Some("linux")).expect_err("a platform with no arch is refused");
        assert!(err.to_string().contains("linux"), "{err}");
        assert_eq!(store.requests().len(), before, "nothing was requested");
    }

    #[test]
    fn pull_refuses_an_index_entry_the_registry_serves_under_another_digest() {
        let store = crate::oci::test_helpers::ManifestStore::new("test/pulllie");
        let artifact = store.artifact("v1");
        let dir = module_dir_marked("real build");
        crate::oci::push_module(dir.path(), &artifact, Some("linux/amd64"), None).unwrap();
        let real = store.stored("v1");
        // The index names a digest whose stored document hashes to something else.
        store.seed_bytes("sha256:0000", &real);
        store.seed(
            "v1",
            &serde_json::json!({
                "schemaVersion": 2,
                "mediaType": crate::oci::MEDIA_TYPE_OCI_INDEX,
                "manifests": [{
                    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                    "digest": "sha256:0000",
                    "size": real.len(),
                    "platform": { "os": "linux", "architecture": "amd64" },
                }],
            }),
        );

        let err =
            pulled_mark(&artifact, Some("linux/amd64")).expect_err("a mismatched entry is refused");
        assert!(
            err.to_string().contains("manifest digest mismatch"),
            "{err}"
        );
    }

    /// Two platforms pushed to `v1` of a fresh store, answering the store,
    /// the artifact and the index digest the tag resolves to.
    fn two_platform_store(repo: &str) -> (crate::oci::test_helpers::ManifestStore, String, String) {
        let store = crate::oci::test_helpers::ManifestStore::new(repo);
        let artifact = store.artifact("v1");
        let mut index = String::new();
        for platform in ["linux/amd64", "linux/arm64"] {
            let dir = module_dir_marked(&format!("{platform} build"));
            let pushed =
                crate::oci::push_module(dir.path(), &artifact, Some(platform), None).unwrap();
            index = pushed.index_digest.unwrap_or_default();
        }
        (store, artifact, index)
    }

    fn keyed_checks(key: &str) -> PullChecks<'_> {
        let opts = VerifyOptions {
            key: Some(key),
            identity: None,
            issuer: None,
        };
        PullChecks {
            signature: SignaturePolicy::RequireKey { path: key },
            attestation: Some(("slsaprovenance1", opts)),
        }
    }

    #[test]
    #[serial_test::serial]
    fn pull_verifies_the_digest_of_the_one_tag_read_it_extracts_from() {
        let shim = crate::test_helpers::CosignTestShim::builder()
            .with_argv_logging(true)
            .with_exit(0)
            .install();
        let (store, artifact, index) = two_platform_store("test/pullsig");
        let before = store.requests().len();
        let out = tempfile::tempdir().unwrap();

        let outcome = pull_module(
            &artifact,
            out.path(),
            keyed_checks("cosign.pub"),
            Some("linux/arm64"),
            None,
        )
        .unwrap();

        assert_eq!(outcome.index_digest.as_deref(), Some(index.as_str()));
        let reads: Vec<_> = store.requests()[before..]
            .iter()
            .filter(|r| r.as_str() == "GET v1")
            .cloned()
            .collect();
        assert_eq!(reads, ["GET v1"], "the tag is read once");
        let argv = shim.argv_log();
        let subject = format!("{}@{index}", artifact.trim_end_matches(":v1"));
        let checked: Vec<&str> = argv.lines().collect();
        assert_eq!(checked.len(), 2, "{argv}");
        assert!(
            checked[0].starts_with("verify ") && checked[0].ends_with(&subject),
            "{argv}"
        );
        assert!(
            checked[1].starts_with("verify-attestation ") && checked[1].ends_with(&subject),
            "{argv}"
        );
        assert_eq!(
            std::fs::read_to_string(out.path().join("README.md")).unwrap(),
            "linux/arm64 build"
        );
    }

    /// A tag served from mockito with `body`, plus `header` as its
    /// `Docker-Content-Digest` when one is given.
    fn tag_served(
        server: &mut mockito::ServerGuard,
        repo: &str,
        body: &str,
        header: Option<&str>,
    ) -> String {
        let mut mock = server
            .mock("GET", format!("/v2/{repo}/manifests/v1").as_str())
            .with_status(200)
            .with_header("Content-Type", MEDIA_TYPE_OCI_MANIFEST)
            .with_body(body);
        if let Some(digest) = header {
            mock = mock.with_header("Docker-Content-Digest", digest);
        }
        mock.create();
        format!("{}/{repo}:v1", registry_from_url(&server.url()))
    }

    #[test]
    #[serial_test::serial]
    fn pull_refuses_a_tag_whose_digest_header_names_other_bytes_before_any_check() {
        let shim = crate::test_helpers::CosignTestShim::builder()
            .with_argv_logging(true)
            .with_exit(0)
            .install();
        let mut server = mockito::Server::new();
        let body = r#"{"schemaVersion":2,"layers":[]}"#;
        let claimed = format!("sha256:{}", "b".repeat(64));
        let artifact = tag_served(&mut server, "test/pulllie", body, Some(&claimed));
        let out = tempfile::tempdir().unwrap();

        let err = pull_module(
            &artifact,
            out.path(),
            keyed_checks("cosign.pub"),
            None,
            None,
        )
        .expect_err("a header naming other bytes is refused");
        let message = err.to_string();
        assert!(matches!(err, OciError::RequestFailed { .. }), "{err:?}");
        assert!(message.contains(&claimed), "{message}");
        assert!(
            message.contains(&sha256_digest(body.as_bytes())),
            "{message}"
        );
        assert_eq!(shim.argv_log(), "", "cosign never ran");
    }

    #[test]
    #[serial_test::serial]
    fn pull_without_a_digest_header_checks_the_digest_of_the_bytes_it_read() {
        let shim = crate::test_helpers::CosignTestShim::builder()
            .with_argv_logging(true)
            .with_exit(0)
            .install();
        let mut server = mockito::Server::new();
        let body = r#"{"schemaVersion":2,"layers":[]}"#;
        let artifact = tag_served(&mut server, "test/pullnohdr", body, None);
        let out = tempfile::tempdir().unwrap();

        // No layers, so the pull fails after the checks ran.
        let _ = pull_module(
            &artifact,
            out.path(),
            keyed_checks("cosign.pub"),
            None,
            None,
        );

        let subject = format!(
            "{}@{}",
            artifact.trim_end_matches(":v1"),
            sha256_digest(body.as_bytes())
        );
        let argv = shim.argv_log();
        let calls: Vec<&str> = argv.lines().collect();
        assert_eq!(calls.len(), 2, "{argv}");
        assert!(calls.iter().all(|c| c.ends_with(&subject)), "{argv}");
    }

    #[test]
    #[serial_test::serial]
    fn pull_types_an_attestation_failure_apart_from_a_signature_failure() {
        let _shim = crate::test_helpers::CosignTestShim::builder()
            .with_exit(1)
            .with_stderr("no matching attestations")
            .install();
        let (_store, artifact, _) = two_platform_store("test/pullatt");
        let out = tempfile::tempdir().unwrap();
        let checks = PullChecks {
            signature: SignaturePolicy::None,
            ..keyed_checks("cosign.pub")
        };

        let err = pull_module(&artifact, out.path(), checks, Some("linux/amd64"), None)
            .expect_err("a rejected attestation stops the pull");
        assert!(matches!(err, OciError::AttestationError { .. }), "{err:?}");
        assert!(
            !out.path().join("README.md").exists(),
            "nothing is extracted"
        );
    }

    #[test]
    fn pull_names_the_entry_digest_an_index_points_at_when_it_is_missing() {
        let store = crate::oci::test_helpers::ManifestStore::new("test/pullgone");
        let artifact = store.artifact("v1");
        let missing = format!("sha256:{}", "a".repeat(64));
        store.seed(
            "v1",
            &serde_json::json!({
                "schemaVersion": 2,
                "mediaType": crate::oci::MEDIA_TYPE_OCI_INDEX,
                "manifests": [{
                    "mediaType": MEDIA_TYPE_OCI_MANIFEST,
                    "digest": missing,
                    "size": 1,
                    "platform": { "os": "linux", "architecture": "amd64" },
                }],
            }),
        );

        let err = pulled_mark(&artifact, Some("linux/amd64")).expect_err("the entry is gone");
        let entry = format!("{}@{missing}", artifact.trim_end_matches(":v1"));
        match &err {
            OciError::ManifestNotFound { reference } => {
                assert!(reference.starts_with(&format!("{entry}: ")), "{reference}");
            }
            other => panic!("expected ManifestNotFound, got {other:?}"),
        }
    }

    #[test]
    fn pull_row_states_the_digest_platform_and_index_it_took() {
        let (_store, artifact, index) = two_platform_store("test/pullrow");
        let out = tempfile::tempdir().unwrap();
        let (printer, cap) = crate::output::Printer::for_test_doc();
        let outcome = pull_module(
            &artifact,
            out.path(),
            PullChecks::default(),
            Some("linux/amd64"),
            Some(&printer),
        )
        .unwrap();
        drop(printer);

        let human = cap.human();
        let detail =
            crate::oci::artifact_row_detail(&outcome.digest, "linux/amd64", Some(index.as_str()));
        assert!(human.contains(&detail), "{human}");
    }

    #[test]
    fn pull_row_of_a_single_manifest_states_its_annotated_platform() {
        let store = crate::oci::test_helpers::ManifestStore::new("test/pullrow1");
        let artifact = store.artifact("v1");
        let dir = module_dir_marked("only build");
        let pushed =
            crate::oci::push_module(dir.path(), &artifact, Some("plan9/mips"), None).unwrap();
        let out = tempfile::tempdir().unwrap();
        let (printer, cap) = crate::output::Printer::for_test_doc();
        pull_module(
            &artifact,
            out.path(),
            PullChecks::default(),
            None,
            Some(&printer),
        )
        .unwrap();
        drop(printer);

        let human = cap.human();
        let detail = crate::oci::artifact_row_detail(&pushed.digest, "plan9/mips", None);
        assert!(human.contains(&detail), "{human}");
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
            PullChecks::default(),
            None,
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
            PullChecks::default(),
            None,
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

        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());
        let output_dir = tempfile::tempdir().unwrap();
        let artifact_ref = format!("{}/test/sigfail:v1", registry);
        // The signature is checked against the digest the tag read answers,
        // so the tag is read first.
        server
            .mock("GET", "/v2/test/sigfail/manifests/v1")
            .with_status(200)
            .with_body(r#"{"schemaVersion":2,"layers":[]}"#)
            .create();

        let key_dir = tempfile::tempdir().unwrap();
        let key_path = key_dir.path().join("cosign.pub");
        std::fs::write(&key_path, "fake-public-key").unwrap();
        let key_path_str = key_path.to_str().unwrap();

        let policy = SignaturePolicy::RequireKey { path: key_path_str };
        let result = pull_module(
            &artifact_ref,
            output_dir.path(),
            PullChecks {
                signature: policy,
                attestation: None,
            },
            None,
            None,
        );
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
        let result = pull_module(
            &artifact_ref,
            output_dir.path(),
            PullChecks {
                signature: policy,
                attestation: None,
            },
            None,
            None,
        );
        assert!(result.is_ok(), "pull_module failed: {:?}", result.err());
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
            PullChecks::default(),
            None,
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
            PullChecks::default(),
            None,
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
            PullChecks::default(),
            None,
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
            PullChecks::default(),
            None,
            None,
        );
        assert!(matches!(result, Err(OciError::RequestFailed { .. })));
    }
}

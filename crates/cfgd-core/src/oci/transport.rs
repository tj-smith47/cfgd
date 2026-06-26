// Authenticated HTTP transport: 401-with-Bearer-challenge token exchange,
// blob upload (HEAD existence check + monolithic POST/PUT flow).

use std::io::Read;

use ureq::Body;
use ureq::http::Response;

use crate::errors::OciError;
use crate::sha256_digest;

use super::OciReference;
use super::auth::{RegistryAuth, get_bearer_token};

/// Read a response header value as a UTF-8 `&str`, mirroring ureq 2's
/// `Response::header` (`Option<&str>`) over ureq 3's `http::HeaderMap`.
fn header<'a>(resp: &'a Response<Body>, name: &str) -> Option<&'a str> {
    resp.headers().get(name).and_then(|v| v.to_str().ok())
}

/// Run an `http::Request` through `agent` with HTTP-status-as-error disabled so
/// the caller can inspect 4xx/5xx responses (notably the 401 token challenge)
/// exactly as ureq 2 surfaced them in `Error::Status(code, resp)`.
fn run_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    content_type: Option<&str>,
    accept: Option<&str>,
    authorization: Option<&str>,
    body: Option<&[u8]>,
) -> Result<Response<Body>, ureq::Error> {
    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    if let Some(ct) = content_type {
        builder = builder.header("Content-Type", ct);
    }
    if let Some(acc) = accept {
        builder = builder.header("Accept", acc);
    }
    if let Some(authz) = authorization {
        builder = builder.header("Authorization", authz);
    }
    let request = builder
        .body(body.map(<[u8]>::to_vec).unwrap_or_default())
        .map_err(ureq::Error::from)?;

    let configured = agent
        .configure_request(request)
        .http_status_as_error(false)
        .build();
    agent.run(configured)
}

/// Maximum blob size accepted by [`fetch_blob`] (512 MiB). Mirrors the cap in
/// `pull_module`; a base image layer larger than this is rejected rather than
/// risking OOM from a malicious or mis-sized descriptor.
const MAX_BLOB_SIZE: u64 = 512 * 1024 * 1024;

/// Make an authenticated request. Handles 401 → token exchange flow.
pub(super) fn authenticated_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    auth: Option<&RegistryAuth>,
    accept: Option<&str>,
    content_type: Option<&str>,
    body: Option<&[u8]>,
) -> Result<Response<Body>, OciError> {
    let basic_authz = auth.map(|cred| cred.basic_auth_header());

    // First attempt — may get 401. `run_request` disables status-as-error so a
    // 401 is returned as `Ok(resp)` and we can read its Www-Authenticate header.
    let resp = run_request(
        agent,
        method,
        url,
        content_type,
        accept,
        basic_authz.as_deref(),
        body,
    )
    .map_err(|e| OciError::RequestFailed {
        message: format!("{e}"),
    })?;

    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(resp);
    }

    if status == 401 {
        // Get the Www-Authenticate header and try token auth
        let www_auth = header(&resp, "Www-Authenticate")
            .or_else(|| header(&resp, "www-authenticate"))
            .unwrap_or("")
            .to_string();

        if www_auth.is_empty() || !www_auth.contains("Bearer") {
            return Err(OciError::AuthFailed {
                registry: url.to_string(),
                message: "401 with no Bearer challenge".to_string(),
            });
        }

        let token = get_bearer_token(agent, &www_auth, auth)?;

        // Retry with bearer token
        let resp2 = run_request(
            agent,
            method,
            url,
            content_type,
            accept,
            Some(&format!("Bearer {}", token)),
            body,
        )
        .map_err(|e| OciError::RequestFailed {
            message: format!("{e}"),
        })?;

        let status2 = resp2.status().as_u16();
        if (200..300).contains(&status2) {
            return Ok(resp2);
        }
        return Err(OciError::RequestFailed {
            message: format!("HTTP {status2} from {url}"),
        });
    }

    Err(OciError::RequestFailed {
        message: format!("HTTP {status} from {url}"),
    })
}

/// Resolve the authoritative digest of a just-PUT manifest/index.
///
/// Prefers the registry's `Docker-Content-Digest` response header, falling back
/// to hashing the exact bytes we sent. A conformant registry echoes the digest
/// of those bytes — but one that re-canonicalizes the manifest stores (and
/// addresses) a different digest, so the value a caller pins (e.g. a Kubernetes
/// `volume.image` reference) must come from the registry whenever it provides one.
pub(super) fn resolve_pushed_digest(resp: &Response<Body>, sent_bytes: &[u8]) -> String {
    header(resp, "Docker-Content-Digest")
        .or_else(|| header(resp, "docker-content-digest"))
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| sha256_digest(sent_bytes))
}

/// Upload a blob to the registry via the monolithic upload flow.
/// POST /v2/{name}/blobs/uploads/ → PUT with digest.
pub(super) fn upload_blob(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    data: &[u8],
    content_type: &str,
) -> Result<String, OciError> {
    let digest = sha256_digest(data);

    // Check if blob already exists (HEAD)
    let head_url = format!(
        "{}/{}/blobs/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        digest
    );
    let head_result = authenticated_request(agent, "HEAD", &head_url, auth, None, None, None);
    if head_result.is_ok() {
        tracing::debug!(digest = %digest, "blob already exists, skipping upload");
        return Ok(digest);
    }

    // Start upload
    let upload_url = format!(
        "{}/{}/blobs/uploads/",
        oci_ref.api_base(),
        oci_ref.repository,
    );

    let resp = authenticated_request(agent, "POST", &upload_url, auth, None, None, Some(&[]))
        .map_err(|e| OciError::BlobUploadFailed {
            digest: digest.clone(),
            message: format!("upload initiation failed: {e}"),
        })?;

    let location = header(&resp, "Location")
        .ok_or_else(|| OciError::BlobUploadFailed {
            digest: digest.clone(),
            message: "no Location header in upload response".to_string(),
        })?
        .to_string();

    // Complete upload with PUT — monolithic upload
    let sep = if location.contains('?') { "&" } else { "?" };
    let put_url = format!("{location}{sep}digest={digest}");

    // For the PUT, we need to handle the case where location is a relative URL
    let put_url = if put_url.starts_with("http://") || put_url.starts_with("https://") {
        put_url
    } else {
        format!(
            "{}{}",
            oci_ref
                .api_base()
                .trim_end_matches("/v2")
                .trim_end_matches("/v2/"),
            put_url
        )
    };

    authenticated_request(
        agent,
        "PUT",
        &put_url,
        auth,
        None,
        Some(content_type),
        Some(data),
    )
    .map_err(|e| OciError::BlobUploadFailed {
        digest: digest.clone(),
        message: format!("blob PUT failed: {e}"),
    })?;

    tracing::debug!(digest = %digest, size = data.len(), "blob uploaded");
    Ok(digest)
}

/// Download a blob's bytes from `oci_ref`'s repository.
///
/// GETs `{api_base}/{repo}/blobs/{digest}` and reads the body (capped at
/// [`MAX_BLOB_SIZE`]). The caller is responsible for digest verification; this
/// helper only transports bytes.
pub(super) fn fetch_blob(
    agent: &ureq::Agent,
    oci_ref: &OciReference,
    auth: Option<&RegistryAuth>,
    digest: &str,
) -> Result<Vec<u8>, OciError> {
    let blob_url = format!(
        "{}/{}/blobs/{}",
        oci_ref.api_base(),
        oci_ref.repository,
        digest
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
        digest: format!("{digest}: {e}"),
    })?;

    let mut data = Vec::new();
    resp.into_body()
        .into_reader()
        .take(MAX_BLOB_SIZE + 1024)
        .read_to_end(&mut data)?;

    if data.len() as u64 > MAX_BLOB_SIZE {
        return Err(OciError::RequestFailed {
            message: format!("blob {digest} exceeds maximum allowed size ({MAX_BLOB_SIZE} bytes)"),
        });
    }

    Ok(data)
}

/// Ensure the blob `digest` is present in the destination repository `dst`.
///
/// Resolution order:
/// 1. HEAD `{dst}/blobs/{digest}` — if it returns 200, the blob is already
///    present and nothing more is done.
/// 2. Same-registry cross-repo mount — when `src.registry == dst.registry`,
///    POST `{dst}/blobs/uploads/?mount=<digest>&from=<src repo>`. A `201`
///    means the registry mounted the blob directly (no bytes transferred).
///    Any other status falls through to the copy path.
/// 3. Copy — `fetch_blob(src)` → verify `sha256_digest(bytes) == digest`
///    (guards against a registry serving the wrong content) → `upload_blob(dst)`.
pub(super) fn ensure_blob_present(
    agent: &ureq::Agent,
    src: &OciReference,
    dst: &OciReference,
    auth_src: Option<&RegistryAuth>,
    auth_dst: Option<&RegistryAuth>,
    digest: &str,
    media_type: &str,
) -> Result<(), OciError> {
    // 1. Already present in the destination repo?
    let head_url = format!("{}/{}/blobs/{}", dst.api_base(), dst.repository, digest);
    if authenticated_request(agent, "HEAD", &head_url, auth_dst, None, None, None).is_ok() {
        tracing::debug!(digest = %digest, "base blob already present in target, skipping");
        return Ok(());
    }

    // 2. Same-registry cross-repo mount — avoids transferring the bytes.
    if src.registry == dst.registry {
        let mount_url = format!(
            "{}/{}/blobs/uploads/?mount={}&from={}",
            dst.api_base(),
            dst.repository,
            digest,
            src.repository
        );
        if let Ok(resp) =
            authenticated_request(agent, "POST", &mount_url, auth_dst, None, None, Some(&[]))
            && resp.status().as_u16() == 201
        {
            tracing::debug!(digest = %digest, "base blob mounted from source repo");
            return Ok(());
        }
    }

    // 3. Copy: fetch from source, verify, re-upload to destination.
    let bytes = fetch_blob(agent, src, auth_src, digest)?;
    let actual = sha256_digest(&bytes);
    if actual != digest {
        return Err(OciError::RequestFailed {
            message: format!("base blob digest mismatch: expected {digest}, got {actual}"),
        });
    }
    upload_blob(agent, dst, auth_dst, &bytes, media_type)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::ReferenceKind;
    use crate::oci::test_helpers::registry_from_url;

    #[test]
    fn upload_blob_posts_then_puts() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry: registry.clone(),
            repository: "test/mod".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        let data = b"hello blob data";
        let expected_digest = sha256_digest(data);

        // HEAD check returns 404 (blob doesn't exist)
        let head_mock = server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .create();

        // POST to initiate upload -> 202 with Location
        let upload_location = format!("{}/v2/test/mod/blobs/uploads/some-uuid", server.url());
        let post_mock = server
            .mock("POST", "/v2/test/mod/blobs/uploads/")
            .with_status(202)
            .with_header("Location", &upload_location)
            .create();

        // PUT to complete upload
        let put_mock = server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/test/mod/blobs/uploads/some-uuid\?digest=sha256:.*".to_string(),
                ),
            )
            .with_status(201)
            .create();

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();

        let result = upload_blob(&agent, &oci_ref, None, data, "application/octet-stream");
        assert!(result.is_ok(), "upload_blob failed: {:?}", result.err());
        assert_eq!(result.unwrap(), expected_digest);

        head_mock.assert();
        post_mock.assert();
        put_mock.assert();
    }

    #[test]
    fn upload_blob_skips_when_already_exists() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/mod".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        let data = b"existing blob";
        let expected_digest = sha256_digest(data);

        // HEAD check returns 200 (blob exists)
        let head_mock = server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
            )
            .with_status(200)
            .create();

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();

        let result = upload_blob(&agent, &oci_ref, None, data, "application/octet-stream");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), expected_digest);

        head_mock.assert();
        // POST and PUT should NOT have been called
    }

    #[test]
    fn upload_blob_handles_relative_location() {
        let mut server = mockito::Server::new();
        let registry = registry_from_url(&server.url());

        let oci_ref = OciReference {
            registry,
            repository: "test/mod".to_string(),
            reference: ReferenceKind::Tag("v1".to_string()),
        };

        let data = b"blob with relative location";

        // HEAD returns 404
        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .create();

        // POST returns relative Location
        server
            .mock("POST", "/v2/test/mod/blobs/uploads/")
            .with_status(202)
            .with_header("Location", "/v2/test/mod/blobs/uploads/rel-uuid")
            .create();

        // PUT with relative path (should be made absolute)
        let put_mock = server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/test/mod/blobs/uploads/rel-uuid\?digest=sha256:.*".to_string(),
                ),
            )
            .with_status(201)
            .create();

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();

        let result = upload_blob(&agent, &oci_ref, None, data, "application/octet-stream");
        assert!(
            result.is_ok(),
            "upload_blob with relative location failed: {:?}",
            result.err()
        );
        put_mock.assert();
    }

    #[test]
    fn authenticated_request_handles_401_token_exchange() {
        let mut server = mockito::Server::new();

        let token_url = format!("{}/token", server.url());
        let www_auth = format!(
            r#"Bearer realm="{token_url}",service="test.io",scope="repository:test/repo:pull""#,
        );

        // First request returns 401 with Www-Authenticate
        server
            .mock("GET", "/v2/test/repo/manifests/v1")
            .with_status(401)
            .with_header("Www-Authenticate", &www_auth)
            .expect(1)
            .create();

        // Token endpoint returns a bearer token
        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/token\?service=.*&scope=.*".to_string()),
            )
            .with_status(200)
            .with_body(r#"{"token":"my-bearer-token"}"#)
            .create();

        // Retry with bearer token succeeds
        server
            .mock("GET", "/v2/test/repo/manifests/v1")
            .match_header("Authorization", "Bearer my-bearer-token")
            .with_status(200)
            .with_body("manifest content")
            .create();

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();

        let url = format!("{}/v2/test/repo/manifests/v1", server.url());
        let result = authenticated_request(&agent, "GET", &url, None, None, None, None);
        assert!(
            result.is_ok(),
            "authenticated_request failed: {:?}",
            result.err()
        );
    }

    #[test]
    fn authenticated_request_fails_on_401_without_bearer() {
        let mut server = mockito::Server::new();

        // 401 without Bearer challenge
        server
            .mock("GET", "/v2/test/repo/manifests/v1")
            .with_status(401)
            .with_header("Www-Authenticate", "Basic realm=\"test\"")
            .create();

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();

        let url = format!("{}/v2/test/repo/manifests/v1", server.url());
        let result = authenticated_request(&agent, "GET", &url, None, None, None, None);
        assert!(matches!(result, Err(OciError::AuthFailed { .. })));
    }

    #[test]
    fn authenticated_request_passes_basic_auth() {
        let mut server = mockito::Server::new();

        let auth = RegistryAuth {
            username: "user".to_string(),
            password: "pass".to_string(),
        };

        server
            .mock("GET", "/v2/test/repo/tags/list")
            .match_header(
                "Authorization",
                mockito::Matcher::Regex("Basic .*".to_string()),
            )
            .with_status(200)
            .with_body("{}")
            .create();

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();

        let url = format!("{}/v2/test/repo/tags/list", server.url());
        let result = authenticated_request(&agent, "GET", &url, Some(&auth), None, None, None);
        let resp = result.expect("authenticated request should succeed with basic auth");
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[test]
    fn authenticated_request_sends_body_with_put() {
        let mut server = mockito::Server::new();

        let body_content = b"test manifest content";
        server
            .mock("PUT", "/v2/test/repo/manifests/v1")
            .match_header("Content-Type", "application/vnd.oci.image.manifest.v1+json")
            .match_body(mockito::Matcher::Any)
            .with_status(201)
            .create();

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();

        let url = format!("{}/v2/test/repo/manifests/v1", server.url());
        let result = authenticated_request(
            &agent,
            "PUT",
            &url,
            None,
            None,
            Some("application/vnd.oci.image.manifest.v1+json"),
            Some(body_content),
        );
        let resp = result.expect("PUT with body should succeed");
        assert_eq!(resp.status().as_u16(), 201);
    }

    fn put_response_with(headers: &[(&str, &str)]) -> Response<Body> {
        let mut server = mockito::Server::new();
        let mut m = server
            .mock("PUT", "/v2/test/repo/manifests/v1")
            .with_status(201);
        for (k, v) in headers {
            m = m.with_header(*k, v);
        }
        m.create();
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();
        let url = format!("{}/v2/test/repo/manifests/v1", server.url());
        authenticated_request(&agent, "PUT", &url, None, None, None, Some(b"x"))
            .expect("PUT should succeed")
    }

    #[test]
    fn resolve_pushed_digest_prefers_registry_header() {
        let resp = put_response_with(&[("Docker-Content-Digest", "sha256:deadbeef")]);
        // Header value wins over the local hash of the sent bytes.
        assert_eq!(
            resolve_pushed_digest(&resp, b"different bytes"),
            "sha256:deadbeef"
        );
    }

    #[test]
    fn resolve_pushed_digest_falls_back_when_header_absent() {
        let resp = put_response_with(&[]);
        // Pin a precomputed digest (not `sha256_digest(sent)`) so the test fails
        // if the fallback ever hashes the wrong bytes or the algorithm drifts.
        assert_eq!(
            resolve_pushed_digest(&resp, b"manifest bytes"),
            "sha256:66444334cfc47d81840f88c1e690564d3c130e9237482a58d7631124e9b9a815"
        );
    }

    #[test]
    fn resolve_pushed_digest_falls_back_when_header_empty() {
        let resp = put_response_with(&[("Docker-Content-Digest", "")]);
        // An empty header must be ignored, not returned — fall back to the
        // precomputed digest of the sent bytes.
        assert_eq!(
            resolve_pushed_digest(&resp, b"manifest bytes"),
            "sha256:66444334cfc47d81840f88c1e690564d3c130e9237482a58d7631124e9b9a815"
        );
    }

    #[test]
    fn authenticated_request_returns_error_on_500() {
        let mut server = mockito::Server::new();

        server
            .mock("GET", "/v2/test/repo/tags/list")
            .with_status(500)
            .create();

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();

        let url = format!("{}/v2/test/repo/tags/list", server.url());
        let result = authenticated_request(&agent, "GET", &url, None, None, None, None);
        assert!(matches!(result, Err(OciError::RequestFailed { .. })));
    }

    #[test]
    fn authenticated_request_returns_request_failed_on_unreachable_host() {
        // Pointing the agent at a closed port triggers the catch-all
        // `Err(e) => Err(OciError::RequestFailed { ... })` arm.
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_millis(250)))
            .build()
            .new_agent();

        // 127.0.0.1:1 is reserved/unused; the connection refuses immediately.
        let url = "http://127.0.0.1:1/v2/test/repo/tags/list";
        let result = authenticated_request(&agent, "GET", url, None, None, None, None);
        assert!(matches!(result, Err(OciError::RequestFailed { .. })));
    }
}

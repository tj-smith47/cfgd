// Authenticated HTTP transport: 401-with-Bearer-challenge token exchange,
// blob upload (HEAD existence check + monolithic POST/PUT flow).

use crate::errors::OciError;
use crate::sha256_digest;

use super::OciReference;
use super::auth::{RegistryAuth, get_bearer_token};

/// Make an authenticated request. Handles 401 → token exchange flow.
pub(super) fn authenticated_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    auth: Option<&RegistryAuth>,
    accept: Option<&str>,
    content_type: Option<&str>,
    body: Option<&[u8]>,
) -> Result<ureq::Response, OciError> {
    // First attempt — may get 401
    let mut req = match method {
        "GET" => agent.get(url),
        "PUT" => agent.put(url),
        "POST" => agent.post(url),
        "HEAD" => agent.head(url),
        "PATCH" => agent.request("PATCH", url),
        _ => agent.get(url),
    };

    if let Some(ct) = content_type {
        req = req.set("Content-Type", ct);
    }
    if let Some(acc) = accept {
        req = req.set("Accept", acc);
    }
    if let Some(cred) = auth {
        req = req.set("Authorization", &cred.basic_auth_header());
    }

    let result = if let Some(b) = body {
        req.send_bytes(b)
    } else {
        req.call()
    };

    match result {
        Ok(resp) => Ok(resp),
        Err(ureq::Error::Status(401, resp)) => {
            // Get the Www-Authenticate header and try token auth
            let www_auth = resp
                .header("Www-Authenticate")
                .or_else(|| resp.header("www-authenticate"))
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
            let mut req2 = match method {
                "GET" => agent.get(url),
                "PUT" => agent.put(url),
                "POST" => agent.post(url),
                "HEAD" => agent.head(url),
                "PATCH" => agent.request("PATCH", url),
                _ => agent.get(url),
            };

            if let Some(ct) = content_type {
                req2 = req2.set("Content-Type", ct);
            }
            if let Some(acc) = accept {
                req2 = req2.set("Accept", acc);
            }
            req2 = req2.set("Authorization", &format!("Bearer {}", token));

            let result2 = if let Some(b) = body {
                req2.send_bytes(b)
            } else {
                req2.call()
            };

            result2.map_err(|e| OciError::RequestFailed {
                message: format!("{e}"),
            })
        }
        Err(ureq::Error::Status(code, _)) => Err(OciError::RequestFailed {
            message: format!("HTTP {code} from {url}"),
        }),
        Err(e) => Err(OciError::RequestFailed {
            message: format!("{e}"),
        }),
    }
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

    let location = resp
        .header("Location")
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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let url = format!("{}/v2/test/repo/tags/list", server.url());
        let result = authenticated_request(&agent, "GET", &url, Some(&auth), None, None, None);
        let resp = result.expect("authenticated request should succeed with basic auth");
        assert_eq!(resp.status(), 200);
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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

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
        assert_eq!(resp.status(), 201);
    }

    #[test]
    fn authenticated_request_returns_error_on_500() {
        let mut server = mockito::Server::new();

        server
            .mock("GET", "/v2/test/repo/tags/list")
            .with_status(500)
            .create();

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let url = format!("{}/v2/test/repo/tags/list", server.url());
        let result = authenticated_request(&agent, "GET", &url, None, None, None, None);
        assert!(matches!(result, Err(OciError::RequestFailed { .. })));
    }

    #[test]
    fn authenticated_request_returns_request_failed_on_unreachable_host() {
        // Pointing the agent at a closed port triggers the catch-all
        // `Err(e) => Err(OciError::RequestFailed { ... })` arm.
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_millis(250))
            .build();

        // 127.0.0.1:1 is reserved/unused; the connection refuses immediately.
        let url = "http://127.0.0.1:1/v2/test/repo/tags/list";
        let result = authenticated_request(&agent, "GET", url, None, None, None, None);
        assert!(matches!(result, Err(OciError::RequestFailed { .. })));
    }
}

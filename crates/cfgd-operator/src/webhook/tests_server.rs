//! End-to-end tests for `run_webhook_server` — drive the production TLS
//! entry point against an in-process self-signed cert and a stub kube
//! client. Verifies the bind/accept/TLS-handshake loop and at least one
//! HTTP request round-trip through the production path.
#![cfg(test)]

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use kube::Client;
use prometheus_client::registry::Registry;
use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::ServerName;
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tower::service_fn;

use super::run_webhook_server;
use crate::errors::OperatorError;
use crate::metrics::{Metrics, WebhookLabels};

// Test-side rustls crypto provider install — one-shot, ignored if already set.
fn install_crypto_provider() {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
}

fn stub_kube_client() -> Client {
    let svc = service_fn(|_req: hyper::Request<kube::client::Body>| async {
        Ok::<_, std::convert::Infallible>(
            hyper::Response::builder()
                .status(200)
                .body(kube::client::Body::from(Bytes::from_static(b"{}")))
                .expect("test response must build"),
        )
    });
    Client::new(svc, "default")
}

fn fresh_metrics() -> Metrics {
    let mut registry = Registry::default();
    Metrics::new(&mut registry)
}

/// Write a fresh self-signed cert + key pair into `dir` as `tls.crt` /
/// `tls.key`. Returns the DER-encoded cert so the test client can pin it
/// as its sole trust anchor.
fn write_self_signed(dir: &std::path::Path) -> rustls::pki_types::CertificateDer<'static> {
    let key_pair = KeyPair::generate().expect("generate key pair");
    let mut params = CertificateParams::new(vec!["localhost".to_string()])
        .expect("certificate params from san list");
    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress("127.0.0.1".parse().unwrap()));
    let cert = params
        .self_signed(&key_pair)
        .expect("self-sign certificate");

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    let cert_path = dir.join("tls.crt");
    let key_path = dir.join("tls.key");

    let mut cf = std::fs::File::create(&cert_path).expect("create cert file");
    cf.write_all(cert_pem.as_bytes()).expect("write cert");
    let mut kf = std::fs::File::create(&key_path).expect("create key file");
    kf.write_all(key_pem.as_bytes()).expect("write key");

    cert.der().clone()
}

/// Build a TLS connector that trusts only the supplied DER cert. Used
/// from tests so the self-signed cert produced inline is accepted
/// without weakening rustls validation.
fn pinned_tls_connector(cert_der: rustls::pki_types::CertificateDer<'static>) -> TlsConnector {
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    roots.add(cert_der).expect("pin self-signed cert");
    let cfg = tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(cfg))
}

/// Open a fresh TLS connection to `addr` using the pinned connector and
/// return a hyper client sender bound to that connection. Each
/// invocation establishes a brand new TCP + TLS session — keeps the
/// test linear and avoids connection pooling subtleties.
async fn https_sender(
    addr: std::net::SocketAddr,
    connector: &TlsConnector,
) -> hyper::client::conn::http1::SendRequest<http_body_util::Full<Bytes>> {
    let stream = TcpStream::connect(addr).await.expect("tcp connect");
    let domain = ServerName::try_from("localhost").expect("server name");
    let tls = connector
        .connect(domain, stream)
        .await
        .expect("tls handshake");
    let io = TokioIo::new(tls);
    let (sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("http handshake");
    tokio::spawn(async move {
        // Drive the connection in the background; ignore the result —
        // dropping the sender closes it.
        let _ = conn.await;
    });
    sender
}

// ---------------------------------------------------------------------------
// Happy path — server accepts a TLS GET /healthz and replies "ok"
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_webhook_server_serves_healthz_over_tls() {
    install_crypto_provider();

    let dir = TempDir::new().expect("tempdir");
    let cert_der = write_self_signed(dir.path());
    let connector = pinned_tls_connector(cert_der);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind 0");
    let local_addr = listener.local_addr().expect("local addr");

    let metrics = fresh_metrics();
    let client = stub_kube_client();
    let cert_dir = dir.path().to_string_lossy().into_owned();

    let server =
        tokio::spawn(async move { run_webhook_server(&cert_dir, listener, metrics, client).await });

    // Issue GET /healthz over the pinned TLS connector.
    let mut sender = https_sender(local_addr, &connector).await;
    let request = hyper::Request::builder()
        .method("GET")
        .uri("/healthz")
        .header("host", "localhost")
        .body(http_body_util::Full::new(Bytes::new()))
        .expect("build request");
    let response = timeout(Duration::from_secs(5), sender.send_request(request))
        .await
        .expect("response within deadline")
        .expect("response ok");

    assert_eq!(response.status(), hyper::StatusCode::OK);
    let body_bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("collect body")
        .to_bytes();
    assert_eq!(body_bytes.as_ref(), b"ok");

    server.abort();
    let _ = server.await;
    // Keep `dir` alive until here so the cert files outlive the request.
    drop(dir);
}

// ---------------------------------------------------------------------------
// Happy path — POST /validate-machineconfig over TLS exercises the full
// production stack (TLS accept → router dispatch → AdmissionResponse JSON).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_webhook_server_handles_admission_post_over_tls() {
    install_crypto_provider();

    let dir = TempDir::new().expect("tempdir");
    let cert_der = write_self_signed(dir.path());
    let connector = pinned_tls_connector(cert_der);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind 0");
    let local_addr = listener.local_addr().expect("local addr");

    let metrics = fresh_metrics();
    let metrics_for_assert = metrics.clone();
    let client = stub_kube_client();
    let cert_dir = dir.path().to_string_lossy().into_owned();

    let server =
        tokio::spawn(async move { run_webhook_server(&cert_dir, listener, metrics, client).await });

    let review_body = serde_json::to_vec(&serde_json::json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "tls-uid",
            "kind": {"group": "cfgd.io", "version": "v1alpha1", "kind": "MachineConfig"},
            "resource": {"group": "cfgd.io", "version": "v1alpha1", "resource": "machineconfigs"},
            "operation": "CREATE",
            "userInfo": {"username": "tls-test"},
            "object": {
                "apiVersion": "cfgd.io/v1alpha1",
                "kind": "MachineConfig",
                "metadata": {"name": "t", "namespace": "default"},
                "spec": {"hostname": "tls-host", "profile": "default"},
            }
        }
    }))
    .expect("review json");

    let mut sender = https_sender(local_addr, &connector).await;
    let request = hyper::Request::builder()
        .method("POST")
        .uri("/validate-machineconfig")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .body(http_body_util::Full::new(Bytes::from(review_body)))
        .expect("build request");

    let response = timeout(Duration::from_secs(5), sender.send_request(request))
        .await
        .expect("response within deadline")
        .expect("response ok");

    assert_eq!(response.status(), hyper::StatusCode::OK);
    let body_bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("collect body")
        .to_bytes();
    let review: kube::core::admission::AdmissionReview<kube::core::DynamicObject> =
        serde_json::from_slice(&body_bytes).expect("parse review");
    let resp = review.response.expect("response present");
    assert!(resp.allowed, "valid machineconfig must be allowed over TLS");

    let allowed = metrics_for_assert
        .webhook_requests_total
        .get_or_create(&WebhookLabels {
            operation: "validate_machineconfig".to_string(),
            result: "allowed".to_string(),
        })
        .get();
    assert_eq!(
        allowed, 1,
        "TLS-served admission request must record metric"
    );

    server.abort();
    let _ = server.await;
    drop(dir);
}

// ---------------------------------------------------------------------------
// Sad path — empty cert file yields the explicit "no certificates found"
// branch. This exercises L42-47 in mod.rs which the cert-loader unit
// tests don't cover (they exercise load_certs but not the empty-vec
// guard inside run_webhook_server).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_webhook_server_errors_when_cert_pem_has_no_certificates() {
    install_crypto_provider();

    let dir = TempDir::new().expect("tempdir");
    // Empty PEM file: rustls_pemfile returns Ok(empty) — the function's
    // explicit `if certs.is_empty()` guard fires.
    std::fs::write(dir.path().join("tls.crt"), b"").expect("write empty cert");
    std::fs::write(
        dir.path().join("tls.key"),
        concat!(
            "-----BEGIN PRIVATE KEY-----\n",
            "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQga3KUJ4hz1zMz6GHC\n",
            "Ez1DepmVH1duH9pbtd+sHCkfAh2hRANCAAR6OpUdKpU3veAXvpMtRlaobAfv15zM\n",
            "VUBq/Ykf/sveBO+h+09KlFAwgDoBfpeQni8dxRrW2/AGxYF06hExdOxQ\n",
            "-----END PRIVATE KEY-----\n"
        )
        .as_bytes(),
    )
    .expect("write key");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let metrics = fresh_metrics();
    let client = stub_kube_client();

    let result = run_webhook_server(&dir.path().to_string_lossy(), listener, metrics, client).await;
    match result {
        Err(OperatorError::Webhook(msg)) => {
            assert!(
                msg.contains("no certificates found"),
                "expected 'no certificates found' error, got: {msg}"
            );
        }
        other => panic!("expected Webhook error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Sad path — missing cert file yields the load_certs open-error branch
// surfaced through run_webhook_server's `?` propagation.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_webhook_server_errors_when_cert_file_missing() {
    install_crypto_provider();

    let dir = TempDir::new().expect("tempdir");
    // No tls.crt at all.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let metrics = fresh_metrics();
    let client = stub_kube_client();

    let result = run_webhook_server(&dir.path().to_string_lossy(), listener, metrics, client).await;
    match result {
        Err(OperatorError::Webhook(msg)) => {
            assert!(
                msg.contains("failed to open cert file"),
                "expected 'failed to open cert file' error, got: {msg}"
            );
        }
        other => panic!("expected Webhook error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Sad path — TLS handshake failure (plain HTTP client against TLS port)
// must NOT crash the accept loop. The server logs and continues serving
// the next connection.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_webhook_server_recovers_from_failed_tls_handshake() {
    install_crypto_provider();

    let dir = TempDir::new().expect("tempdir");
    let cert_der = write_self_signed(dir.path());
    let connector = pinned_tls_connector(cert_der);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind 0");
    let local_addr = listener.local_addr().expect("local addr");

    let metrics = fresh_metrics();
    let client = stub_kube_client();
    let cert_dir = dir.path().to_string_lossy().into_owned();

    let server =
        tokio::spawn(async move { run_webhook_server(&cert_dir, listener, metrics, client).await });

    // First connection: open TCP, write garbage, close — TLS handshake fails.
    {
        use tokio::io::AsyncWriteExt;
        let mut bad = TcpStream::connect(local_addr).await.expect("bad tcp");
        bad.write_all(b"GET / HTTP/1.0\r\n\r\n")
            .await
            .expect("write garbage");
        drop(bad);
    }

    // Second connection: must still succeed — proves accept loop survived.
    let mut sender = https_sender(local_addr, &connector).await;
    let request = hyper::Request::builder()
        .method("GET")
        .uri("/healthz")
        .header("host", "localhost")
        .body(http_body_util::Full::new(Bytes::new()))
        .expect("build request");
    let response = timeout(Duration::from_secs(5), sender.send_request(request))
        .await
        .expect("response within deadline")
        .expect("response ok");
    assert_eq!(response.status(), hyper::StatusCode::OK);

    server.abort();
    let _ = server.await;
    drop(dir);
}

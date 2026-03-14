use std::path::Path;
use std::sync::Arc;

use axum::routing::post;
use axum::{Json, Router};
use hyper_util::rt::{TokioExecutor, TokioIo};
use kube::core::DynamicObject;
use kube::core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;
use tracing::{info, warn};

use crate::crds::{ConfigPolicySpec, MachineConfigSpec};
use crate::errors::OperatorError;

pub async fn run_webhook_server(cert_dir: &str, port: u16) -> Result<(), OperatorError> {
    let cert_path = Path::new(cert_dir).join("tls.crt");
    let key_path = Path::new(cert_dir).join("tls.key");

    let certs = load_certs(&cert_path)?;
    let key = load_private_key(&key_path)?;

    let tls_config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| OperatorError::Webhook(format!("TLS config error: {e}")))?;

    let acceptor = TlsAcceptor::from(Arc::new(tls_config));

    let app = Router::new()
        .route(
            "/validate-machineconfig",
            post(handle_validate_machine_config),
        )
        .route(
            "/validate-configpolicy",
            post(handle_validate_config_policy),
        )
        .route("/healthz", axum::routing::get(|| async { "ok" }));

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| OperatorError::Webhook(format!("failed to bind {addr}: {e}")))?;

    info!(addr = %addr, "Webhook server listening");

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                warn!(error = %e, "Failed to accept TCP connection");
                continue;
            }
        };

        let acceptor = acceptor.clone();
        let app = app.clone();

        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(peer = %peer_addr, error = %e, "TLS handshake failed");
                    return;
                }
            };

            let io = TokioIo::new(tls_stream);
            let hyper_service = hyper::service::service_fn(
                move |request: hyper::Request<hyper::body::Incoming>| {
                    let app = app.clone();
                    async move {
                        match app.oneshot(request.map(axum::body::Body::new)).await {
                            Ok(response) => Ok::<_, std::convert::Infallible>(response),
                            Err(e) => match e {},
                        }
                    }
                },
            );

            if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
            {
                warn!(peer = %peer_addr, error = %e, "HTTP connection error");
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Webhook handlers — delegate to shared validation in crds/mod.rs
// ---------------------------------------------------------------------------

async fn handle_validate_machine_config(
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    let req: AdmissionRequest<DynamicObject> =
        match AdmissionReview::<DynamicObject>::try_into(review) {
            Ok(r) => r,
            Err(e) => {
                return Json(
                    AdmissionResponse::invalid(format!("bad admission request: {e}")).into_review(),
                );
            }
        };

    let resp = match validate_object_spec::<MachineConfigSpec>(&req) {
        Ok(()) => AdmissionResponse::from(&req),
        Err(reason) => AdmissionResponse::from(&req).deny(reason),
    };

    Json(resp.into_review())
}

async fn handle_validate_config_policy(
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    let req: AdmissionRequest<DynamicObject> =
        match AdmissionReview::<DynamicObject>::try_into(review) {
            Ok(r) => r,
            Err(e) => {
                return Json(
                    AdmissionResponse::invalid(format!("bad admission request: {e}")).into_review(),
                );
            }
        };

    let resp = match validate_object_spec::<ConfigPolicySpec>(&req) {
        Ok(()) => AdmissionResponse::from(&req),
        Err(reason) => AdmissionResponse::from(&req).deny(reason),
    };

    Json(resp.into_review())
}

/// Trait for CRD specs that have a validate method.
trait Validatable: serde::de::DeserializeOwned {
    fn validate(&self) -> Result<(), Vec<String>>;
}

impl Validatable for MachineConfigSpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        MachineConfigSpec::validate(self)
    }
}

impl Validatable for ConfigPolicySpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        ConfigPolicySpec::validate(self)
    }
}

/// Extract spec from a DynamicObject admission request and validate it.
fn validate_object_spec<S: Validatable>(
    req: &AdmissionRequest<DynamicObject>,
) -> Result<(), String> {
    let Some(obj) = &req.object else {
        return Ok(()); // DELETE — no object to validate
    };

    let spec_value: serde_json::Value = obj
        .data
        .get("spec")
        .cloned()
        .ok_or_else(|| "spec is required".to_string())?;

    let spec: S = serde_json::from_value(spec_value).map_err(|e| format!("invalid spec: {e}"))?;

    spec.validate().map_err(|errors| errors.join("; "))
}

// ---------------------------------------------------------------------------
// TLS cert loading
// ---------------------------------------------------------------------------

fn load_certs(
    path: &Path,
) -> Result<Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>, OperatorError> {
    let file = std::fs::File::open(path).map_err(|e| {
        OperatorError::Webhook(format!("failed to open cert file {}: {e}", path.display()))
    })?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            OperatorError::Webhook(format!(
                "failed to parse certs from {}: {e}",
                path.display()
            ))
        })
}

fn load_private_key(
    path: &Path,
) -> Result<tokio_rustls::rustls::pki_types::PrivateKeyDer<'static>, OperatorError> {
    let file = std::fs::File::open(path).map_err(|e| {
        OperatorError::Webhook(format!("failed to open key file {}: {e}", path.display()))
    })?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| {
            OperatorError::Webhook(format!("failed to parse key from {}: {e}", path.display()))
        })?
        .ok_or_else(|| {
            OperatorError::Webhook(format!("no private key found in {}", path.display()))
        })
}

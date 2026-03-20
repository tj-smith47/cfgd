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

use crate::crds::{ClusterConfigPolicySpec, ConfigPolicySpec, DriftAlertSpec, MachineConfigSpec};
use crate::errors::OperatorError;
use crate::metrics::{Metrics, WebhookLabels};

pub async fn run_webhook_server(
    cert_dir: &str,
    port: u16,
    metrics: Metrics,
) -> Result<(), OperatorError> {
    let cert_path = Path::new(cert_dir).join("tls.crt");
    let key_path = Path::new(cert_dir).join("tls.key");

    let certs = load_certs(&cert_path)?;
    let key = load_private_key(&key_path)?;

    if certs.is_empty() {
        return Err(OperatorError::Webhook(format!(
            "no certificates found in {}",
            cert_path.display()
        )));
    }

    info!(
        cert_count = certs.len(),
        cert_path = %cert_path.display(),
        key_path = %key_path.display(),
        "TLS certificates loaded and validated"
    );

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
        .route(
            "/validate-clusterconfigpolicy",
            post(handle_validate_cluster_config_policy),
        )
        .route("/validate-driftalert", post(handle_validate_drift_alert))
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .with_state(metrics);

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

/// Generic webhook validation handler — shared logic for all CRD types.
fn handle_validate<S: Validatable + 'static>(
    operation: &'static str,
    metrics: &Metrics,
    review: AdmissionReview<DynamicObject>,
) -> Json<AdmissionReview<DynamicObject>> {
    let start = std::time::Instant::now();
    let req: AdmissionRequest<DynamicObject> =
        match AdmissionReview::<DynamicObject>::try_into(review) {
            Ok(r) => r,
            Err(e) => {
                record_webhook_metrics(metrics, operation, "error", start);
                return Json(
                    AdmissionResponse::invalid(format!("bad admission request: {e}")).into_review(),
                );
            }
        };

    let (resp, result) = match validate_object_spec::<S>(&req) {
        Ok(()) => (AdmissionResponse::from(&req), "allowed"),
        Err(reason) => (AdmissionResponse::from(&req).deny(reason), "denied"),
    };
    record_webhook_metrics(metrics, operation, result, start);

    Json(resp.into_review())
}

async fn handle_validate_machine_config(
    axum::extract::State(metrics): axum::extract::State<Metrics>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    handle_validate::<MachineConfigSpec>("validate_machineconfig", &metrics, review)
}

async fn handle_validate_config_policy(
    axum::extract::State(metrics): axum::extract::State<Metrics>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    handle_validate::<ConfigPolicySpec>("validate_configpolicy", &metrics, review)
}

async fn handle_validate_cluster_config_policy(
    axum::extract::State(metrics): axum::extract::State<Metrics>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    handle_validate::<ClusterConfigPolicySpec>("validate_clusterconfigpolicy", &metrics, review)
}

async fn handle_validate_drift_alert(
    axum::extract::State(metrics): axum::extract::State<Metrics>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    handle_validate::<DriftAlertSpec>("validate_driftalert", &metrics, review)
}

fn record_webhook_metrics(
    metrics: &Metrics,
    operation: &str,
    result: &str,
    start: std::time::Instant,
) {
    let labels = WebhookLabels {
        operation: operation.to_string(),
        result: result.to_string(),
    };
    metrics.webhook_requests_total.get_or_create(&labels).inc();
    metrics
        .webhook_duration_seconds
        .get_or_create(&labels)
        .observe(start.elapsed().as_secs_f64());
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

impl Validatable for ClusterConfigPolicySpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        ClusterConfigPolicySpec::validate(self)
    }
}

impl Validatable for DriftAlertSpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        DriftAlertSpec::validate(self)
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

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::DynamicObject;
    use kube::core::admission::{AdmissionRequest, AdmissionReview};

    fn make_review(spec_json: serde_json::Value) -> AdmissionReview<DynamicObject> {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "test-uid",
                "kind": {"group": "cfgd.io", "version": "v1alpha1", "kind": "MachineConfig"},
                "resource": {"group": "cfgd.io", "version": "v1alpha1", "resource": "machineconfigs"},
                "operation": "CREATE",
                "userInfo": {"username": "test-user"},
                "object": {
                    "apiVersion": "cfgd.io/v1alpha1",
                    "kind": "MachineConfig",
                    "metadata": {"name": "test", "namespace": "default"},
                    "spec": spec_json
                }
            }
        }))
        .expect("test review")
    }

    fn extract_req(review: AdmissionReview<DynamicObject>) -> AdmissionRequest<DynamicObject> {
        AdmissionReview::<DynamicObject>::try_into(review).expect("parse review")
    }

    #[test]
    fn validate_mc_spec_valid() {
        let review = make_review(serde_json::json!({
            "hostname": "test-host",
            "profile": "developer"
        }));
        let req = extract_req(review);
        assert!(validate_object_spec::<MachineConfigSpec>(&req).is_ok());
    }

    #[test]
    fn validate_mc_spec_empty_hostname_denied() {
        let review = make_review(serde_json::json!({
            "hostname": "",
            "profile": "developer"
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<MachineConfigSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("hostname"));
    }

    #[test]
    fn validate_mc_spec_missing_spec_denied() {
        let review: AdmissionReview<DynamicObject> = serde_json::from_value(serde_json::json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "test-uid",
                "kind": {"group": "cfgd.io", "version": "v1alpha1", "kind": "MachineConfig"},
                "resource": {"group": "cfgd.io", "version": "v1alpha1", "resource": "machineconfigs"},
                "operation": "CREATE",
                "userInfo": {"username": "test-user"},
                "object": {
                    "apiVersion": "cfgd.io/v1alpha1",
                    "kind": "MachineConfig",
                    "metadata": {"name": "test", "namespace": "default"}
                }
            }
        }))
        .expect("test review");
        let req = extract_req(review);
        let result = validate_object_spec::<MachineConfigSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("spec is required"));
    }

    #[test]
    fn validate_delete_operation_allowed() {
        // DELETE operations have no object — should be allowed
        let review: AdmissionReview<DynamicObject> = serde_json::from_value(serde_json::json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "test-uid",
                "kind": {"group": "cfgd.io", "version": "v1alpha1", "kind": "MachineConfig"},
                "resource": {"group": "cfgd.io", "version": "v1alpha1", "resource": "machineconfigs"},
                "operation": "DELETE",
                "userInfo": {"username": "test-user"}
            }
        }))
        .expect("test review");
        let req = extract_req(review);
        assert!(validate_object_spec::<MachineConfigSpec>(&req).is_ok());
    }

    #[test]
    fn validate_config_policy_valid() {
        let review = make_review(serde_json::json!({
            "packages": [{"name": "vim"}],
            "requiredModules": [{"name": "corp-vpn"}]
        }));
        let req = extract_req(review);
        assert!(validate_object_spec::<ConfigPolicySpec>(&req).is_ok());
    }

    #[test]
    fn validate_config_policy_empty_package_name_denied() {
        let review = make_review(serde_json::json!({
            "packages": [{"name": ""}]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ConfigPolicySpec>(&req);
        assert!(result.is_err());
    }

    #[test]
    fn validate_drift_alert_valid() {
        let review = make_review(serde_json::json!({
            "deviceId": "dev-1",
            "machineConfigRef": {"name": "mc-1"},
            "severity": "High"
        }));
        let req = extract_req(review);
        assert!(validate_object_spec::<DriftAlertSpec>(&req).is_ok());
    }

    #[test]
    fn validate_drift_alert_empty_device_id_denied() {
        let review = make_review(serde_json::json!({
            "deviceId": "",
            "machineConfigRef": {"name": "mc-1"},
            "severity": "Low"
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<DriftAlertSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("deviceId"));
    }

    #[test]
    fn handle_validate_generic_allows_valid() {
        let metrics = test_metrics();
        let review = make_review(serde_json::json!({
            "hostname": "test-host",
            "profile": "developer"
        }));
        let result = handle_validate::<MachineConfigSpec>("test_op", &metrics, review);
        let review_resp: serde_json::Value =
            serde_json::to_value(result.0).expect("serialize response");
        let allowed = review_resp["response"]["allowed"].as_bool().unwrap();
        assert!(allowed);
    }

    #[test]
    fn handle_validate_generic_denies_invalid() {
        let metrics = test_metrics();
        let review = make_review(serde_json::json!({
            "hostname": "",
            "profile": "developer"
        }));
        let result = handle_validate::<MachineConfigSpec>("test_op", &metrics, review);
        let review_resp: serde_json::Value =
            serde_json::to_value(result.0).expect("serialize response");
        let allowed = review_resp["response"]["allowed"].as_bool().unwrap();
        assert!(!allowed);
    }

    fn test_metrics() -> Metrics {
        let mut registry = prometheus_client::registry::Registry::default();
        Metrics::new(&mut registry)
    }
}

use std::path::Path;
use std::sync::Arc;

use axum::routing::post;
use axum::{Json, Router};
use hyper_util::rt::{TokioExecutor, TokioIo};
use kube::Client;
use kube::api::{Api, ListParams};
use kube::core::DynamicObject;
use kube::core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;
use tracing::{info, warn};

use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicySpec, ConfigPolicySpec, DriftAlertSpec,
    MachineConfigSpec, ModuleSpec,
};
use crate::errors::OperatorError;
use crate::metrics::{Metrics, WebhookLabels};

#[derive(Clone)]
struct WebhookState {
    metrics: Metrics,
    client: Client,
}

pub async fn run_webhook_server(
    cert_dir: &str,
    port: u16,
    metrics: Metrics,
    client: Client,
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
        .route("/validate-module", post(handle_validate_module))
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .with_state(WebhookState { metrics, client });

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
    axum::extract::State(state): axum::extract::State<WebhookState>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    handle_validate::<MachineConfigSpec>("validate_machineconfig", &state.metrics, review)
}

async fn handle_validate_config_policy(
    axum::extract::State(state): axum::extract::State<WebhookState>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    handle_validate::<ConfigPolicySpec>("validate_configpolicy", &state.metrics, review)
}

async fn handle_validate_cluster_config_policy(
    axum::extract::State(state): axum::extract::State<WebhookState>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    handle_validate::<ClusterConfigPolicySpec>(
        "validate_clusterconfigpolicy",
        &state.metrics,
        review,
    )
}

async fn handle_validate_drift_alert(
    axum::extract::State(state): axum::extract::State<WebhookState>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    handle_validate::<DriftAlertSpec>("validate_driftalert", &state.metrics, review)
}

async fn handle_validate_module(
    axum::extract::State(state): axum::extract::State<WebhookState>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    let start = std::time::Instant::now();
    let req: AdmissionRequest<DynamicObject> =
        match AdmissionReview::<DynamicObject>::try_into(review) {
            Ok(r) => r,
            Err(e) => {
                record_webhook_metrics(&state.metrics, "validate_module", "error", start);
                return Json(
                    AdmissionResponse::invalid(format!("bad admission request: {e}")).into_review(),
                );
            }
        };

    // Structural validation first
    let validation_result = validate_object_spec::<ModuleSpec>(&req);
    if let Err(reason) = validation_result {
        record_webhook_metrics(&state.metrics, "validate_module", "denied", start);
        return Json(AdmissionResponse::from(&req).deny(reason).into_review());
    }

    // Policy enforcement: trusted registries + unsigned rejection
    if let Some(obj) = &req.object
        && let Some(spec_value) = obj.data.get("spec")
        && let Ok(spec) = serde_json::from_value::<ModuleSpec>(spec_value.clone())
        && let Err(reason) = enforce_module_policy(&state.client, &spec).await
    {
        record_webhook_metrics(&state.metrics, "validate_module", "denied", start);
        return Json(AdmissionResponse::from(&req).deny(reason).into_review());
    }

    record_webhook_metrics(&state.metrics, "validate_module", "allowed", start);
    Json(AdmissionResponse::from(&req).into_review())
}

/// Enforce ClusterConfigPolicy rules on a Module: trusted registries and unsigned rejection.
async fn enforce_module_policy(client: &Client, spec: &ModuleSpec) -> Result<(), String> {
    let ccpols: Api<ClusterConfigPolicy> = Api::all(client.clone());
    let policies = ccpols
        .list(&ListParams::default())
        .await
        .map_err(|e| format!("failed to list ClusterConfigPolicies: {e}"))?;

    let mut all_registries: Vec<String> = Vec::new();
    let mut any_disallow_unsigned = false;

    for policy in &policies {
        all_registries.extend(policy.spec.security.trusted_registries.clone());
        if !policy.spec.security.allow_unsigned {
            any_disallow_unsigned = true;
        }
    }

    check_trusted_registries(spec, &all_registries)?;
    check_unsigned_policy(spec, any_disallow_unsigned)?;
    Ok(())
}

/// Check that a Module's OCI artifact matches at least one trusted registry pattern.
fn check_trusted_registries(spec: &ModuleSpec, registries: &[String]) -> Result<(), String> {
    if let Some(ref oci_ref) = spec.oci_artifact
        && !registries.is_empty()
    {
        let matches = registries.iter().any(|pattern| {
            if let Some(prefix) = pattern.strip_suffix('*') {
                oci_ref.starts_with(prefix)
            } else {
                oci_ref.starts_with(pattern)
            }
        });
        if !matches {
            return Err(format!(
                "spec.ociArtifact '{}' does not match any trusted registry",
                oci_ref
            ));
        }
    }
    Ok(())
}

/// Reject unsigned modules when any ClusterConfigPolicy disallows unsigned.
fn check_unsigned_policy(spec: &ModuleSpec, disallow_unsigned: bool) -> Result<(), String> {
    if disallow_unsigned && spec.oci_artifact.is_some() {
        let has_signing = spec
            .signature
            .as_ref()
            .and_then(|s| s.cosign.as_ref())
            .map(|c| {
                c.keyless
                    || c.public_key
                        .as_ref()
                        .is_some_and(|pk| !pk.is_empty())
            })
            .unwrap_or(false);
        if !has_signing {
            return Err(
                "unsigned modules are not allowed: configure spec.signature.cosign with publicKey or keyless"
                    .to_string(),
            );
        }
    }
    Ok(())
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

impl Validatable for ModuleSpec {
    fn validate(&self) -> Result<(), Vec<String>> {
        ModuleSpec::validate(self)
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

    fn make_module_review(spec_json: serde_json::Value) -> AdmissionReview<DynamicObject> {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "test-uid",
                "kind": {"group": "cfgd.io", "version": "v1alpha1", "kind": "Module"},
                "resource": {"group": "cfgd.io", "version": "v1alpha1", "resource": "modules"},
                "operation": "CREATE",
                "userInfo": {"username": "test-user"},
                "object": {
                    "apiVersion": "cfgd.io/v1alpha1",
                    "kind": "Module",
                    "metadata": {"name": "test-module"},
                    "spec": spec_json
                }
            }
        }))
        .expect("test module review")
    }

    #[test]
    fn validate_module_accepts_valid() {
        let review = make_module_review(serde_json::json!({
            "packages": [{"name": "vim"}],
            "files": [{"source": "vimrc", "target": "~/.vimrc"}],
            "env": [{"name": "EDITOR", "value": "vim"}],
            "depends": ["base"],
            "ociArtifact": "registry.example.com/modules/vim:v1",
            "signature": {
                "cosign": {
                    "publicKey": "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----"
                }
            }
        }));
        let req = extract_req(review);
        assert!(validate_object_spec::<ModuleSpec>(&req).is_ok());
    }

    #[test]
    fn validate_module_rejects_empty_package_name() {
        let review = make_module_review(serde_json::json!({
            "packages": [{"name": ""}]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ModuleSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("packages"));
    }

    #[test]
    fn validate_module_rejects_malformed_oci_reference() {
        let review = make_module_review(serde_json::json!({
            "ociArtifact": "  "
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ModuleSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ociArtifact"));
    }

    #[test]
    fn validate_module_rejects_invalid_pem_key() {
        let review = make_module_review(serde_json::json!({
            "signature": {
                "cosign": {
                    "publicKey": "not-a-pem-key"
                }
            }
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ModuleSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("publicKey"));
    }

    #[test]
    fn handle_validate_module_allows_valid() {
        let metrics = test_metrics();
        let review = make_module_review(serde_json::json!({
            "packages": [{"name": "vim"}]
        }));
        let result = handle_validate::<ModuleSpec>("validate_module", &metrics, review);
        let review_resp: serde_json::Value =
            serde_json::to_value(result.0).expect("serialize response");
        let allowed = review_resp["response"]["allowed"].as_bool().unwrap();
        assert!(allowed);
    }

    #[test]
    fn handle_validate_module_denies_invalid() {
        let metrics = test_metrics();
        let review = make_module_review(serde_json::json!({
            "packages": [{"name": ""}]
        }));
        let result = handle_validate::<ModuleSpec>("validate_module", &metrics, review);
        let review_resp: serde_json::Value =
            serde_json::to_value(result.0).expect("serialize response");
        let allowed = review_resp["response"]["allowed"].as_bool().unwrap();
        assert!(!allowed);
    }

    #[test]
    fn enforce_module_policy_rejects_untrusted_registry() {
        // Test the policy logic directly — no kube client needed
        let spec = ModuleSpec {
            oci_artifact: Some("evil.registry.io/modules/hack:v1".to_string()),
            ..Default::default()
        };
        let registries = vec!["trusted.registry.io/".to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("trusted registry"));
    }

    #[test]
    fn enforce_module_policy_allows_trusted_registry() {
        let spec = ModuleSpec {
            oci_artifact: Some("trusted.registry.io/modules/vim:v1".to_string()),
            ..Default::default()
        };
        let registries = vec!["trusted.registry.io/*".to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(result.is_ok());
    }

    #[test]
    fn enforce_module_policy_rejects_unsigned_when_required() {
        let spec = ModuleSpec {
            oci_artifact: Some("registry.example.com/mod:v1".to_string()),
            ..Default::default()
        };
        let result = check_unsigned_policy(&spec, true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsigned"));
    }

    #[test]
    fn enforce_module_policy_allows_signed_when_required() {
        use crate::crds::{CosignSignature, ModuleSignature};
        let spec = ModuleSpec {
            oci_artifact: Some("registry.example.com/mod:v1".to_string()),
            signature: Some(ModuleSignature {
                cosign: Some(CosignSignature {
                    public_key: Some(
                        "-----BEGIN PUBLIC KEY-----\ndata\n-----END PUBLIC KEY-----".to_string(),
                    ),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let result = check_unsigned_policy(&spec, true);
        assert!(result.is_ok());
    }

    #[test]
    fn enforce_module_policy_allows_keyless_when_required() {
        use crate::crds::{CosignSignature, ModuleSignature};
        let spec = ModuleSpec {
            oci_artifact: Some("registry.example.com/mod:v1".to_string()),
            signature: Some(ModuleSignature {
                cosign: Some(CosignSignature {
                    keyless: true,
                    certificate_identity: Some("user@example.com".to_string()),
                    certificate_oidc_issuer: Some("https://accounts.google.com".to_string()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let result = check_unsigned_policy(&spec, true);
        assert!(result.is_ok());
    }

    fn test_metrics() -> Metrics {
        let mut registry = prometheus_client::registry::Registry::default();
        Metrics::new(&mut registry)
    }
}

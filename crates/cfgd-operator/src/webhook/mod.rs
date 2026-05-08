use std::path::Path;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
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
    ClusterConfigPolicy, ClusterConfigPolicySpec, ConfigPolicy, ConfigPolicySpec, DriftAlertSpec,
    MachineConfigSpec, Module, ModuleSpec, MountPolicy,
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

    let app = build_webhook_router(WebhookState { metrics, client });

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| OperatorError::Webhook(format!("failed to bind {addr}: {e}")))?;

    info!(addr = %addr, "webhook server listening");

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                warn!(error = %e, "failed to accept TCP connection");
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

// 1 MiB cap on AdmissionReview request bodies. Typical reviews are a few KiB;
// the kube-apiserver enforces a ~3 MiB server-side cap, but we bound it
// tighter at the webhook to shed DoS load before we spend CPU on JSON.
const WEBHOOK_MAX_BODY_BYTES: usize = 1024 * 1024;

/// Build the webhook Router with the same middleware stack the production
/// `run_webhook_server` uses. Tests drive this via `Router::oneshot`.
fn build_webhook_router(state: WebhookState) -> Router {
    Router::new()
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
        .route("/mutate-pods", post(handle_mutate_pods))
        .route("/healthz", axum::routing::get(liveness_ok))
        .layer(DefaultBodyLimit::max(WEBHOOK_MAX_BODY_BYTES))
        .with_state(state)
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

// Liveness probe for the webhook pod. Kubernetes liveness semantics are
// "the process is alive and serving" — if we're accepting TCP here we are.
// Intentionally does not consult `HealthState` (which gates readiness /
// leader status) because liveness must stay green even when the operator
// is voluntarily paused. Matches `health::healthz_handler`'s unconditional
// OK response — keep in sync if that handler ever changes.
async fn liveness_ok() -> (axum::http::StatusCode, &'static str) {
    (axum::http::StatusCode::OK, "ok")
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
            } else if pattern.ends_with('/') {
                // Pattern already has path boundary (e.g. "ghcr.io/myorg/")
                oci_ref.starts_with(pattern.as_str())
            } else {
                // Exact registry match: require full match or path-prefix boundary
                // to prevent "ghcr.io/myorg" matching "ghcr.io/myorgEVIL/image"
                oci_ref == pattern || oci_ref.starts_with(&format!("{}/", pattern))
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
            .map(|c| c.keyless || c.public_key.as_ref().is_some_and(|pk| !pk.is_empty()))
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

// ---------------------------------------------------------------------------
// Pod module mutating webhook — /mutate-pods
// ---------------------------------------------------------------------------

const MODULES_ANNOTATION: &str = cfgd_core::MODULES_ANNOTATION;

/// Parse the `cfgd.io/modules` annotation value into (name, version) pairs.
/// Format: `"name:version,name:version"` (commas separate, colons delimit name:version).
fn parse_module_annotations(value: &str) -> Vec<(String, String)> {
    value
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let (name, version) = entry.split_once(':')?;
            let name = name.trim();
            let version = version.trim();
            if name.is_empty() || version.is_empty() {
                return None;
            }
            Some((name.to_string(), version.to_string()))
        })
        .collect()
}

/// Modules collected from policies, split by mount behavior.
struct PolicyModules {
    required: Vec<String>,
    debug: Vec<String>,
}

/// Collect required and debug modules from ConfigPolicy and ClusterConfigPolicy.
/// ClusterConfigPolicy entries are filtered by their namespaceSelector.
async fn collect_policy_modules(client: &Client, namespace: &str) -> PolicyModules {
    let mut result = PolicyModules {
        required: Vec::new(),
        debug: Vec::new(),
    };

    // Namespace-scoped ConfigPolicy
    if let Ok(policies) = Api::<ConfigPolicy>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await
    {
        for policy in &policies {
            for module_ref in &policy.spec.required_modules {
                if module_ref.required && !result.required.contains(&module_ref.name) {
                    result.required.push(module_ref.name.clone());
                }
            }
            for module_ref in &policy.spec.debug_modules {
                if !result.debug.contains(&module_ref.name) {
                    result.debug.push(module_ref.name.clone());
                }
            }
        }
    }

    // Cluster-scoped ClusterConfigPolicy — filter by namespaceSelector
    let ns_labels = Api::<k8s_openapi::api::core::v1::Namespace>::all(client.clone())
        .get(namespace)
        .await
        .ok()
        .and_then(|ns| ns.metadata.labels);

    if let Ok(policies) = Api::<ClusterConfigPolicy>::all(client.clone())
        .list(&ListParams::default())
        .await
    {
        for policy in &policies {
            if !crate::controllers::matches_selector(
                ns_labels.as_ref(),
                &policy.spec.namespace_selector,
            ) {
                continue;
            }
            for module_ref in &policy.spec.required_modules {
                if module_ref.required && !result.required.contains(&module_ref.name) {
                    result.required.push(module_ref.name.clone());
                }
            }
            for module_ref in &policy.spec.debug_modules {
                if !result.debug.contains(&module_ref.name) {
                    result.debug.push(module_ref.name.clone());
                }
            }
        }
    }

    result
}

fn ptr(path: &str) -> jsonptr::PointerBuf {
    jsonptr::PointerBuf::parse(path).unwrap_or_else(|_| jsonptr::PointerBuf::default())
}

/// Build JSON patch operations to inject CSI volumes, volumeMounts, and env vars.
fn build_injection_patches(
    pod: &serde_json::Value,
    modules: &[(String, String, ModuleSpec)],
) -> Vec<json_patch::PatchOperation> {
    let mut patches = Vec::new();
    let containers = pod
        .pointer("/spec/containers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let has_volumes = pod.pointer("/spec/volumes").is_some();
    let has_init_containers = pod.pointer("/spec/initContainers").is_some();

    if modules.is_empty() {
        return patches;
    }

    // Ensure /spec/volumes exists
    if !has_volumes {
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: ptr("/spec/volumes"),
            value: serde_json::json!([]),
        }));
    }

    let mut needs_scripts_emptydir = false;

    // Ensure volumeMounts and env arrays exist on each container
    for (i, container) in containers.iter().enumerate() {
        if container.pointer("/volumeMounts").is_none() {
            patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: ptr(&format!("/spec/containers/{i}/volumeMounts")),
                value: serde_json::json!([]),
            }));
        }
        if container.pointer("/env").is_none() {
            patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: ptr(&format!("/spec/containers/{i}/env")),
                value: serde_json::json!([]),
            }));
        }
    }

    for (name, version, spec) in modules {
        let safe_name = cfgd_core::sanitize_k8s_name(name);
        let vol_name = format!("cfgd-module-{safe_name}");
        let mount_path = format!("/cfgd-modules/{safe_name}");

        // Add CSI volume
        let mut vol_attrs = serde_json::json!({
            "module": name,
            "version": version
        });
        if let Some(ref oci_ref) = spec.oci_artifact {
            vol_attrs["ociRef"] = serde_json::Value::String(oci_ref.clone());
        } else {
            warn!(
                module = name,
                "module CRD has no ociArtifact — CSI driver will use fallback registry"
            );
        }
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: ptr("/spec/volumes/-"),
            value: serde_json::json!({
                "name": vol_name,
                "csi": {
                    "driver": cfgd_core::CSI_DRIVER_NAME,
                    "readOnly": true,
                    "volumeAttributes": vol_attrs
                }
            }),
        }));

        // Debug modules: volume only, no volumeMount/env on declared containers.
        // They are only accessible via ephemeral debug containers.
        if spec.mount_policy == MountPolicy::Debug {
            continue;
        }

        // Always modules: volumeMount + env vars on each declared container
        for (i, _) in containers.iter().enumerate() {
            patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: ptr(&format!("/spec/containers/{i}/volumeMounts/-")),
                value: serde_json::json!({
                    "name": vol_name,
                    "mountPath": mount_path,
                    "readOnly": true
                }),
            }));

            for env_var in &spec.env {
                if env_var.append {
                    patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                        path: ptr(&format!("/spec/containers/{i}/env/-")),
                        value: serde_json::json!({
                            "name": env_var.name,
                            "value": format!("{}:$({})", env_var.value, env_var.name)
                        }),
                    }));
                } else {
                    patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                        path: ptr(&format!("/spec/containers/{i}/env/-")),
                        value: serde_json::json!({
                            "name": env_var.name,
                            "value": env_var.value
                        }),
                    }));
                }
            }
        }

        if spec.scripts.post_apply.is_some() {
            needs_scripts_emptydir = true;
        }
    }

    // Add init containers for modules with postApply scripts
    let script_modules: Vec<_> = modules
        .iter()
        .filter(|(_, _, spec)| spec.scripts.post_apply.is_some())
        .collect();

    if !script_modules.is_empty() {
        if !has_init_containers {
            patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: ptr("/spec/initContainers"),
                value: serde_json::json!([]),
            }));
        }

        if needs_scripts_emptydir {
            patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: ptr("/spec/volumes/-"),
                value: serde_json::json!({
                    "name": "cfgd-scripts",
                    "emptyDir": {}
                }),
            }));
        }

        for (name, _version, spec) in &script_modules {
            let safe_name = cfgd_core::sanitize_k8s_name(name);
            let script_path = spec
                .scripts
                .post_apply
                .as_deref()
                .unwrap_or("post-apply.sh");
            patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: ptr("/spec/initContainers/-"),
                value: serde_json::json!({
                    "name": format!("cfgd-init-{safe_name}"),
                    "image": "busybox:1.36",
                    "command": ["sh", "-c", format!("/cfgd-modules/{name}/{script_path}")],
                    "volumeMounts": [
                        {
                            "name": format!("cfgd-module-{safe_name}"),
                            "mountPath": format!("/cfgd-modules/{name}"),
                            "readOnly": true
                        },
                        {
                            "name": "cfgd-scripts",
                            "mountPath": "/cfgd-scripts"
                        }
                    ]
                }),
            }));
        }
    }

    patches
}

async fn handle_mutate_pods(
    axum::extract::State(state): axum::extract::State<WebhookState>,
    Json(review): Json<AdmissionReview<DynamicObject>>,
) -> Json<AdmissionReview<DynamicObject>> {
    let start = std::time::Instant::now();
    let req: AdmissionRequest<DynamicObject> =
        match AdmissionReview::<DynamicObject>::try_into(review) {
            Ok(r) => r,
            Err(e) => {
                record_webhook_metrics(&state.metrics, "mutate_pods", "error", start);
                return Json(
                    AdmissionResponse::invalid(format!("bad admission request: {e}")).into_review(),
                );
            }
        };

    let namespace = req.namespace.as_deref().unwrap_or("default");

    // Parse pod annotations for cfgd.io/modules
    let pod_obj = match &req.object {
        Some(obj) => match serde_json::to_value(obj) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "failed to serialize pod object");
                record_webhook_metrics(&state.metrics, "mutate_pods", "error", start);
                return Json(AdmissionResponse::from(&req).into_review());
            }
        },
        None => {
            record_webhook_metrics(&state.metrics, "mutate_pods", "allowed", start);
            return Json(AdmissionResponse::from(&req).into_review());
        }
    };

    let annotations = pod_obj
        .pointer("/metadata/annotations")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let annotation_modules = annotations
        .get(MODULES_ANNOTATION)
        .and_then(|v| v.as_str())
        .map(parse_module_annotations)
        .unwrap_or_default();

    // Collect required + debug modules from ConfigPolicy + ClusterConfigPolicy
    let policy = collect_policy_modules(&state.client, namespace).await;

    // Merge: annotation modules + policy required modules
    let mut all_modules: Vec<(String, String)> = annotation_modules;
    for name in &policy.required {
        if !all_modules.iter().any(|(n, _)| n == name) {
            all_modules.push((name.clone(), "latest".to_string()));
        }
    }
    // Add policy debug modules (these will use MountPolicy::Debug from the Module CRD,
    // or be treated as debug-only if the CRD has mountPolicy: Debug)
    for name in &policy.debug {
        if !all_modules.iter().any(|(n, _)| n == name) {
            all_modules.push((name.clone(), "latest".to_string()));
        }
    }

    if all_modules.is_empty() {
        record_webhook_metrics(&state.metrics, "mutate_pods", "allowed", start);
        return Json(AdmissionResponse::from(&req).into_review());
    }

    // Look up Module CRDs for each module
    let modules_api: Api<Module> = Api::all(state.client.clone());
    let mut resolved = Vec::new();

    for (name, version) in &all_modules {
        match modules_api.get(name).await {
            Ok(module) => {
                let mut spec = module.spec.clone();
                // Policy debugModules overrides the Module CRD's mountPolicy
                if policy.debug.contains(name) {
                    spec.mount_policy = MountPolicy::Debug;
                }
                resolved.push((name.clone(), version.clone(), spec));
            }
            Err(e) => {
                warn!(module = name, error = %e, "module CRD not found, skipping injection");
            }
        }
    }

    if resolved.is_empty() {
        record_webhook_metrics(&state.metrics, "mutate_pods", "allowed", start);
        return Json(AdmissionResponse::from(&req).into_review());
    }

    // Build JSON patches
    let patches = build_injection_patches(&pod_obj, &resolved);

    let resp = match AdmissionResponse::from(&req).with_patch(json_patch::Patch(patches)) {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "failed to serialize patch");
            record_webhook_metrics(&state.metrics, "mutate_pods", "error", start);
            return Json(AdmissionResponse::from(&req).into_review());
        }
    };

    let module_names: Vec<_> = resolved.iter().map(|(n, _, _)| n.as_str()).collect();
    info!(
        namespace = namespace,
        modules = ?module_names,
        "injected modules into pod"
    );

    record_webhook_metrics(&state.metrics, "mutate_pods", "patched", start);
    Json(resp.into_review())
}

#[cfg(test)]
mod test_router;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_kube_router;
#[cfg(test)]
mod tests_router;

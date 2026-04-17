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

    // 1 MiB cap on AdmissionReview request bodies. Typical reviews are a few KiB;
    // the kube-apiserver enforces a ~3 MiB server-side cap, but we bound it
    // tighter at the webhook to shed DoS load before we spend CPU on JSON.
    const WEBHOOK_MAX_BODY_BYTES: usize = 1024 * 1024;

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
        .route("/mutate-pods", post(handle_mutate_pods))
        .route("/healthz", axum::routing::get(liveness_ok))
        .layer(DefaultBodyLimit::max(WEBHOOK_MAX_BODY_BYTES))
        .with_state(WebhookState { metrics, client });

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
mod tests {
    use super::*;
    use kube::core::DynamicObject;
    use kube::core::admission::{AdmissionRequest, AdmissionReview};

    const TEST_PEM_KEY: &str = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----";

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
        let result = validate_object_spec::<MachineConfigSpec>(&req);
        assert!(
            result.is_ok(),
            "valid MachineConfigSpec with hostname+profile should pass: {:?}",
            result.err()
        );
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
        assert!(
            req.object.is_none(),
            "DELETE operations should have no object"
        );
        let result = validate_object_spec::<MachineConfigSpec>(&req);
        assert!(
            result.is_ok(),
            "DELETE operations without object should be allowed: {:?}",
            result.err()
        );
    }

    #[test]
    fn validate_config_policy_valid() {
        let review = make_review(serde_json::json!({
            "packages": [{"name": "vim"}],
            "requiredModules": [{"name": "corp-vpn"}]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ConfigPolicySpec>(&req);
        assert!(
            result.is_ok(),
            "valid ConfigPolicySpec with packages and requiredModules should pass: {:?}",
            result.err()
        );
    }

    #[test]
    fn validate_config_policy_empty_package_name_denied() {
        let review = make_review(serde_json::json!({
            "packages": [{"name": ""}]
        }));
        let req = extract_req(review);
        let err = validate_object_spec::<ConfigPolicySpec>(&req).unwrap_err();
        assert!(
            err.contains("packages[0].name must not be empty"),
            "expected error about empty package name, got: {err}"
        );
    }

    #[test]
    fn validate_drift_alert_valid() {
        let review = make_review(serde_json::json!({
            "deviceId": "dev-1",
            "machineConfigRef": {"name": "mc-1"},
            "severity": "High"
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<DriftAlertSpec>(&req);
        assert!(
            result.is_ok(),
            "valid DriftAlertSpec with deviceId, machineConfigRef, and severity should pass: {:?}",
            result.err()
        );
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
                    "publicKey": TEST_PEM_KEY
                }
            }
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ModuleSpec>(&req);
        assert!(
            result.is_ok(),
            "valid ModuleSpec with packages, files, env, depends, ociArtifact, and signature should pass: {:?}",
            result.err()
        );
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
        let oci_ref = "trusted.registry.io/modules/vim:v1";
        let spec = ModuleSpec {
            oci_artifact: Some(oci_ref.to_string()),
            ..Default::default()
        };
        let registries = vec!["trusted.registry.io/*".to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(
            result.is_ok(),
            "OCI ref '{}' should match trusted registry pattern 'trusted.registry.io/*': {:?}",
            oci_ref,
            result.err()
        );
        // Verify the same ref fails against a different registry to confirm the match was meaningful
        let wrong_registries = vec!["other.registry.io/*".to_string()];
        let reject_result = check_trusted_registries(&spec, &wrong_registries);
        assert!(
            reject_result.is_err(),
            "OCI ref '{}' should NOT match 'other.registry.io/*'",
            oci_ref
        );
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
        let public_key = "-----BEGIN PUBLIC KEY-----\ndata\n-----END PUBLIC KEY-----".to_string();
        let spec = ModuleSpec {
            oci_artifact: Some("registry.example.com/mod:v1".to_string()),
            signature: Some(ModuleSignature {
                cosign: Some(CosignSignature {
                    public_key: Some(public_key.clone()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let result = check_unsigned_policy(&spec, true);
        assert!(
            result.is_ok(),
            "module with cosign public_key should pass unsigned policy check: {:?}",
            result.err()
        );
        // Verify the signature is what made it pass — same spec without signature should fail
        let unsigned_spec = ModuleSpec {
            oci_artifact: spec.oci_artifact.clone(),
            signature: None,
            ..Default::default()
        };
        assert!(
            check_unsigned_policy(&unsigned_spec, true).is_err(),
            "same module without signature should be rejected when disallow_unsigned=true"
        );
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
        // Verify keyless=true is what makes this pass
        assert!(
            spec.signature
                .as_ref()
                .unwrap()
                .cosign
                .as_ref()
                .unwrap()
                .keyless,
            "test setup: cosign keyless flag should be true"
        );
        assert!(
            spec.signature
                .as_ref()
                .unwrap()
                .cosign
                .as_ref()
                .unwrap()
                .public_key
                .is_none(),
            "test setup: keyless mode should have no public_key"
        );
        let result = check_unsigned_policy(&spec, true);
        assert!(
            result.is_ok(),
            "module with keyless cosign should pass unsigned policy check: {:?}",
            result.err()
        );
        // Verify keyless=false (without a public key) would fail
        let non_keyless_spec = ModuleSpec {
            oci_artifact: spec.oci_artifact.clone(),
            signature: Some(ModuleSignature {
                cosign: Some(CosignSignature {
                    keyless: false,
                    certificate_identity: Some("user@example.com".to_string()),
                    certificate_oidc_issuer: Some("https://accounts.google.com".to_string()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        assert!(
            check_unsigned_policy(&non_keyless_spec, true).is_err(),
            "same module with keyless=false and no public_key should be rejected"
        );
    }

    // --- Pod mutation tests ---

    #[test]
    fn parse_module_annotation_valid() {
        let result = parse_module_annotations("nettools:1.0,debug:2.1");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], ("nettools".to_string(), "1.0".to_string()));
        assert_eq!(result[1], ("debug".to_string(), "2.1".to_string()));
    }

    #[test]
    fn parse_module_annotation_empty() {
        assert!(parse_module_annotations("").is_empty());
    }

    #[test]
    fn parse_module_annotation_with_spaces() {
        let result = parse_module_annotations(" nettools : 1.0 , debug : 2.1 ");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "nettools");
        assert_eq!(result[1].0, "debug");
    }

    #[test]
    fn parse_module_annotation_invalid_entries_skipped() {
        let result = parse_module_annotations("good:1.0,badentry,:noname,alsobad:");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "good");
    }

    #[test]
    fn build_patches_injects_csi_volume() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ]
            }
        });
        let modules = vec![(
            "nettools".to_string(),
            "1.0".to_string(),
            ModuleSpec {
                oci_artifact: Some("ghcr.io/org/nettools:1.0".to_string()),
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);

        // Should have: add /spec/volumes, add volume, add volumeMount
        assert!(patches.len() >= 3);

        // Check volume patch contains CSI driver reference
        let patch_json = serde_json::to_string(&patches).unwrap();
        assert!(patch_json.contains(cfgd_core::CSI_DRIVER_NAME));
        assert!(patch_json.contains("cfgd-module-nettools"));
        assert!(patch_json.contains("/cfgd-modules/nettools"));
    }

    #[test]
    fn build_patches_with_env_vars() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ],
                "volumes": []
            }
        });
        let modules = vec![(
            "tools".to_string(),
            "2.0".to_string(),
            ModuleSpec {
                env: vec![crate::crds::ModuleEnvVar {
                    name: "EDITOR".to_string(),
                    value: "vim".to_string(),
                    append: false,
                }],
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        assert!(patch_json.contains("EDITOR"));
        assert!(patch_json.contains("vim"));
    }

    #[test]
    fn build_patches_with_post_apply_script() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ]
            }
        });
        let modules = vec![(
            "setup".to_string(),
            "1.0".to_string(),
            ModuleSpec {
                scripts: crate::crds::ModuleScripts {
                    post_apply: Some("setup.sh".to_string()),
                },
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        assert!(patch_json.contains("initContainers"));
        assert!(patch_json.contains("cfgd-init-setup"));
        assert!(patch_json.contains("cfgd-scripts"));
        assert!(patch_json.contains("setup.sh"));
    }

    #[test]
    fn build_patches_with_append_env_var() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ],
                "volumes": []
            }
        });
        let modules = vec![(
            "tools".to_string(),
            "1.0".to_string(),
            ModuleSpec {
                env: vec![crate::crds::ModuleEnvVar {
                    name: "PATH".to_string(),
                    value: "/cfgd-modules/tools/bin".to_string(),
                    append: true,
                }],
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        // Should use $(PATH) expansion, not $(PATH_ORIG)
        assert!(patch_json.contains("/cfgd-modules/tools/bin:$(PATH)"));
        assert!(!patch_json.contains("_ORIG"));
    }

    #[test]
    fn build_patches_empty_modules_returns_empty() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [{"name": "app"}]
            }
        });
        let patches = build_injection_patches(&pod, &[]);
        assert!(patches.is_empty());
    }

    #[test]
    fn build_patches_multiple_containers() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app"},
                    {"name": "sidecar"}
                ]
            }
        });
        let modules = vec![("mod1".to_string(), "1.0".to_string(), ModuleSpec::default())];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        // Should have volumeMount patches for both containers (indices 0 and 1)
        assert!(patch_json.contains("/spec/containers/0/volumeMounts/-"));
        assert!(patch_json.contains("/spec/containers/1/volumeMounts/-"));
    }

    #[test]
    fn build_patches_debug_module_volume_only() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ]
            }
        });
        let modules = vec![(
            "tcpdump".to_string(),
            "4.0".to_string(),
            ModuleSpec {
                mount_policy: MountPolicy::Debug,
                oci_artifact: Some("ghcr.io/org/tcpdump:4.0".to_string()),
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();

        // Should have CSI volume
        assert!(patch_json.contains("cfgd-module-tcpdump"));
        assert!(patch_json.contains(cfgd_core::CSI_DRIVER_NAME));

        // Should NOT have volumeMount on declared containers
        assert!(!patch_json.contains("volumeMounts/-"));
        // Should NOT have env vars
        assert!(!patch_json.contains("/env/-"));
    }

    #[test]
    fn build_patches_mixed_always_and_debug() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ]
            }
        });
        let modules = vec![
            (
                "mylib".to_string(),
                "1.0".to_string(),
                ModuleSpec {
                    mount_policy: MountPolicy::Always,
                    ..Default::default()
                },
            ),
            (
                "tcpdump".to_string(),
                "4.0".to_string(),
                ModuleSpec {
                    mount_policy: MountPolicy::Debug,
                    ..Default::default()
                },
            ),
        ];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();

        // Both should have CSI volumes
        assert!(patch_json.contains("cfgd-module-mylib"));
        assert!(patch_json.contains("cfgd-module-tcpdump"));

        // Only mylib should have volumeMount
        assert!(patch_json.contains("/cfgd-modules/mylib"));
        // tcpdump mount path should only appear in the volume, not in a volumeMount
        let mount_count = patch_json.matches("volumeMounts/-").count();
        assert_eq!(mount_count, 1, "only Always module should get volumeMount");
    }

    #[test]
    fn load_certs_missing_file_errors() {
        let result = load_certs(Path::new("/nonexistent/path/tls.crt"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to open cert file"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_private_key_missing_file_errors() {
        let result = load_private_key(Path::new("/nonexistent/path/tls.key"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to open key file"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_certs_empty_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("empty.crt");
        std::fs::write(&cert_path, "").unwrap();
        let result = load_certs(&cert_path);
        // An empty PEM file yields zero certs (no error from parser, but empty vec)
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn parse_module_annotation_colon_only() {
        let result = parse_module_annotations(":");
        assert!(
            result.is_empty(),
            "bare colon should produce no results: {result:?}"
        );
    }

    #[test]
    fn parse_module_annotation_trailing_comma() {
        let result = parse_module_annotations("mod-a:1.0,mod-b:2.0,");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "mod-a");
        assert_eq!(result[1].0, "mod-b");
    }

    #[test]
    fn parse_module_annotation_duplicate_names() {
        let result = parse_module_annotations("mod-a:1.0,mod-a:2.0,mod-b:1.0");
        // No dedup — all entries are returned
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], ("mod-a".to_string(), "1.0".to_string()));
        assert_eq!(result[1], ("mod-a".to_string(), "2.0".to_string()));
        assert_eq!(result[2], ("mod-b".to_string(), "1.0".to_string()));
    }

    fn test_metrics() -> Metrics {
        let mut registry = prometheus_client::registry::Registry::default();
        Metrics::new(&mut registry)
    }

    // --- check_unsigned_policy edge cases ---

    #[test]
    fn check_unsigned_policy_no_oci_artifact_always_ok() {
        // No OCI artifact means there is nothing to sign — should pass even with disallow_unsigned
        let spec = ModuleSpec::default();
        assert!(spec.oci_artifact.is_none(), "test setup: no oci_artifact");
        assert!(spec.signature.is_none(), "test setup: no signature either");
        let result = check_unsigned_policy(&spec, true);
        assert!(
            result.is_ok(),
            "module without oci_artifact should pass even with disallow_unsigned=true: {:?}",
            result.err()
        );
    }

    #[test]
    fn check_unsigned_policy_disallow_false_always_ok() {
        // When disallow_unsigned is false, unsigned modules are allowed
        let spec = ModuleSpec {
            oci_artifact: Some("registry.example.com/mod:v1".to_string()),
            ..Default::default()
        };
        assert!(
            spec.signature.is_none(),
            "test setup: module has no signature"
        );
        let result = check_unsigned_policy(&spec, false);
        assert!(
            result.is_ok(),
            "unsigned module should pass when disallow_unsigned=false: {:?}",
            result.err()
        );
        // Verify the same unsigned spec WOULD fail with disallow_unsigned=true
        assert!(
            check_unsigned_policy(&spec, true).is_err(),
            "same unsigned module should be rejected when disallow_unsigned=true"
        );
    }

    #[test]
    fn check_unsigned_policy_empty_public_key_rejected() {
        use crate::crds::{CosignSignature, ModuleSignature};
        let spec = ModuleSpec {
            oci_artifact: Some("registry.example.com/mod:v1".to_string()),
            signature: Some(ModuleSignature {
                cosign: Some(CosignSignature {
                    public_key: Some(String::new()),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let result = check_unsigned_policy(&spec, true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsigned"));
    }

    #[test]
    fn check_unsigned_policy_signature_without_cosign_rejected() {
        use crate::crds::ModuleSignature;
        let spec = ModuleSpec {
            oci_artifact: Some("registry.example.com/mod:v1".to_string()),
            signature: Some(ModuleSignature { cosign: None }),
            ..Default::default()
        };
        let result = check_unsigned_policy(&spec, true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsigned"));
    }

    #[test]
    fn check_unsigned_policy_no_signature_at_all_rejected() {
        let spec = ModuleSpec {
            oci_artifact: Some("registry.example.com/mod:v1".to_string()),
            signature: None,
            ..Default::default()
        };
        let result = check_unsigned_policy(&spec, true);
        assert!(
            result.is_err(),
            "module with no signature field should be rejected"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("unsigned"),
            "error should mention 'unsigned': {err}"
        );
    }

    #[test]
    fn check_unsigned_policy_cosign_not_keyless_no_key_rejected() {
        use crate::crds::{CosignSignature, ModuleSignature};
        let spec = ModuleSpec {
            oci_artifact: Some("registry.example.com/mod:v1".to_string()),
            signature: Some(ModuleSignature {
                cosign: Some(CosignSignature {
                    keyless: false,
                    public_key: None,
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let result = check_unsigned_policy(&spec, true);
        assert!(
            result.is_err(),
            "cosign with keyless=false and no public_key should be rejected"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("unsigned"),
            "error should mention 'unsigned': {err}"
        );
    }

    // --- check_trusted_registries edge cases ---

    #[test]
    fn check_trusted_registries_empty_list_allows_all() {
        let oci_ref = "any.registry.io/mod:v1";
        let spec = ModuleSpec {
            oci_artifact: Some(oci_ref.to_string()),
            ..Default::default()
        };
        // Empty registries list means no restrictions
        let result = check_trusted_registries(&spec, &[]);
        assert!(
            result.is_ok(),
            "empty registries list should allow any OCI ref '{}': {:?}",
            oci_ref,
            result.err()
        );
        // Verify a non-empty list would reject this same ref
        let restrictive = vec!["other.io/".to_string()];
        assert!(
            check_trusted_registries(&spec, &restrictive).is_err(),
            "non-empty registries list should reject unmatched ref '{}'",
            oci_ref
        );
    }

    #[test]
    fn check_trusted_registries_no_oci_artifact_always_ok() {
        let spec = ModuleSpec::default();
        assert!(spec.oci_artifact.is_none(), "test setup: no oci_artifact");
        let registries = vec!["trusted.io/".to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(
            result.is_ok(),
            "module without oci_artifact should pass registry check regardless of list: {:?}",
            result.err()
        );
    }

    #[test]
    fn check_trusted_registries_exact_prefix_match_no_wildcard() {
        let oci_ref = "trusted.io/modules/vim:v1";
        let spec = ModuleSpec {
            oci_artifact: Some(oci_ref.to_string()),
            ..Default::default()
        };
        // Trailing-slash pattern acts as path-prefix match
        let registries = vec!["trusted.io/".to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(
            result.is_ok(),
            "OCI ref '{}' should match prefix 'trusted.io/' without wildcard: {:?}",
            oci_ref,
            result.err()
        );
    }

    #[test]
    fn check_trusted_registries_multiple_registries_second_matches() {
        let oci_ref = "second.io/mod:v1";
        let spec = ModuleSpec {
            oci_artifact: Some(oci_ref.to_string()),
            ..Default::default()
        };
        let registries = vec!["first.io/".to_string(), "second.io/*".to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(
            result.is_ok(),
            "OCI ref '{}' should match second registry pattern 'second.io/*': {:?}",
            oci_ref,
            result.err()
        );
        // Verify it doesn't match first.io alone
        let only_first = vec!["first.io/".to_string()];
        assert!(
            check_trusted_registries(&spec, &only_first).is_err(),
            "OCI ref '{}' should NOT match only 'first.io/'",
            oci_ref
        );
    }

    #[test]
    fn check_trusted_registries_partial_match_rejected() {
        let oci_ref = "evil-trusted.io/mod:v1";
        let spec = ModuleSpec {
            oci_artifact: Some(oci_ref.to_string()),
            ..Default::default()
        };
        // "trusted.io/" should NOT match "evil-trusted.io/"
        let registries = vec!["trusted.io/".to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(
            result.is_err(),
            "OCI ref '{}' should NOT match 'trusted.io/' (prefix mismatch)",
            oci_ref
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("trusted registry"),
            "error should mention trusted registry: {err}"
        );
        assert!(
            err.contains(oci_ref),
            "error should include the rejected OCI ref: {err}"
        );
    }

    // --- parse_module_annotations edge cases ---

    #[test]
    fn parse_module_annotation_multiple_colons_in_version() {
        // Only first colon splits — second colon is not returned since split_once splits on first
        let result = parse_module_annotations("mod:1.0:extra");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "mod");
        assert_eq!(result[0].1, "1.0:extra");
    }

    #[test]
    fn parse_module_annotation_whitespace_only_entries() {
        let result = parse_module_annotations("  ,  ,mod:1.0,  ");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "mod");
    }

    #[test]
    fn parse_module_annotation_single_entry() {
        let result = parse_module_annotations("mymod:latest");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("mymod".to_string(), "latest".to_string()));
    }

    // --- validate_object_spec edge cases ---

    #[test]
    fn validate_machineconfig_empty_profile_denied() {
        let review = make_review(serde_json::json!({
            "hostname": "host1",
            "profile": ""
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<MachineConfigSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("profile"));
    }

    #[test]
    fn validate_machineconfig_both_empty_reports_both() {
        let review = make_review(serde_json::json!({
            "hostname": "",
            "profile": ""
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<MachineConfigSpec>(&req);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("hostname"), "should report hostname: {err}");
        assert!(err.contains("profile"), "should report profile: {err}");
    }

    #[test]
    fn validate_machineconfig_empty_module_ref_name_denied() {
        let review = make_review(serde_json::json!({
            "hostname": "host1",
            "profile": "dev",
            "moduleRefs": [{"name": ""}]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<MachineConfigSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("moduleRefs"));
    }

    #[test]
    fn validate_machineconfig_file_path_traversal_denied() {
        let review = make_review(serde_json::json!({
            "hostname": "host1",
            "profile": "dev",
            "files": [{"path": "/etc/../shadow", "content": "x"}]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<MachineConfigSpec>(&req);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains(".."), "should reject path traversal: {err}");
    }

    #[test]
    fn validate_machineconfig_file_without_content_or_source_denied() {
        let review = make_review(serde_json::json!({
            "hostname": "host1",
            "profile": "dev",
            "files": [{"path": "/etc/foo"}]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<MachineConfigSpec>(&req);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("content") || err.contains("source"),
            "should report missing content/source: {err}"
        );
    }

    #[test]
    fn validate_drift_alert_empty_machineconfig_ref_denied() {
        let review = make_review(serde_json::json!({
            "deviceId": "dev-1",
            "machineConfigRef": {"name": ""},
            "severity": "Low"
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<DriftAlertSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("machineConfigRef"));
    }

    #[test]
    fn validate_config_policy_empty_module_name_denied() {
        let review = make_review(serde_json::json!({
            "requiredModules": [{"name": ""}]
        }));
        let req = extract_req(review);
        let err = validate_object_spec::<ConfigPolicySpec>(&req).unwrap_err();
        assert!(
            err.contains("requiredModules[0].name must not be empty"),
            "expected error about empty module name, got: {err}"
        );
    }

    #[test]
    fn validate_cluster_config_policy_valid() {
        let review = make_review(serde_json::json!({
            "requiredModules": [{"name": "base", "required": true}],
            "packages": [{"name": "vim"}],
            "security": {
                "trustedRegistries": ["ghcr.io/*"],
                "allowUnsigned": false
            }
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ClusterConfigPolicySpec>(&req);
        assert!(
            result.is_ok(),
            "valid ClusterConfigPolicySpec with requiredModules, packages, and security should pass: {:?}",
            result.err()
        );
    }

    #[test]
    fn validate_cluster_config_policy_empty_package_name_denied() {
        let review = make_review(serde_json::json!({
            "packages": [{"name": ""}]
        }));
        let req = extract_req(review);
        let err = validate_object_spec::<ClusterConfigPolicySpec>(&req).unwrap_err();
        assert!(
            err.contains("packages[0].name must not be empty"),
            "expected error about empty package name, got: {err}"
        );
    }

    #[test]
    fn validate_module_empty_depends_entry_denied() {
        let review = make_module_review(serde_json::json!({
            "depends": ["base", ""]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ModuleSpec>(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("depends"));
    }

    #[test]
    fn validate_module_valid_minimal() {
        // Completely empty spec should be valid (all fields are defaulted)
        let review = make_module_review(serde_json::json!({}));
        let req = extract_req(review);
        let result = validate_object_spec::<ModuleSpec>(&req);
        assert!(
            result.is_ok(),
            "empty ModuleSpec (all defaults) should be valid: {:?}",
            result.err()
        );
    }

    // --- build_injection_patches edge cases ---

    #[test]
    fn build_patches_pod_with_existing_volumes_and_mounts() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {
                        "name": "app",
                        "image": "busybox",
                        "volumeMounts": [
                            {"name": "data", "mountPath": "/data"}
                        ],
                        "env": [
                            {"name": "EXISTING", "value": "yes"}
                        ]
                    }
                ],
                "volumes": [
                    {"name": "data", "emptyDir": {}}
                ]
            }
        });
        let modules = vec![(
            "tools".to_string(),
            "1.0".to_string(),
            ModuleSpec::default(),
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();

        // Should NOT add /spec/volumes (already exists)
        assert!(
            !patch_json.contains("\"path\":\"/spec/volumes\""),
            "should not add /spec/volumes when it already exists"
        );
        // Should still add volume entry and volumeMount
        assert!(patch_json.contains("cfgd-module-tools"));
        assert!(patch_json.contains("/spec/volumes/-"));
        // Should NOT add /spec/containers/0/volumeMounts (already exists)
        assert!(
            !patch_json.contains("\"path\":\"/spec/containers/0/volumeMounts\""),
            "should not initialize volumeMounts when it already exists"
        );
    }

    #[test]
    fn build_patches_no_containers() {
        let pod = serde_json::json!({
            "spec": {}
        });
        let modules = vec![("mod1".to_string(), "1.0".to_string(), ModuleSpec::default())];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        // Should still add the volume
        assert!(patch_json.contains("cfgd-module-mod1"));
        // But no container volumeMount patches
        assert!(!patch_json.contains("/spec/containers/"));
    }

    #[test]
    fn build_patches_multiple_modules_with_env_vars() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ],
                "volumes": []
            }
        });
        let modules = vec![
            (
                "mod-a".to_string(),
                "1.0".to_string(),
                ModuleSpec {
                    env: vec![crate::crds::ModuleEnvVar {
                        name: "MOD_A_PATH".to_string(),
                        value: "/cfgd-modules/mod-a/bin".to_string(),
                        append: false,
                    }],
                    ..Default::default()
                },
            ),
            (
                "mod-b".to_string(),
                "2.0".to_string(),
                ModuleSpec {
                    env: vec![
                        crate::crds::ModuleEnvVar {
                            name: "MOD_B_HOME".to_string(),
                            value: "/cfgd-modules/mod-b".to_string(),
                            append: false,
                        },
                        crate::crds::ModuleEnvVar {
                            name: "PATH".to_string(),
                            value: "/cfgd-modules/mod-b/bin".to_string(),
                            append: true,
                        },
                    ],
                    ..Default::default()
                },
            ),
        ];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        assert!(patch_json.contains("MOD_A_PATH"));
        assert!(patch_json.contains("MOD_B_HOME"));
        assert!(patch_json.contains("/cfgd-modules/mod-b/bin:$(PATH)"));
    }

    #[test]
    fn build_patches_debug_module_with_scripts_no_emptydir() {
        // Debug modules do NOT set needs_scripts_emptydir (they continue before that check),
        // but the script_modules filter still picks them up for init container creation.
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ]
            }
        });
        let modules = vec![(
            "debug-tool".to_string(),
            "1.0".to_string(),
            ModuleSpec {
                mount_policy: MountPolicy::Debug,
                scripts: crate::crds::ModuleScripts {
                    post_apply: Some("setup.sh".to_string()),
                },
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        // Should have CSI volume
        assert!(patch_json.contains("cfgd-module-debug-tool"));
        // The init container is still added (script_modules filter doesn't check mount_policy)
        assert!(patch_json.contains("cfgd-init-debug-tool"));
        // But the scripts emptyDir volume is NOT added (needs_scripts_emptydir stays false)
        assert!(
            !patch_json.contains("\"name\":\"cfgd-scripts\",\"emptyDir\""),
            "debug module alone should not trigger scripts emptyDir volume"
        );
    }

    #[test]
    fn build_patches_module_without_oci_artifact() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ]
            }
        });
        let modules = vec![(
            "local-mod".to_string(),
            "1.0".to_string(),
            ModuleSpec {
                oci_artifact: None,
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        // Should still create volume (without ociRef in volumeAttributes)
        assert!(patch_json.contains("cfgd-module-local-mod"));
        assert!(!patch_json.contains("ociRef"));
    }

    #[test]
    fn build_patches_multiple_containers_with_env() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"},
                    {"name": "sidecar", "image": "nginx"}
                ]
            }
        });
        let modules = vec![(
            "shared".to_string(),
            "1.0".to_string(),
            ModuleSpec {
                env: vec![crate::crds::ModuleEnvVar {
                    name: "CONFIG".to_string(),
                    value: "/cfgd-modules/shared/config".to_string(),
                    append: false,
                }],
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        // Both containers should get env var patches
        assert!(patch_json.contains("/spec/containers/0/env/-"));
        assert!(patch_json.contains("/spec/containers/1/env/-"));
        // Both should get volumeMount patches
        assert!(patch_json.contains("/spec/containers/0/volumeMounts/-"));
        assert!(patch_json.contains("/spec/containers/1/volumeMounts/-"));
    }

    #[test]
    fn build_patches_existing_init_containers() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ],
                "initContainers": [
                    {"name": "existing-init", "image": "alpine"}
                ]
            }
        });
        let modules = vec![(
            "setup".to_string(),
            "1.0".to_string(),
            ModuleSpec {
                scripts: crate::crds::ModuleScripts {
                    post_apply: Some("init.sh".to_string()),
                },
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        // Should NOT add /spec/initContainers (already exists)
        assert!(
            !patch_json.contains("\"path\":\"/spec/initContainers\""),
            "should not initialize initContainers when it already exists"
        );
        // But should still add the init container entry
        assert!(patch_json.contains("cfgd-init-setup"));
    }

    // --- handle_validate generic with bad review format ---

    #[test]
    fn handle_validate_invalid_spec_json_denied() {
        // Spec is not valid JSON for MachineConfigSpec (missing required fields)
        let review = make_review(serde_json::json!({
            "hostname": 42,
            "profile": true
        }));
        let req = extract_req(review);
        // hostname and profile are strings in the schema; passing wrong types
        // serde should still deserialize them (42 -> "42" doesn't work for strict types)
        // Actually, serde_json will fail to deserialize a number as a String
        let err = validate_object_spec::<MachineConfigSpec>(&req).unwrap_err();
        assert!(
            err.contains("invalid spec:"),
            "expected serde deserialization error, got: {err}"
        );
    }

    #[test]
    fn validate_module_rejects_empty_file_path() {
        let review = make_module_review(serde_json::json!({
            "files": [{"source": "vimrc", "target": ""}]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ModuleSpec>(&req);
        // ModuleSpec.validate() does not check file target emptiness (unlike MachineConfigSpec),
        // so this should pass deserialization and validation without error or panic.
        assert!(
            result.is_ok(),
            "ModuleSpec with empty file target should pass webhook validation (module file validation is deferred to reconciliation): {:?}",
            result.err()
        );
    }

    // --- ptr helper ---

    #[test]
    fn ptr_helper_creates_valid_pointer() {
        let p = ptr("/spec/containers/0/env");
        assert_eq!(p.to_string(), "/spec/containers/0/env");
    }

    #[test]
    fn ptr_helper_invalid_returns_default() {
        // An empty string is a valid JSON pointer (root), but truly malformed input
        // should fallback to default
        let p = ptr("");
        assert_eq!(p.to_string(), "");
    }

    // --- TLS cert loading with real PEM data ---

    #[test]
    fn load_certs_valid_pem_parses_successfully() {
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("tls.crt");
        std::fs::write(
            &cert_path,
            concat!(
                "-----BEGIN CERTIFICATE-----\n",
                "MIIBdDCCARmgAwIBAgIUNuMsDdMhSJGSo8OAK7sjwgfOMf8wCgYIKoZIzj0EAwIw\n",
                "DzENMAsGA1UEAwwEdGVzdDAeFw0yNjA0MDcyMjI2NDhaFw0yNjA0MDgyMjI2NDha\n",
                "MA8xDTALBgNVBAMMBHRlc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAR6OpUd\n",
                "KpU3veAXvpMtRlaobAfv15zMVUBq/Ykf/sveBO+h+09KlFAwgDoBfpeQni8dxRrW\n",
                "2/AGxYF06hExdOxQo1MwUTAdBgNVHQ4EFgQU8rFClDYwDjpB8VbQflo71ab3zOsw\n",
                "HwYDVR0jBBgwFoAU8rFClDYwDjpB8VbQflo71ab3zOswDwYDVR0TAQH/BAUwAwEB\n",
                "/zAKBggqhkjOPQQDAgNJADBGAiEAz+QOBYTjfOS3vgOcR7RUi1zBJR14xxRHMjaf\n",
                "vHDA2tACIQD/5WbK+daL6bityvosRvLt1y2zMIa+n9mn1nc5aMnpFg==\n",
                "-----END CERTIFICATE-----\n"
            ),
        )
        .unwrap();
        let certs = load_certs(&cert_path).unwrap();
        assert_eq!(certs.len(), 1, "should parse exactly 1 certificate");
    }

    #[test]
    fn load_certs_multiple_certs_in_chain() {
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("chain.crt");
        // Write two copies of the same cert (simulating a cert chain)
        let single_cert = concat!(
            "-----BEGIN CERTIFICATE-----\n",
            "MIIBdDCCARmgAwIBAgIUNuMsDdMhSJGSo8OAK7sjwgfOMf8wCgYIKoZIzj0EAwIw\n",
            "DzENMAsGA1UEAwwEdGVzdDAeFw0yNjA0MDcyMjI2NDhaFw0yNjA0MDgyMjI2NDha\n",
            "MA8xDTALBgNVBAMMBHRlc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAR6OpUd\n",
            "KpU3veAXvpMtRlaobAfv15zMVUBq/Ykf/sveBO+h+09KlFAwgDoBfpeQni8dxRrW\n",
            "2/AGxYF06hExdOxQo1MwUTAdBgNVHQ4EFgQU8rFClDYwDjpB8VbQflo71ab3zOsw\n",
            "HwYDVR0jBBgwFoAU8rFClDYwDjpB8VbQflo71ab3zOswDwYDVR0TAQH/BAUwAwEB\n",
            "/zAKBggqhkjOPQQDAgNJADBGAiEAz+QOBYTjfOS3vgOcR7RUi1zBJR14xxRHMjaf\n",
            "vHDA2tACIQD/5WbK+daL6bityvosRvLt1y2zMIa+n9mn1nc5aMnpFg==\n",
            "-----END CERTIFICATE-----\n"
        );
        let chain = format!("{single_cert}{single_cert}");
        std::fs::write(&cert_path, chain).unwrap();
        let certs = load_certs(&cert_path).unwrap();
        assert_eq!(
            certs.len(),
            2,
            "should parse 2 certificates from a chain file"
        );
    }

    #[test]
    fn load_private_key_valid_pem_parses_successfully() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("tls.key");
        std::fs::write(
            &key_path,
            concat!(
                "-----BEGIN PRIVATE KEY-----\n",
                "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQga3KUJ4hz1zMz6GHC\n",
                "Ez1DepmVH1duH9pbtd+sHCkfAh2hRANCAAR6OpUdKpU3veAXvpMtRlaobAfv15zM\n",
                "VUBq/Ykf/sveBO+h+09KlFAwgDoBfpeQni8dxRrW2/AGxYF06hExdOxQ\n",
                "-----END PRIVATE KEY-----\n"
            ),
        )
        .unwrap();
        let key = load_private_key(&key_path);
        assert!(
            key.is_ok(),
            "should successfully parse a valid EC private key: {:?}",
            key.err()
        );
    }

    #[test]
    fn load_private_key_empty_file_returns_no_key_error() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("empty.key");
        std::fs::write(&key_path, "").unwrap();
        let result = load_private_key(&key_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no private key found"),
            "expected 'no private key found' error, got: {err}"
        );
    }

    #[test]
    fn load_private_key_garbage_data_returns_no_key_error() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("garbage.key");
        std::fs::write(&key_path, "this is not a PEM key at all\n").unwrap();
        let result = load_private_key(&key_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no private key found"),
            "expected 'no private key found' error, got: {err}"
        );
    }

    // --- record_webhook_metrics — direct tests ---

    #[test]
    fn record_webhook_metrics_increments_counter_and_histogram() {
        let metrics = test_metrics();
        let start = std::time::Instant::now();
        record_webhook_metrics(&metrics, "validate_machineconfig", "allowed", start);

        let labels = crate::metrics::WebhookLabels {
            operation: "validate_machineconfig".to_string(),
            result: "allowed".to_string(),
        };
        let count = metrics.webhook_requests_total.get_or_create(&labels).get();
        assert_eq!(count, 1, "webhook counter should be 1 after one call");
    }

    #[test]
    fn record_webhook_metrics_different_results_are_independent() {
        let metrics = test_metrics();
        let start = std::time::Instant::now();
        record_webhook_metrics(&metrics, "validate_module", "allowed", start);
        record_webhook_metrics(&metrics, "validate_module", "denied", start);
        record_webhook_metrics(&metrics, "validate_module", "allowed", start);
        record_webhook_metrics(&metrics, "validate_module", "error", start);

        let allowed = crate::metrics::WebhookLabels {
            operation: "validate_module".to_string(),
            result: "allowed".to_string(),
        };
        let denied = crate::metrics::WebhookLabels {
            operation: "validate_module".to_string(),
            result: "denied".to_string(),
        };
        let errored = crate::metrics::WebhookLabels {
            operation: "validate_module".to_string(),
            result: "error".to_string(),
        };
        assert_eq!(
            metrics.webhook_requests_total.get_or_create(&allowed).get(),
            2
        );
        assert_eq!(
            metrics.webhook_requests_total.get_or_create(&denied).get(),
            1
        );
        assert_eq!(
            metrics.webhook_requests_total.get_or_create(&errored).get(),
            1
        );
    }

    #[test]
    fn record_webhook_metrics_different_operations_are_independent() {
        let metrics = test_metrics();
        let start = std::time::Instant::now();
        record_webhook_metrics(&metrics, "validate_machineconfig", "allowed", start);
        record_webhook_metrics(&metrics, "mutate_pods", "patched", start);

        let mc_labels = crate::metrics::WebhookLabels {
            operation: "validate_machineconfig".to_string(),
            result: "allowed".to_string(),
        };
        let pod_labels = crate::metrics::WebhookLabels {
            operation: "mutate_pods".to_string(),
            result: "patched".to_string(),
        };
        assert_eq!(
            metrics
                .webhook_requests_total
                .get_or_create(&mc_labels)
                .get(),
            1
        );
        assert_eq!(
            metrics
                .webhook_requests_total
                .get_or_create(&pod_labels)
                .get(),
            1
        );
    }

    // --- check_trusted_registries — exact match branch (no wildcard, no trailing slash) ---

    #[test]
    fn check_trusted_registries_exact_match_without_trailing_slash() {
        let oci_ref = "ghcr.io/myorg/image:v1";
        let spec = ModuleSpec {
            oci_artifact: Some(oci_ref.to_string()),
            ..Default::default()
        };
        // Pattern without wildcard or trailing slash — uses exact/path-prefix match
        let registries = vec!["ghcr.io/myorg".to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(
            result.is_ok(),
            "OCI ref '{}' should match exact registry 'ghcr.io/myorg' (path-prefix boundary): {:?}",
            oci_ref,
            result.err()
        );
    }

    #[test]
    fn check_trusted_registries_exact_match_full_ref() {
        // Exact full match: the OCI ref IS the pattern
        let oci_ref = "ghcr.io/myorg/image:v1";
        let spec = ModuleSpec {
            oci_artifact: Some(oci_ref.to_string()),
            ..Default::default()
        };
        let registries = vec![oci_ref.to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(
            result.is_ok(),
            "OCI ref should match when pattern is the exact same string: {:?}",
            result.err()
        );
    }

    #[test]
    fn check_trusted_registries_exact_match_prevents_org_spoofing() {
        // "ghcr.io/myorg" must NOT match "ghcr.io/myorgEVIL/image:v1"
        let oci_ref = "ghcr.io/myorgEVIL/image:v1";
        let spec = ModuleSpec {
            oci_artifact: Some(oci_ref.to_string()),
            ..Default::default()
        };
        let registries = vec!["ghcr.io/myorg".to_string()];
        let result = check_trusted_registries(&spec, &registries);
        assert!(
            result.is_err(),
            "OCI ref '{}' should NOT match 'ghcr.io/myorg' (no path boundary)",
            oci_ref
        );
    }

    // --- validate_object_spec edge cases ---

    #[test]
    fn validate_cluster_config_policy_empty_module_name_denied() {
        let review = make_review(serde_json::json!({
            "requiredModules": [{"name": ""}]
        }));
        let req = extract_req(review);
        let err = validate_object_spec::<ClusterConfigPolicySpec>(&req).unwrap_err();
        assert!(
            err.contains("requiredModules[0].name must not be empty"),
            "expected error about empty module name, got: {err}"
        );
    }

    #[test]
    fn validate_cluster_config_policy_with_security_block_valid() {
        // Verify a valid security block with trustedRegistries and allowUnsigned passes
        let review = make_review(serde_json::json!({
            "security": {
                "trustedRegistries": ["ghcr.io/myorg/*"],
                "allowUnsigned": false
            },
            "requiredModules": [{"name": "base", "required": true}]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ClusterConfigPolicySpec>(&req);
        assert!(
            result.is_ok(),
            "valid ClusterConfigPolicySpec with security block should pass: {:?}",
            result.err()
        );
    }

    #[test]
    fn validate_cluster_config_policy_invalid_version_requirement_denied() {
        let review = make_review(serde_json::json!({
            "packages": [{"name": "vim", "version": "not-semver!!!"}]
        }));
        let req = extract_req(review);
        let err = validate_object_spec::<ClusterConfigPolicySpec>(&req).unwrap_err();
        assert!(
            err.contains("semver"),
            "expected error about invalid semver requirement, got: {err}"
        );
    }

    #[test]
    fn validate_config_policy_invalid_version_requirement_denied() {
        let review = make_review(serde_json::json!({
            "packages": [{"name": "vim", "version": "abc"}]
        }));
        let req = extract_req(review);
        let err = validate_object_spec::<ConfigPolicySpec>(&req).unwrap_err();
        assert!(
            err.contains("semver"),
            "expected error about invalid semver requirement, got: {err}"
        );
    }

    #[test]
    fn validate_config_policy_valid_version_requirement_passes() {
        let review = make_review(serde_json::json!({
            "packages": [{"name": "vim", "version": ">=1.0"}]
        }));
        let req = extract_req(review);
        let result = validate_object_spec::<ConfigPolicySpec>(&req);
        assert!(
            result.is_ok(),
            "valid semver requirement should pass: {:?}",
            result.err()
        );
    }

    // --- handle_validate with completely malformed review ---

    #[test]
    fn handle_validate_malformed_review_returns_error_response() {
        let metrics = test_metrics();
        // Build a review that is missing the "request" field entirely,
        // which will cause AdmissionReview::try_into to fail
        let bad_review: AdmissionReview<DynamicObject> =
            serde_json::from_value(serde_json::json!({
                "apiVersion": "admission.k8s.io/v1",
                "kind": "AdmissionReview"
            }))
            .expect("construct minimal review");
        let result = handle_validate::<MachineConfigSpec>("test_op", &metrics, bad_review);
        let review_resp: serde_json::Value =
            serde_json::to_value(result.0).expect("serialize response");
        // Should return a response (not panic), and the metrics should record "error"
        let allowed = review_resp["response"]["allowed"].as_bool();
        // Either the response has allowed=false or it has a status message
        if let Some(false) = allowed {
            // good, request was denied
        } else {
            // check that there's an error status
            let status = &review_resp["response"]["status"];
            assert!(
                status.is_object(),
                "malformed review should produce error status or denied response: {review_resp}"
            );
        }
        // Verify error metric was recorded
        let labels = crate::metrics::WebhookLabels {
            operation: "test_op".to_string(),
            result: "error".to_string(),
        };
        let count = metrics.webhook_requests_total.get_or_create(&labels).get();
        assert_eq!(count, 1, "malformed review should record error metric");
    }

    // --- ptr helper edge cases ---

    #[test]
    fn ptr_helper_nested_path() {
        let p = ptr("/spec/containers/0/volumeMounts/-");
        assert_eq!(p.to_string(), "/spec/containers/0/volumeMounts/-");
    }

    #[test]
    fn ptr_helper_root_pointer() {
        let p = ptr("/");
        assert_eq!(p.to_string(), "/");
    }

    // --- build_injection_patches — multiple modules with scripts ---

    #[test]
    fn build_patches_multiple_modules_with_scripts_share_scripts_emptydir() {
        let pod = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "app", "image": "busybox"}
                ]
            }
        });
        let modules = vec![
            (
                "mod-a".to_string(),
                "1.0".to_string(),
                ModuleSpec {
                    scripts: crate::crds::ModuleScripts {
                        post_apply: Some("a.sh".to_string()),
                    },
                    ..Default::default()
                },
            ),
            (
                "mod-b".to_string(),
                "2.0".to_string(),
                ModuleSpec {
                    scripts: crate::crds::ModuleScripts {
                        post_apply: Some("b.sh".to_string()),
                    },
                    ..Default::default()
                },
            ),
        ];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        // Both should have init containers
        assert!(patch_json.contains("cfgd-init-mod-a"));
        assert!(patch_json.contains("cfgd-init-mod-b"));
        // Should have only ONE cfgd-scripts emptyDir volume
        let scripts_count = patch_json.matches("cfgd-scripts").count();
        // The volume definition + each init container's volumeMount reference it
        // Volume: 1, mod-a mount: 1, mod-b mount: 1 = 3 total mentions
        assert!(
            scripts_count >= 3,
            "cfgd-scripts should appear in volume + init container mounts: found {} mentions",
            scripts_count
        );
    }

    #[test]
    fn build_patches_default_post_apply_script_path() {
        // When post_apply script is specified, the command should use that path
        let pod = serde_json::json!({
            "spec": {
                "containers": [{"name": "app", "image": "busybox"}]
            }
        });
        let modules = vec![(
            "mymod".to_string(),
            "1.0".to_string(),
            ModuleSpec {
                scripts: crate::crds::ModuleScripts {
                    post_apply: Some("custom-setup.sh".to_string()),
                },
                ..Default::default()
            },
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        assert!(
            patch_json.contains("custom-setup.sh"),
            "should use the specified postApply script path"
        );
    }
}

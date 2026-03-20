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
    ClusterConfigPolicy, ClusterConfigPolicySpec, ConfigPolicy, ConfigPolicySpec, DriftAlertSpec,
    MachineConfigSpec, Module, ModuleSpec,
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
        .route("/mutate-pods", post(handle_mutate_pods))
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

// ---------------------------------------------------------------------------
// Pod module mutating webhook — /mutate-pods
// ---------------------------------------------------------------------------

const MODULES_ANNOTATION: &str = "cfgd.io/modules";

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

/// Sanitize a module name for use in Kubernetes object names (RFC 1123 DNS label).
fn sanitize_k8s_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .replace('_', "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Collect required modules from ConfigPolicy and ClusterConfigPolicy.
/// ClusterConfigPolicy entries are filtered by their namespaceSelector against
/// the pod's namespace labels.
async fn collect_required_modules(
    client: &Client,
    namespace: &str,
) -> Vec<String> {
    let mut required = Vec::new();

    // Namespace-scoped ConfigPolicy
    if let Ok(policies) = Api::<ConfigPolicy>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await
    {
        for policy in &policies {
            for module_ref in &policy.spec.required_modules {
                if module_ref.required && !required.contains(&module_ref.name) {
                    required.push(module_ref.name.clone());
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
                if module_ref.required && !required.contains(&module_ref.name) {
                    required.push(module_ref.name.clone());
                }
            }
        }
    }

    required
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
        let safe_name = sanitize_k8s_name(name);
        let vol_name = format!("cfgd-module-{safe_name}");
        let mount_path = format!("/cfgd-modules/{name}");

        // Add CSI volume
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: ptr("/spec/volumes/-"),
            value: serde_json::json!({
                "name": vol_name,
                "csi": {
                    "driver": "csi.cfgd.io",
                    "readOnly": true,
                    "volumeAttributes": {
                        "module": name,
                        "version": version,
                        "ociRef": spec.oci_artifact.as_deref().unwrap_or("")
                    }
                }
            }),
        }));

        // Add volumeMount + env vars to each container
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
                    // PATH-style: prepend module value to existing variable.
                    // $(VAR) is expanded by kubelet against pod-spec-defined env vars.
                    patches.push(json_patch::PatchOperation::Add(
                        json_patch::AddOperation {
                            path: ptr(&format!("/spec/containers/{i}/env/-")),
                            value: serde_json::json!({
                                "name": env_var.name,
                                "value": format!("{}:$({})", env_var.value, env_var.name)
                            }),
                        },
                    ));
                } else {
                    patches.push(json_patch::PatchOperation::Add(
                        json_patch::AddOperation {
                            path: ptr(&format!("/spec/containers/{i}/env/-")),
                            value: serde_json::json!({
                                "name": env_var.name,
                                "value": env_var.value
                            }),
                        },
                    ));
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
            let safe_name = sanitize_k8s_name(name);
            let script_path = spec.scripts.post_apply.as_deref().unwrap_or("post-apply.sh");
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

    // Collect required modules from ConfigPolicy + ClusterConfigPolicy
    let required_names = collect_required_modules(&state.client, namespace).await;

    // Merge: annotation modules + required modules (annotation takes precedence for version)
    let mut all_modules: Vec<(String, String)> = annotation_modules;
    for name in &required_names {
        if !all_modules.iter().any(|(n, _)| n == name) {
            // Required module without explicit version — use "latest"
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
                resolved.push((name.clone(), version.clone(), module.spec.clone()));
            }
            Err(e) => {
                warn!(module = name, error = %e, "Module CRD not found, skipping injection");
            }
        }
    }

    if resolved.is_empty() {
        record_webhook_metrics(&state.metrics, "mutate_pods", "allowed", start);
        return Json(AdmissionResponse::from(&req).into_review());
    }

    // Build JSON patches
    let patches = build_injection_patches(&pod_obj, &resolved);

    let resp = match AdmissionResponse::from(&req)
        .with_patch(json_patch::Patch(patches))
    {
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

    const TEST_PEM_KEY: &str =
        "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----";

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
                    "publicKey": TEST_PEM_KEY
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
        assert!(patch_json.contains("csi.cfgd.io"));
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
                env: vec![
                    crate::crds::ModuleEnvVar {
                        name: "EDITOR".to_string(),
                        value: "vim".to_string(),
                        append: false,
                    },
                ],
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
    fn sanitize_k8s_name_handles_special_chars() {
        assert_eq!(sanitize_k8s_name("my_Module"), "my-module");
        assert_eq!(sanitize_k8s_name("simple"), "simple");
        assert_eq!(sanitize_k8s_name("--leading--"), "leading");
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
        let modules = vec![(
            "mod1".to_string(),
            "1.0".to_string(),
            ModuleSpec::default(),
        )];
        let patches = build_injection_patches(&pod, &modules);
        let patch_json = serde_json::to_string(&patches).unwrap();
        // Should have volumeMount patches for both containers (indices 0 and 1)
        assert!(patch_json.contains("/spec/containers/0/volumeMounts/-"));
        assert!(patch_json.contains("/spec/containers/1/volumeMounts/-"));
    }

    fn test_metrics() -> Metrics {
        let mut registry = prometheus_client::registry::Registry::default();
        Metrics::new(&mut registry)
    }
}

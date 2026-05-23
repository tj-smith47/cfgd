//! Webhook router tests that drive the real `Api::list` / `Api::get`
//! call sequence through the shared `MockKubeHarness`.
//!
//! These cover the kube-touching branches that the stub-client tests in
//! `tests_router.rs` skip: `enforce_module_policy` (list ClusterConfigPolicy)
//! and `collect_policy_modules` + Module CRD GET inside `mutate-pods`.
#![cfg(test)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kube::core::DynamicObject;
use kube::core::admission::AdmissionReview;
use serde_json::json;
use tower::ServiceExt;

use super::test_router::test_webhook_router_with_client;
use crate::controllers::test_kube_harness::{ExpectedCall, MockKubeHarness};

const NS: &str = "test-ns";

// -----------------------------------------------------------------------
// Path helpers
// -----------------------------------------------------------------------

fn cluster_config_policies_path() -> &'static str {
    "/apis/cfgd.io/v1alpha1/clusterconfigpolicies"
}

fn config_policies_path(ns: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{ns}/configpolicies")
}

fn namespace_path(ns: &str) -> String {
    format!("/api/v1/namespaces/{ns}")
}

fn module_path(name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/modules/{name}")
}

// -----------------------------------------------------------------------
// JSON list/object builders
// -----------------------------------------------------------------------

fn ccp_list(items: Vec<serde_json::Value>) -> serde_json::Value {
    json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "ClusterConfigPolicyList",
        "items": items,
        "metadata": {},
    })
}

fn cp_list(items: Vec<serde_json::Value>) -> serde_json::Value {
    json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "ConfigPolicyList",
        "items": items,
        "metadata": {},
    })
}

fn namespace_object(name: &str, labels: serde_json::Value) -> serde_json::Value {
    json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": { "name": name, "labels": labels },
    })
}

fn ccp_object(
    name: &str,
    trusted_registries: &[&str],
    allow_unsigned: bool,
    required_modules: &[&str],
) -> serde_json::Value {
    let req: Vec<_> = required_modules
        .iter()
        .map(|n| json!({ "name": n, "required": true }))
        .collect();
    json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "ClusterConfigPolicy",
        "metadata": { "name": name, "uid": format!("uid-{name}"), "generation": 1 },
        "spec": {
            "namespaceSelector": {},
            "requiredModules": req,
            "security": {
                "trustedRegistries": trusted_registries,
                "allowUnsigned": allow_unsigned,
            },
        },
    })
}

/// Build a ClusterConfigPolicy with explicit debugModules and a
/// `match_labels` namespaceSelector.
fn ccp_object_with_debug_modules(
    name: &str,
    required_modules: &[&str],
    debug_modules: &[&str],
    match_labels: serde_json::Value,
) -> serde_json::Value {
    let req: Vec<_> = required_modules
        .iter()
        .map(|n| json!({ "name": n, "required": true }))
        .collect();
    let dbg: Vec<_> = debug_modules.iter().map(|n| json!({ "name": n })).collect();
    json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "ClusterConfigPolicy",
        "metadata": { "name": name, "uid": format!("uid-{name}"), "generation": 1 },
        "spec": {
            "namespaceSelector": { "matchLabels": match_labels },
            "requiredModules": req,
            "debugModules": dbg,
            "security": {
                "trustedRegistries": [],
                "allowUnsigned": true,
            },
        },
    })
}

/// Build a namespaced ConfigPolicy with required + debug modules.
fn cp_object(name: &str, required_modules: &[&str], debug_modules: &[&str]) -> serde_json::Value {
    let req: Vec<_> = required_modules
        .iter()
        .map(|n| json!({ "name": n, "required": true }))
        .collect();
    let dbg: Vec<_> = debug_modules.iter().map(|n| json!({ "name": n })).collect();
    json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "ConfigPolicy",
        "metadata": { "name": name, "namespace": NS, "uid": format!("uid-{name}"), "generation": 1 },
        "spec": {
            "requiredModules": req,
            "debugModules": dbg,
            "packages": [],
        },
    })
}

fn module_object(name: &str, oci_artifact: Option<&str>) -> serde_json::Value {
    let mut spec = json!({
        "packages": [],
        "files": [],
        "scripts": {},
        "env": [],
        "depends": [],
        "mountPolicy": "Always",
    });
    if let Some(oci) = oci_artifact {
        spec["ociArtifact"] = json!(oci);
    }
    json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "Module",
        "metadata": { "name": name, "uid": format!("uid-{name}"), "generation": 1 },
        "spec": spec,
    })
}

// -----------------------------------------------------------------------
// Request helpers
// -----------------------------------------------------------------------

fn module_admission_review(spec: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "module-uid",
            "kind": {"group": "cfgd.io", "version": "v1alpha1", "kind": "Module"},
            "resource": {"group": "cfgd.io", "version": "v1alpha1", "resource": "modules"},
            "operation": "CREATE",
            "userInfo": {"username": "test"},
            "object": {
                "apiVersion": "cfgd.io/v1alpha1",
                "kind": "Module",
                "metadata": {"name": "mod-1"},
                "spec": spec,
            }
        }
    }))
    .expect("admission review json")
}

fn pod_admission_review(annotations: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "request": {
            "uid": "pod-uid",
            "kind": {"group": "", "version": "v1", "kind": "Pod"},
            "resource": {"group": "", "version": "v1", "resource": "pods"},
            "operation": "CREATE",
            "namespace": NS,
            "userInfo": {"username": "test"},
            "object": {
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {"name": "pod-1", "namespace": NS, "annotations": annotations},
                "spec": {"containers": [{"name": "main", "image": "alpine:latest"}]},
            }
        }
    }))
    .expect("pod admission review")
}

fn post(path: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("request must build")
}

async fn parse_response(response: axum::response::Response) -> AdmissionReview<DynamicObject> {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("admission review")
}

// -----------------------------------------------------------------------
// validate-module — kube path
// -----------------------------------------------------------------------

#[tokio::test]
async fn validate_module_denies_when_oci_artifact_outside_trusted_registries() {
    let ccp = ccp_object("ccp-1", &["ghcr.io/myorg/*"], true, &[]);
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![ccp])),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post(
            "/validate-module",
            module_admission_review(json!({
                "ociArtifact": "docker.io/baduser/img:v1",
                "packages": [],
            })),
        ))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);

    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(
        !resp.allowed,
        "ociArtifact outside trusted registries must be denied"
    );
    assert!(
        resp.result.message.contains("trusted registry"),
        "denial must reference trusted registry: {}",
        resp.result.message
    );

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 1, "exactly one LIST CCP");
}

#[tokio::test]
async fn validate_module_denies_unsigned_module_when_policy_disallows() {
    let ccp = ccp_object("ccp-strict", &[], false, &[]);
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![ccp])),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post(
            "/validate-module",
            module_admission_review(json!({
                "ociArtifact": "ghcr.io/myorg/img:v1",
                "packages": [],
            })),
        ))
        .await
        .unwrap();
    let review = parse_response(response).await;
    let resp = review.response.unwrap();
    assert!(!resp.allowed, "unsigned module must be denied");
    assert!(
        resp.result.message.contains("unsigned"),
        "denial must reference unsigned: {}",
        resp.result.message
    );

    let _ = harness.finish().await;
}

#[tokio::test]
async fn validate_module_allows_when_no_policies_present() {
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![])),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post(
            "/validate-module",
            module_admission_review(json!({
                "ociArtifact": "docker.io/anything/img:v1",
                "packages": [],
            })),
        ))
        .await
        .unwrap();
    let review = parse_response(response).await;
    let resp = review.response.unwrap();
    assert!(
        resp.allowed,
        "with empty CCP list, module must be allowed regardless of registry"
    );

    let _ = harness.finish().await;
}

// -----------------------------------------------------------------------
// mutate-pods — kube path through collect_policy_modules + Module GET
// -----------------------------------------------------------------------

#[tokio::test]
async fn mutate_pods_with_annotation_emits_volume_patch_when_module_crd_resolves() {
    let module = module_object("editor", Some("ghcr.io/myorg/editor:v1"));
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        // collect_policy_modules: namespaced CP list, namespace get, cluster CCP list.
        ExpectedCall::list(config_policies_path(NS)).returning_json(&cp_list(vec![])),
        ExpectedCall::get(namespace_path(NS)).returning_json(&namespace_object(NS, json!({}))),
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![])),
        // Module GET for the annotation entry.
        ExpectedCall::get(module_path("editor")).returning_json(&module),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post(
            "/mutate-pods",
            pod_admission_review(json!({
                cfgd_core::MODULES_ANNOTATION: "editor:v1",
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(resp.allowed, "must allow with patch");
    let patch = resp
        .patch
        .expect("patch must be present when modules resolve");
    let patch_str = String::from_utf8(patch).expect("patch is utf-8 json");
    assert!(
        patch_str.contains("cfgd-module-editor"),
        "patch must add CSI volume named cfgd-module-editor: {patch_str}"
    );
    assert!(
        patch_str.contains("ghcr.io/myorg/editor:v1"),
        "patch must include ociRef volumeAttribute: {patch_str}"
    );

    let report = harness.finish().await;
    assert_eq!(report.captured.len(), 4);
}

#[tokio::test]
async fn mutate_pods_skips_injection_when_module_crd_missing() {
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(config_policies_path(NS)).returning_json(&cp_list(vec![])),
        ExpectedCall::get(namespace_path(NS)).returning_json(&namespace_object(NS, json!({}))),
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![])),
        // Module GET returns 404 — handler logs warn and skips.
        ExpectedCall::get(module_path("missing")).returning_404("missing"),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post(
            "/mutate-pods",
            pod_admission_review(json!({
                cfgd_core::MODULES_ANNOTATION: "missing:v1",
            })),
        ))
        .await
        .unwrap();
    let review = parse_response(response).await;
    let resp = review.response.unwrap();
    assert!(resp.allowed, "missing module must still allow the pod");
    assert!(
        resp.patch.is_none(),
        "no resolved modules → no patch attached"
    );

    let _ = harness.finish().await;
}

#[tokio::test]
async fn mutate_pods_uses_required_modules_from_cluster_config_policy() {
    // No pod annotation — module is pulled in via CCP requiredModules with a
    // namespaceSelector that matches the pod's namespace. Drives the CCP
    // selector branch + the path that adds policy.required to all_modules.
    let ccp = ccp_object("ccp-prod", &[], true, &["logger"]);
    let module = module_object("logger", Some("ghcr.io/myorg/logger:v1"));

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(config_policies_path(NS)).returning_json(&cp_list(vec![])),
        ExpectedCall::get(namespace_path(NS))
            .returning_json(&namespace_object(NS, json!({"env": "prod"}))),
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![ccp])),
        ExpectedCall::get(module_path("logger")).returning_json(&module),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post("/mutate-pods", pod_admission_review(json!({}))))
        .await
        .unwrap();
    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(resp.allowed);
    let patch = resp
        .patch
        .expect("CCP-required module must produce a patch");
    let patch_str = String::from_utf8(patch).unwrap();
    assert!(
        patch_str.contains("cfgd-module-logger"),
        "patch must add the CCP-required module's volume: {patch_str}"
    );

    let _ = harness.finish().await;
}

// -----------------------------------------------------------------------
// Policy LIST error branches — 5xx on ClusterConfigPolicy / ConfigPolicy
// LIST. validate-module surfaces the kube error verbatim; mutate-pods
// gracefully degrades because collect_policy_modules wraps each LIST in
// `if let Ok(...)` and silently treats the error as "no policies".
// -----------------------------------------------------------------------

#[tokio::test]
async fn validate_module_denies_when_ccp_list_returns_5xx() {
    // Apiserver returns a 503 Status response on the CCP LIST. The webhook
    // must surface that as a denial (not a panic, not a silent allow) so
    // operators can observe a misconfigured/unhealthy apiserver via the
    // admission-review denial reason.
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(cluster_config_policies_path())
            .returning_server_error(503, "apiserver overloaded"),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post(
            "/validate-module",
            module_admission_review(json!({
                "ociArtifact": "ghcr.io/myorg/img:v1",
                "packages": [],
            })),
        ))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);

    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(
        !resp.allowed,
        "5xx on CCP LIST must surface as denial — fail-closed semantics"
    );
    assert!(
        resp.result
            .message
            .contains("failed to list ClusterConfigPolicies"),
        "denial reason must surface the policy LIST failure: {}",
        resp.result.message
    );

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        1,
        "exactly one CCP LIST attempt — no retry on 5xx within the handler"
    );
}

#[tokio::test]
async fn mutate_pods_proceeds_without_policy_modules_when_ccp_list_returns_5xx() {
    // collect_policy_modules wraps the CCP LIST in `if let Ok(policies)` —
    // a 5xx is silently swallowed and treated as "no policy modules".
    // The pod's annotation-driven module is still resolved and patched in.
    let module = module_object("editor", Some("ghcr.io/myorg/editor:v1"));
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(config_policies_path(NS)).returning_json(&cp_list(vec![])),
        ExpectedCall::get(namespace_path(NS)).returning_json(&namespace_object(NS, json!({}))),
        ExpectedCall::list(cluster_config_policies_path())
            .returning_server_error(503, "apiserver overloaded"),
        ExpectedCall::get(module_path("editor")).returning_json(&module),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post(
            "/mutate-pods",
            pod_admission_review(json!({
                cfgd_core::MODULES_ANNOTATION: "editor:v1",
            })),
        ))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);

    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(
        resp.allowed,
        "5xx on CCP LIST must not block mutate-pods — degraded mode allows the pod"
    );
    let patch = resp
        .patch
        .expect("annotation-driven module still resolves through Module GET");
    let patch_str = String::from_utf8(patch).expect("patch is utf-8");
    assert!(
        patch_str.contains("cfgd-module-editor"),
        "annotation module must still produce a CSI volume patch: {patch_str}"
    );

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        4,
        "all four kube calls observed: CP LIST, NS GET, CCP LIST (5xx), Module GET"
    );
}

#[tokio::test]
async fn mutate_pods_proceeds_without_policy_modules_when_cp_list_returns_5xx() {
    // The first call in collect_policy_modules — namespaced CP LIST —
    // returns 5xx. The handler must still attempt the namespace GET and
    // cluster CCP LIST that follow, because each LIST is independently
    // wrapped in `if let Ok(...)`. With no annotation and empty CCP, the
    // pod is allowed without a patch.
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(config_policies_path(NS)).returning_server_error(500, "internal error"),
        ExpectedCall::get(namespace_path(NS)).returning_json(&namespace_object(NS, json!({}))),
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![])),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post("/mutate-pods", pod_admission_review(json!({}))))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);

    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(
        resp.allowed,
        "no annotation + no policies + CP LIST 5xx → allow without patch"
    );
    assert!(
        resp.patch.is_none(),
        "no resolved modules → no patch even when one LIST 5xx'd"
    );

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        3,
        "all three policy-collection calls observed even after CP LIST 5xx"
    );
}

// -----------------------------------------------------------------------
// validate-module — structural validation denial through the live router
// (exercises the handle_validate_module deny arm + metric)
// -----------------------------------------------------------------------

#[tokio::test]
async fn validate_module_router_denies_invalid_spec_before_policy_call() {
    // Structural validation fires before enforce_module_policy — when the
    // spec itself is invalid (empty package name), the handler must deny
    // and record the "denied" metric without making any kube calls.
    let (ctx, _registry, harness) = MockKubeHarness::new(vec![]);

    let (router, metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post(
            "/validate-module",
            module_admission_review(json!({
                "packages": [{"name": ""}],
            })),
        ))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);

    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(
        !resp.allowed,
        "empty package name must be denied by structural validation"
    );
    assert!(
        resp.result.message.contains("packages"),
        "denial must reference packages: {}",
        resp.result.message
    );

    let denied = metrics
        .webhook_requests_total
        .get_or_create(&crate::metrics::WebhookLabels {
            operation: "validate_module".to_string(),
            result: "denied".to_string(),
        })
        .get();
    assert_eq!(
        denied, 1,
        "structural-validation denial must record denied metric"
    );

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "structural validation must short-circuit before any kube call"
    );
}

// -----------------------------------------------------------------------
// mutate-pods — required modules pulled from namespaced ConfigPolicy
// (exercises collect_policy_modules ConfigPolicy items branch +
// "policy.required added to all_modules" loop)
// -----------------------------------------------------------------------

#[tokio::test]
async fn mutate_pods_uses_required_modules_from_namespaced_config_policy() {
    let cp = cp_object("cp-app", &["sidecar"], &[]);
    let module = module_object("sidecar", Some("ghcr.io/myorg/sidecar:v1"));

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(config_policies_path(NS)).returning_json(&cp_list(vec![cp])),
        ExpectedCall::get(namespace_path(NS)).returning_json(&namespace_object(NS, json!({}))),
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![])),
        ExpectedCall::get(module_path("sidecar")).returning_json(&module),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post("/mutate-pods", pod_admission_review(json!({}))))
        .await
        .expect("router responds");
    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(resp.allowed);
    let patch = resp
        .patch
        .expect("namespaced CP-required module must produce a patch");
    let patch_str = String::from_utf8(patch).expect("patch is utf-8");
    assert!(
        patch_str.contains("cfgd-module-sidecar"),
        "patch must add namespaced-CP-required module volume: {patch_str}"
    );

    let _ = harness.finish().await;
}

// -----------------------------------------------------------------------
// mutate-pods — debug modules from CCP override Module.mountPolicy
// (exercises lines: CCP debug_modules add to policy.debug, policy.debug
// loop adds to all_modules, and the policy.debug.contains() branch that
// flips spec.mount_policy to Debug)
// -----------------------------------------------------------------------

#[tokio::test]
async fn mutate_pods_ccp_debug_modules_emit_csi_volume_without_volume_mount() {
    // CCP requires no modules but lists "tcpdump" as a debug module. The
    // Module CRD says Always, but the policy.debug branch must flip it to
    // Debug — so the patch contains the CSI volume but no volumeMount
    // entry referencing it on the declared container.
    let ccp = ccp_object_with_debug_modules("ccp-debug", &[], &["tcpdump"], json!({}));
    // Module CRD declares mountPolicy: Always; policy.debug override
    // must change it.
    let module = module_object("tcpdump", Some("ghcr.io/myorg/tcpdump:v1"));

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(config_policies_path(NS)).returning_json(&cp_list(vec![])),
        ExpectedCall::get(namespace_path(NS)).returning_json(&namespace_object(NS, json!({}))),
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![ccp])),
        ExpectedCall::get(module_path("tcpdump")).returning_json(&module),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post("/mutate-pods", pod_admission_review(json!({}))))
        .await
        .expect("router responds");
    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(resp.allowed);
    let patch = resp.patch.expect("debug module must still produce a patch");
    let patch_str = String::from_utf8(patch).expect("patch is utf-8");
    assert!(
        patch_str.contains("cfgd-module-tcpdump"),
        "debug module must add CSI volume: {patch_str}"
    );
    // Debug modules must NOT inject a volumeMount on declared containers.
    // The patch will not contain any volumeMount op referencing tcpdump's
    // mountPath.
    assert!(
        !patch_str.contains("\"mountPath\":\"/cfgd-modules/tcpdump\""),
        "debug module must NOT add volumeMount: {patch_str}"
    );

    let _ = harness.finish().await;
}

// -----------------------------------------------------------------------
// mutate-pods — CCP that doesn't match namespace labels is skipped
// (exercises the `if !matches_selector { continue; }` branch in
// collect_policy_modules)
// -----------------------------------------------------------------------

#[tokio::test]
async fn mutate_pods_ccp_with_non_matching_namespace_selector_is_skipped() {
    // CCP requires "logger" but its namespaceSelector only matches
    // namespaces labelled env=prod. The target namespace has env=dev,
    // so the CCP must be skipped and the pod allowed without a patch.
    let ccp = ccp_object_with_debug_modules("ccp-prod", &["logger"], &[], json!({ "env": "prod" }));

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(config_policies_path(NS)).returning_json(&cp_list(vec![])),
        ExpectedCall::get(namespace_path(NS))
            .returning_json(&namespace_object(NS, json!({ "env": "dev" }))),
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![ccp])),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post("/mutate-pods", pod_admission_review(json!({}))))
        .await
        .expect("router responds");
    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(
        resp.allowed,
        "selector-skipped CCP must allow the pod without injection"
    );
    assert!(
        resp.patch.is_none(),
        "selector-skipped CCP must not inject any module: patch={:?}",
        resp.patch
    );

    let _ = harness.finish().await;
}

// -----------------------------------------------------------------------
// mutate-pods — namespaced ConfigPolicy debug_modules collected and
// applied (exercises the ConfigPolicy debug_modules push loop + the
// MountPolicy::Debug override on the Module CRD)
// -----------------------------------------------------------------------

#[tokio::test]
async fn mutate_pods_namespaced_cp_debug_modules_override_module_mount_policy() {
    let cp = cp_object("cp-dbg", &[], &["strace"]);
    // Module CRD declares Always, policy.debug must override.
    let module = module_object("strace", Some("ghcr.io/myorg/strace:v1"));

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(config_policies_path(NS)).returning_json(&cp_list(vec![cp])),
        ExpectedCall::get(namespace_path(NS)).returning_json(&namespace_object(NS, json!({}))),
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![])),
        ExpectedCall::get(module_path("strace")).returning_json(&module),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post("/mutate-pods", pod_admission_review(json!({}))))
        .await
        .expect("router responds");
    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(resp.allowed);
    let patch = resp.patch.expect("patch must contain CSI volume");
    let patch_str = String::from_utf8(patch).expect("patch is utf-8");
    assert!(
        patch_str.contains("cfgd-module-strace"),
        "patch must add CSI volume for namespaced-CP debug module: {patch_str}"
    );
    assert!(
        !patch_str.contains("\"mountPath\":\"/cfgd-modules/strace\""),
        "namespaced-CP debug module must NOT add volumeMount: {patch_str}"
    );

    let _ = harness.finish().await;
}

// -----------------------------------------------------------------------
// mutate-pods — annotation already requests a module, then CCP debug
// pushes it as debug. The annotation arrives first → the policy.debug
// loop's `iter().any(...)` dedup keeps the annotation entry, but the
// policy.debug.contains() check still flips mount_policy to Debug on
// the resolved spec. The patch should still emit volume-only (no mount).
// -----------------------------------------------------------------------

#[tokio::test]
async fn mutate_pods_annotation_module_listed_as_ccp_debug_is_flipped_to_debug_mount() {
    let ccp = ccp_object_with_debug_modules("ccp-dbg", &[], &["editor"], json!({}));
    let module = module_object("editor", Some("ghcr.io/myorg/editor:v1"));

    let (ctx, _registry, harness) = MockKubeHarness::new(vec![
        ExpectedCall::list(config_policies_path(NS)).returning_json(&cp_list(vec![])),
        ExpectedCall::get(namespace_path(NS)).returning_json(&namespace_object(NS, json!({}))),
        ExpectedCall::list(cluster_config_policies_path()).returning_json(&ccp_list(vec![ccp])),
        ExpectedCall::get(module_path("editor")).returning_json(&module),
    ]);

    let (router, _metrics) = test_webhook_router_with_client(ctx.client.clone());

    let response = router
        .oneshot(post(
            "/mutate-pods",
            pod_admission_review(json!({
                cfgd_core::MODULES_ANNOTATION: "editor:v1",
            })),
        ))
        .await
        .expect("router responds");
    let review = parse_response(response).await;
    let resp = review.response.expect("response present");
    assert!(resp.allowed);
    let patch = resp.patch.expect("patch must be present");
    let patch_str = String::from_utf8(patch).expect("patch is utf-8");
    assert!(
        patch_str.contains("cfgd-module-editor"),
        "annotation module must produce CSI volume: {patch_str}"
    );
    // policy.debug override → no volumeMount for the editor.
    assert!(
        !patch_str.contains("\"mountPath\":\"/cfgd-modules/editor\""),
        "policy.debug override must suppress volumeMount even for annotation-listed module: {patch_str}"
    );

    let _ = harness.finish().await;
}

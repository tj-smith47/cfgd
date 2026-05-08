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

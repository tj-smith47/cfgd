use cfgd_core::output::{OutputFormat, Printer};
use cfgd_core::test_helpers::{EnvVarGuard, test_printer};
use serial_test::serial;

use super::*;

#[test]
fn parse_module_arg_valid() {
    let (name, version) = parse_module_arg("nettools:1.0").unwrap();
    assert_eq!(name, "nettools");
    assert_eq!(version, "1.0");
}

#[test]
fn parse_module_arg_no_version_fails() {
    assert!(parse_module_arg("nettools").is_err());
}

#[test]
fn build_volume_mount_correct_structure() {
    let mount = build_volume_mount("nettools");
    assert_eq!(mount["name"], "cfgd-module-nettools");
    assert_eq!(mount["mountPath"], "/cfgd-modules/nettools");
    assert_eq!(mount["readOnly"], true);
}

#[test]
fn build_volume_mount_sanitizes_name() {
    let mount = build_volume_mount("my_Module");
    assert_eq!(mount["name"], "cfgd-module-my-module");
}

// --- parse_module_arg edge cases ---

#[test]
fn parse_module_arg_with_semver_version() {
    let (name, version) = parse_module_arg("myapp:2.1.3").unwrap();
    assert_eq!(name, "myapp");
    assert_eq!(version, "2.1.3");
}

#[test]
fn parse_module_arg_with_latest() {
    let (name, version) = parse_module_arg("tools:latest").unwrap();
    assert_eq!(name, "tools");
    assert_eq!(version, "latest");
}

#[test]
fn parse_module_arg_with_multiple_colons() {
    // split_once returns only the first split, so "a:b:c" -> ("a", "b:c")
    let (name, version) = parse_module_arg("registry.io/module:v1.0").unwrap();
    assert_eq!(name, "registry.io/module");
    assert_eq!(version, "v1.0");
}

#[test]
fn parse_module_arg_empty_name_with_colon() {
    // ":1.0" -> name is empty, version is "1.0"
    let (name, version) = parse_module_arg(":1.0").unwrap();
    assert_eq!(name, "");
    assert_eq!(version, "1.0");
}

#[test]
fn parse_module_arg_empty_version_with_colon() {
    // "name:" -> name is "name", version is ""
    let (name, version) = parse_module_arg("name:").unwrap();
    assert_eq!(name, "name");
    assert_eq!(version, "");
}

#[test]
fn parse_module_arg_empty_string_fails() {
    assert!(parse_module_arg("").is_err());
}

#[test]
fn parse_module_arg_error_message_contains_input() {
    let err = parse_module_arg("badformat").unwrap_err();
    assert!(
        err.to_string().contains("badformat"),
        "error should contain the invalid input, got: {err}"
    );
    assert!(
        err.to_string().contains("expected name:version"),
        "error should hint at expected format, got: {err}"
    );
}

// --- build_volume_mount additional tests ---

#[test]
fn build_volume_mount_preserves_original_in_mount_path() {
    // mountPath uses the original name (not sanitized)
    let mount = build_volume_mount("My_Cool_Module");
    assert_eq!(mount["mountPath"], "/cfgd-modules/My_Cool_Module");
    // But the volume name is sanitized for k8s
    assert_eq!(mount["name"], "cfgd-module-my-cool-module");
}

#[test]
fn build_volume_mount_read_only_always_true() {
    let mount = build_volume_mount("test");
    assert_eq!(mount["readOnly"], true);
}

#[test]
fn build_volume_mount_with_dots_in_name() {
    let mount = build_volume_mount("org.example.tool");
    assert_eq!(mount["mountPath"], "/cfgd-modules/org.example.tool");
    // sanitize_k8s_name strips dots (not alphanumeric or dash)
    assert_eq!(mount["name"], "cfgd-module-orgexampletool");
}

// --- PluginCli parsing tests ---

#[test]
fn plugin_cli_parse_debug_command() {
    let cli = PluginCli::try_parse_from([
        "kubectl-cfgd",
        "debug",
        "my-pod",
        "-m",
        "nettools:1.0",
        "-n",
        "prod",
        "--image",
        "alpine:3.18",
    ])
    .unwrap();

    match cli.command {
        PluginCommand::Debug {
            pod,
            module,
            namespace,
            image,
        } => {
            assert_eq!(pod, "my-pod");
            assert_eq!(module, vec!["nettools:1.0"]);
            assert_eq!(namespace, "prod");
            assert_eq!(image, "alpine:3.18");
        }
        _ => panic!("Expected Debug command"),
    }
}

#[test]
fn plugin_cli_parse_debug_default_namespace_and_image() {
    let cli =
        PluginCli::try_parse_from(["kubectl-cfgd", "debug", "my-pod", "-m", "tools:1.0"]).unwrap();

    match cli.command {
        PluginCommand::Debug {
            namespace, image, ..
        } => {
            assert_eq!(namespace, "default");
            assert_eq!(image, "ubuntu:22.04");
        }
        _ => panic!("Expected Debug command"),
    }
}

#[test]
fn plugin_cli_parse_debug_multiple_modules() {
    let cli = PluginCli::try_parse_from([
        "kubectl-cfgd",
        "debug",
        "my-pod",
        "-m",
        "nettools:1.0",
        "-m",
        "debug-utils:2.0",
    ])
    .unwrap();

    match cli.command {
        PluginCommand::Debug { module, .. } => {
            assert_eq!(module.len(), 2);
            assert_eq!(module[0], "nettools:1.0");
            assert_eq!(module[1], "debug-utils:2.0");
        }
        _ => panic!("Expected Debug command"),
    }
}

#[test]
fn plugin_cli_parse_exec_command() {
    let cli = PluginCli::try_parse_from([
        "kubectl-cfgd",
        "exec",
        "my-pod",
        "-m",
        "tools:1.0",
        "--",
        "ls",
        "-la",
    ])
    .unwrap();

    match cli.command {
        PluginCommand::Exec {
            pod,
            module,
            namespace,
            command,
        } => {
            assert_eq!(pod, "my-pod");
            assert_eq!(module, vec!["tools:1.0"]);
            assert_eq!(namespace, "default");
            assert_eq!(command, vec!["ls", "-la"]);
        }
        _ => panic!("Expected Exec command"),
    }
}

#[test]
fn plugin_cli_parse_inject_command() {
    let cli = PluginCli::try_parse_from([
        "kubectl-cfgd",
        "inject",
        "deployment/myapp",
        "-m",
        "cfg:1.0",
        "-n",
        "staging",
    ])
    .unwrap();

    match cli.command {
        PluginCommand::Inject {
            resource,
            module,
            namespace,
        } => {
            assert_eq!(resource, "deployment/myapp");
            assert_eq!(module, vec!["cfg:1.0"]);
            assert_eq!(namespace, "staging");
        }
        _ => panic!("Expected Inject command"),
    }
}

#[test]
fn plugin_cli_parse_status_command() {
    let cli = PluginCli::try_parse_from(["kubectl-cfgd", "status"]).unwrap();
    assert!(matches!(cli.command, PluginCommand::Status));
}

#[test]
fn plugin_cli_parse_version_command() {
    let cli = PluginCli::try_parse_from(["kubectl-cfgd", "version"]).unwrap();
    assert!(matches!(cli.command, PluginCommand::Version));
}

#[test]
fn plugin_cli_no_subcommand_fails() {
    let result = PluginCli::try_parse_from(["kubectl-cfgd"]);
    assert!(result.is_err(), "missing subcommand should fail");
}

// --- cmd_debug / cmd_exec / cmd_inject validation logic ---

#[test]
fn cmd_debug_no_modules_fails() {
    let printer = test_printer();
    let result = cmd_debug(&printer, "pod", &[], "default", "ubuntu:22.04");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("module"),
        "should mention module requirement"
    );
}

#[test]
fn cmd_debug_invalid_module_format_fails() {
    let printer = test_printer();
    let modules = vec!["bad-format".to_string()];
    let result = cmd_debug(&printer, "pod", &modules, "default", "ubuntu:22.04");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("expected name:version"),
        "should mention expected format"
    );
}

#[test]
fn cmd_exec_no_modules_fails() {
    let printer = test_printer();
    let result = cmd_exec(&printer, "pod", &[], "default", &["ls".to_string()]);
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("module"),
        "should mention module requirement"
    );
}

#[test]
fn cmd_exec_no_command_fails() {
    let printer = test_printer();
    let modules = vec!["tool:1.0".to_string()];
    let result = cmd_exec(&printer, "pod", &modules, "default", &[]);
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("command"),
        "should mention command requirement"
    );
}

#[test]
fn cmd_exec_invalid_module_format_fails() {
    let printer = test_printer();
    let modules = vec!["noversion".to_string()];
    let result = cmd_exec(&printer, "pod", &modules, "default", &["ls".to_string()]);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("expected name:version"),
        "should mention expected format"
    );
}

#[test]
fn cmd_inject_no_modules_fails() {
    let printer = test_printer();
    let result = cmd_inject(&printer, "deployment/myapp", &[], "default");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("module"),
        "should mention module requirement"
    );
}

#[test]
fn cmd_inject_invalid_resource_format_fails() {
    let printer = test_printer();
    let modules = vec!["tool:1.0".to_string()];
    let result = cmd_inject(&printer, "bad-resource-format", &modules, "default");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("expected kind/name"),
        "should mention expected kind/name format, got: {err_msg}"
    );
}

#[test]
fn cmd_inject_invalid_module_format_fails() {
    let printer = test_printer();
    let modules = vec!["noversion".to_string()];
    let result = cmd_inject(&printer, "deployment/myapp", &modules, "default");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("expected name:version"),
        "should mention expected format"
    );
}

// --- MODULE_REQUIRED constant ---

#[test]
fn module_required_message_is_descriptive() {
    assert!(
        MODULE_REQUIRED.contains("module"),
        "constant should mention module"
    );
}

// ============================================================================
// build_inject_patch_json — strategic-merge patch shape
//
// The patch annotates `spec.template.metadata.annotations` (pod template),
// not the workload's own metadata. Moving the annotation up to workload
// metadata would skip the mutating webhook's CSI-injection on new pods.
// ============================================================================

#[test]
fn build_inject_patch_json_sets_annotation_on_pod_template_not_workload() {
    let modules = vec!["nettools:1.0".to_string()];
    let patch = build_inject_patch_json(&modules);

    // Pod template path: spec.template.metadata.annotations
    let pt = patch
        .pointer("/spec/template/metadata/annotations")
        .expect("patch must annotate spec.template, not top-level metadata");
    let val = pt
        .get(cfgd_core::MODULES_ANNOTATION)
        .and_then(|v| v.as_str())
        .expect("annotation must be a string");
    assert_eq!(val, "nettools:1.0");

    // Negative: top-level metadata.annotations must NOT exist.
    assert!(
        patch.pointer("/metadata/annotations").is_none(),
        "patch must not set workload-level metadata.annotations"
    );
}

#[test]
fn build_inject_patch_json_joins_multiple_modules_with_commas() {
    let modules = vec![
        "nettools:1.0".to_string(),
        "auditpkgs:2.3".to_string(),
        "debug-utils:latest".to_string(),
    ];
    let patch = build_inject_patch_json(&modules);
    let val = patch
        .pointer("/spec/template/metadata/annotations")
        .and_then(|m| m.get(cfgd_core::MODULES_ANNOTATION))
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(val, "nettools:1.0,auditpkgs:2.3,debug-utils:latest");
}

#[test]
fn build_inject_patch_json_preserves_module_ordering() {
    // The annotation value is interpreted as an ordered list by downstream
    // consumers (webhook, CSI driver). Reordering would change which volume
    // wins on conflicting mount paths.
    let modules = vec!["z:1".to_string(), "a:1".to_string(), "m:1".to_string()];
    let patch = build_inject_patch_json(&modules);
    let val = patch
        .pointer("/spec/template/metadata/annotations")
        .and_then(|m| m.get(cfgd_core::MODULES_ANNOTATION))
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(val, "z:1,a:1,m:1", "input order must be preserved verbatim");
}

#[test]
fn build_inject_patch_json_empty_input_emits_empty_annotation_value() {
    // Pinned: empty input → empty-string annotation (not absent). Callers
    // guard against empty modules before calling, but the helper must not
    // panic — an empty value would clear a previously-set annotation, which
    // is a defensible default.
    let modules: Vec<String> = Vec::new();
    let patch = build_inject_patch_json(&modules);
    let val = patch
        .pointer("/spec/template/metadata/annotations")
        .and_then(|m| m.get(cfgd_core::MODULES_ANNOTATION))
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(val, "");
}

#[test]
#[serial]
fn cmd_debug_kube_connect_failed_emits_error_doc() {
    let _kc = EnvVarGuard::set("KUBECONFIG", "/nonexistent-kubeconfig-cfgd-test");
    let (printer, cap) = Printer::for_test_doc();
    let modules = vec!["nettools:1.0.0".to_string()];

    let err = cmd_debug(&printer, "mypod", &modules, "prod", "ubuntu:22.04");
    drop(printer);

    assert!(err.is_err(), "must fail when cluster is unreachable");
    let err_msg = err.unwrap_err().to_string();
    assert!(
        err_msg.contains("connect") || err_msg.contains("cluster") || err_msg.contains("kube"),
        "error must mention cluster connection, got: {err_msg}"
    );

    let json = cap.json().expect("error_doc must carry with_data payload");
    assert_eq!(
        json["error"], "kube_connect_failed",
        "error kind must be kube_connect_failed, got: {}",
        json["error"]
    );
    assert_eq!(json["namespace"], "prod");
    assert_eq!(json["pod"], "mypod");
}

#[test]
#[serial]
fn cmd_debug_kube_connect_failed_with_format_json() {
    let _kc = EnvVarGuard::set("KUBECONFIG", "/nonexistent-kubeconfig-cfgd-test");
    let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
    let modules = vec!["nettools:1.0.0".to_string()];

    let err = cmd_debug(&printer, "mypod", &modules, "default", "ubuntu:22.04");
    drop(printer);

    assert!(err.is_err(), "must fail when cluster is unreachable");
    let json = cap.json().expect("error_doc must carry with_data payload");
    assert_eq!(json["error"], "kube_connect_failed");
}

#[test]
#[serial]
fn cmd_status_kube_connect_failed_emits_error_doc() {
    let _kc = EnvVarGuard::set("KUBECONFIG", "/nonexistent-kubeconfig-cfgd-test");
    let (printer, cap) = Printer::for_test_doc();

    let err = cmd_status(&printer);
    drop(printer);

    assert!(err.is_err(), "must fail when cluster is unreachable");
    let err_msg = err.unwrap_err().to_string();
    assert!(
        err_msg.contains("connect") || err_msg.contains("cluster") || err_msg.contains("kube"),
        "error must mention cluster connection, got: {err_msg}"
    );

    let json = cap.json().expect("error_doc must carry with_data payload");
    assert_eq!(
        json["error"], "kube_connect_failed",
        "error kind must be kube_connect_failed, got: {}",
        json["error"]
    );
}

#[test]
#[serial]
fn cmd_version_no_cluster_succeeds_with_not_connected_label() {
    let _kc = EnvVarGuard::set("KUBECONFIG", "/nonexistent-kubeconfig-cfgd-test");
    let (printer, cap) = Printer::for_test_doc();

    cmd_version(&printer).expect("cmd_version must succeed even without cluster");
    drop(printer);

    let json = cap
        .json()
        .expect("cmd_version must emit a with_data payload");
    assert_eq!(
        json["kubectl"], "not connected",
        "disconnected label must be 'not connected', got: {}",
        json["kubectl"]
    );
    assert!(
        json["version"].as_str().is_some(),
        "version field must be a string"
    );
    assert_eq!(
        json["version"], json["cfgd"],
        "version and cfgd fields must match"
    );
}

#[test]
#[serial]
fn cmd_version_emits_client_version_string() {
    let _kc = EnvVarGuard::set("KUBECONFIG", "/nonexistent-kubeconfig-cfgd-test");
    let (printer, cap) = Printer::for_test_doc();

    cmd_version(&printer).expect("cmd_version must succeed");
    drop(printer);

    let json = cap.json().expect("doc must carry json payload");
    let version = json["version"].as_str().expect("version must be a string");
    assert!(!version.is_empty(), "version string must not be empty");
    assert_eq!(
        json["cfgd"].as_str().expect("cfgd field must be a string"),
        version,
        "cfgd field must equal version field"
    );
}

// ============================================================================
// Mock kube-rs tests — happy paths via injected Client
// ============================================================================

mod mock_kube {
    use super::*;
    use http::Response;
    use kube::client::Body;
    use std::convert::Infallible;

    fn mock_client<F>(handler: F) -> kube::Client
    where
        F: Fn(http::Request<Body>) -> Response<Body> + Send + Clone + 'static,
    {
        let svc = tower::service_fn(move |req: http::Request<Body>| {
            let resp = handler(req);
            async move { Ok::<_, Infallible>(resp) }
        });
        kube::Client::new(svc, "default")
    }

    fn json_response(status: u16, body: &serde_json::Value) -> Response<Body> {
        let bytes = serde_json::to_vec(body).unwrap();
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(bytes))
            .unwrap()
    }

    fn pod_json(name: &str, namespace: &str) -> serde_json::Value {
        serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": name,
                "namespace": namespace
            },
            "spec": {
                "containers": [{"name": "main", "image": "nginx:1.25"}],
                "ephemeralContainers": [{
                    "name": "cfgd-debug",
                    "image": "ubuntu:22.04"
                }]
            }
        })
    }

    // --- cmd_debug happy path ---

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_debug_happy_path_creates_ephemeral_container() {
        let client = mock_client(|req| {
            let path = req.uri().path().to_string();
            if path.contains("/ephemeralcontainers") {
                json_response(200, &pod_json("mypod", "prod"))
            } else {
                json_response(404, &serde_json::json!({"message": "not found"}))
            }
        });

        let (printer, cap) = Printer::for_test_doc();
        let parsed = [("nettools", "1.0.0")];

        let result = cmd_debug_async(
            &printer,
            "mypod",
            &parsed,
            "prod",
            "ubuntu:22.04",
            Some(client),
        )
        .await;
        drop(printer);

        assert!(result.is_ok(), "cmd_debug should succeed, got: {result:?}");
        let json = cap.json().expect("success doc must carry data payload");
        assert_eq!(json["pod"], "mypod");
        assert_eq!(json["namespace"], "prod");
        assert_eq!(json["verified"], true);
        assert_eq!(json["modules"][0], "nettools:1.0.0");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_debug_multiple_modules_happy_path() {
        let client = mock_client(|req| {
            let path = req.uri().path().to_string();
            if path.contains("/ephemeralcontainers") {
                json_response(200, &pod_json("mypod", "default"))
            } else {
                json_response(404, &serde_json::json!({"message": "not found"}))
            }
        });

        let (printer, cap) = Printer::for_test_doc();
        let parsed = [("nettools", "1.0.0"), ("debug-utils", "2.1.0")];

        let result = cmd_debug_async(
            &printer,
            "mypod",
            &parsed,
            "default",
            "alpine:3.20",
            Some(client),
        )
        .await;
        drop(printer);

        assert!(result.is_ok(), "cmd_debug should succeed, got: {result:?}");
        let json = cap.json().unwrap();
        assert_eq!(json["modules"][0], "nettools:1.0.0");
        assert_eq!(json["modules"][1], "debug-utils:2.1.0");
        assert_eq!(json["image"], "alpine:3.20");
    }

    // --- cmd_debug error paths ---

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_debug_pod_not_found_returns_error() {
        let client = mock_client(|_req| {
            let err_body = serde_json::json!({
                "kind": "Status",
                "apiVersion": "v1",
                "metadata": {},
                "status": "Failure",
                "message": "pods \"ghost\" not found",
                "reason": "NotFound",
                "code": 404
            });
            json_response(404, &err_body)
        });

        let (printer, _cap) = Printer::for_test_doc();
        let parsed = [("nettools", "1.0.0")];

        let result = cmd_debug_async(
            &printer,
            "ghost",
            &parsed,
            "default",
            "ubuntu:22.04",
            Some(client),
        )
        .await;

        assert!(result.is_err(), "must fail when pod is not found");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("ephemeral container") || err_msg.contains("not found"),
            "error should mention the failure, got: {err_msg}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_debug_permission_denied_returns_error() {
        let client = mock_client(|_req| {
            let err_body = serde_json::json!({
                "kind": "Status",
                "apiVersion": "v1",
                "metadata": {},
                "status": "Failure",
                "message": "pods \"mypod\" is forbidden: User \"test\" cannot patch resource \"pods/ephemeralcontainers\"",
                "reason": "Forbidden",
                "code": 403
            });
            json_response(403, &err_body)
        });

        let (printer, _cap) = Printer::for_test_doc();
        let parsed = [("nettools", "1.0.0")];

        let result = cmd_debug_async(
            &printer,
            "mypod",
            &parsed,
            "default",
            "ubuntu:22.04",
            Some(client),
        )
        .await;

        assert!(result.is_err(), "must fail on 403");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("forbidden")
                || err_msg.contains("Forbidden")
                || err_msg.contains("ephemeral"),
            "error should mention permission failure, got: {err_msg}"
        );
    }

    // --- cmd_status happy path ---

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_status_happy_path_lists_modules() {
        let client = mock_client(|req| {
            let path = req.uri().path().to_string();
            if path.contains("/modules") {
                let list = serde_json::json!({
                    "apiVersion": "cfgd.io/v1alpha1",
                    "kind": "ModuleList",
                    "metadata": {"resourceVersion": "1234"},
                    "items": [
                        {
                            "apiVersion": "cfgd.io/v1alpha1",
                            "kind": "Module",
                            "metadata": {"name": "nettools"},
                            "spec": {"ociArtifact": "ghcr.io/cfgd/nettools:1.0.0"},
                            "status": {"verified": true}
                        },
                        {
                            "apiVersion": "cfgd.io/v1alpha1",
                            "kind": "Module",
                            "metadata": {"name": "debug-utils"},
                            "spec": {"ociArtifact": "ghcr.io/cfgd/debug-utils:2.0.0"},
                            "status": {"verified": false}
                        }
                    ]
                });
                json_response(200, &list)
            } else {
                json_response(404, &serde_json::json!({"message": "not found"}))
            }
        });

        let (printer, cap) = Printer::for_test_doc();

        let result = cmd_status_async(&printer, Some(client)).await;
        drop(printer);

        assert!(result.is_ok(), "cmd_status should succeed, got: {result:?}");
        let json = cap.json().expect("status doc must carry data payload");
        let modules = json["modules"].as_array().expect("modules should be array");
        assert_eq!(modules.len(), 2);
        assert_eq!(modules[0]["name"], "nettools");
        assert_eq!(modules[0]["artifact"], "ghcr.io/cfgd/nettools:1.0.0");
        assert_eq!(modules[0]["verified"], true);
        assert_eq!(modules[1]["name"], "debug-utils");
        assert_eq!(modules[1]["verified"], false);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_status_empty_module_list() {
        let client = mock_client(|req| {
            let path = req.uri().path().to_string();
            if path.contains("/modules") {
                let list = serde_json::json!({
                    "apiVersion": "cfgd.io/v1alpha1",
                    "kind": "ModuleList",
                    "metadata": {"resourceVersion": "1"},
                    "items": []
                });
                json_response(200, &list)
            } else {
                json_response(404, &serde_json::json!({"message": "not found"}))
            }
        });

        let (printer, cap) = Printer::for_test_doc();

        let result = cmd_status_async(&printer, Some(client)).await;
        drop(printer);

        assert!(result.is_ok(), "cmd_status should succeed with empty list");
        let json = cap.json().unwrap();
        let modules = json["modules"].as_array().unwrap();
        assert!(modules.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_status_permission_denied_returns_error() {
        let client = mock_client(|_req| {
            let err_body = serde_json::json!({
                "kind": "Status",
                "apiVersion": "v1",
                "metadata": {},
                "status": "Failure",
                "message": "modules.cfgd.io is forbidden: User \"test\" cannot list resource \"modules\"",
                "reason": "Forbidden",
                "code": 403
            });
            json_response(403, &err_body)
        });

        let (printer, _cap) = Printer::for_test_doc();

        let result = cmd_status_async(&printer, Some(client)).await;

        assert!(result.is_err(), "must fail on 403");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("list modules")
                || err_msg.contains("forbidden")
                || err_msg.contains("Forbidden"),
            "error should mention permission failure, got: {err_msg}"
        );
    }

    // --- cmd_version happy path ---

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_version_happy_path_reports_server_version() {
        let client = mock_client(|req| {
            let path = req.uri().path().to_string();
            if path == "/version" || path == "/version/" {
                let version_info = serde_json::json!({
                    "major": "1",
                    "minor": "29",
                    "gitVersion": "v1.29.2",
                    "gitCommit": "abc123",
                    "gitTreeState": "clean",
                    "buildDate": "2024-02-14T00:00:00Z",
                    "goVersion": "go1.21.7",
                    "compiler": "gc",
                    "platform": "linux/amd64"
                });
                json_response(200, &version_info)
            } else {
                json_response(404, &serde_json::json!({"message": "not found"}))
            }
        });

        let (printer, cap) = Printer::for_test_doc();

        let result = cmd_version_async(&printer, Some(client)).await;
        drop(printer);

        assert!(
            result.is_ok(),
            "cmd_version should succeed, got: {result:?}"
        );
        let json = cap.json().expect("version doc must carry data payload");
        assert_eq!(
            json["kubectl"], "1.29",
            "server version should be major.minor"
        );
        assert!(json["version"].as_str().is_some());
        assert_eq!(json["version"], json["cfgd"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cmd_version_server_unreachable_shows_not_connected() {
        let client = mock_client(|_req| {
            json_response(
                500,
                &serde_json::json!({"message": "internal server error"}),
            )
        });

        let (printer, cap) = Printer::for_test_doc();

        let result = cmd_version_async(&printer, Some(client)).await;
        drop(printer);

        assert!(
            result.is_ok(),
            "cmd_version succeeds even when server is unreachable"
        );
        let json = cap.json().unwrap();
        assert_eq!(
            json["kubectl"], "not connected",
            "should show 'not connected' when server returns error"
        );
    }
}

// ============================================================================
// PATH-shimmed `kubectl` tests — drive `cmd_inject` and `cmd_exec` through a
// fake kubectl on PATH so their post-`run_inherit` / `run_argv_inherit` code
// (success doc emit, kubectl-failure error_doc emit) executes without
// requiring a real cluster. Mirrors the `CFGD_COSIGN_BIN` shim pattern but
// driven via PATH because `kubectl.rs::run_inherit` calls
// `Command::new("kubectl")` literally.
// ============================================================================

#[cfg(unix)]
mod kubectl_shim {
    use super::*;
    /// RAII shim that writes a `kubectl` script returning `exit_code` to a
    /// tempdir and prepends that tempdir to PATH for the lifetime of the
    /// guard. Restores prior PATH on drop. Pair with `#[serial]` because
    /// PATH mutation is process-global.
    struct KubectlPathShim {
        _tmp: tempfile::TempDir,
        _path_guard: cfgd_core::test_helpers::EnvVarGuard,
    }

    impl KubectlPathShim {
        fn new(exit_code: i32) -> Self {
            let (tmp, path_guard) = cfgd_core::test_helpers::install_named_path_shim(
                "kubectl",
                exit_code as u8,
                "",
                "",
            );
            Self {
                _tmp: tmp,
                _path_guard: path_guard,
            }
        }
    }

    // --- cmd_inject: success doc on kubectl exit 0 ---

    #[test]
    #[serial]
    fn cmd_inject_emits_success_doc_when_kubectl_exits_zero() {
        let _shim = KubectlPathShim::new(0);
        let (printer, cap) = Printer::for_test_doc();
        let modules = vec!["nettools:1.0.0".to_string()];

        let result = cmd_inject(&printer, "deployment/myapp", &modules, "prod");
        drop(printer);

        result.expect("cmd_inject must succeed when kubectl exits 0");
        let json = cap.json().expect("success doc must carry data payload");
        assert_eq!(json["namespace"], "prod");
        assert_eq!(json["resource"], "deployment/myapp");
        assert_eq!(json["kind"], "deployment");
        assert_eq!(json["name"], "myapp");
        assert_eq!(json["modules"][0], "nettools:1.0.0");
        let patched = json["patched"].as_array().expect("patched is array");
        assert_eq!(patched.len(), 1);
        assert_eq!(patched[0], "myapp");
    }

    // --- cmd_inject: error doc on kubectl exit non-zero ---

    #[test]
    #[serial]
    fn cmd_inject_emits_error_doc_when_kubectl_exits_nonzero() {
        let _shim = KubectlPathShim::new(1);
        let (printer, cap) = Printer::for_test_doc();
        let modules = vec!["nettools:1.0.0".to_string()];

        let err = cmd_inject(&printer, "statefulset/db", &modules, "staging")
            .expect_err("cmd_inject must fail when kubectl exits non-zero");
        drop(printer);

        assert!(
            err.to_string().contains("kubectl patch failed"),
            "error must mention kubectl patch failure, got: {err}"
        );
        let json = cap.json().expect("error_doc must carry data payload");
        assert_eq!(json["error"], "inject_failed");
        assert_eq!(json["namespace"], "staging");
        assert_eq!(json["resource"], "statefulset/db");
        assert_eq!(json["kind"], "statefulset");
        assert_eq!(json["name"], "db");
        assert_eq!(json["exitCode"], 1);
    }

    #[test]
    #[serial]
    fn cmd_inject_multi_module_success_joins_annotation_csv() {
        let _shim = KubectlPathShim::new(0);
        let (printer, cap) = Printer::for_test_doc();
        let modules = vec![
            "nettools:1.0.0".to_string(),
            "debug-utils:2.1.0".to_string(),
        ];

        cmd_inject(&printer, "deployment/myapp", &modules, "default")
            .expect("happy path must succeed");
        drop(printer);

        let json = cap.json().expect("success doc must carry data payload");
        let mods = json["modules"].as_array().expect("modules array");
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0], "nettools:1.0.0");
        assert_eq!(mods[1], "debug-utils:2.1.0");
    }

    // --- cmd_exec: success path (kubectl exec exit 0) ---

    #[test]
    #[serial]
    fn cmd_exec_succeeds_when_kubectl_exits_zero() {
        let _shim = KubectlPathShim::new(0);
        let (printer, cap) = Printer::for_test_doc();
        let modules = vec!["nettools:1.0.0".to_string()];
        let command = vec!["echo".to_string(), "hello".to_string()];

        cmd_exec(&printer, "mypod", &modules, "prod", &command).expect("cmd_exec must succeed");
        drop(printer);

        let json = cap.json().expect("info doc must carry data payload");
        assert_eq!(json["pod"], "mypod");
        assert_eq!(json["namespace"], "prod");
        assert_eq!(json["modules"][0], "nettools:1.0.0");
        let cmd_arr = json["command"].as_array().expect("command array");
        assert_eq!(cmd_arr[0], "echo");
        assert_eq!(cmd_arr[1], "hello");
    }
}

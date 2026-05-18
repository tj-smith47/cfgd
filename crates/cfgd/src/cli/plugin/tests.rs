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
    let printer = Printer::new(cfgd_core::output_v2::Verbosity::Quiet);
    let result = cmd_debug(&printer, "pod", &[], "default", "ubuntu:22.04");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("module"),
        "should mention module requirement"
    );
}

#[test]
fn cmd_debug_invalid_module_format_fails() {
    let printer = Printer::new(cfgd_core::output_v2::Verbosity::Quiet);
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
    let printer = Printer::new(cfgd_core::output_v2::Verbosity::Quiet);
    let result = cmd_exec(&printer, "pod", &[], "default", &["ls".to_string()]);
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("module"),
        "should mention module requirement"
    );
}

#[test]
fn cmd_exec_no_command_fails() {
    let printer = Printer::new(cfgd_core::output_v2::Verbosity::Quiet);
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
    let printer = Printer::new(cfgd_core::output_v2::Verbosity::Quiet);
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
    let printer = Printer::new(cfgd_core::output_v2::Verbosity::Quiet);
    let result = cmd_inject(&printer, "deployment/myapp", &[], "default");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("module"),
        "should mention module requirement"
    );
}

#[test]
fn cmd_inject_invalid_resource_format_fails() {
    let printer = Printer::new(cfgd_core::output_v2::Verbosity::Quiet);
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
    let printer = Printer::new(cfgd_core::output_v2::Verbosity::Quiet);
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

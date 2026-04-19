use clap::{Parser, Subcommand};

use cfgd_core::output::Printer;

#[derive(Parser)]
#[command(
    name = "kubectl-cfgd",
    about = "cfgd kubectl plugin — manage modules on pods",
    long_about = "Manage cfgd modules on pods via kubectl.\n\n\
                  Installed as a kubectl plugin (via Krew or PATH), this binary \
                  proxies module operations (debug, exec, inject) through the \
                  CSI driver and operator.\n\n\
                  Examples:\n  \
                  kubectl cfgd debug mypod --module nettools:1.0.0\n  \
                  kubectl cfgd exec mypod --module nettools:1.0.0 -- curl -v http://svc\n  \
                  kubectl cfgd inject deployment/myapp --module nettools:1.0.0\n  \
                  kubectl cfgd status\n  \
                  kubectl cfgd version"
)]
struct PluginCli {
    #[command(subcommand)]
    command: PluginCommand,
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Create an ephemeral debug container with cfgd modules
    #[command(
        long_about = "Attach an ephemeral debug container to a running pod with one or more cfgd modules mounted.\n\n\
                      Examples:\n  \
                      kubectl cfgd debug mypod --module nettools:1.0.0\n  \
                      kubectl cfgd debug mypod -m nettools:1.0.0 -m dig-utils:2.3.1 --namespace prod\n  \
                      kubectl cfgd debug mypod --module nettools:1.0.0 --image alpine:3.20"
    )]
    Debug {
        /// Pod name
        pod: String,
        /// Module(s) to inject (format: name:version, repeatable)
        #[arg(long, short)]
        module: Vec<String>,
        /// Namespace
        #[arg(long, short, default_value = "default")]
        namespace: String,
        /// Container image for ephemeral container
        #[arg(long, default_value = "ubuntu:22.04")]
        image: String,
    },
    /// Execute a command in a pod with module environment
    #[command(
        long_about = "Run a command inside a running pod with cfgd modules mounted and PATH extended.\n\n\
                      Examples:\n  \
                      kubectl cfgd exec mypod --module nettools:1.0.0 -- curl -v http://svc\n  \
                      kubectl cfgd exec mypod -m nettools:1.0.0 --namespace prod -- dig example.com"
    )]
    Exec {
        /// Pod name
        pod: String,
        /// Module(s) to load
        #[arg(long, short)]
        module: Vec<String>,
        /// Namespace
        #[arg(long, short, default_value = "default")]
        namespace: String,
        /// Command to execute (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Inject modules into a workload (patches pod template, triggers rollout)
    #[command(
        long_about = "Patch a workload's pod template to mount cfgd modules on every replica and trigger a rollout.\n\n\
                      Examples:\n  \
                      kubectl cfgd inject deployment/myapp --module nettools:1.0.0\n  \
                      kubectl cfgd inject statefulset/db -m dbtools:3.1.0 --namespace prod"
    )]
    Inject {
        /// Resource in kind/name format (e.g. deployment/myapp, statefulset/db)
        resource: String,
        /// Module(s) to inject
        #[arg(long, short)]
        module: Vec<String>,
        /// Namespace
        #[arg(long, short, default_value = "default")]
        namespace: String,
    },
    /// Show fleet module status
    #[command(
        long_about = "Show per-node module status across the cluster (cache hits, pull errors, staged versions).\n\n\
                      Examples:\n  \
                      kubectl cfgd status"
    )]
    Status,
    /// Show client and server version
    #[command(
        long_about = "Print the client plugin version plus the deployed operator/CSI driver versions detected from the cluster.\n\n\
                      Examples:\n  \
                      kubectl cfgd version"
    )]
    Version,
}

const MODULE_REQUIRED: &str = "at least one --module is required";

fn parse_module_arg(arg: &str) -> anyhow::Result<(&str, &str)> {
    arg.split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid module format '{arg}' — expected name:version"))
}

fn build_volume_mount(name: &str) -> serde_json::Value {
    let safe = cfgd_core::sanitize_k8s_name(name);
    serde_json::json!({
        "name": format!("cfgd-module-{safe}"),
        "mountPath": format!("/cfgd-modules/{name}"),
        "readOnly": true
    })
}

pub fn plugin_main() -> anyhow::Result<()> {
    // rustls CryptoProvider is already installed by main() before dispatching here
    let cli = PluginCli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(cfgd_core::tracing_env_filter("warn"))
        .with_target(false)
        .without_time()
        .init();

    if std::env::var_os("NO_COLOR").is_some() {
        Printer::disable_colors();
    }

    let printer = Printer::with_format(
        cfgd_core::output::Verbosity::Normal,
        None,
        cfgd_core::output::OutputFormat::Table,
    );

    match cli.command {
        PluginCommand::Debug {
            pod,
            module,
            namespace,
            image,
        } => cmd_debug(&printer, &pod, &module, &namespace, &image),
        PluginCommand::Exec {
            pod,
            module,
            namespace,
            command,
        } => cmd_exec(&printer, &pod, &module, &namespace, &command),
        PluginCommand::Inject {
            resource,
            module,
            namespace,
        } => cmd_inject(&printer, &resource, &module, &namespace),
        PluginCommand::Status => cmd_status(&printer),
        PluginCommand::Version => cmd_version(&printer),
    }
}

fn cmd_debug(
    printer: &Printer,
    pod: &str,
    modules: &[String],
    namespace: &str,
    image: &str,
) -> anyhow::Result<()> {
    if modules.is_empty() {
        anyhow::bail!(MODULE_REQUIRED);
    }

    let parsed: Vec<(&str, &str)> = modules
        .iter()
        .map(|m| parse_module_arg(m))
        .collect::<Result<_, _>>()?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let client = kube::Client::try_default().await?;
        let pods: kube::Api<k8s_openapi::api::core::v1::Pod> =
            kube::Api::namespaced(client, namespace);

        // Ephemeral containers can only mount volumes already on the pod.
        // The pod must have been created with modules injected (via mutating
        // webhook or `kubectl cfgd inject` followed by rollout).
        let mut volume_mounts = Vec::new();
        let mut path_extensions = Vec::new();

        for (name, _version) in &parsed {
            volume_mounts.push(build_volume_mount(name));
            path_extensions.push(format!("/cfgd-modules/{name}/bin"));
        }

        let path_prefix = path_extensions.join(":");
        let module_names: Vec<_> = parsed.iter().map(|(n, v)| format!("{n}:{v}")).collect();
        let ps1 = format!("\\[cfgd:{}\\] \\w $ ", module_names.join(","));

        let ec = serde_json::json!({
            "name": "cfgd-debug",
            "image": image,
            "command": ["sh"],
            "stdin": true,
            "tty": true,
            "env": [
                {"name": "PATH", "value": format!("{path_prefix}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")},
                {"name": "PS1", "value": ps1}
            ],
            "volumeMounts": volume_mounts
        });

        let patch = serde_json::json!({
            "spec": {
                "ephemeralContainers": [ec]
            }
        });

        pods.patch_ephemeral_containers(
            pod,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Strategic(patch),
        )
        .await
        .map_err(|e| anyhow::anyhow!("failed to create ephemeral container: {e}"))?;

        printer.success(&format!("Ephemeral debug container created on pod {namespace}/{pod}"));
        printer.info(&format!("Modules: {}", module_names.join(", ")));
        printer.info(&format!(
            "Attach with: kubectl attach -n {namespace} {pod} -c cfgd-debug -it"
        ));

        Ok(())
    })
}

fn cmd_exec(
    printer: &Printer,
    pod: &str,
    modules: &[String],
    namespace: &str,
    command: &[String],
) -> anyhow::Result<()> {
    if modules.is_empty() {
        anyhow::bail!(MODULE_REQUIRED);
    }
    if command.is_empty() {
        anyhow::bail!("command is required after --");
    }

    let parsed: Vec<(&str, &str)> = modules
        .iter()
        .map(|m| parse_module_arg(m))
        .collect::<Result<_, _>>()?;

    let path_extensions: Vec<_> = parsed
        .iter()
        .map(|(name, _)| format!("/cfgd-modules/{name}/bin"))
        .collect();
    let path_prefix = path_extensions.join(":");

    // Wrap in sh -c so $PATH is expanded by the container's shell
    let inner_cmd = command
        .iter()
        .map(|c| cfgd_core::shell_escape_value(c))
        .collect::<Vec<_>>()
        .join(" ");
    let exec_args = vec![
        "kubectl".to_string(),
        "exec".to_string(),
        "-n".to_string(),
        namespace.to_string(),
        pod.to_string(),
        "--".to_string(),
        "sh".to_string(),
        "-c".to_string(),
        format!("export PATH={path_prefix}:$PATH; exec {inner_cmd}"),
    ];

    printer.info(&format!("Executing in {namespace}/{pod} with modules"));

    let code = super::kubectl::run_argv_inherit(&exec_args)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn cmd_inject(
    printer: &Printer,
    resource: &str,
    modules: &[String],
    namespace: &str,
) -> anyhow::Result<()> {
    if modules.is_empty() {
        anyhow::bail!(MODULE_REQUIRED);
    }

    let (kind, name) = resource.split_once('/').ok_or_else(|| {
        anyhow::anyhow!(
            "invalid resource format '{resource}' — expected kind/name (e.g. deployment/myapp)"
        )
    })?;

    let parsed: Vec<(&str, &str)> = modules
        .iter()
        .map(|m| parse_module_arg(m))
        .collect::<Result<_, _>>()?;

    let module_names: Vec<_> = parsed.iter().map(|(n, v)| format!("{n}:{v}")).collect();
    let annotation_value = module_names.join(",");

    // Patch the workload's pod template with the cfgd.io/modules annotation.
    // The mutating webhook will inject CSI volumes on the next pod creation.
    let patch_json = serde_json::json!({
        "spec": {
            "template": {
                "metadata": {
                    "annotations": {
                        cfgd_core::MODULES_ANNOTATION: annotation_value
                    }
                }
            }
        }
    });

    let patch_str = serde_json::to_string(&patch_json)?;

    let code = super::kubectl::run_inherit(&[
        "patch",
        kind,
        name,
        "-n",
        namespace,
        "--type",
        "strategic",
        "-p",
        &patch_str,
    ])?;
    if code != 0 {
        anyhow::bail!("kubectl patch failed");
    }

    printer.success(&format!(
        "Injected modules into {namespace}/{kind}/{name}: {}",
        module_names.join(", ")
    ));
    printer.info("Pods will receive modules on next rollout");

    Ok(())
}

fn cmd_status(printer: &Printer) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let client = kube::Client::try_default().await?;

        // List Module CRDs
        let modules: kube::Api<kube::core::DynamicObject> = kube::Api::all_with(
            client,
            &kube::discovery::ApiResource {
                group: "cfgd.io".into(),
                version: "v1alpha1".into(),
                api_version: cfgd_core::API_VERSION.into(),
                kind: "Module".into(),
                plural: "modules".into(),
            },
        );

        let list = modules
            .list(&kube::api::ListParams::default())
            .await
            .map_err(|e| anyhow::anyhow!("failed to list modules: {e}"))?;

        printer.header("Modules");
        if list.items.is_empty() {
            printer.info("No modules found");
        } else {
            for module in &list.items {
                let name = module.metadata.name.as_deref().unwrap_or("?");
                let verified = module
                    .data
                    .pointer("/status/verified")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let artifact = module
                    .data
                    .pointer("/spec/ociArtifact")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let status_icon = if verified { "verified" } else { "unverified" };
                printer.key_value(name, &format!("{artifact} ({status_icon})"));
            }
        }

        Ok(())
    })
}

fn cmd_version(printer: &Printer) -> anyhow::Result<()> {
    printer.key_value("Client", env!("CARGO_PKG_VERSION"));

    let rt = tokio::runtime::Runtime::new()?;
    let server_version = rt.block_on(async {
        let client = kube::Client::try_default().await.ok()?;
        let version = client.apiserver_version().await.ok()?;
        Some(format!("{}.{}", version.major, version.minor))
    });

    match server_version {
        Some(v) => printer.key_value("Server (k8s)", &v),
        None => printer.key_value("Server (k8s)", "not connected"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
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
        let cli = PluginCli::try_parse_from(["kubectl-cfgd", "debug", "my-pod", "-m", "tools:1.0"])
            .unwrap();

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
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = cmd_debug(&printer, "pod", &[], "default", "ubuntu:22.04");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("module"),
            "should mention module requirement"
        );
    }

    #[test]
    fn cmd_debug_invalid_module_format_fails() {
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
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
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = cmd_exec(&printer, "pod", &[], "default", &["ls".to_string()]);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("module"),
            "should mention module requirement"
        );
    }

    #[test]
    fn cmd_exec_no_command_fails() {
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
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
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
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
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let result = cmd_inject(&printer, "deployment/myapp", &[], "default");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("module"),
            "should mention module requirement"
        );
    }

    #[test]
    fn cmd_inject_invalid_resource_format_fails() {
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
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
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
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
}

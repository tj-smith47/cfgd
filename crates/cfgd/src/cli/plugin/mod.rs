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

/// Build the strategic-merge JSON patch body that `kubectl cfgd inject` sends
/// to the workload (Deployment/StatefulSet/etc). The patch sets the
/// `cfgd.io/modules` annotation on the *pod template*, not the workload —
/// the mutating webhook reads that annotation off newly created pods and
/// injects CSI volumes. Moving the annotation up to the workload's metadata
/// would cause the webhook to skip injection on existing pods that get
/// re-created, breaking the rollout contract.
fn build_inject_patch_json(module_refs: &[String]) -> serde_json::Value {
    let annotation_value = module_refs.join(",");
    serde_json::json!({
        "spec": {
            "template": {
                "metadata": {
                    "annotations": {
                        cfgd_core::MODULES_ANNOTATION: annotation_value
                    }
                }
            }
        }
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

    let module_names: Vec<String> = parsed.iter().map(|(n, v)| format!("{n}:{v}")).collect();

    let patch_json = build_inject_patch_json(&module_names);
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
mod tests;

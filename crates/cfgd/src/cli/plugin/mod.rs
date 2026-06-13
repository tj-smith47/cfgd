use clap::{Parser, Subcommand};

use cfgd_core::output::{Doc, Printer, Role, Verbosity};

use crate::cli::OutputFormatArg;

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
    /// Output format: table, wide, json, yaml, name, jsonpath=EXPR, template=TMPL, template-file=PATH
    #[arg(long, short = 'o', global = true, default_value = "table")]
    output: OutputFormatArg,

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
    /// Show client, server, operator, and CSI versions
    #[command(
        long_about = "Print the client plugin version, the Kubernetes apiserver version, and the \
                      deployed cfgd operator + CSI driver versions detected from the cluster.\n\n\
                      Operator/CSI versions are read from the running images' tags in the cfgd \
                      namespace. When the cluster is unreachable, a component is not deployed, or \
                      RBAC forbids the lookup, that field degrades gracefully (the command still \
                      exits 0).\n\n\
                      Examples:\n  \
                      kubectl cfgd version\n  \
                      kubectl cfgd version --namespace cfgd-system\n  \
                      kubectl cfgd -o json version"
    )]
    Version {
        /// Namespace the operator + CSI driver are deployed into
        #[arg(long, short, default_value = cfgd_core::CFGD_SYSTEM_NAMESPACE)]
        namespace: String,
    },
    /// Pin image-volume references to packed digests, then print or apply
    #[command(
        long_about = "Rewrite `volumes[].image.reference` fields in Kubernetes manifests to the \
                      pinned digests recorded in an image lockfile (written by `cfgd image pack --lock`), \
                      so you deploy the exact bytes you packed instead of a mutable tag.\n\n\
                      By default the rewritten manifest is printed to stdout (pipe it to kubectl). \
                      Pass --apply to run `kubectl apply` directly.\n\n\
                      Examples:\n  \
                      kubectl cfgd deploy -f pod.yaml\n  \
                      kubectl cfgd deploy -f pod.yaml --lock cfgd-images.lock | kubectl apply -f -\n  \
                      kubectl cfgd deploy -f pod.yaml -f svc.yaml --apply -n prod"
    )]
    Deploy {
        /// Manifest file(s) to process (repeatable)
        #[arg(long = "filename", short = 'f', value_name = "FILE")]
        filename: Vec<String>,
        /// Image lockfile to read pinned digests from
        #[arg(long, value_name = "FILE", default_value = crate::cli::image::lockfile::DEFAULT_IMAGE_LOCKFILE)]
        lock: String,
        /// Apply the rewritten manifests via `kubectl apply` instead of printing
        #[arg(long)]
        apply: bool,
        /// Namespace passed to `kubectl apply` (only used with --apply)
        #[arg(long, short, default_value = "default")]
        namespace: String,
    },
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

    // Tracing to stderr; stdout is reserved for `-o` machine output.
    tracing_subscriber::fmt()
        .with_env_filter(cfgd_core::tracing_env_filter("warn"))
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();

    if std::env::var_os("NO_COLOR").is_some() {
        Printer::disable_colors();
    }

    let printer = Printer::with_format(Verbosity::Normal, None, cli.output.0);

    let result = match cli.command {
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
        PluginCommand::Version { namespace } => cmd_version(&printer, &namespace),
        PluginCommand::Deploy {
            filename,
            lock,
            apply,
            namespace,
        } => cmd_deploy(&printer, &filename, &lock, apply, &namespace),
    };

    // The plugin has its OWN entry (main.rs returns here directly, never reaching the
    // normal-CLI dispatch), so it must route failures through the SAME central sink
    // rather than letting Rust's Termination print an unstyled `Error: {:?}`. This
    // renders exactly one failure representation (one ✗ / one structured payload) and
    // exits with the resolved code — there is one error sink for the whole binary.
    if let Err(e) = result {
        crate::cli::error::render_cli_error(&printer, &e).exit();
    }
    Ok(())
}

pub fn cmd_debug(
    printer: &Printer,
    pod: &str,
    modules: &[String],
    namespace: &str,
    image: &str,
) -> anyhow::Result<()> {
    if modules.is_empty() {
        return Err(crate::cli::cli_error(
            pod,
            "module_required",
            MODULE_REQUIRED.to_string(),
            serde_json::json!({ "namespace": namespace, "pod": pod }),
        ));
    }

    let parsed: Vec<(&str, &str)> = modules
        .iter()
        .map(|m| parse_module_arg(m))
        .collect::<Result<_, _>>()?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(cmd_debug_async(
        printer, pod, &parsed, namespace, image, None,
    ))
}

pub(crate) async fn cmd_debug_async(
    printer: &Printer,
    pod: &str,
    parsed: &[(&str, &str)],
    namespace: &str,
    image: &str,
    client: Option<kube::Client>,
) -> anyhow::Result<()> {
    let client = match client {
        Some(c) => c,
        None => kube::Client::try_default().await.map_err(|e| {
            crate::cli::cli_error(
                pod,
                "kube_connect_failed",
                format!("Failed to connect to cluster: {e}"),
                serde_json::json!({ "namespace": namespace, "pod": pod }),
            )
        })?,
    };
    let pods: kube::Api<k8s_openapi::api::core::v1::Pod> = kube::Api::namespaced(client, namespace);

    let mut volume_mounts = Vec::new();
    let mut path_extensions = Vec::new();

    for (name, _version) in parsed {
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
    .map_err(|e| {
        crate::cli::cli_error(
            pod,
            "inject_failed",
            format!("failed to create ephemeral container: {e}"),
            serde_json::json!({ "namespace": namespace, "pod": pod }),
        )
    })?;

    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Ephemeral debug container created on pod {namespace}/{pod}"),
            )
            .kv("Modules", module_names.join(", "))
            .hint(format!(
                "Attach with: kubectl attach -n {namespace} {pod} -c cfgd-debug -it"
            ))
            .with_data(serde_json::json!({
                "namespace": namespace,
                "pod": pod,
                "modules": &module_names,
                "image": image,
                "verified": true,
            })),
    );

    Ok(())
}

pub fn cmd_exec(
    printer: &Printer,
    pod: &str,
    modules: &[String],
    namespace: &str,
    command: &[String],
) -> anyhow::Result<()> {
    if modules.is_empty() {
        return Err(crate::cli::cli_error(
            pod,
            "module_required",
            MODULE_REQUIRED.to_string(),
            serde_json::json!({ "namespace": namespace, "pod": pod }),
        ));
    }
    if command.is_empty() {
        return Err(crate::cli::cli_error(
            pod,
            "command_required",
            "command is required after --".to_string(),
            serde_json::json!({ "namespace": namespace, "pod": pod }),
        ));
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

    let module_names: Vec<String> = parsed.iter().map(|(n, v)| format!("{n}:{v}")).collect();

    printer.emit(
        Doc::new()
            .status(
                Role::Info,
                format!("Executing in {namespace}/{pod} with modules"),
            )
            .kv("Modules", module_names.join(", "))
            .with_data(serde_json::json!({
                "namespace": namespace,
                "pod": pod,
                "modules": &module_names,
                "command": command,
            })),
    );

    let code = super::kubectl::run_argv_inherit(&exec_args)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

pub fn cmd_inject(
    printer: &Printer,
    resource: &str,
    modules: &[String],
    namespace: &str,
) -> anyhow::Result<()> {
    if modules.is_empty() {
        return Err(crate::cli::cli_error(
            resource,
            "module_required",
            MODULE_REQUIRED.to_string(),
            serde_json::json!({ "namespace": namespace, "resource": resource }),
        ));
    }

    let (kind, name) = resource.split_once('/').ok_or_else(|| {
        crate::cli::cli_error(
            resource,
            "invalid_resource",
            format!(
                "invalid resource format '{resource}' — expected kind/name (e.g. deployment/myapp)"
            ),
            serde_json::json!({ "namespace": namespace, "resource": resource }),
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
        return Err(crate::cli::cli_error(
            resource,
            "inject_failed",
            "kubectl patch failed".to_string(),
            serde_json::json!({
                "namespace": namespace,
                "resource": resource,
                "kind": kind,
                "name": name,
                "exitCode": code,
            }),
        ));
    }

    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!(
                    "Injected modules into {namespace}/{kind}/{name}: {}",
                    module_names.join(", ")
                ),
            )
            .hint("Pods will receive modules on next rollout")
            .with_data(serde_json::json!({
                "namespace": namespace,
                "resource": resource,
                "kind": kind,
                "name": name,
                "modules": &module_names,
                "patched": [name],
            })),
    );

    Ok(())
}

/// Recursively walk a YAML value, pinning every `image.reference` string that
/// matches a key in `map` to its pinned digest. Walks generically (not by path)
/// so it catches `volumes[].image.reference` at any depth — bare-Pod
/// `spec.volumes[]` and workload `spec.template.spec.volumes[]` alike. Each
/// rewrite is recorded as `(old, new)` in `rewrites`.
fn rewrite_image_refs(
    value: &mut serde_yaml::Value,
    map: &std::collections::HashMap<&str, &str>,
    rewrites: &mut Vec<(String, String)>,
) {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            // If this mapping has an `image` whose value is itself a mapping with
            // a string `reference` present in the map, pin it in place.
            if let Some(serde_yaml::Value::Mapping(image_map)) =
                mapping.get_mut(serde_yaml::Value::from("image"))
                && let Some(serde_yaml::Value::String(reference)) =
                    image_map.get_mut(serde_yaml::Value::from("reference"))
                && let Some(pinned) = map.get(reference.as_str())
            {
                let old = reference.clone();
                *reference = (*pinned).to_string();
                rewrites.push((old, (*pinned).to_string()));
            }
            for (_k, v) in mapping.iter_mut() {
                rewrite_image_refs(v, map, rewrites);
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for v in seq.iter_mut() {
                rewrite_image_refs(v, map, rewrites);
            }
        }
        _ => {}
    }
}

/// Rewrite image-volume references in Kubernetes manifests to their pinned
/// digests from an image lockfile, then print (default) or `kubectl apply` them.
pub fn cmd_deploy(
    printer: &Printer,
    filenames: &[String],
    lock: &str,
    apply: bool,
    namespace: &str,
) -> anyhow::Result<()> {
    use std::path::Path;

    use serde::Deserialize;

    if filenames.is_empty() {
        return Err(crate::cli::cli_error(
            "deploy",
            "filename_required",
            "at least one -f/--filename is required".to_string(),
            serde_json::json!({ "lock": lock }),
        ));
    }

    let lockfile = crate::cli::image::lockfile::load_images_lockfile(Path::new(lock))?;
    if lockfile.images.is_empty() {
        return Err(crate::cli::cli_error(
            "deploy",
            "empty_lockfile",
            format!(
                "image lockfile '{lock}' has no entries to pin against — run `cfgd image pack --lock {lock}` first"
            ),
            serde_json::json!({ "lock": lock }),
        ));
    }

    let map: std::collections::HashMap<&str, &str> = lockfile
        .images
        .iter()
        .map(|e| (e.reference.as_str(), e.pinned.as_str()))
        .collect();

    let mut rewrites: Vec<(String, String)> = Vec::new();
    let mut out_docs: Vec<String> = Vec::new();

    for filename in filenames {
        let content = std::fs::read_to_string(filename).map_err(|e| {
            crate::cli::cli_error(
                "deploy",
                "read_failed",
                format!("failed to read manifest '{filename}': {e}"),
                serde_json::json!({ "file": filename, "lock": lock }),
            )
        })?;

        for doc in serde_yaml::Deserializer::from_str(&content) {
            let mut value = serde_yaml::Value::deserialize(doc).map_err(|e| {
                crate::cli::cli_error(
                    "deploy",
                    "parse_failed",
                    format!("failed to parse YAML in '{filename}': {e}"),
                    serde_json::json!({ "file": filename }),
                )
            })?;
            // A trailing/blank YAML document round-trips to Null — skip it so it
            // neither inflates the document count nor emits a stray `null` doc.
            if value.is_null() {
                continue;
            }
            rewrite_image_refs(&mut value, &map, &mut rewrites);
            out_docs.push(serde_yaml::to_string(&value)?);
        }
    }

    let yaml_out = out_docs.join("---\n");
    let rewrites_json: Vec<serde_json::Value> = rewrites
        .iter()
        .map(|(old, new)| serde_json::json!({ "reference": old, "pinned": new }))
        .collect();

    if apply {
        let apply_args = ["apply", "-n", namespace, "-f", "-"];
        // In structured mode kubectl's human output must NOT reach stdout (it
        // would corrupt the JSON/YAML stream), so capture it and fold it into
        // the Doc payload. In human mode inherited stdout is the right behavior.
        let (code, kubectl_output) = if printer.is_structured() {
            let (code, out) =
                super::kubectl::run_with_stdin_capture_stdout(&apply_args, &yaml_out)?;
            (code, Some(out))
        } else {
            let code = super::kubectl::run_with_stdin(&apply_args, &yaml_out)?;
            (code, None)
        };
        if code != 0 {
            return Err(crate::cli::cli_error(
                "deploy",
                "apply_failed",
                format!("kubectl apply failed with exit code {code}"),
                serde_json::json!({ "exitCode": code, "namespace": namespace }),
            ));
        }
        let mut payload = serde_json::json!({
            "files": filenames,
            "rewrites": rewrites_json,
            "applied": true,
            "namespace": namespace,
        });
        if let Some(out) = kubectl_output {
            payload["kubectlOutput"] = serde_json::Value::String(out);
        }
        printer.emit(
            Doc::new()
                .status(
                    Role::Ok,
                    format!(
                        "Applied {} document(s), {} reference(s) pinned",
                        out_docs.len(),
                        rewrites.len()
                    ),
                )
                .with_data(payload),
        );
        return Ok(());
    }

    if printer.is_structured() {
        // Structured consumers get the manifest as a field — do NOT also dump
        // raw YAML, which would corrupt the JSON/YAML output stream.
        printer.emit(Doc::new().with_data(serde_json::json!({
            "files": filenames,
            "rewrites": rewrites_json,
            "applied": false,
            "manifest": yaml_out,
        })));
    } else {
        // Human/table mode: stdout must stay a clean pipe (pipeable to kubectl),
        // so the rewrite summary goes to STDERR via tracing, never stdout.
        for (old, new) in &rewrites {
            tracing::info!(reference = %old, pinned = %new, "pinned image reference");
        }
        printer.data_line(&yaml_out);
    }

    Ok(())
}

pub fn cmd_status(printer: &Printer) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(cmd_status_async(printer, None))
}

pub(crate) async fn cmd_status_async(
    printer: &Printer,
    client: Option<kube::Client>,
) -> anyhow::Result<()> {
    let client = match client {
        Some(c) => c,
        None => kube::Client::try_default().await.map_err(|e| {
            crate::cli::cli_error(
                "cluster",
                "kube_connect_failed",
                format!("Failed to connect to cluster: {e}"),
                serde_json::json!({}),
            )
        })?,
    };

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
        .map_err(|e| {
            crate::cli::cli_error(
                "modules",
                "list_failed",
                format!("failed to list modules: {e}"),
                serde_json::json!({}),
            )
        })?;

    let entries: Vec<serde_json::Value> = list
        .items
        .iter()
        .map(|module| {
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
            serde_json::json!({
                "name": name,
                "artifact": artifact,
                "verified": verified,
            })
        })
        .collect();

    let doc = Doc::new().section_or_collapse("Modules", |sb| {
        if list.items.is_empty() {
            sb.status(Role::Info, "No modules found")
        } else {
            let mut sb = sb;
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
                sb = sb.kv(name, format!("{artifact} ({status_icon})"));
            }
            sb
        }
    });

    printer.emit(doc.with_data(serde_json::json!({
        "modules": entries,
        "pods": Vec::<serde_json::Value>::new(),
    })));

    Ok(())
}

pub fn cmd_version(printer: &Printer, namespace: &str) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(cmd_version_async(printer, None, namespace))
}

/// Extract the version (image tag) from a container image reference.
///
/// The tag is the segment after the LAST `:` within the final path component,
/// so a registry host with an explicit port (`host:5000/repo:1.2.3`) is parsed
/// correctly. Any `@sha256:...` digest suffix is stripped first. Returns `None`
/// when the image carries no tag (digest-only or bare repo).
fn image_tag_version(image: &str) -> Option<String> {
    // Strip a digest suffix (`@sha256:...`) — it is not a human version.
    let without_digest = image.split('@').next().unwrap_or(image);
    // The tag lives in the last path component; the host:port colon is in an
    // earlier component, so scope the colon search to after the last `/`.
    let last_component = without_digest.rsplit('/').next().unwrap_or(without_digest);
    last_component
        .rsplit_once(':')
        .map(|(_, tag)| tag.to_string())
        .filter(|tag| !tag.is_empty())
}

/// Resolve a cfgd component version from the container images of a workload's
/// pod template. Prefers the container whose image repository contains
/// `repo_hint` (e.g. `cfgd-operator`); falls back to the first container that
/// carries a parseable tag. Returns `None` when no container has a tag.
fn version_from_containers(images: &[String], repo_hint: &str) -> Option<String> {
    images
        .iter()
        .find(|img| img.contains(repo_hint))
        .and_then(|img| image_tag_version(img))
        .or_else(|| images.iter().find_map(|img| image_tag_version(img)))
}

/// Result of probing a single cluster component (operator Deployment / CSI
/// DaemonSet) for its deployed version. Each variant maps to a stable label so
/// the command degrades to an exit-0, never-panic outcome regardless of cluster
/// state or RBAC.
enum ComponentVersion {
    NotConnected,
    NotDeployed,
    Forbidden,
    Version(String),
}

impl ComponentVersion {
    fn label(&self) -> String {
        match self {
            Self::NotConnected => "not connected".to_string(),
            Self::NotDeployed => "not deployed".to_string(),
            Self::Forbidden => "unknown (forbidden)".to_string(),
            Self::Version(v) => v.clone(),
        }
    }
}

/// Per-probe deadline for `kubectl cfgd version`. This is the connectivity-check
/// command, so a stalled apiserver (TCP accepted, response withheld) must not
/// hang it. Deliberately short — this is an interactive info command, not the
/// crate-wide `COMMAND_TIMEOUT` (2 min) used for real work.
const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Run a `ComponentVersion` probe under `VERSION_PROBE_TIMEOUT`. On timeout the
/// component degrades to `NotConnected` ("not connected"), keeping the command
/// at exit 0.
async fn probe_with_timeout<F>(fut: F) -> ComponentVersion
where
    F: std::future::Future<Output = ComponentVersion>,
{
    probe_with_deadline(VERSION_PROBE_TIMEOUT, fut).await
}

/// Inner timeout wrapper parameterized on the deadline so tests can drive the
/// timeout path with a tiny duration (no real-clock wait, no `test-util`).
async fn probe_with_deadline<F>(deadline: std::time::Duration, fut: F) -> ComponentVersion
where
    F: std::future::Future<Output = ComponentVersion>,
{
    match tokio::time::timeout(deadline, fut).await {
        Ok(v) => v,
        Err(_) => ComponentVersion::NotConnected,
    }
}

/// Map a kube API error to a degraded component label. A 403 becomes
/// `Forbidden`; everything else (connection refused, timeout, 5xx) becomes
/// `NotConnected` — the command never fails because the cluster is unhealthy.
fn degrade_kube_error(err: &kube::Error) -> ComponentVersion {
    match err {
        kube::Error::Api(resp) if resp.code == 403 => ComponentVersion::Forbidden,
        _ => ComponentVersion::NotConnected,
    }
}

async fn operator_version(client: kube::Client, namespace: &str) -> ComponentVersion {
    use k8s_openapi::api::apps::v1::Deployment;
    let api: kube::Api<Deployment> = kube::Api::namespaced(client, namespace);
    match api.list(&kube::api::ListParams::default()).await {
        Ok(list) => {
            let images = deployment_images(&list, "cfgd-operator");
            match images.and_then(|imgs| version_from_containers(&imgs, "cfgd-operator")) {
                Some(v) => ComponentVersion::Version(v),
                None => ComponentVersion::NotDeployed,
            }
        }
        Err(e) => degrade_kube_error(&e),
    }
}

async fn csi_version(client: kube::Client, namespace: &str) -> ComponentVersion {
    use k8s_openapi::api::apps::v1::DaemonSet;
    let api: kube::Api<DaemonSet> = kube::Api::namespaced(client, namespace);
    match api.list(&kube::api::ListParams::default()).await {
        Ok(list) => {
            let images = list
                .items
                .iter()
                .find_map(|ds| {
                    let imgs = pod_template_images(
                        ds.spec.as_ref().and_then(|s| s.template.spec.as_ref()),
                    );
                    imgs.iter().any(|i| i.contains("cfgd-csi")).then_some(imgs)
                })
                .or_else(|| {
                    list.items.first().map(|ds| {
                        pod_template_images(ds.spec.as_ref().and_then(|s| s.template.spec.as_ref()))
                    })
                });
            match images.and_then(|imgs| version_from_containers(&imgs, "cfgd-csi")) {
                Some(v) => ComponentVersion::Version(v),
                None => ComponentVersion::NotDeployed,
            }
        }
        Err(e) => degrade_kube_error(&e),
    }
}

/// Collect container images from the Deployment whose pod template references
/// `repo_hint` (falling back to the first Deployment), so a namespace with
/// unrelated Deployments does not shadow the operator.
fn deployment_images(
    list: &kube::core::ObjectList<k8s_openapi::api::apps::v1::Deployment>,
    repo_hint: &str,
) -> Option<Vec<String>> {
    list.items
        .iter()
        .find_map(|dep| {
            let imgs =
                pod_template_images(dep.spec.as_ref().and_then(|s| s.template.spec.as_ref()));
            imgs.iter().any(|i| i.contains(repo_hint)).then_some(imgs)
        })
        .or_else(|| {
            list.items.first().map(|dep| {
                pod_template_images(dep.spec.as_ref().and_then(|s| s.template.spec.as_ref()))
            })
        })
}

fn pod_template_images(spec: Option<&k8s_openapi::api::core::v1::PodSpec>) -> Vec<String> {
    spec.map(|s| {
        s.containers
            .iter()
            .filter_map(|c| c.image.clone())
            .collect()
    })
    .unwrap_or_default()
}

pub(crate) async fn cmd_version_async(
    printer: &Printer,
    client: Option<kube::Client>,
    namespace: &str,
) -> anyhow::Result<()> {
    // Resolve a client once; reuse it for every cluster probe. A missing client
    // means every cluster-derived field degrades to "not connected".
    let client = match client {
        Some(c) => Some(c),
        None => kube::Client::try_default().await.ok(),
    };

    // Every cluster probe runs under a short deadline so a stalled apiserver
    // cannot hang this connectivity-check command — on timeout the field
    // degrades to "not connected" and the command still exits 0.
    let server_label = match &client {
        Some(c) => {
            let probe = async {
                c.apiserver_version()
                    .await
                    .ok()
                    .map(|v| format!("{}.{}", v.major, v.minor))
            };
            tokio::time::timeout(VERSION_PROBE_TIMEOUT, probe)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "not connected".to_string())
        }
        None => "not connected".to_string(),
    };

    let (operator_label, csi_label) = match &client {
        Some(c) => (
            probe_with_timeout(operator_version(c.clone(), namespace))
                .await
                .label(),
            probe_with_timeout(csi_version(c.clone(), namespace))
                .await
                .label(),
        ),
        None => (
            ComponentVersion::NotConnected.label(),
            ComponentVersion::NotConnected.label(),
        ),
    };

    printer.emit(
        Doc::new()
            .kv("Client", env!("CARGO_PKG_VERSION"))
            .kv("Server (k8s)", &server_label)
            .kv("Operator", &operator_label)
            .kv("CSI", &csi_label)
            .with_data(serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "kubectl": server_label,
                "cfgd": env!("CARGO_PKG_VERSION"),
                "operator": operator_label,
                "csi": csi_label,
            })),
    );

    Ok(())
}

#[cfg(test)]
mod tests;

use clap::{Parser, Subcommand};

use cfgd_core::output::{Doc, KvPair, Printer, Role, Verbosity};

use crate::cli::{ColorWhen, OutputFormatArg};

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

    /// Disable colored output (alias for --color never)
    #[arg(long, global = true)]
    no_color: bool,

    /// When to colorize output: auto (follow the terminal), always, never
    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        env = "CFGD_COLOR",
        default_value = "auto"
    )]
    color: ColorWhen,

    /// Theme preset for this invocation (overrides spec.theme.name; spec.theme.overrides still apply)
    #[arg(
        long,
        global = true,
        value_name = "NAME",
        env = "CFGD_THEME",
        value_parser = clap::builder::PossibleValuesParser::new(cfgd_core::output::Theme::PRESET_NAMES)
    )]
    theme: Option<String>,

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
        /// Namespace (defaults to the kubeconfig current context's namespace, then "default")
        #[arg(long, short)]
        namespace: Option<String>,
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
        /// Namespace (defaults to the kubeconfig current context's namespace, then "default")
        #[arg(long, short)]
        namespace: Option<String>,
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
        /// Namespace (defaults to the kubeconfig current context's namespace, then "default")
        #[arg(long, short)]
        namespace: Option<String>,
    },
    /// Show fleet module status
    #[command(
        long_about = "Show the registered modules and the pods asking for them.\n\n\
                      Modules are cluster-scoped, so every one is listed. Pods are read from a \
                      single namespace: the one --namespace names, else the kubeconfig current \
                      context's, else \"default\".\n\n\
                      Examples:\n  \
                      kubectl cfgd status\n  \
                      kubectl cfgd status --namespace demo\n  \
                      kubectl cfgd -o json status"
    )]
    Status {
        /// Namespace to list module-requesting pods from (defaults to the
        /// kubeconfig current context's namespace, then "default")
        #[arg(long, short)]
        namespace: Option<String>,
    },
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
        /// Namespace passed to `kubectl apply` (only used with --apply); defaults to
        /// the kubeconfig current context's namespace, then "default"
        #[arg(long, short)]
        namespace: Option<String>,
    },
}

const MODULE_REQUIRED: &str = "at least one --module is required";

/// Resolve a namespace-taking subcommand's effective namespace. An explicit
/// `--namespace`/`-n` always wins; when it is omitted, resolve the
/// kubeconfig current context's namespace — the same source plain `kubectl`
/// reads for the same omission — falling back to `"default"` only when the
/// context names none.
fn resolve_namespace(explicit: Option<String>) -> String {
    explicit.unwrap_or_else(current_context_namespace)
}

/// The kubeconfig current context's namespace, via `kube`'s own config
/// loader — the same crate `cmd_debug`/`cmd_status`/`cmd_version` already
/// trust for every other kubeconfig question (it honors `KUBECONFIG`), so a
/// `KUBECONFIG`-fixture test needs no real `kubectl` binary on PATH and no
/// new controlled-command-layer entry. `Config::infer` itself falls back to
/// `"default"` when the context names no namespace; this wrapper does the
/// same when no kubeconfig or in-cluster config can be found at all (a
/// devbox with nothing configured), so resolution never fails — a broken or
/// absent kubeconfig surfaces as a normal connection error later, at the
/// point the command actually contacts the cluster, not as a namespace
/// resolution failure here.
fn current_context_namespace() -> String {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return "default".to_string();
    };
    rt.block_on(async {
        kube::Config::infer()
            .await
            .map(|c| c.default_namespace)
            .unwrap_or_else(|_| "default".to_string())
    })
}

fn parse_module_arg(arg: &str) -> anyhow::Result<(&str, &str)> {
    arg.split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid module format '{arg}' — expected name:version"))
}

fn build_volume_mount(name: &str) -> serde_json::Value {
    let safe = cfgd_core::sanitize_k8s_name(name);
    serde_json::json!({
        "name": format!("cfgd-module-{safe}"),
        "mountPath": module_mount_dir(name),
        "readOnly": true
    })
}

/// Where a module is mounted inside the target container. The `bin/`
/// directory under it is what goes on `PATH`, so a caller composing the PATH
/// prefix appends `/bin` here rather than spelling the root a second time:
/// `debug` writes the mount into the pod patch while `exec` writes it into an
/// `export PATH=`, and a byte of divergence puts a module on a path nothing
/// mounted.
fn module_mount_dir(name: &str) -> String {
    format!("/cfgd-modules/{name}")
}

/// The mount roots and the `PATH` prefix for the modules a caller named, in
/// the order they named them.
fn module_mounts(parsed: &[(&str, &str)]) -> (Vec<String>, String) {
    let dirs: Vec<String> = parsed
        .iter()
        .map(|(name, _)| module_mount_dir(name))
        .collect();
    let path_prefix = dirs
        .iter()
        .map(|dir| format!("{dir}/bin"))
        .collect::<Vec<_>>()
        .join(":");
    (dirs, path_prefix)
}

/// The ephemeral container `kubectl cfgd debug` adds to the pod: an
/// interactive `sh` with every requested module mounted read-only under
/// `/cfgd-modules/<name>` and each module's `bin/` ahead of the image's own
/// `PATH`.
///
/// Its `PS1` names the mounted modules with LITERAL brackets
/// (`[cfgd:nettools:v1] \w $ `). The bash escapes `\[` / `\]` are not
/// brackets: they mark a span as non-printing, so wrapping visible text in
/// them makes bash miscount the prompt's width and redraw long command lines
/// over themselves, while busybox `ash` drops the markers and draws no
/// brackets at all.
fn debug_ephemeral_container(parsed: &[(&str, &str)], image: &str) -> serde_json::Value {
    let volume_mounts: Vec<_> = parsed
        .iter()
        .map(|(name, _version)| build_volume_mount(name))
        .collect();
    let (_, path_prefix) = module_mounts(parsed);
    let module_names: Vec<_> = parsed.iter().map(|(n, v)| format!("{n}:{v}")).collect();
    let ps1 = format!("[cfgd:{}] \\w $ ", module_names.join(","));

    serde_json::json!({
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

    // Tracing to stderr; stdout is reserved for `-o` machine output. Through
    // the same live-region writer the primary CLI installs: this entry point
    // builds a real `Printer` too, and an event written straight at stderr
    // strands whatever bar that printer has on screen.
    let tracing_writer = cfgd_core::output::LiveTracingWriter::new();
    tracing_subscriber::fmt()
        .with_env_filter(cfgd_core::tracing_env_filter("warn"))
        .with_target(false)
        // Same dialect as the primary CLI: one binary, one stamp.
        .with_timer(cfgd_core::output::LocalTimeOfDay)
        // Same reason as the primary CLI: the writer folds every event, and the
        // fold strips ANSI.
        .with_ansi(false)
        .with_writer(tracing_writer.clone())
        .init();

    // Same precedence as the primary CLI (main.rs), via the one shared
    // resolution both entry points call.
    let color_choice = crate::cli::resolve_color_choice(cli.no_color, cli.color);
    // The plugin carries no `--config` flag of its own, so it honours the rest
    // of the primary CLI's precedence: the environment override first, then the
    // default location.
    let config_path = std::env::var_os("CFGD_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::cli::default_config_file);
    let theme_config = crate::cli::resolve_theme_config(&config_path, cli.theme.as_deref());
    let printer = Printer::with_theme_config(
        Verbosity::Normal,
        theme_config.as_ref(),
        cli.output.0,
        color_choice,
    );
    tracing_writer.attach(&printer);

    let result = match cli.command {
        PluginCommand::Debug {
            pod,
            module,
            namespace,
            image,
        } => {
            let namespace = resolve_namespace(namespace);
            cmd_debug(&printer, &pod, &module, &namespace, &image)
        }
        PluginCommand::Exec {
            pod,
            module,
            namespace,
            command,
        } => {
            let namespace = resolve_namespace(namespace);
            cmd_exec(&printer, &pod, &module, &namespace, &command)
        }
        PluginCommand::Inject {
            resource,
            module,
            namespace,
        } => {
            let namespace = resolve_namespace(namespace);
            cmd_inject(&printer, &resource, &module, &namespace)
        }
        PluginCommand::Status { namespace } => {
            let namespace = resolve_namespace(namespace);
            cmd_status(&printer, &namespace)
        }
        PluginCommand::Version { namespace } => cmd_version(&printer, &namespace),
        PluginCommand::Deploy {
            filename,
            lock,
            apply,
            namespace,
        } => {
            let namespace = resolve_namespace(namespace);
            cmd_deploy(&printer, &filename, &lock, apply, &namespace)
        }
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

    let module_names: Vec<_> = parsed.iter().map(|(n, v)| format!("{n}:{v}")).collect();
    let (mount_dirs, path_prefix) = module_mounts(parsed);
    let ec = debug_ephemeral_container(parsed, image);

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

    printer.emit(build_debug_doc(
        namespace,
        pod,
        &module_names,
        image,
        &mount_dirs,
        &path_prefix,
    ));

    Ok(())
}

/// The render `kubectl cfgd debug` settles on: what was created, and the three
/// facts a reader needs to use it — which modules went in, where they landed,
/// and what the container's `PATH` now leads with.
pub fn build_debug_doc(
    namespace: &str,
    pod: &str,
    module_names: &[String],
    image: &str,
    mount_dirs: &[String],
    path_prefix: &str,
) -> Doc {
    Doc::new()
        .status(
            Role::Ok,
            format!("Created ephemeral debug container on pod {namespace}/{pod}"),
        )
        .kv_block([
            ("Modules", module_names.join(", ")),
            ("Mount Path", mount_dirs.join(", ")),
            ("Path Prefix", path_prefix.to_string()),
        ])
        .hint(format!(
            "Attach with `kubectl attach -n {namespace} {pod} -c cfgd-debug -it`"
        ))
        .with_data(serde_json::json!({
            "namespace": namespace,
            "pod": pod,
            "modules": module_names,
            "image": image,
            "mountPath": mount_dirs,
            "pathPrefix": path_prefix,
        }))
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

    let (mount_dirs, path_prefix) = module_mounts(&parsed);

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

    printer.emit(build_exec_doc(
        namespace,
        pod,
        &module_names,
        command,
        &mount_dirs,
        &path_prefix,
    ));

    let code = super::kubectl::run_argv_inherit(&exec_args)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// The render `kubectl cfgd exec` settles on before handing the terminal to
/// `kubectl`: the same three module facts `debug` reports, so a reader moving
/// between the two commands reads one shape.
pub fn build_exec_doc(
    namespace: &str,
    pod: &str,
    module_names: &[String],
    command: &[String],
    mount_dirs: &[String],
    path_prefix: &str,
) -> Doc {
    Doc::new()
        .status(
            Role::Info,
            format!("Executing in {namespace}/{pod} with modules"),
        )
        .kv_block([
            ("Modules", module_names.join(", ")),
            ("Mount Path", mount_dirs.join(", ")),
            ("Path Prefix", path_prefix.to_string()),
        ])
        .with_data(serde_json::json!({
            "namespace": namespace,
            "pod": pod,
            "modules": module_names,
            "command": command,
            "mountPath": mount_dirs,
            "pathPrefix": path_prefix,
        }))
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
            // The patch rewrote the pod template, so the controller is
            // already rolling; name the command that watches it land.
            .hint(format!(
                "Run `kubectl rollout status {kind}/{name} -n {namespace}` to watch the pods pick them up"
            ))
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
                        "Applied {}, {} pinned",
                        cfgd_core::pluralize(out_docs.len(), "document"),
                        cfgd_core::pluralize(rewrites.len(), "reference")
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
        // so the rewrite summary goes to STDERR — through the Printer, which is
        // the human channel, rather than through tracing, whose default filter
        // means nobody reads it.
        for (old, new) in &rewrites {
            printer.status_simple(
                Role::Info,
                format!("pinned {old} {} {new}", printer.arrow()),
            );
        }
        printer.data_line(&yaml_out);
    }

    Ok(())
}

pub fn cmd_status(printer: &Printer, namespace: &str) -> anyhow::Result<()> {
    let context = current_context_name();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(cmd_status_async(printer, None, &context, namespace))
}

/// The kubeconfig's current-context NAME, which `kube::Config` does not carry
/// (it resolves a context into a connection and keeps no label for it). Read
/// from the kubeconfig directly so the fact block names the same context
/// `kubectl config current-context` would; an in-cluster or absent kubeconfig
/// has no context to name.
fn current_context_name() -> String {
    kube::config::Kubeconfig::read()
        .ok()
        .and_then(|kc| kc.current_context)
        .unwrap_or_else(|| "in-cluster".to_string())
}

/// One module row: the facts both the human render and `-o json` answer from.
struct ModuleRow {
    name: String,
    artifact: String,
    verified: bool,
    signature: String,
}

impl ModuleRow {
    fn from_object(module: &kube::core::DynamicObject) -> Self {
        let verified = module
            .data
            .pointer("/status/verified")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Self {
            name: module.metadata.name.clone().unwrap_or_else(|| "?".into()),
            artifact: module
                .data
                .pointer("/spec/ociArtifact")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(cfgd_core::ABSENT)
                .to_string(),
            verified,
            // The controller writes the verdict; a Module it has not reconciled
            // yet carries none, and the same three-word vocabulary is derived
            // here rather than spelling a fourth wording for that case.
            signature: module
                .data
                .pointer("/status/signature")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    let declared = module.data.pointer("/spec/signature").is_some();
                    cfgd_crd::ModuleStatus::signature_verdict(verified, declared).to_string()
                }),
        }
    }
}

/// One pod row: which cfgd modules the pod's annotation asks for.
struct PodRow {
    name: String,
    modules: Vec<String>,
}

pub(crate) async fn cmd_status_async(
    printer: &Printer,
    client: Option<kube::Client>,
    context: &str,
    namespace: &str,
) -> anyhow::Result<()> {
    let client = match client {
        Some(c) => c,
        None => kube::Client::try_default().await.map_err(|e| {
            crate::cli::cli_error(
                "cluster",
                "kube_connect_failed",
                format!("Failed to connect to cluster: {e}"),
                serde_json::json!({ "namespace": namespace }),
            )
        })?,
    };

    let modules: kube::Api<kube::core::DynamicObject> = kube::Api::all_with(
        client.clone(),
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
                serde_json::json!({ "namespace": namespace }),
            )
        })?;
    let module_rows: Vec<ModuleRow> = list.items.iter().map(ModuleRow::from_object).collect();

    let pods: kube::Api<k8s_openapi::api::core::v1::Pod> = kube::Api::namespaced(client, namespace);
    let pod_list = pods
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|e| {
            crate::cli::cli_error(
                "pods",
                "list_failed",
                format!("failed to list pods: {e}"),
                serde_json::json!({ "namespace": namespace }),
            )
        })?;
    let pod_rows: Vec<PodRow> = pod_list
        .items
        .iter()
        .filter_map(|pod| {
            let annotation = pod
                .metadata
                .annotations
                .as_ref()?
                .get(cfgd_core::MODULES_ANNOTATION)?;
            Some(PodRow {
                name: pod.metadata.name.clone().unwrap_or_else(|| "?".into()),
                modules: annotation
                    .split(',')
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(str::to_string)
                    .collect(),
            })
        })
        .collect();

    let doc = Doc::new()
        .heading("Status")
        .kv_block([("Context", context), ("Namespace", namespace)])
        .section_or_collapse("Modules", |sb| {
            if module_rows.is_empty() {
                sb.status(Role::Info, "No modules found")
            } else {
                module_rows.iter().fold(sb, |sb, row| {
                    sb.kv_rows([KvPair::annotated(&row.name, &row.artifact, &row.signature)])
                })
            }
        })
        .section_or_collapse("Pods", |sb| {
            if pod_rows.is_empty() {
                sb.status(Role::Info, "No pods requesting modules")
            } else {
                pod_rows
                    .iter()
                    .fold(sb, |sb, row| sb.kv(&row.name, row.modules.join(", ")))
            }
        });

    printer.emit(doc.with_data(serde_json::json!({
        "context": context,
        "namespace": namespace,
        "modules": module_rows
            .iter()
            .map(|row| serde_json::json!({
                "name": row.name,
                "artifact": row.artifact,
                "verified": row.verified,
                "signature": row.signature,
            }))
            .collect::<Vec<_>>(),
        "pods": pod_rows
            .iter()
            .map(|row| serde_json::json!({ "name": row.name, "modules": row.modules }))
            .collect::<Vec<_>>(),
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
                // Which namespace the operator/CSI labels above describe: a
                // reader comparing two runs cannot tell "not deployed" from
                // "not deployed THERE" without it.
                "namespace": namespace,
            })),
    );

    Ok(())
}

#[cfg(test)]
mod tests;

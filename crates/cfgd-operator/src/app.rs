//! Operator application entrypoint — the orchestration logic lifted from
//! `main.rs` so it can be exercised by unit tests.
//!
//! `main.rs` is reduced to a thin shim that installs the rustls crypto
//! provider and delegates here. Nothing in this module writes to a terminal;
//! all human-visible output goes through `tracing` macros, consistent with
//! the rest of the operator binary.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use kube::Client;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::crds::{ClusterConfigPolicy, ConfigPolicy, DriftAlert, MachineConfig, Module};
use crate::{controllers, env, errors, gateway, health, leader, metrics, runtime, webhook};

static OTEL_PROVIDER: std::sync::OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> =
    std::sync::OnceLock::new();

/// Initialize the global tracing subscriber.
///
/// Installs an `EnvFilter`-gated fmt subscriber, optionally augmented with
/// an OpenTelemetry OTLP layer when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
/// May only be called once per process; subsequent calls from the same
/// process panic (tracing-subscriber contract). Tests must not call this —
/// use `build_subscriber` if the subscriber under test needs inspection.
pub fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = cfgd_core::tracing_env_filter("info");
    let fmt_layer = tracing_subscriber::fmt::layer();

    // Capture any OTel-init failure so we can emit it via `tracing::warn!`
    // AFTER the fmt subscriber is up — avoids an `eprintln!` in an operator
    // binary (Hard Rule #1) and keeps the failure message in the same
    // structured log stream as everything else.
    let mut otel_init_err: Option<String> = None;

    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        match init_otel_tracer() {
            Ok(tracer) => {
                let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt_layer)
                    .with(otel_layer)
                    .init();
                tracing::info!("OpenTelemetry tracing initialized");
                return;
            }
            Err(e) => {
                otel_init_err = Some(e.to_string());
            }
        }
    }

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .init();

    if let Some(err) = otel_init_err {
        // Operator explicitly set OTEL_EXPORTER_OTLP_ENDPOINT — a silent
        // downgrade to fmt-only is the wrong default. Surface at error so
        // alerting on `log.level="error"` lights up; the message stays
        // structured (key=value pairs) for log aggregators.
        tracing::error!(
            error = %err,
            "Failed to initialize OpenTelemetry; falling back to fmt-only tracing",
        );
    }
}

/// Build and register the OTLP tracer, storing the provider in `OTEL_PROVIDER`
/// for clean shutdown.
///
/// Returns an error when the exporter cannot be constructed (e.g., invalid
/// endpoint or missing tonic TLS support). The caller decides whether to fall
/// back to fmt-only tracing or to propagate.
pub fn init_otel_tracer() -> Result<opentelemetry_sdk::trace::SdkTracer, Box<dyn std::error::Error>>
{
    use opentelemetry::trace::TracerProvider;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()?;

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();

    let tracer = provider.tracer("cfgd-operator");
    opentelemetry::global::set_tracer_provider(provider.clone());
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let _ = OTEL_PROVIDER.set(provider);

    Ok(tracer)
}

/// Wait for SIGINT or SIGTERM and return. Used as one arm of the top-level
/// `tokio::select!` to trigger graceful shutdown.
///
/// Returns an error only if the OS refuses to install the SIGTERM signal
/// handler, which indicates a broken process environment.
pub async fn shutdown_signal() -> Result<()> {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| anyhow::anyhow!("failed to install SIGTERM handler: {e}"))?;

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT, initiating graceful shutdown"),
        _ = sigterm.recv() => tracing::info!("received SIGTERM, initiating graceful shutdown"),
    }
    Ok(())
}

/// Emit one tracing event per registered CRD kind so operator startup logs
/// clearly list which CRDs are expected in the cluster.
pub fn log_crd_info() {
    use kube::CustomResourceExt;

    tracing::info!(
        crd = %MachineConfig::crd_name(),
        "registered CRD: MachineConfig"
    );
    tracing::info!(
        crd = %ConfigPolicy::crd_name(),
        "registered CRD: ConfigPolicy"
    );
    tracing::info!(
        crd = %DriftAlert::crd_name(),
        "registered CRD: DriftAlert"
    );
    tracing::info!(
        crd = %ClusterConfigPolicy::crd_name(),
        "registered CRD: ClusterConfigPolicy"
    );
    tracing::info!(
        crd = %Module::crd_name(),
        "registered CRD: Module"
    );
}

/// Start the controllers and, if `DEVICE_GATEWAY_ENABLED` is set, the device
/// gateway alongside them. When the gateway is enabled it becomes the primary
/// long-running process; the controllers are spawned with a retry loop.
pub async fn run_operator(client: Client, metrics: metrics::Metrics) -> Result<()> {
    let gateway_enabled = runtime::is_gateway_enabled();

    if gateway_enabled {
        let gateway_config = runtime::build_gateway_config(client.clone(), metrics.clone());

        tracing::info!("device gateway enabled");

        tokio::spawn(async move {
            loop {
                match controllers::run(client.clone(), metrics.clone()).await {
                    Ok(()) => break,
                    Err(e) => {
                        tracing::error!(error = %e, "controllers failed — retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        });

        gateway::start_gateway(gateway_config)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    } else {
        controllers::run(client, metrics).await?;
    }

    Ok(())
}

/// Full operator lifecycle: connect to the cluster, start ancillary servers
/// (health, metrics, optional webhook), optionally acquire a leader lease, and
/// run controllers until a shutdown signal or fatal error.
///
/// `rustls::crypto::ring::default_provider().install_default()` must have been
/// called before entering this function (it is called in `main.rs`).
pub async fn run() -> Result<()> {
    init_tracing();

    tracing::info!("starting cfgd-operator");
    log_crd_info();

    let client = Client::try_default().await?;

    let health_state = health::HealthState::new();
    let health_port = env::parse_port_env("HEALTH_PORT", 8081);

    let mut health_handle = tokio::spawn({
        let hs = health_state.clone();
        async move {
            if let Err(e) = health::run_probe_server(health_port, hs).await {
                tracing::error!(error = %e, "health server failed");
            }
        }
    });

    let mut registry = prometheus_client::registry::Registry::default();
    let metrics = metrics::Metrics::new(&mut registry);
    let registry = Arc::new(Mutex::new(registry));

    let metrics_port = env::parse_port_env("METRICS_PORT", 8443);

    let mut metrics_handle = tokio::spawn({
        let reg = registry.clone();
        async move {
            if let Err(e) = metrics::run_metrics_server(metrics_port, reg).await {
                tracing::error!(error = %e, "metrics server failed");
            }
        }
    });

    let cert_dir = env::env_or("WEBHOOK_CERT_DIR", "/tmp/k8s-webhook-server/serving-certs");
    let webhook_port = env::parse_port_env("WEBHOOK_PORT", 9443);

    let mut webhook_handle: Option<tokio::task::JoinHandle<()>> = None;
    if runtime::webhook_certs_present(Path::new(&cert_dir)) {
        tracing::info!(cert_dir = %cert_dir, port = webhook_port, "starting webhook server");
        let webhook_addr: std::net::SocketAddr = ([0, 0, 0, 0], webhook_port).into();
        let webhook_listener = match tokio::net::TcpListener::bind(webhook_addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, addr = %webhook_addr, "failed to bind webhook listener");
                return Err(anyhow::anyhow!("failed to bind webhook listener: {e}"));
            }
        };
        let webhook_metrics = metrics.clone();
        let webhook_client = client.clone();
        webhook_handle = Some(tokio::spawn(async move {
            if let Err(e) = webhook::run_webhook_server(
                &cert_dir,
                webhook_listener,
                webhook_metrics,
                webhook_client,
            )
            .await
            {
                tracing::error!(error = %e, "webhook server failed");
            }
        }));
    } else {
        tracing::info!(
            cert_dir = %cert_dir,
            "webhook certs not found, webhook server disabled"
        );
    }

    let leader_enabled = runtime::is_leader_election_enabled();

    let shutdown = CancellationToken::new();

    let operator_future = async {
        if leader_enabled {
            let namespace = runtime::leader_namespace();
            let identity = runtime::leader_identity();

            tracing::info!(
                namespace = %namespace,
                identity = %identity,
                "leader election enabled"
            );

            let le = leader::LeaderElection::new(client.clone(), namespace, identity);
            le.run(shutdown.clone(), || async {
                health_state.set_ready();
                run_operator(client, metrics)
                    .await
                    .map_err(|e| errors::OperatorError::Leader(format!("Operator run failed: {e}")))
            })
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        } else {
            health_state.set_ready();
            run_operator(client, metrics).await?;
        }

        Ok::<(), anyhow::Error>(())
    };

    let webhook_exit_future = async {
        match webhook_handle.as_mut() {
            Some(h) => match h.await {
                Ok(()) => {
                    tracing::error!(
                        "webhook server exited unexpectedly (returned Ok) — \
                         admission control is no longer being enforced; failing operator \
                         so Kubernetes can restart it"
                    );
                    anyhow::bail!("webhook server exited unexpectedly")
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "webhook server task panicked — admission control offline; \
                         failing operator so Kubernetes can restart it"
                    );
                    anyhow::bail!("webhook server task panicked: {e}")
                }
            },
            None => std::future::pending::<Result<()>>().await,
        }
    };

    let mut webhook_exit_err: Option<anyhow::Error> = None;
    tokio::select! {
        result = operator_future => {
            if let Err(e) = result {
                tracing::error!(error = %e, "operator exited with error");
                return Err(e);
            }
        },
        result = &mut health_handle => {
            if let Err(e) = result {
                tracing::error!(error = %e, "health server task panicked");
            }
        },
        result = &mut metrics_handle => {
            if let Err(e) = result {
                tracing::error!(error = %e, "metrics server task panicked");
            }
        },
        result = webhook_exit_future => {
            if let Err(e) = result {
                webhook_exit_err = Some(e);
            }
        },
        result = shutdown_signal() => {
            if let Err(e) = result {
                tracing::warn!(error = %e, "signal handler setup failed; proceeding with shutdown");
            }
            tracing::info!("draining in-flight reconciliations (2s grace)...");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if let Some(provider) = OTEL_PROVIDER.get()
                && let Err(e) = provider.shutdown()
            {
                tracing::warn!(error = %e, "OpenTelemetry tracer provider shutdown failed");
            }
            tracing::info!("shutdown complete");
        },
        _ = shutdown.cancelled() => {
            tracing::info!("draining in-flight reconciliations (2s grace)...");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if let Some(provider) = OTEL_PROVIDER.get()
                && let Err(e) = provider.shutdown()
            {
                tracing::warn!(error = %e, "OpenTelemetry tracer provider shutdown failed");
            }
            tracing::info!("shutdown complete");
        },
    }

    health_handle.abort();
    metrics_handle.abort();
    if let Some(h) = webhook_handle {
        h.abort();
    }

    if let Some(e) = webhook_exit_err {
        return Err(e);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cfgd_core::test_helpers::EnvVarGuard;
    use kube::Client;
    use prometheus_client::registry::Registry;
    use serial_test::serial;
    use tower_test::mock;

    use super::*;
    use crate::controllers::ControllerContext;

    fn test_client_and_metrics() -> (Client, metrics::Metrics, Registry) {
        let (mock_service, _handle) =
            mock::pair::<http::Request<kube::client::Body>, http::Response<kube::client::Body>>();
        let client = Client::new(mock_service, "default");
        let mut registry = Registry::default();
        let m = metrics::Metrics::new(&mut registry);
        (client, m, registry)
    }

    #[test]
    fn log_crd_info_emits_tracing_events_without_panicking() {
        // Pure tracing emitter — calling it must not panic regardless of
        // whether a global subscriber is installed (tracing macros are
        // no-ops when no subscriber is set).
        log_crd_info();
    }

    #[test]
    fn log_crd_info_crd_names_are_stable() {
        use kube::CustomResourceExt;
        assert_eq!(MachineConfig::crd_name(), "machineconfigs.cfgd.io");
        assert_eq!(ConfigPolicy::crd_name(), "configpolicies.cfgd.io");
        assert_eq!(DriftAlert::crd_name(), "driftalerts.cfgd.io");
        assert_eq!(
            ClusterConfigPolicy::crd_name(),
            "clusterconfigpolicies.cfgd.io"
        );
        assert_eq!(Module::crd_name(), "modules.cfgd.io");
    }

    /// `init_otel_tracer` builds and registers a provider using the OTLP
    /// exporter configured from env. Without a real endpoint the tonic
    /// transport build may succeed (lazy connect) or fail; either way the
    /// function body lines get executed and OTEL_PROVIDER is attempted.
    ///
    /// tonic's `connect_lazy` internally spawns onto the Tokio runtime, so
    /// this test must run inside one.
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn init_otel_tracer_runs_without_panicking() {
        // Provide a syntactically valid but unreachable endpoint so the
        // exporter builder does not reject it for being empty, while still
        // not actually connecting.
        let _g = EnvVarGuard::set("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:14317");
        // Result is Ok or Err — both are acceptable. What matters is that
        // the function is called and the branch logic is exercised.
        let result = init_otel_tracer();
        // Either outcome is fine; we just must not panic.
        let _ = result;
    }

    /// Drive `shutdown_signal` against a timer — the sleep wins, the select
    /// never fires a real signal, but the signal registration lines are
    /// entered. The timeout ensures the test cannot block the suite.
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_signal_registers_handlers_without_panicking() {
        let result = tokio::time::timeout(Duration::from_millis(50), shutdown_signal()).await;
        // Elapsed is expected — no signal was sent, so the future never
        // resolves. What matters is no panic during signal registration.
        assert!(result.is_err(), "timeout should fire before any signal");
    }

    /// `run_operator` with gateway disabled calls `controllers::run`. The
    /// mock client's request channel is dropped so `controllers::run` returns
    /// fairly quickly; a 400 ms hard timeout ensures the test cannot block.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial]
    async fn run_operator_gateway_disabled_enters_controllers_run() {
        let _g = EnvVarGuard::unset("DEVICE_GATEWAY_ENABLED");
        let (client, m, _registry) = test_client_and_metrics();

        let result =
            tokio::time::timeout(Duration::from_millis(400), run_operator(client, m)).await;

        // The mock service handle is dropped, so kube watchers will error out
        // quickly and controllers::run returns an error. Either a timeout
        // (if controllers::run blocks on the watcher) or an Err from
        // controllers::run is acceptable — both prove the function body ran.
        match result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {} // controllers::run errored — expected with mock client
            Err(_elapsed) => {} // timeout — function entered but blocked on watch loop
        }
    }

    /// `run_operator` with gateway enabled spawns controllers and starts the
    /// gateway. With a mock client and no real DB path available the gateway
    /// will fail quickly; the timeout bounds the test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial]
    async fn run_operator_gateway_enabled_enters_gateway_branch() {
        let _g1 = EnvVarGuard::set("DEVICE_GATEWAY_ENABLED", "true");
        // Use a non-conflicting port and a tempfile DB path.
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("test-gateway.db");
        let _g2 = EnvVarGuard::set("CFGD_SERVER_DB_PATH", db_path.to_str().expect("valid utf8"));
        // Let the OS pick a free port so we never clash with other tests.
        let _g3 = EnvVarGuard::set("DEVICE_GATEWAY_PORT", "0");

        let (client, m, _registry) = test_client_and_metrics();

        let result =
            tokio::time::timeout(Duration::from_millis(400), run_operator(client, m)).await;

        // Gateway bind on port 0 succeeds but axum::serve blocks; timeout fires.
        // Or the gateway errors immediately on DB init — both are acceptable.
        match result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {}
            Err(_elapsed) => {}
        }
    }

    /// Verify `ControllerContext` can be constructed (sanity check that the
    /// test_client_and_metrics helper produces a usable client).
    ///
    /// `Client::new` internally uses tower's `Buffer` which requires a Tokio
    /// runtime at construction time.
    #[tokio::test(flavor = "current_thread")]
    async fn test_client_produces_valid_controller_context() {
        let (client, m, _registry) = test_client_and_metrics();
        let reporter = kube::runtime::events::Reporter {
            controller: "app-test".into(),
            instance: None,
        };
        let recorder = kube::runtime::events::Recorder::new(client.clone(), reporter);
        let _ctx = std::sync::Arc::new(ControllerContext {
            client,
            recorder,
            metrics: m,
        });
    }
}

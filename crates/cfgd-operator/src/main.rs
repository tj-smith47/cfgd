use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use kube::Client;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use cfgd_operator::{controllers, env, errors, gateway, health, leader, metrics, runtime, webhook};

static OTEL_PROVIDER: std::sync::OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> =
    std::sync::OnceLock::new();

use cfgd_operator::crds::{ClusterConfigPolicy, ConfigPolicy, DriftAlert, MachineConfig, Module};

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

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
        _ = shutdown_signal() => {
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

async fn run_operator(client: Client, metrics: metrics::Metrics) -> Result<()> {
    // Device gateway — optional HTTP server for device checkin, enrollment, drift, web UI
    let gateway_enabled = runtime::is_gateway_enabled();

    if gateway_enabled {
        let gateway_config = runtime::build_gateway_config(client.clone(), metrics.clone());

        tracing::info!("device gateway enabled");

        // Spawn controllers with retry — if they fail, retry after delay
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

        // Gateway is the primary process — if it exits, we exit
        gateway::start_gateway(gateway_config)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    } else {
        controllers::run(client, metrics).await?;
    }

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT, initiating graceful shutdown"),
        _ = sigterm.recv() => tracing::info!("received SIGTERM, initiating graceful shutdown"),
    }
}

fn init_tracing() {
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

fn init_otel_tracer() -> Result<opentelemetry_sdk::trace::SdkTracer, Box<dyn std::error::Error>> {
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

fn log_crd_info() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_crd_info_emits_tracing_events_without_panicking() {
        // Pure tracing emitter — calling it must not panic regardless of
        // whether a global subscriber is installed (tracing macros are
        // no-ops when no subscriber is set).
        log_crd_info();
    }
}

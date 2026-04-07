mod controllers;
mod crds;
mod errors;
mod gateway;
mod health;
mod leader;
mod metrics;
mod webhook;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use kube::Client;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

static OTEL_PROVIDER: std::sync::OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> =
    std::sync::OnceLock::new();

use crate::crds::{ClusterConfigPolicy, ConfigPolicy, DriftAlert, MachineConfig, Module};
use crate::gateway::GatewayConfig;

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
    let health_port: u16 = match std::env::var("HEALTH_PORT") {
        Ok(val) => val.parse().unwrap_or_else(|e| {
            tracing::warn!(value = %val, error = %e, "invalid HEALTH_PORT, using default 8081");
            8081
        }),
        Err(_) => 8081,
    };

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

    let metrics_port: u16 = match std::env::var("METRICS_PORT") {
        Ok(val) => val.parse().unwrap_or_else(|e| {
            tracing::warn!(value = %val, error = %e, "invalid METRICS_PORT, using default 8443");
            8443
        }),
        Err(_) => 8443,
    };

    let mut metrics_handle = tokio::spawn({
        let reg = registry.clone();
        async move {
            if let Err(e) = metrics::run_metrics_server(metrics_port, reg).await {
                tracing::error!(error = %e, "metrics server failed");
            }
        }
    });

    let cert_dir = std::env::var("WEBHOOK_CERT_DIR")
        .unwrap_or_else(|_| "/tmp/k8s-webhook-server/serving-certs".to_string());
    let webhook_port: u16 = match std::env::var("WEBHOOK_PORT") {
        Ok(val) => val.parse().unwrap_or_else(|e| {
            tracing::warn!(value = %val, error = %e, "invalid WEBHOOK_PORT, using default 9443");
            9443
        }),
        Err(_) => 9443,
    };

    if Path::new(&cert_dir).join("tls.crt").exists() {
        tracing::info!(cert_dir = %cert_dir, port = webhook_port, "starting webhook server");
        let webhook_metrics = metrics.clone();
        let webhook_client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = webhook::run_webhook_server(
                &cert_dir,
                webhook_port,
                webhook_metrics,
                webhook_client,
            )
            .await
            {
                tracing::error!(error = %e, "webhook server failed");
            }
        });
    } else {
        tracing::info!(
            cert_dir = %cert_dir,
            "webhook certs not found, webhook server disabled"
        );
    }

    let leader_enabled = std::env::var("LEADER_ELECTION_ENABLED")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    let shutdown = CancellationToken::new();

    let operator_future = async {
        if leader_enabled {
            let namespace =
                std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "cfgd-system".to_string());
            let identity = std::env::var("POD_NAME").unwrap_or_else(|_| Uuid::new_v4().to_string());

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

    // Abort remaining spawned tasks on exit
    health_handle.abort();
    metrics_handle.abort();

    Ok(())
}

async fn run_operator(client: Client, metrics: metrics::Metrics) -> Result<()> {
    // Device gateway — optional HTTP server for device checkin, enrollment, drift, web UI
    let gateway_enabled = std::env::var("DEVICE_GATEWAY_ENABLED")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if gateway_enabled {
        let gateway_config = GatewayConfig {
            port: std::env::var("DEVICE_GATEWAY_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8080),
            db_path: std::env::var("CFGD_SERVER_DB_PATH")
                .unwrap_or_else(|_| "/data/cfgd-gateway.db".to_string()),
            kube_client: Some(client.clone()),
            retention_days: std::env::var("CFGD_RETENTION_DAYS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(90),
            metrics: Some(metrics.clone()),
        };

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

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer();

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
                // Tracing not yet initialized for structured output; log to stderr
                eprintln!("Failed to initialize OpenTelemetry: {e}, falling back to fmt only");
            }
        }
    }

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .init();
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

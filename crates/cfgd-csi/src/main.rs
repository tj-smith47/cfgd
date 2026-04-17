use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

use cfgd_csi::cache::Cache;
use cfgd_csi::csi::v1::identity_server::IdentityServer;
use cfgd_csi::csi::v1::node_server::NodeServer;
use cfgd_csi::identity::CfgdIdentity;
use cfgd_csi::metrics::CsiMetrics;
use cfgd_csi::node::CfgdNode;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(cfgd_core::tracing_env_filter("info"))
        .json()
        .init();

    let socket_path = env_or("CSI_ENDPOINT", "/csi/csi.sock");
    let cache_dir = PathBuf::from(env_or("CACHE_DIR", "/var/lib/cfgd-csi/cache"));
    let cache_max_str = env_or("CACHE_MAX_BYTES", "5368709120");
    let cache_max: u64 = cache_max_str.parse().unwrap_or_else(|e| {
        tracing::warn!(value = %cache_max_str, error = %e, "invalid CACHE_MAX_BYTES, using default 5GB");
        5_368_709_120
    });
    let metrics_port_str = env_or("METRICS_PORT", "9090");
    let metrics_port: u16 = metrics_port_str.parse().unwrap_or_else(|e| {
        tracing::warn!(value = %metrics_port_str, error = %e, "invalid METRICS_PORT, using default 9090");
        9090
    });

    let node_id = cfgd_core::hostname_string();

    tracing::info!(
        socket = %socket_path,
        cache_dir = %cache_dir.display(),
        cache_max_bytes = cache_max,
        node_id = %node_id,
        "starting cfgd-csi"
    );

    let cache = Arc::new(Cache::new(cache_dir.clone(), cache_max)?);

    let mut registry = prometheus_client::registry::Registry::default();
    let metrics = Arc::new(CsiMetrics::new(&mut registry));
    let registry = Arc::new(registry);

    // Metrics HTTP server (axum, matching operator pattern). Handler shape
    // mirrors `cfgd-operator::metrics::metrics_handler` — same Content-Type
    // on both success and error so Prometheus scrapers don't see two
    // different response formats depending on which component they hit.
    let metrics_registry = registry.clone();
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/metrics",
            axum::routing::get(move || {
                let r = metrics_registry.clone();
                async move {
                    let mut buf = String::new();
                    if prometheus_client::encoding::text::encode(&mut buf, &r).is_err() {
                        return (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            [(
                                axum::http::header::CONTENT_TYPE,
                                "text/plain; charset=utf-8",
                            )],
                            "Failed to encode metrics".to_string(),
                        );
                    }
                    (
                        axum::http::StatusCode::OK,
                        [(
                            axum::http::header::CONTENT_TYPE,
                            "application/openmetrics-text; version=1.0.0; charset=utf-8",
                        )],
                        buf,
                    )
                }
            }),
        );
        let listener = match tokio::net::TcpListener::bind(("0.0.0.0", metrics_port)).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(port = metrics_port, error = %e, "failed to bind metrics port");
                return;
            }
        };
        tracing::info!(port = metrics_port, "metrics server listening");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "metrics server failed");
        }
    });

    // Remove stale socket
    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = PathBuf::from(&socket_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    let stream = UnixListenerStream::new(listener);

    tracing::info!(socket = %socket_path, "gRPC server listening");

    Server::builder()
        .add_service(IdentityServer::new(CfgdIdentity::new(cache_dir)))
        .add_service(NodeServer::new(CfgdNode::new(cache, metrics, node_id)))
        .serve_with_incoming_shutdown(stream, async {
            shutdown_signal().await;
            tracing::info!("received shutdown signal, draining");
        })
        .await?;

    tracing::info!("cfgd-csi stopped");
    Ok(())
}

async fn shutdown_signal() {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to register SIGTERM handler");
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = ctrl_c => {}
    }
}

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
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    let socket_path = env_or("CSI_ENDPOINT", "/csi/csi.sock");
    let cache_dir = PathBuf::from(env_or("CACHE_DIR", "/var/lib/cfgd-csi/cache"));
    let cache_max: u64 = env_or("CACHE_MAX_BYTES", "5368709120")
        .parse()
        .unwrap_or(5_368_709_120);
    let metrics_port: u16 = env_or("METRICS_PORT", "9090")
        .parse()
        .unwrap_or(9090);

    let node_id = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    tracing::info!(
        socket = %socket_path,
        cache_dir = %cache_dir.display(),
        cache_max_bytes = cache_max,
        node_id = %node_id,
        "starting cfgd-csi"
    );

    let cache = Arc::new(Cache::new(cache_dir.clone(), cache_max)?);

    // Metrics
    let mut registry = prometheus_client::registry::Registry::default();
    let _metrics = Arc::new(CsiMetrics::new(&mut registry));
    let registry = Arc::new(registry);

    // Metrics HTTP server
    let metrics_registry = registry.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_metrics(metrics_port, metrics_registry).await {
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
        .add_service(NodeServer::new(CfgdNode::new(cache, node_id)))
        .serve_with_incoming_shutdown(stream, async {
            shutdown_signal().await;
            tracing::info!("received shutdown signal, draining");
        })
        .await?;

    tracing::info!("cfgd-csi stopped");
    Ok(())
}

async fn shutdown_signal() {
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = ctrl_c => {}
    }
}

async fn serve_metrics(
    port: u16,
    registry: Arc<prometheus_client::registry::Registry>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port = port, "metrics server listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let registry = registry.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |_req| {
                let registry = registry.clone();
                async move {
                    let mut buf = String::new();
                    if prometheus_client::encoding::text::encode(&mut buf, &registry).is_err() {
                        return Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .status(500)
                                .body(http_body_util::Full::new(hyper::body::Bytes::from(
                                    "encoding error",
                                )))
                                .unwrap(),
                        );
                    }
                    let body_bytes = buf.into_bytes();
                    Ok(hyper::Response::builder()
                        .status(200)
                        .header("Content-Type", "text/plain; charset=utf-8")
                        .header("Content-Length", body_bytes.len())
                        .body(http_body_util::Full::new(hyper::body::Bytes::from(
                            body_bytes,
                        )))
                        .unwrap())
                }
            });
            let io = hyper_util::rt::TokioIo::new(stream);
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

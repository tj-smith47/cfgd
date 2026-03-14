use std::net::SocketAddr;
use std::sync::Arc;

use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

mod api;
mod db;
mod errors;
mod fleet;
mod web;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            std::env::var("CFGD_SERVER_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(8080);
    let db_path = args
        .get(2)
        .map(|s| s.to_string())
        .or_else(|| std::env::var("CFGD_SERVER_DB_PATH").ok())
        .unwrap_or_else(|| "cfgd-server.db".to_string());

    let db = db::ServerDb::open(&db_path)?;

    let kube_client = match kube::Client::try_default().await {
        Ok(client) => {
            tracing::info!(
                "Kubernetes client initialized — DriftAlert CRDs will be created on drift events"
            );
            Some(client)
        }
        Err(e) => {
            tracing::info!(
                "No Kubernetes access ({}) — running in standalone mode, drift events recorded in database only",
                e
            );
            None
        }
    };

    if std::env::var("CFGD_API_KEY").is_ok() {
        tracing::info!("API key authentication enabled (CFGD_API_KEY set)");
    } else {
        tracing::error!("==========================================================");
        tracing::error!("  WARNING: CFGD_API_KEY is not set!");
        tracing::error!("  All API and web UI endpoints are unauthenticated.");
        tracing::error!("  Set CFGD_API_KEY to enable authentication.");
        tracing::error!("==========================================================");
    }

    // Run retention cleanup on startup
    db.cleanup_old_events(90)?;

    let (event_tx, _) = tokio::sync::broadcast::channel(256);

    let state = api::AppState {
        db: Arc::new(tokio::sync::Mutex::new(db)),
        kube_client,
        event_tx,
    };

    let app = api::router(state.clone())
        .merge(web::router())
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, db_path = %db_path, "cfgd-server starting");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

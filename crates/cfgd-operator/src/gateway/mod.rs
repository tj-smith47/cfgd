pub mod api;
pub mod db;
pub mod errors;
pub mod fleet;
pub mod web;

use std::net::SocketAddr;
use std::sync::Arc;

use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::gateway::api::AppState;
use crate::gateway::db::ServerDb;
use crate::metrics::Metrics;

/// Configuration for the device gateway HTTP server.
pub struct GatewayConfig {
    pub port: u16,
    pub db_path: String,
    pub kube_client: Option<kube::Client>,
    pub retention_days: u32,
    pub metrics: Option<Metrics>,
}

/// Start the device gateway HTTP server.
/// Returns when the server shuts down or encounters a fatal error.
pub async fn start_gateway(config: GatewayConfig) -> Result<(), Box<dyn std::error::Error>> {
    let db = ServerDb::open(&config.db_path)
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

    if std::env::var("CFGD_API_KEY").is_ok() {
        tracing::info!("Device gateway: API key authentication enabled");
    } else {
        tracing::warn!(
            "CFGD_API_KEY is not set — all API requests will be treated as admin. Set CFGD_API_KEY for production use."
        );
    }

    db.cleanup_old_events(config.retention_days)
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

    let (event_tx, _) = tokio::sync::broadcast::channel(256);
    let enrollment_method = api::EnrollmentMethod::from_env();
    tracing::info!(method = ?enrollment_method, "Device gateway: enrollment method configured");

    let state = AppState {
        db: Arc::new(tokio::sync::Mutex::new(db)),
        kube_client: config.kube_client,
        event_tx,
        enrollment_method,
        metrics: config.metrics,
    };

    // Periodic event cleanup — runs daily to prevent unbounded DB growth
    let cleanup_state = state.db.clone();
    let retention_days = config.retention_days;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(86400));
        interval.tick().await; // skip immediate first tick (cleanup already ran above)
        loop {
            interval.tick().await;
            let db = cleanup_state.lock().await;
            match db.cleanup_old_events(retention_days) {
                Ok(count) => {
                    if count > 0 {
                        tracing::info!(deleted = count, "periodic event cleanup completed");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "periodic event cleanup failed"),
            }
        }
    });

    let app = api::router(state.clone())
        .merge(web::router())
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    tracing::info!(%addr, db_path = %config.db_path, "Device gateway starting");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

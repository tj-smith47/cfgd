pub mod api;
pub mod db;
pub mod errors;
pub mod fleet;
pub mod web;

use std::net::SocketAddr;

use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::gateway::api::AppState;
use crate::gateway::db::ServerDb;
use crate::metrics::Metrics;

// 1 MiB request body cap for gateway endpoints. JSON request bodies
// (enroll / checkin / config) are well under this; anything larger is
// almost certainly a DoS attempt.
const GATEWAY_MAX_BODY_BYTES: usize = 1024 * 1024;

// Env var listing allowed browser origins, comma-separated. When unset or
// empty, the gateway rejects all cross-origin requests (same-origin fetches
// from the dashboard continue to work). `*` re-enables the legacy permissive
// behaviour (dev-only). Each entry must be a scheme+host(+port) URL, e.g.
// `https://fleet.internal,https://ops.example.com`.
const GATEWAY_ALLOWED_ORIGINS_ENV: &str = "CFGD_GATEWAY_ALLOWED_ORIGINS";

fn build_cors_layer() -> CorsLayer {
    let raw = std::env::var(GATEWAY_ALLOWED_ORIGINS_ENV).unwrap_or_default();
    let trimmed = raw.trim();

    let base = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ]);

    if trimmed.is_empty() {
        tracing::info!(
            env = GATEWAY_ALLOWED_ORIGINS_ENV,
            "gateway CORS: no cross-origin browsers allowed (set env to a comma-separated origin list to enable)"
        );
        return base.allow_origin(AllowOrigin::list(std::iter::empty::<HeaderValue>()));
    }

    if trimmed == "*" {
        tracing::warn!(
            env = GATEWAY_ALLOWED_ORIGINS_ENV,
            "gateway CORS: wildcard origin — allowing any browser origin. Restrict in production by setting explicit origins."
        );
        return base.allow_origin(AllowOrigin::any());
    }

    let parsed: Vec<HeaderValue> = trimmed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| match HeaderValue::from_str(s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(origin = %s, error = %e, "gateway CORS: dropping invalid origin");
                None
            }
        })
        .collect();

    if parsed.is_empty() {
        tracing::warn!(
            env = GATEWAY_ALLOWED_ORIGINS_ENV,
            raw = %trimmed,
            "gateway CORS: no valid origins parsed from env; denying cross-origin"
        );
        return base.allow_origin(AllowOrigin::list(std::iter::empty::<HeaderValue>()));
    }

    tracing::info!(
        env = GATEWAY_ALLOWED_ORIGINS_ENV,
        count = parsed.len(),
        "gateway CORS: allowing explicit origins"
    );
    base.allow_origin(AllowOrigin::list(parsed))
}

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
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?
        .with_metrics(config.metrics.clone());

    if std::env::var("CFGD_API_KEY").is_ok() {
        tracing::info!("device gateway: API key authentication enabled");
    } else {
        tracing::warn!(
            "CFGD_API_KEY is not set — admin API access is disabled. Set CFGD_API_KEY to enable admin operations."
        );
    }

    db.cleanup_old_events(config.retention_days)
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

    let (event_tx, _) = tokio::sync::broadcast::channel(256);
    let enrollment_method = api::EnrollmentMethod::from_env();
    tracing::info!(method = ?enrollment_method, "device gateway: enrollment method configured");

    let state = AppState {
        db,
        kube_client: config.kube_client,
        event_tx,
        enrollment_method,
        metrics: config.metrics,
    };

    // Periodic event cleanup — runs daily to prevent unbounded DB growth
    let cleanup_db = state.db.clone();
    let retention_days = config.retention_days;
    let cleanup_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(86400));
        interval.tick().await; // skip immediate first tick (cleanup already ran above)
        loop {
            interval.tick().await;
            match cleanup_db.cleanup_old_events(retention_days).await {
                Ok(count) if count > 0 => {
                    tracing::info!(deleted = count, "periodic event cleanup completed");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "periodic event cleanup failed"),
            }
        }
    });

    // 1-Hz sampler: publish reader pool in-use gauge while the gateway is running.
    if let Some(metrics) = state.metrics.clone() {
        let pool = state.db.readers_handle();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                let st = pool.state();
                let in_use = st.connections.saturating_sub(st.idle_connections);
                metrics
                    .db_pool_in_use
                    .get_or_create(&crate::metrics::DbPoolLabels {
                        role: "reader".to_string(),
                    })
                    .set(in_use as i64);
            }
        });
    }

    let app = api::router(state.clone())
        .merge(web::router())
        .layer(DefaultBodyLimit::max(GATEWAY_MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .layer(build_cors_layer())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    tracing::info!(%addr, db_path = %config.db_path, "device gateway starting");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let result = axum::serve(listener, app).await;

    // Abort background task on server exit
    cleanup_handle.abort();

    result?;
    Ok(())
}

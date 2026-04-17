use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("database pool exhausted: {0}")]
    PoolExhausted(String),

    #[error("database task panicked")]
    DatabaseTaskPanicked,

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden: {0}")]
    Forbidden(String),
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            GatewayError::Database(e) => {
                tracing::error!("database error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            GatewayError::PoolExhausted(msg) => {
                tracing::warn!(error = %msg, "db pool exhausted — load shedding");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "service overloaded; retry".to_string(),
                )
            }
            GatewayError::DatabaseTaskPanicked => {
                // Already logged at the spawn_blocking boundary.
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            GatewayError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            GatewayError::InvalidRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            GatewayError::Internal(msg) => {
                tracing::error!("internal error: {msg}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            GatewayError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            GatewayError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
        };

        let body = json!({ "error": message });
        (status, axum::Json(body)).into_response()
    }
}

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

    #[error("rate limited: retry after {0}s")]
    RateLimited(u64),
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
            GatewayError::RateLimited(retry_after_secs) => {
                let body = json!({
                    "error": "too many requests; slow down",
                    "retry_after_secs": retry_after_secs,
                });
                let mut resp = (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response();
                if let Ok(hv) = axum::http::HeaderValue::from_str(&retry_after_secs.to_string()) {
                    resp.headers_mut().insert("retry-after", hv);
                }
                return resp;
            }
        };

        let body = json!({ "error": message });
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("parse body as JSON")
    }

    // -----------------------------------------------------------------------
    // Display — every variant renders the exact #[error(...)] template
    // -----------------------------------------------------------------------

    #[test]
    fn database_display_prefixes_with_database_error_and_includes_inner() {
        let inner = rusqlite::Error::QueryReturnedNoRows;
        let inner_text = inner.to_string();
        let e = GatewayError::Database(inner);
        let rendered = format!("{e}");
        assert!(
            rendered.starts_with("database error: "),
            "expected prefix, got {rendered:?}"
        );
        assert!(
            rendered.contains(&inner_text),
            "expected rendered={rendered:?} to contain inner={inner_text:?}"
        );
    }

    #[test]
    fn pool_exhausted_display_shows_inner_message() {
        let e = GatewayError::PoolExhausted("max connections (10) reached".into());
        assert_eq!(
            format!("{e}"),
            "database pool exhausted: max connections (10) reached"
        );
    }

    #[test]
    fn database_task_panicked_display_is_fixed_string() {
        let e = GatewayError::DatabaseTaskPanicked;
        assert_eq!(format!("{e}"), "database task panicked");
    }

    #[test]
    fn not_found_display_includes_inner() {
        let e = GatewayError::NotFound("device 'foo' not registered".into());
        assert_eq!(format!("{e}"), "not found: device 'foo' not registered");
    }

    #[test]
    fn invalid_request_display_includes_inner() {
        let e = GatewayError::InvalidRequest("missing field 'name'".into());
        assert_eq!(format!("{e}"), "invalid request: missing field 'name'");
    }

    #[test]
    fn internal_display_includes_inner() {
        let e = GatewayError::Internal("connection refused to upstream".into());
        assert_eq!(
            format!("{e}"),
            "internal error: connection refused to upstream"
        );
    }

    #[test]
    fn unauthorized_display_is_fixed_string() {
        let e = GatewayError::Unauthorized;
        assert_eq!(format!("{e}"), "unauthorized");
    }

    #[test]
    fn forbidden_display_includes_inner() {
        let e = GatewayError::Forbidden("token expired".into());
        assert_eq!(format!("{e}"), "forbidden: token expired");
    }

    // -----------------------------------------------------------------------
    // IntoResponse — status code AND body for every variant.
    // The client body must NEVER leak inner detail for the security-sensitive
    // variants (Database, PoolExhausted, DatabaseTaskPanicked, Internal).
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn into_response_database_returns_500_and_hides_rusqlite_detail() {
        let inner_text = rusqlite::Error::QueryReturnedNoRows.to_string();
        let resp = GatewayError::Database(rusqlite::Error::QueryReturnedNoRows).into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "internal server error");
        let body_str = body.to_string();
        assert!(
            !body_str.contains(&inner_text),
            "Database body must not leak rusqlite Display ({inner_text:?}); got {body_str}"
        );
    }

    #[tokio::test]
    async fn into_response_pool_exhausted_returns_503_with_load_shed_message() {
        let inner = "max connections (10) reached";
        let resp = GatewayError::PoolExhausted(inner.into()).into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "service overloaded; retry");
        assert!(
            !body.to_string().contains(inner),
            "PoolExhausted body must not leak inner detail {inner:?}"
        );
    }

    #[tokio::test]
    async fn into_response_database_task_panicked_returns_500_with_generic_message() {
        let resp = GatewayError::DatabaseTaskPanicked.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "internal server error");
    }

    #[tokio::test]
    async fn into_response_not_found_returns_404_with_inner_message() {
        let resp = GatewayError::NotFound("device 'foo' not registered".into()).into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "device 'foo' not registered");
    }

    #[tokio::test]
    async fn into_response_invalid_request_returns_400_with_inner_message() {
        let resp = GatewayError::InvalidRequest("missing field 'name'".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "missing field 'name'");
    }

    #[tokio::test]
    async fn into_response_internal_returns_500_and_hides_inner_message() {
        let inner = "connection refused to upstream";
        let resp = GatewayError::Internal(inner.into()).into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "internal server error");
        assert!(
            !body.to_string().contains(inner),
            "Internal body must not leak inner detail {inner:?}"
        );
    }

    #[tokio::test]
    async fn into_response_unauthorized_returns_401_with_fixed_message() {
        let resp = GatewayError::Unauthorized.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "unauthorized");
    }

    #[tokio::test]
    async fn into_response_forbidden_returns_403_with_inner_message() {
        let resp = GatewayError::Forbidden("token expired".into()).into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "token expired");
    }

    // -----------------------------------------------------------------------
    // From<rusqlite::Error> wraps inside Database without losing inner state.
    // -----------------------------------------------------------------------

    #[test]
    fn from_rusqlite_error_wraps_in_database_variant_preserving_inner_display() {
        let rusqlite_err = rusqlite::Error::QueryReturnedNoRows;
        let expected = rusqlite_err.to_string();
        let gw: GatewayError = rusqlite_err.into();
        match gw {
            GatewayError::Database(e) => assert_eq!(e.to_string(), expected),
            other => panic!("expected Database variant, got {other:?}"),
        }
    }
}

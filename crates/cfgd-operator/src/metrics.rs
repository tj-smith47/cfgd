use std::sync::Arc;

use prometheus_client::encoding::text::encode;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;
use tokio::sync::Mutex;

use crate::errors::OperatorError;

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug)]
pub struct ReconcileLabels {
    pub controller: String,
    pub result: String,
}

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug)]
pub struct DriftLabels {
    pub severity: String,
    pub namespace: String,
}

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug)]
pub struct WebhookLabels {
    pub operation: String,
    pub result: String,
}

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug)]
pub struct PolicyLabels {
    pub policy: String,
    pub namespace: String,
}

#[derive(Clone)]
pub struct Metrics {
    pub reconciliations_total: Family<ReconcileLabels, Counter>,
    pub reconciliation_duration_seconds: Family<ReconcileLabels, Histogram>,
    pub drift_events_total: Family<DriftLabels, Counter>,
    pub webhook_requests_total: Family<WebhookLabels, Counter>,
    pub webhook_duration_seconds: Family<WebhookLabels, Histogram>,
    pub devices_compliant: Family<PolicyLabels, Gauge>,
    pub devices_enrolled_total: Counter,
}

impl Metrics {
    pub fn new(registry: &mut Registry) -> Self {
        let sub = registry.sub_registry_with_prefix("cfgd_operator");

        let reconciliations_total = Family::<ReconcileLabels, Counter>::default();
        sub.register(
            "reconciliations_total",
            "Total number of reconciliation attempts",
            reconciliations_total.clone(),
        );

        let reconciliation_duration_seconds = Family::<ReconcileLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(exponential_buckets(0.001, 2.0, 16))
        });
        sub.register(
            "reconciliation_duration_seconds",
            "Duration of reconciliation attempts in seconds",
            reconciliation_duration_seconds.clone(),
        );

        let drift_events_total = Family::<DriftLabels, Counter>::default();
        sub.register(
            "drift_events_total",
            "Total number of drift events detected",
            drift_events_total.clone(),
        );

        let webhook_requests_total = Family::<WebhookLabels, Counter>::default();
        sub.register(
            "webhook_requests_total",
            "Total number of webhook requests",
            webhook_requests_total.clone(),
        );

        let webhook_duration_seconds = Family::<WebhookLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(exponential_buckets(0.001, 2.0, 16))
        });
        sub.register(
            "webhook_duration_seconds",
            "Duration of webhook requests in seconds",
            webhook_duration_seconds.clone(),
        );

        let devices_compliant = Family::<PolicyLabels, Gauge>::default();
        sub.register(
            "devices_compliant",
            "Number of devices compliant with a policy",
            devices_compliant.clone(),
        );

        let devices_enrolled_total = Counter::default();
        sub.register(
            "devices_enrolled_total",
            "Total device enrollments",
            devices_enrolled_total.clone(),
        );

        Self {
            reconciliations_total,
            reconciliation_duration_seconds,
            drift_events_total,
            webhook_requests_total,
            webhook_duration_seconds,
            devices_compliant,
            devices_enrolled_total,
        }
    }
}

async fn metrics_handler(
    axum::extract::State(registry): axum::extract::State<Arc<Mutex<Registry>>>,
) -> (axum::http::StatusCode, [(axum::http::header::HeaderName, &'static str); 1], String) {
    let reg = registry.lock().await;
    let mut buf = String::new();
    if encode(&mut buf, &reg).is_err() {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "Failed to encode metrics".to_string(),
        );
    }
    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/openmetrics-text; version=1.0.0; charset=utf-8")],
        buf,
    )
}

pub async fn run_metrics_server(
    port: u16,
    registry: Arc<Mutex<Registry>>,
) -> Result<(), OperatorError> {
    let app = axum::Router::new()
        .route("/metrics", axum::routing::get(metrics_handler))
        .with_state(registry);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| OperatorError::Metrics(format!("bind {addr}: {e}")))?;

    tracing::info!(%addr, "Metrics server listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| OperatorError::Metrics(format!("serve: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_register_without_panic() {
        let mut registry = Registry::default();
        let _metrics = Metrics::new(&mut registry);
    }

    #[test]
    fn metrics_encode_produces_output() {
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);
        metrics
            .reconciliations_total
            .get_or_create(&ReconcileLabels {
                controller: "test".to_string(),
                result: "success".to_string(),
            })
            .inc();
        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("cfgd_operator_reconciliations_total"));
    }
}

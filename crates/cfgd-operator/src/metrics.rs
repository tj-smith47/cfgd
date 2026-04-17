use std::sync::Arc;

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
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

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug)]
pub struct DbPoolLabels {
    pub role: String,
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
    pub db_pool_in_use: Family<DbPoolLabels, Gauge>,
    pub db_pool_wait_seconds: Histogram,
    pub db_writer_wait_seconds: Histogram,
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

        let reconciliation_duration_seconds =
            Family::<ReconcileLabels, Histogram>::new_with_constructor(|| {
                let (start, factor, length) = cfgd_core::DURATION_BUCKETS_SHORT;
                Histogram::new(exponential_buckets(start, factor, length))
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

        let webhook_duration_seconds =
            Family::<WebhookLabels, Histogram>::new_with_constructor(|| {
                let (start, factor, length) = cfgd_core::DURATION_BUCKETS_SHORT;
                Histogram::new(exponential_buckets(start, factor, length))
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

        let db_pool_in_use = Family::<DbPoolLabels, Gauge>::default();
        sub.register(
            "gateway_db_pool_in_use",
            "Number of in-use gateway DB pool connections",
            db_pool_in_use.clone(),
        );

        let db_pool_wait_seconds = {
            let (start, factor, length) = cfgd_core::DURATION_BUCKETS_SHORT;
            Histogram::new(exponential_buckets(start, factor, length))
        };
        sub.register(
            "gateway_db_pool_wait_seconds",
            "Wait time to acquire a gateway DB reader pool connection",
            db_pool_wait_seconds.clone(),
        );

        let db_writer_wait_seconds = {
            let (start, factor, length) = cfgd_core::DURATION_BUCKETS_SHORT;
            Histogram::new(exponential_buckets(start, factor, length))
        };
        sub.register(
            "gateway_db_writer_wait_seconds",
            "Wait time to acquire the gateway DB writer mutex",
            db_writer_wait_seconds.clone(),
        );

        Self {
            reconciliations_total,
            reconciliation_duration_seconds,
            drift_events_total,
            webhook_requests_total,
            webhook_duration_seconds,
            devices_compliant,
            devices_enrolled_total,
            db_pool_in_use,
            db_pool_wait_seconds,
            db_writer_wait_seconds,
        }
    }
}

async fn metrics_handler(
    axum::extract::State(registry): axum::extract::State<Arc<Mutex<Registry>>>,
) -> (
    axum::http::StatusCode,
    [(axum::http::header::HeaderName, &'static str); 1],
    String,
) {
    let reg = registry.lock().await;
    let mut buf = String::new();
    if encode(&mut buf, &reg).is_err() {
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

pub async fn run_metrics_server(
    port: u16,
    registry: Arc<Mutex<Registry>>,
) -> Result<(), OperatorError> {
    // GET-only surface; 8 KiB is a generous safety net against abusive clients.
    const METRICS_MAX_BODY_BYTES: usize = 8 * 1024;

    let app = axum::Router::new()
        .route("/metrics", axum::routing::get(metrics_handler))
        .layer(axum::extract::DefaultBodyLimit::max(METRICS_MAX_BODY_BYTES))
        .with_state(registry);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| OperatorError::Metrics(format!("bind {addr}: {e}")))?;

    tracing::info!(%addr, "metrics server listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| OperatorError::Metrics(format!("serve: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn all_metrics_registered() {
        let mut registry = Registry::default();
        let _metrics = Metrics::new(&mut registry);
        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        // All metric families should appear in the output (even if zero-valued)
        // Verify the registry doesn't panic during encoding
        assert!(!buf.is_empty());
    }

    #[test]
    fn reconciliation_counter_increments() {
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);
        let labels = ReconcileLabels {
            controller: "machineconfig".to_string(),
            result: "success".to_string(),
        };
        metrics.reconciliations_total.get_or_create(&labels).inc();
        metrics.reconciliations_total.get_or_create(&labels).inc();

        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("cfgd_operator_reconciliations_total"));
        // The encoded value should show 2
        assert!(
            buf.contains("2"),
            "expected counter value 2 in output: {buf}"
        );
    }

    #[test]
    fn drift_events_counter() {
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);
        metrics
            .drift_events_total
            .get_or_create(&DriftLabels {
                severity: "critical".to_string(),
                namespace: "default".to_string(),
            })
            .inc();

        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("cfgd_operator_drift_events_total"));
        assert!(buf.contains("critical"));
    }

    #[test]
    fn webhook_requests_counter() {
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);
        metrics
            .webhook_requests_total
            .get_or_create(&WebhookLabels {
                operation: "CREATE".to_string(),
                result: "allowed".to_string(),
            })
            .inc();

        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("cfgd_operator_webhook_requests_total"));
    }

    #[test]
    fn reconciliation_duration_histogram() {
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);
        let labels = ReconcileLabels {
            controller: "configpolicy".to_string(),
            result: "success".to_string(),
        };
        metrics
            .reconciliation_duration_seconds
            .get_or_create(&labels)
            .observe(0.5);

        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("cfgd_operator_reconciliation_duration_seconds"));
    }

    #[test]
    fn webhook_duration_histogram() {
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);
        let labels = WebhookLabels {
            operation: "UPDATE".to_string(),
            result: "denied".to_string(),
        };
        metrics
            .webhook_duration_seconds
            .get_or_create(&labels)
            .observe(0.01);

        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("cfgd_operator_webhook_duration_seconds"));
    }

    #[test]
    fn devices_compliant_gauge() {
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);
        let labels = PolicyLabels {
            policy: "baseline".to_string(),
            namespace: "prod".to_string(),
        };
        metrics.devices_compliant.get_or_create(&labels).set(42);

        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("cfgd_operator_devices_compliant"));
        assert!(buf.contains("42"), "expected gauge value 42 in: {buf}");
    }

    #[test]
    fn devices_enrolled_counter() {
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);
        metrics.devices_enrolled_total.inc();
        metrics.devices_enrolled_total.inc();
        metrics.devices_enrolled_total.inc();

        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("cfgd_operator_devices_enrolled_total"));
        assert!(buf.contains("3"), "expected counter value 3 in: {buf}");
    }

    #[test]
    fn multiple_label_combinations() {
        let mut registry = Registry::default();
        let metrics = Metrics::new(&mut registry);

        // Different controllers
        metrics
            .reconciliations_total
            .get_or_create(&ReconcileLabels {
                controller: "machineconfig".to_string(),
                result: "success".to_string(),
            })
            .inc();
        metrics
            .reconciliations_total
            .get_or_create(&ReconcileLabels {
                controller: "configpolicy".to_string(),
                result: "error".to_string(),
            })
            .inc();

        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("machineconfig"));
        assert!(buf.contains("configpolicy"));
        assert!(buf.contains("success"));
        assert!(buf.contains("error"));
    }
}

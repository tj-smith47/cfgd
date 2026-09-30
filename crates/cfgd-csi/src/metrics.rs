use std::sync::Arc;

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;

use crate::errors::CsiError;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PublishLabels {
    pub module: String,
    pub result: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PullLabels {
    pub module: String,
    pub cached: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ModuleLabels {
    pub module: String,
}

/// Build a `Histogram` using cfgd's canonical long-duration exponential buckets.
/// Centralizes the SLO-adjacent bucket choice from `cfgd_core::DURATION_BUCKETS_LONG`
/// so the inline `exponential_buckets(...)` pattern doesn't drift.
fn long_histogram() -> Histogram {
    let (start, factor, length) = cfgd_core::DURATION_BUCKETS_LONG;
    Histogram::new(exponential_buckets(start, factor, length))
}

pub struct CsiMetrics {
    pub volume_publish_total: Family<PublishLabels, Counter>,
    pub pull_duration_seconds: Family<PullLabels, Histogram>,
    pub cache_size_bytes: Gauge,
    pub cache_hits_total: Family<ModuleLabels, Counter>,
}

impl CsiMetrics {
    pub fn new(registry: &mut Registry) -> Self {
        let volume_publish_total = Family::<PublishLabels, Counter>::default();
        registry.register(
            "cfgd_csi_volume_publish",
            "Total CSI volume publish operations",
            volume_publish_total.clone(),
        );

        let pull_duration_seconds =
            Family::<PullLabels, Histogram>::new_with_constructor(long_histogram);
        registry.register(
            "cfgd_csi_pull_duration_seconds",
            "Duration of OCI module pull operations",
            pull_duration_seconds.clone(),
        );

        let cache_size_bytes = Gauge::default();
        registry.register(
            "cfgd_csi_cache_size_bytes",
            "Current cache size in bytes",
            cache_size_bytes.clone(),
        );

        let cache_hits_total = Family::<ModuleLabels, Counter>::default();
        registry.register(
            "cfgd_csi_cache_hits",
            "Mounts served from the node cache without a pull, counted once per mount: at NodeStageVolume for a staged volume, at NodePublishVolume for an inline ephemeral one.",
            cache_hits_total.clone(),
        );

        Self {
            volume_publish_total,
            pull_duration_seconds,
            cache_size_bytes,
            cache_hits_total,
        }
    }
}

async fn metrics_handler(
    axum::extract::State(registry): axum::extract::State<Arc<Registry>>,
) -> (
    axum::http::StatusCode,
    [(axum::http::header::HeaderName, &'static str); 1],
    String,
) {
    let mut buf = String::new();
    if encode(&mut buf, &registry).is_err() {
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

/// Serve the Prometheus `/metrics` endpoint on the given port.
///
/// Matches the shape of `cfgd_operator::metrics::run_metrics_server` so both
/// components have a consistent surface for tests and deployment configs.
pub(crate) async fn serve_metrics(port: u16, registry: Arc<Registry>) -> Result<(), CsiError> {
    const METRICS_MAX_BODY_BYTES: usize = 8 * 1024;

    let app = axum::Router::new()
        .route("/metrics", axum::routing::get(metrics_handler))
        .layer(axum::extract::DefaultBodyLimit::max(METRICS_MAX_BODY_BYTES))
        .with_state(registry);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| CsiError::Metrics(format!("bind {addr}: {e}")))?;

    tracing::info!(%addr, "metrics server listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| CsiError::Metrics(format!("serve: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn metrics_encode_produces_output() {
        let mut registry = Registry::default();
        let metrics = CsiMetrics::new(&mut registry);

        metrics
            .volume_publish_total
            .get_or_create(&PublishLabels {
                module: "nettools".to_string(),
                result: "success".to_string(),
            })
            .inc();

        metrics
            .cache_hits_total
            .get_or_create(&ModuleLabels {
                module: "nettools".to_string(),
            })
            .inc();
        metrics.cache_size_bytes.set(42);

        // Record a pull-duration observation: a histogram Family with no
        // observed label set emits no series at all, so the metric name would
        // be absent from the encoded text. Observe one so the assertion below
        // exercises real output.
        metrics
            .pull_duration_seconds
            .get_or_create(&PullLabels {
                module: "nettools".to_string(),
                cached: "false".to_string(),
            })
            .observe(0.5);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &registry).unwrap();
        assert!(
            buf.contains(
                "\ncfgd_csi_volume_publish_total{module=\"nettools\",result=\"success\"} 1\n"
            ),
            "{buf}"
        );
        assert!(
            buf.contains("\ncfgd_csi_cache_hits_total{module=\"nettools\"} 1\n"),
            "{buf}"
        );
        assert!(buf.contains("cfgd_csi_cache_size_bytes"));
        assert!(buf.contains("cfgd_csi_pull_duration_seconds"));
    }

    /// prometheus-client appends `_total` to every counter sample, so each
    /// counter family renders the name the docs promise exactly once.
    #[test]
    fn every_counter_sample_renders_total_once() {
        let mut registry = Registry::default();
        let metrics = CsiMetrics::new(&mut registry);
        metrics
            .volume_publish_total
            .get_or_create(&PublishLabels {
                module: "nettools".to_string(),
                result: "success".to_string(),
            })
            .inc();
        metrics
            .cache_hits_total
            .get_or_create(&ModuleLabels {
                module: "nettools".to_string(),
            })
            .inc();

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &registry).unwrap();
        let report = cfgd_core::test_helpers::counter_samples(&buf, "cfgd_csi");
        assert_eq!(
            report.families,
            ["cfgd_csi_volume_publish", "cfgd_csi_cache_hits"]
        );
        assert!(
            report.violations.is_empty(),
            "{:#?}\n{buf}",
            report.violations
        );
    }

    /// A name registered with `_total` renders `_total_total`, so the walk
    /// holds every registration in this source to its bare name.
    #[test]
    fn no_registered_metric_name_ends_in_total() {
        let names = cfgd_core::test_helpers::registered_metric_names(std::path::Path::new(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/metrics.rs"),
        ));
        assert!(
            names.iter().any(|n| n == "cfgd_csi_volume_publish"),
            "the walk read no registration: {names:?}"
        );
        let suffixed: Vec<_> = names.iter().filter(|n| n.ends_with("_total")).collect();
        assert!(suffixed.is_empty(), "registered with _total: {suffixed:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_handler_returns_openmetrics_body() {
        let mut registry = Registry::default();
        let metrics = CsiMetrics::new(&mut registry);
        metrics.cache_size_bytes.set(1024);
        let shared = Arc::new(registry);

        let (status, headers, body) = metrics_handler(axum::extract::State(shared)).await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(headers[0].0, axum::http::header::CONTENT_TYPE);
        assert!(headers[0].1.contains("openmetrics-text"));
        assert!(body.contains("cfgd_csi_cache_size_bytes"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serve_metrics_setup_runs_until_serve_loop_blocks() {
        let registry = Arc::new(Registry::default());
        // Port 0 lets the kernel pick a free port; axum::serve then blocks
        // on accept. The timeout fires and the future is dropped — no panic.
        let result =
            tokio::time::timeout(Duration::from_millis(250), serve_metrics(0, registry)).await;
        assert!(
            result.is_err(),
            "serve_metrics should block in serve loop, got {result:?}"
        );
    }

    // Linux-only: tests the privileged-port (port 1) bind-failure path.
    // macOS GHA runners can bind port 1 as non-root (Apple's port-privilege
    // semantics differ from POSIX), and windows lacks the privileged-port
    // concept altogether. Skipped when run as root locally — root can bind
    // port 1 successfully.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "current_thread")]
    async fn serve_metrics_bind_failure_returns_metrics_error_as_non_root() {
        if cfgd_core::is_root() {
            return;
        }
        let registry = Arc::new(Registry::default());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            serve_metrics(1, registry),
        )
        .await
        .expect("serve_metrics must return within 5s");

        match result {
            Err(crate::errors::CsiError::Metrics(msg)) => {
                assert!(msg.contains("bind"), "expected bind error, got {msg}");
            }
            other => panic!("expected CsiError::Metrics, got {other:?}"),
        }
    }
}

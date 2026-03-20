use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PublishLabels {
    pub module: String,
    pub result: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PullLabels {
    pub module: String,
}

pub struct CsiMetrics {
    pub volume_publish_total: Family<PublishLabels, Counter>,
    pub pull_duration_seconds: Family<PullLabels, Histogram>,
    pub cache_size_bytes: Gauge,
    pub cache_hits_total: Counter,
}

impl CsiMetrics {
    pub fn new(registry: &mut Registry) -> Self {
        let volume_publish_total = Family::<PublishLabels, Counter>::default();
        registry.register(
            "cfgd_csi_volume_publish_total",
            "Total CSI volume publish operations",
            volume_publish_total.clone(),
        );

        let pull_duration_seconds = Family::<PullLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(exponential_buckets(0.1, 2.0, 10))
        });
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

        let cache_hits_total = Counter::default();
        registry.register(
            "cfgd_csi_cache_hits_total",
            "Total cache hit count",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_register_without_panic() {
        let mut registry = Registry::default();
        let _metrics = CsiMetrics::new(&mut registry);
    }

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

        metrics.cache_hits_total.inc();
        metrics.cache_size_bytes.set(42);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &registry).unwrap();
        assert!(buf.contains("cfgd_csi_volume_publish_total"));
        assert!(buf.contains("cfgd_csi_cache_hits_total"));
        assert!(buf.contains("cfgd_csi_cache_size_bytes"));
        assert!(buf.contains("cfgd_csi_pull_duration_seconds"));
    }
}

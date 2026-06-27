/// The canonical API version string used in all cfgd YAML documents (local and CRD).
pub const API_VERSION: &str = "cfgd.io/v1alpha1";
pub const CSI_DRIVER_NAME: &str = "csi.cfgd.io";
pub const MODULES_ANNOTATION: &str = "cfgd.io/modules";

/// Default namespace the cfgd operator + CSI driver are deployed into. Used by
/// `kubectl cfgd version` to locate the operator Deployment and CSI DaemonSet
/// when no explicit `--namespace` is given.
pub const CFGD_SYSTEM_NAMESPACE: &str = "cfgd-system";

/// Kubernetes label key pointing at the `MachineConfig` resource an object
/// was derived from (e.g. DriftAlert -> MachineConfig).
pub const LABEL_MACHINE_CONFIG: &str = "cfgd.io/machine-config";
/// Kubernetes label key identifying the fleet device an object belongs to.
pub const LABEL_DEVICE_ID: &str = "cfgd.io/device-id";
/// OCI manifest annotation key carrying the `os/arch` platform string that a
/// pushed module artifact was built for (parsed by the CSI cache on pull).
pub const OCI_ANNOTATION_PLATFORM: &str = "cfgd.io/platform";
/// Standard OCI image-spec annotation recording artifact creation time
/// (RFC 3339); injected on every pushed module manifest.
pub const OCI_ANNOTATION_CREATED: &str = "org.opencontainers.image.created";

/// Default timeout for external commands (2 minutes).
pub const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Default timeout for git network operations (5 minutes).
pub const GIT_NETWORK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Default timeout for profile-level scripts (5 minutes).
pub const PROFILE_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Maximum file size (10 MB) for backup content capture.
/// Files larger than this are tracked but their content is not stored in backups.
pub(super) const MAX_BACKUP_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Named exponential-histogram bucket presets for latency metrics. Kept in
/// cfgd-core so the SLO-adjacent choice is auditable in one place rather
/// than divergent inline calls in cfgd-operator and cfgd-csi. Consumers
/// feed the triple into `prometheus_client::metrics::histogram::exponential_buckets(start, factor, length)`.
pub const DURATION_BUCKETS_SHORT: (f64, f64, u16) = (0.001, 2.0, 16);
pub const DURATION_BUCKETS_LONG: (f64, f64, u16) = (0.1, 2.0, 10);

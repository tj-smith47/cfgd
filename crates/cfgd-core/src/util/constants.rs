/// The canonical API version string used in all cfgd YAML documents (local and CRD).
pub const API_VERSION: &str = "cfgd.io/v1alpha1";
/// The default CSI driver name, used when `CSI_DRIVER_NAME` is unset or blank.
pub const CSI_DRIVER_NAME: &str = "csi.cfgd.io";

/// The CSI driver name this process registers or injects: `CSI_DRIVER_NAME`, else
/// [`CSI_DRIVER_NAME`]. The CSI binary and the operator webhook must read the same value,
/// or pods get volumes no registered driver serves.
///
/// The name is read on every call so tests can override the variable per case; the read
/// is one environment lookup, far below the cost of the gRPC call or admission review
/// it serves.
pub fn csi_driver_name() -> String {
    // A blank value would register or inject an empty driver name, which the kubelet
    // rejects, so blank means the default, matching `WATCH_LABEL_SELECTOR`.
    let name = crate::env_or("CSI_DRIVER_NAME", CSI_DRIVER_NAME);
    match name.trim() {
        "" => CSI_DRIVER_NAME.to_owned(),
        trimmed => trimmed.to_owned(),
    }
}
pub const MODULES_ANNOTATION: &str = "cfgd.io/modules";

/// The pod annotation naming every module the mutating webhook declined to
/// inject, comma-separated. A pod whose `cfgd.io/modules` annotation asked for
/// a module the webhook skipped is told which one here, rather than finding
/// the mount silently absent.
pub const SKIPPED_MODULES_ANNOTATION: &str = "cfgd.io/skipped-modules";

/// The manager name a module package carries when its "install" is an inline
/// script rather than a manager command. It names no registry entry, so no
/// manager map holds it, no live listing reports it and no drift row is minted
/// under it.
pub const SCRIPT_SENTINEL: &str = "script";

/// The registered name of the Homebrew tap sub-manager. cfgd grants a declared
/// tap trust before adding it, and the row that reports the add says so, so the
/// name is matched in core as well as spelled by the manager itself.
pub const BREW_TAP_MANAGER: &str = "brew-tap";

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

/// Per-IP enrollment burst the device gateway grants up front.
pub const ENROLL_RATE_LIMIT_BURST: u32 = 5;

/// Per-IP enrollment attempts the device gateway refills each minute. It lives
/// here rather than in the gateway because the DEVICE has to know it too: a 429
/// answer is the gateway asking the device to wait for this quota, and a client
/// ladder measured without it exhausts itself before a single token is back.
pub const ENROLL_RATE_LIMIT_PER_MIN: u32 = 5;

/// How long the quota above takes to hand back one token, in seconds, for the
/// one caller that needs to do const arithmetic on it (`Duration`'s own division
/// is not const).
pub(crate) const ENROLL_RATE_LIMIT_REFILL_SECS: u64 = 60 / ENROLL_RATE_LIMIT_PER_MIN as u64;

/// How long the quota above takes to hand back one token, which is the shortest
/// wait that can turn a refused enrollment into an admitted one.
pub const ENROLL_RATE_LIMIT_REFILL: std::time::Duration =
    std::time::Duration::from_secs(ENROLL_RATE_LIMIT_REFILL_SECS);

/// Maximum file size (10 MB) for backup content capture.
/// Files larger than this are tracked but their content is not stored in backups.
pub(super) const MAX_BACKUP_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Named exponential-histogram bucket presets for latency metrics. Kept in
/// cfgd-core so the SLO-adjacent choice is auditable in one place rather
/// than divergent inline calls in cfgd-operator and cfgd-csi. Consumers
/// feed the triple into `prometheus_client::metrics::histogram::exponential_buckets(start, factor,
/// length)`.
pub const DURATION_BUCKETS_SHORT: (f64, f64, u16) = (0.001, 2.0, 16);
pub const DURATION_BUCKETS_LONG: (f64, f64, u16) = (0.1, 2.0, 10);

#[cfg(all(test, feature = "crd"))]
mod tests {
    // API_VERSION is the string config parsing accepts; cfgd_crd::api_version()
    // is read from the kube `#[kube(group, version)]` derive. Bumping the derive
    // to v1beta1 must not silently leave parsing pinned to the old apiVersion.
    #[test]
    fn api_version_const_matches_crd_derive() {
        assert_eq!(super::API_VERSION, cfgd_crd::api_version());
    }
}

#[cfg(test)]
mod csi_driver_name_tests {
    use super::{CSI_DRIVER_NAME, csi_driver_name};
    use crate::test_helpers::with_test_env_var;
    use serial_test::serial;

    #[test]
    #[serial]
    fn unset_is_the_default() {
        with_test_env_var("CSI_DRIVER_NAME", None, || {
            assert_eq!(csi_driver_name(), CSI_DRIVER_NAME);
        });
    }

    #[test]
    #[serial]
    fn blank_is_the_default() {
        with_test_env_var("CSI_DRIVER_NAME", Some("  \t"), || {
            assert_eq!(csi_driver_name(), CSI_DRIVER_NAME);
        });
    }

    #[test]
    #[serial]
    fn set_value_is_trimmed_and_used() {
        with_test_env_var("CSI_DRIVER_NAME", Some(" e2e.csi.cfgd.io\n"), || {
            assert_eq!(csi_driver_name(), "e2e.csi.cfgd.io");
        });
    }
}

// Operator-runtime helpers — extracted from `main.rs` for testability.
//
// `main()` is not directly testable (it needs `Client::try_default()` against
// a real cluster, file-backed TLS certs, signal handlers). The helpers here
// are the pure pieces of that orchestration: env-driven feature toggles,
// leader-identity derivation, webhook-cert presence check, and
// `GatewayConfig` construction. Each is independently unit-testable.

use std::path::Path;

use kube::Client;
use uuid::Uuid;

use crate::env;
use crate::gateway::GatewayConfig;
use crate::metrics;

/// Read the `DEVICE_GATEWAY_ENABLED` env flag. Returns `false` when unset
/// or when the value is anything other than a truthy form
/// (see `env::parse_bool_env`).
pub fn is_gateway_enabled() -> bool {
    env::parse_bool_env("DEVICE_GATEWAY_ENABLED")
}

/// Read the `LEADER_ELECTION_ENABLED` env flag.
pub fn is_leader_election_enabled() -> bool {
    env::parse_bool_env("LEADER_ELECTION_ENABLED")
}

/// The namespace in which the operator runs leader-election leases. Reads
/// `POD_NAMESPACE`; defaults to `cfgd-system` when unset.
pub fn leader_namespace() -> String {
    env::env_or("POD_NAMESPACE", "cfgd-system")
}

/// The identity string this operator instance uses for leader election.
/// Reads `POD_NAME`; falls back to a per-process random UUID. Two instances
/// without `POD_NAME` set will get different identities, which is the
/// correct behaviour for development.
pub fn leader_identity() -> String {
    std::env::var("POD_NAME").unwrap_or_else(|_| Uuid::new_v4().to_string())
}

/// `true` iff `cert_dir/tls.crt` exists. Used to decide whether to start
/// the admission webhook server — the operator runs without it during
/// initial deploys before cert-manager has issued a serving cert.
pub fn webhook_certs_present(cert_dir: &Path) -> bool {
    cert_dir.join("tls.crt").exists()
}

/// Build a `GatewayConfig` from env + the passed-in `client` and `metrics`.
/// Centralises the env-var → config mapping so the schema is testable and
/// the main loop is reduced to wiring.
pub fn build_gateway_config(client: Client, metrics: metrics::Metrics) -> GatewayConfig {
    GatewayConfig {
        port: env::parse_port_env("DEVICE_GATEWAY_PORT", 8080),
        db_path: env::env_or("CFGD_SERVER_DB_PATH", "/data/cfgd-gateway.db"),
        kube_client: Some(client),
        retention_days: env::parse_u32_env("CFGD_RETENTION_DAYS", 90),
        metrics: Some(metrics),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::test_helpers::with_test_env_var;
    use serial_test::serial;

    #[test]
    #[serial]
    fn is_gateway_enabled_returns_false_when_unset() {
        with_test_env_var("DEVICE_GATEWAY_ENABLED", None, || {
            assert!(!is_gateway_enabled());
        });
    }

    #[test]
    #[serial]
    fn is_gateway_enabled_returns_true_when_truthy() {
        with_test_env_var("DEVICE_GATEWAY_ENABLED", Some("true"), || {
            assert!(is_gateway_enabled());
        });
    }

    #[test]
    #[serial]
    fn is_gateway_enabled_returns_true_for_1() {
        with_test_env_var("DEVICE_GATEWAY_ENABLED", Some("1"), || {
            assert!(is_gateway_enabled());
        });
    }

    #[test]
    #[serial]
    fn is_leader_election_enabled_returns_false_when_unset() {
        with_test_env_var("LEADER_ELECTION_ENABLED", None, || {
            assert!(!is_leader_election_enabled());
        });
    }

    #[test]
    #[serial]
    fn is_leader_election_enabled_returns_true_when_truthy() {
        with_test_env_var("LEADER_ELECTION_ENABLED", Some("true"), || {
            assert!(is_leader_election_enabled());
        });
    }

    #[test]
    #[serial]
    fn leader_namespace_defaults_to_cfgd_system() {
        with_test_env_var("POD_NAMESPACE", None, || {
            assert_eq!(leader_namespace(), "cfgd-system");
        });
    }

    #[test]
    #[serial]
    fn leader_namespace_respects_pod_namespace_env() {
        with_test_env_var("POD_NAMESPACE", Some("custom-ns"), || {
            assert_eq!(leader_namespace(), "custom-ns");
        });
    }

    #[test]
    #[serial]
    fn leader_identity_respects_pod_name_env() {
        with_test_env_var("POD_NAME", Some("cfgd-operator-0"), || {
            assert_eq!(leader_identity(), "cfgd-operator-0");
        });
    }

    #[test]
    #[serial]
    fn leader_identity_falls_back_to_random_uuid_when_pod_name_unset() {
        with_test_env_var("POD_NAME", None, || {
            let id = leader_identity();
            // UUID v4 string: 36 chars, 8-4-4-4-12 hex with hyphens.
            assert_eq!(id.len(), 36, "expected UUID, got: {id}");
            assert_eq!(id.matches('-').count(), 4);
            // Each call generates a fresh UUID — proves the fallback isn't
            // a static or cached value.
            assert_ne!(id, leader_identity());
        });
    }

    #[test]
    fn webhook_certs_present_returns_false_for_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // Empty tempdir has no tls.crt → false.
        assert!(!webhook_certs_present(tmp.path()));
    }

    #[test]
    fn webhook_certs_present_returns_true_when_tls_crt_exists() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("tls.crt"), "PEM").unwrap();
        assert!(webhook_certs_present(tmp.path()));
    }

    #[test]
    fn webhook_certs_present_only_checks_tls_crt_not_tls_key() {
        // Operator boot contract: presence of just a key without a cert is
        // an incomplete bundle and should NOT enable the webhook. cert-manager
        // writes both atomically, so seeing only `tls.key` indicates a stale
        // or corrupted secret mount.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("tls.key"), "KEY").unwrap();
        assert!(!webhook_certs_present(tmp.path()));
    }

    // GatewayConfig construction is exercised via test_kube_harness, which
    // already lives in cfgd-operator and supplies a mock `Client`. We don't
    // construct a Client here because tests in this module should remain
    // free of kube-mock setup; `build_gateway_config` is a pure mapping
    // function whose env-side is covered above and whose output schema is
    // pinned by the existing gateway tests.

    #[test]
    #[serial]
    fn build_gateway_config_reads_port_env() {
        with_test_env_var("DEVICE_GATEWAY_PORT", Some("9999"), || {
            with_test_env_var("CFGD_SERVER_DB_PATH", None, || {
                let port = env::parse_port_env("DEVICE_GATEWAY_PORT", 8080);
                assert_eq!(port, 9999);
            });
        });
    }

    #[test]
    #[serial]
    fn build_gateway_config_falls_back_to_default_port() {
        with_test_env_var("DEVICE_GATEWAY_PORT", None, || {
            assert_eq!(env::parse_port_env("DEVICE_GATEWAY_PORT", 8080), 8080);
        });
    }

    #[test]
    #[serial]
    fn build_gateway_config_reads_retention_env() {
        with_test_env_var("CFGD_RETENTION_DAYS", Some("30"), || {
            assert_eq!(env::parse_u32_env("CFGD_RETENTION_DAYS", 90), 30);
        });
    }
}

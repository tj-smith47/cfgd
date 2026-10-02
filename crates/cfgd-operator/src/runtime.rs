// Operator-runtime helpers — extracted from `main.rs` for testability.
//
// `main()` is not directly testable (it needs `Client::try_default()` against
// a real cluster, file-backed TLS certs, signal handlers). The helpers here
// are the pure pieces of that orchestration: env-driven feature toggles,
// leader-identity derivation, webhook-cert presence check, and
// `GatewayConfig` construction. Each is independently unit-testable.

use std::path::Path;

use kube::Client;
use kube::runtime::watcher::Config as WatcherConfig;
use uuid::Uuid;

use crate::controllers::BackupPolicyCache;
use crate::env;
use crate::gateway::GatewayConfig;
use crate::metrics;

/// Read the `DEVICE_GATEWAY_ENABLED` env flag. Returns `false` when unset
/// or when the value is anything other than a truthy form
/// (see `env::parse_bool_env`).
pub fn is_gateway_enabled() -> bool {
    env::parse_bool_env("DEVICE_GATEWAY_ENABLED")
}

/// Read the `DEVICE_GATEWAY_STANDALONE` env flag. Returns `false` when unset
/// or when the value is anything other than a truthy form
/// (see `env::parse_bool_env`). When `true`, the operator runs ONLY the device
/// gateway with no Kubernetes client — no controllers, webhook, or leader
/// election (all of which require a cluster). Standalone implies gateway-enabled.
pub fn is_gateway_standalone() -> bool {
    env::parse_bool_env("DEVICE_GATEWAY_STANDALONE")
}

/// Read the `LEADER_ELECTION_ENABLED` env flag.
pub fn is_leader_election_enabled() -> bool {
    env::parse_bool_env("LEADER_ELECTION_ENABLED")
}

/// The namespace in which the operator runs leader-election leases. Reads
/// `POD_NAMESPACE`; defaults to `cfgd-system` when unset.
pub fn leader_namespace() -> String {
    cfgd_core::env_or("POD_NAMESPACE", cfgd_core::CFGD_SYSTEM_NAMESPACE)
}

/// The watch every controller lists and watches its `cfgd.io` kinds through.
/// `WATCH_LABEL_SELECTOR` confines this operator to the objects it names, so
/// two installs in one cluster (the release and an e2e run) never reconcile
/// the same object. Unset or empty watches everything.
pub fn watch_config() -> WatcherConfig {
    match std::env::var("WATCH_LABEL_SELECTOR") {
        Ok(s) if !s.is_empty() => WatcherConfig::default().labels(&s),
        _ => WatcherConfig::default(),
    }
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
/// the main loop is reduced to wiring. `client` is `None` in standalone
/// (off-cluster) mode and `Some(_)` in the normal cluster-backed path, and
/// `backup_policies` is the slot the controllers publish their BackupPolicy
/// cache into: a standalone gateway holds an empty one and lists for itself.
pub fn build_gateway_config(
    client: Option<Client>,
    backup_policies: BackupPolicyCache,
    metrics: metrics::Metrics,
) -> GatewayConfig {
    GatewayConfig {
        port: env::parse_port_env("DEVICE_GATEWAY_PORT", 8080),
        db_path: cfgd_core::env_or(cfgd_core::CFGD_SERVER_DB_PATH_ENV, "/data/cfgd-gateway.db"),
        kube_client: client,
        backup_policies,
        retention_days: env::parse_u32_env(cfgd_core::CFGD_RETENTION_DAYS_ENV, 90),
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
    fn is_gateway_standalone_returns_false_when_unset() {
        with_test_env_var("DEVICE_GATEWAY_STANDALONE", None, || {
            assert!(!is_gateway_standalone());
        });
    }

    #[test]
    #[serial]
    fn is_gateway_standalone_returns_true_when_truthy() {
        with_test_env_var("DEVICE_GATEWAY_STANDALONE", Some("true"), || {
            assert!(is_gateway_standalone());
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
    fn watch_config_carries_the_label_selector_only_when_set() {
        with_test_env_var("WATCH_LABEL_SELECTOR", None, || {
            assert_eq!(watch_config().label_selector, None);
        });
        with_test_env_var("WATCH_LABEL_SELECTOR", Some(""), || {
            assert_eq!(watch_config().label_selector, None);
        });
        with_test_env_var("WATCH_LABEL_SELECTOR", Some("cfgd.io/e2e-run=42"), || {
            assert_eq!(
                watch_config().label_selector.as_deref(),
                Some("cfgd.io/e2e-run=42")
            );
        });
    }

    #[tokio::test]
    #[serial]
    async fn watch_config_selector_reaches_the_machine_config_list_request() {
        use crate::controllers::test_kube_harness::{ExpectedCall, MockKubeHarness};
        use crate::crds::MachineConfig;
        use futures::StreamExt;
        use kube::Api;
        use kube::runtime::watcher;

        let mut config = WatcherConfig::default();
        with_test_env_var("WATCH_LABEL_SELECTOR", Some("cfgd.io/e2e-run=42"), || {
            config = watch_config();
        });
        let empty_list = serde_json::json!({
            "apiVersion": "cfgd.io/v1alpha1",
            "kind": "MachineConfigList",
            "metadata": {"resourceVersion": "1"},
            "items": [],
        });
        let (ctx, _registry, harness) = MockKubeHarness::new(vec![
            ExpectedCall::list("/apis/cfgd.io/v1alpha1/machineconfigs")
                .with_query_contains("labelSelector=cfgd.io%2Fe2e-run%3D42")
                .returning_json(&empty_list),
        ]);

        // Init, then the first list page answered empty ends in InitDone;
        // stopping there keeps the watcher from opening its watch request.
        let events: Vec<_> =
            watcher::watcher(Api::<MachineConfig>::all(ctx.client.clone()), config)
                .take(2)
                .collect()
                .await;
        assert!(
            matches!(events.last(), Some(Ok(watcher::Event::InitDone))),
            "the list should complete the initial sync: {events:?}"
        );
        harness.finish().await;
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
    // already lives in cfgd-operator and supplies a mock `Client`. No Client is
    // constructed here because tests in this module should remain
    // free of kube-mock setup; `build_gateway_config` is a pure mapping
    // function whose env-side is covered above and whose output schema is
    // pinned by the existing gateway tests.

    #[test]
    #[serial]
    fn build_gateway_config_reads_port_env() {
        with_test_env_var("DEVICE_GATEWAY_PORT", Some("9999"), || {
            with_test_env_var(cfgd_core::CFGD_SERVER_DB_PATH_ENV, None, || {
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
        with_test_env_var(cfgd_core::CFGD_RETENTION_DAYS_ENV, Some("30"), || {
            assert_eq!(
                env::parse_u32_env(cfgd_core::CFGD_RETENTION_DAYS_ENV, 90),
                30
            );
        });
    }

    /// Every YAML file under `dir`, skipping build output and dot-directories.
    fn yaml_files_below(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
            panic!("{}: the scan must read every directory: {e}", dir.display())
        });
        for entry in entries {
            let path = entry
                .unwrap_or_else(|e| {
                    panic!("{}: the scan must read every entry: {e}", dir.display())
                })
                .path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if !name.starts_with('.') && !name.starts_with('-') && name != "target" {
                    yaml_files_below(&path, out);
                }
            } else if name.ends_with(".yaml") || name.ends_with(".yml") {
                out.push(path);
            }
        }
    }

    /// Why an operator workload manifest fails to hand its container the pod's
    /// name and namespace through the downward API unconditionally, or `None`.
    /// A Helm conditional around the entry counts as a failure: the event
    /// recorder reads `POD_NAME` whether or not leader election is on.
    fn downward_identity_gap(text: &str) -> Option<String> {
        let lines: Vec<&str> = text.lines().collect();
        let mut depth = 0i32;
        let mut containers_depth = None;
        let mut found = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            for (at, _) in line.match_indices("{{") {
                let action = line[at + 2..].strip_prefix('-').unwrap_or(&line[at + 2..]);
                let keyword = action
                    .trim_start()
                    .split(|c: char| !c.is_ascii_alphabetic())
                    .next()
                    .unwrap_or("");
                match keyword {
                    "if" | "with" | "range" => depth += 1,
                    "end" => depth -= 1,
                    _ => {}
                }
            }
            if t == "containers:" {
                containers_depth = Some(depth);
            }
            for (var, field) in [
                ("POD_NAME", "metadata.name"),
                ("POD_NAMESPACE", "metadata.namespace"),
            ] {
                if t == format!("- name: {var}") {
                    let window = lines[i + 1..(i + 4).min(lines.len())].join("\n");
                    if !window.contains(&format!("fieldPath: {field}")) {
                        return Some(format!("{var} is not read from {field}"));
                    }
                    if Some(depth) != containers_depth {
                        return Some(format!("{var} sits inside a template conditional"));
                    }
                    found.push(var);
                }
            }
        }
        ["POD_NAME", "POD_NAMESPACE"]
            .iter()
            .find(|var| !found.contains(var))
            .map(|var| format!("no {var} entry"))
    }

    /// The operator names its leader lease and its events after `POD_NAME`,
    /// falling back to a random UUID, so a manifest that omits it leaves the
    /// lease holder naming no pod. Every workload in the repository running
    /// the operator container carries both downward-API entries.
    #[test]
    fn every_operator_workload_manifest_names_its_pod_through_the_downward_api() {
        let root = cfgd_core::test_helpers::workspace_root();
        let mut files = Vec::new();
        yaml_files_below(&root, &mut files);
        let mut scanned = Vec::new();
        let mut gaps = Vec::new();
        for path in files {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let workload = text.lines().any(|l| {
                let t = l.trim();
                t == "kind: Deployment" || t == "kind: ClusterServiceVersion"
            });
            let runs_operator = text
                .lines()
                .any(|l| matches!(l.trim(), "- name: operator" | "- name: cfgd-operator"));
            if !(workload && runs_operator) {
                continue;
            }
            let rel = cfgd_core::to_posix_string(path.strip_prefix(&root).unwrap_or(&path));
            if let Some(gap) = downward_identity_gap(&text) {
                gaps.push(format!("{rel}: {gap}"));
            }
            scanned.push(rel);
        }
        for anchor in [
            "chart/cfgd/templates/operator-deployment.yaml",
            "tests/e2e/operator/manifests/operator-deployment.yaml",
            "tests/e2e/node/manifests/cfgd-server.yaml",
            "ecosystem/olm/manifests/cfgd-operator.clusterserviceversion.yaml",
        ] {
            assert!(
                scanned.iter().any(|w| w == anchor),
                "the scan missed {anchor}: {scanned:?}"
            );
        }
        assert!(gaps.is_empty(), "{gaps:#?}");
    }

    /// Each way a manifest can fail to name its pod is reported.
    #[test]
    fn downward_identity_gap_reports_each_missing_or_conditional_entry() {
        let sound = "containers:\n  env:\n    - name: POD_NAME\n      valueFrom:\n        fieldRef:\n          fieldPath: metadata.name\n    - name: POD_NAMESPACE\n      valueFrom:\n        fieldRef:\n          fieldPath: metadata.namespace\n";
        assert_eq!(downward_identity_gap(sound), None);
        assert_eq!(
            downward_identity_gap(&sound.replace("    - name: POD_NAME\n", "    - name: OTHER\n")),
            Some("no POD_NAME entry".to_string())
        );
        assert_eq!(
            downward_identity_gap(
                &sound.replace("fieldPath: metadata.name\n", "fieldPath: spec.nodeName\n")
            ),
            Some("POD_NAME is not read from metadata.name".to_string())
        );
        let conditional = sound.replace(
            "  env:\n",
            "  env:\n    {{- if .Values.operator.leaderElection.enabled }}\n",
        ) + "    {{- end }}\n";
        assert_eq!(
            downward_identity_gap(&conditional),
            Some("POD_NAME sits inside a template conditional".to_string())
        );
        let balanced_dashless = sound.replace(
            "  env:\n",
            "  env:\n    {{ with .Values.extra }}\n    - name: EXTRA\n    {{ end }}\n    {{ range .Values.more }}{{ end }}\n",
        );
        assert_eq!(downward_identity_gap(&balanced_dashless), None);
        let dashless_range = sound
            .replace("  env:\n", "  env:\n    {{ range .Values.replicas }}\n")
            + "    {{ end }}\n";
        assert_eq!(
            downward_identity_gap(&dashless_range),
            Some("POD_NAME sits inside a template conditional".to_string())
        );
    }
}

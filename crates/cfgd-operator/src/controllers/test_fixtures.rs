//! CRD object builders for controller tests.
//!
//! Each builder returns a fully-populated CRD object (not just its spec)
//! with the metadata fields the reconcile fns inspect — name, namespace,
//! UID, owner references, and generation. Tests compose them by calling
//! the base builder then mutating the parts they care about.
#![cfg(test)]

use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::ObjectMeta;

use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicySpec, ConfigPolicy, ConfigPolicySpec, DriftAlert,
    DriftAlertSpec, DriftDetail, DriftSeverity, MachineConfig, MachineConfigReference,
    MachineConfigSpec, MachineConfigStatus,
};

pub(super) fn meta(name: &str, namespace: Option<&str>) -> ObjectMeta {
    ObjectMeta {
        name: Some(name.to_string()),
        namespace: namespace.map(|s| s.to_string()),
        uid: Some(format!("uid-{name}")),
        generation: Some(1),
        ..Default::default()
    }
}

pub(super) fn machine_config(name: &str, namespace: &str) -> MachineConfig {
    MachineConfig {
        metadata: meta(name, Some(namespace)),
        spec: MachineConfigSpec {
            hostname: format!("{name}.test"),
            profile: "default".to_string(),
            module_refs: vec![],
            packages: vec![],
            files: vec![],
            system_settings: BTreeMap::new(),
        },
        status: None,
    }
}

pub(super) fn drift_alert(
    name: &str,
    namespace: &str,
    mc_name: &str,
    severity: DriftSeverity,
) -> DriftAlert {
    DriftAlert {
        metadata: meta(name, Some(namespace)),
        spec: DriftAlertSpec {
            device_id: format!("device-{name}"),
            machine_config_ref: MachineConfigReference {
                name: mc_name.to_string(),
                namespace: None,
            },
            drift_details: vec![DriftDetail {
                field: "packages.foo".to_string(),
                expected: "1.0".to_string(),
                actual: "1.1".to_string(),
            }],
            severity,
        },
        status: None,
    }
}

/// A `MachineConfigStatus` whose `conditions` already contains a
/// `DriftDetected=True` entry — useful for tests of the "drift already
/// recorded" path through `reconcile_drift_alert`.
pub(super) fn machine_config_status_with_drift_detected() -> MachineConfigStatus {
    use crate::crds::Condition;

    MachineConfigStatus {
        last_reconciled: Some("2026-01-01T00:00:00Z".to_string()),
        observed_generation: Some(1),
        conditions: vec![Condition {
            condition_type: "DriftDetected".to_string(),
            status: "True".to_string(),
            reason: "DriftActive".to_string(),
            message: "Drift detected".to_string(),
            last_transition_time: "2026-01-01T00:00:00Z".to_string(),
            observed_generation: Some(1),
        }],
        package_versions: BTreeMap::new(),
    }
}

pub(super) fn config_policy(name: &str, namespace: &str) -> ConfigPolicy {
    ConfigPolicy {
        metadata: meta(name, Some(namespace)),
        spec: ConfigPolicySpec::default(),
        status: None,
    }
}

pub(super) fn cluster_config_policy_with_spec(
    name: &str,
    spec: ClusterConfigPolicySpec,
) -> ClusterConfigPolicy {
    ClusterConfigPolicy {
        metadata: meta(name, None),
        spec,
        status: None,
    }
}

/// Owner reference matching the `MachineConfig` returned by
/// [`machine_config`] — i.e., kind=MachineConfig, name=<mc_name>, uid=uid-<mc_name>.
pub(super) fn machine_config_owner_ref(mc_name: &str) -> OwnerReference {
    OwnerReference {
        api_version: cfgd_core::API_VERSION.to_string(),
        kind: "MachineConfig".to_string(),
        name: mc_name.to_string(),
        uid: format!("uid-{mc_name}"),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

/// Build the canonical k8s API path for a namespaced MachineConfig resource.
///
/// `cfgd_core::API_VERSION` is the literal `"cfgd.io/v1alpha1"`; the helper
/// inlines the path string to keep the hot test path free of format!()-over-
/// const concatenation overhead.
pub(crate) fn machine_config_path(namespace: &str, name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{namespace}/machineconfigs/{name}")
}

/// Build a `MachineConfigList` JSON envelope around the given items.
pub(crate) fn mc_list(items: &[MachineConfig]) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cfgd.io/v1alpha1",
        "kind": "MachineConfigList",
        "items": items,
        "metadata": {},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_config_path_formats_namespaced_url() {
        assert_eq!(
            machine_config_path("ns", "foo"),
            "/apis/cfgd.io/v1alpha1/namespaces/ns/machineconfigs/foo"
        );
    }

    #[test]
    fn mc_list_empty_returns_well_formed_envelope() {
        let v = mc_list(&[]);
        assert_eq!(v["apiVersion"], "cfgd.io/v1alpha1");
        assert_eq!(v["kind"], "MachineConfigList");
        assert!(v["items"].is_array());
        assert_eq!(v["items"].as_array().expect("items is array").len(), 0);
        assert!(v["metadata"].is_object());
    }
}

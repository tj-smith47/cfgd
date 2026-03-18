use std::sync::Arc;

use futures::StreamExt;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::Controller;
use kube::runtime::controller::Action;
use kube::{Client, Resource, ResourceExt};
use tracing::{info, warn};

use crate::crds::{
    Condition, ConfigPolicy, ConfigPolicyStatus, DriftAlert, MachineConfig, MachineConfigSpec,
    MachineConfigStatus, version_satisfies,
};
use crate::errors::OperatorError;

pub struct ControllerContext {
    pub client: Client,
}

pub async fn run(client: Client) -> Result<(), OperatorError> {
    let ctx = Arc::new(ControllerContext {
        client: client.clone(),
    });

    let machines: Api<MachineConfig> = Api::all(client.clone());
    let alerts: Api<DriftAlert> = Api::all(client.clone());
    let policies: Api<ConfigPolicy> = Api::all(client.clone());

    let mc_ctx = Arc::clone(&ctx);
    let da_ctx = Arc::clone(&ctx);
    let cp_ctx = Arc::clone(&ctx);

    info!("Starting MachineConfig controller");
    info!("Starting DriftAlert controller");
    info!("Starting ConfigPolicy controller");

    let mc_controller = Controller::new(machines, Default::default())
        .run(reconcile_machine_config, error_policy_mc, mc_ctx)
        .for_each(|result| async move {
            match result {
                Ok((obj_ref, _action)) => {
                    info!(name = %obj_ref.name, "MachineConfig reconciled");
                }
                Err(err) => {
                    warn!(error = %err, "MachineConfig reconciliation error");
                }
            }
        });

    let da_controller = Controller::new(alerts, Default::default())
        .run(reconcile_drift_alert, error_policy_da, da_ctx)
        .for_each(|result| async move {
            match result {
                Ok((obj_ref, _action)) => {
                    info!(name = %obj_ref.name, "DriftAlert reconciled");
                }
                Err(err) => {
                    warn!(error = %err, "DriftAlert reconciliation error");
                }
            }
        });

    let cp_controller = Controller::new(policies, Default::default())
        .run(reconcile_config_policy, error_policy_cp, cp_ctx)
        .for_each(|result| async move {
            match result {
                Ok((obj_ref, _action)) => {
                    info!(name = %obj_ref.name, "ConfigPolicy reconciled");
                }
                Err(err) => {
                    warn!(error = %err, "ConfigPolicy reconciliation error");
                }
            }
        });

    tokio::join!(mc_controller, da_controller, cp_controller);

    Ok(())
}

// ---------------------------------------------------------------------------
// MachineConfig controller
// ---------------------------------------------------------------------------

async fn reconcile_machine_config(
    obj: Arc<MachineConfig>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();

    validate_spec(&obj.spec)?;

    info!(
        name = %name,
        namespace = %namespace,
        hostname = %obj.spec.hostname,
        profile = %obj.spec.profile,
        packages = obj.spec.packages.len(),
        files = obj.spec.files.len(),
        "Reconciling MachineConfig"
    );

    let current_drift = obj
        .status
        .as_ref()
        .map(|s| s.drift_detected)
        .unwrap_or(false);

    let current_generation = obj.meta().generation;
    let observed_generation = obj.status.as_ref().and_then(|s| s.observed_generation);

    // Skip if we've already observed this generation and there's no drift
    let generation_unchanged =
        current_generation.is_some() && current_generation == observed_generation;
    if generation_unchanged && !current_drift {
        info!(name = %name, "Already reconciled this generation, skipping");
        return Ok(Action::requeue(std::time::Duration::from_secs(60)));
    }

    // If not drifted, clean up any stale DriftAlerts for this MachineConfig
    if !current_drift {
        cleanup_drift_alerts(&ctx.client, &namespace, &name).await;
    }

    let machines: Api<MachineConfig> = if namespace.is_empty() {
        Api::all(ctx.client.clone())
    } else {
        Api::namespaced(ctx.client.clone(), &namespace)
    };

    let now = cfgd_core::utc_now_iso8601();

    let (ready_status, ready_reason, ready_message) = if current_drift {
        (
            "False".to_string(),
            "DriftDetected".to_string(),
            format!("MachineConfig {} has detected drift on device", name),
        )
    } else {
        (
            "True".to_string(),
            "ReconcileSuccess".to_string(),
            format!("MachineConfig {} reconciled successfully", name),
        )
    };

    let status = serde_json::json!({
        "status": MachineConfigStatus {
            last_reconciled: Some(now.clone()),
            drift_detected: current_drift,
            observed_generation: current_generation,
            conditions: vec![
                Condition {
                    condition_type: "Ready".to_string(),
                    status: ready_status,
                    reason: ready_reason,
                    message: ready_message,
                    last_transition_time: now,
                },
            ],
        }
    });

    machines
        .patch_status(
            &name,
            &PatchParams::apply("cfgd-operator"),
            &Patch::Merge(status),
        )
        .await
        .map_err(|e| {
            OperatorError::Reconciliation(format!("failed to update status for {name}: {e}"))
        })?;

    info!(name = %name, "Status updated with last_reconciled timestamp");

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

fn validate_spec(spec: &MachineConfigSpec) -> Result<(), OperatorError> {
    spec.validate()
        .map_err(|errors| OperatorError::InvalidSpec(errors.join("; ")))
}

/// Error policy for MachineConfig reconciliation failures.
/// Returns a base requeue duration; kube-rs Controller internally applies
/// exponential backoff (via its scheduler) for repeated failures of the
/// same object, so we don't need manual retry counting here.
fn error_policy_mc(
    _obj: Arc<MachineConfig>,
    error: &OperatorError,
    _ctx: Arc<ControllerContext>,
) -> Action {
    warn!(error = %error, "MachineConfig reconciliation error, requeuing");
    Action::requeue(std::time::Duration::from_secs(30))
}

// ---------------------------------------------------------------------------
// DriftAlert controller — updates MachineConfig drift status
// ---------------------------------------------------------------------------

async fn reconcile_drift_alert(
    obj: Arc<DriftAlert>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    let mc_ref = &obj.spec.machine_config_ref;

    info!(
        name = %name,
        machine_config = %mc_ref,
        device_id = %obj.spec.device_id,
        severity = ?obj.spec.severity,
        details_count = obj.spec.drift_details.len(),
        "Reconciling DriftAlert"
    );

    let machines: Api<MachineConfig> = if namespace.is_empty() {
        Api::all(ctx.client.clone())
    } else {
        Api::namespaced(ctx.client.clone(), &namespace)
    };

    match machines.get(mc_ref).await {
        Ok(mc) => {
            let is_drifted = mc
                .status
                .as_ref()
                .map(|s| s.drift_detected)
                .unwrap_or(false);

            // If MC is no longer drifted, this alert is resolved — delete it
            if !is_drifted && obj.spec.drift_details.is_empty() {
                let alerts: Api<DriftAlert> = if namespace.is_empty() {
                    Api::all(ctx.client.clone())
                } else {
                    Api::namespaced(ctx.client.clone(), &namespace)
                };
                if let Err(e) = alerts.delete(&name, &Default::default()).await {
                    warn!(name = %name, error = %e, "Failed to delete resolved DriftAlert");
                }
                return Ok(Action::requeue(std::time::Duration::from_secs(60)));
            }

            if !is_drifted {
                let now = cfgd_core::utc_now_iso8601();
                let status = serde_json::json!({
                    "status": {
                        "driftDetected": true,
                        "conditions": [
                            {
                                "type": "Ready",
                                "status": "False",
                                "reason": "DriftDetected",
                                "message": format!(
                                    "Drift detected on device {} — {} detail(s)",
                                    obj.spec.device_id,
                                    obj.spec.drift_details.len()
                                ),
                                "lastTransitionTime": now,
                            }
                        ]
                    }
                });

                machines
                    .patch_status(
                        mc_ref,
                        &PatchParams::apply("cfgd-operator"),
                        &Patch::Merge(status),
                    )
                    .await
                    .map_err(|e| {
                        OperatorError::Reconciliation(format!(
                            "failed to update drift status for MachineConfig {mc_ref}: {e}"
                        ))
                    })?;

                info!(
                    machine_config = %mc_ref,
                    "MachineConfig drift_detected set to true"
                );
            }
        }
        Err(kube::Error::Api(resp)) if resp.code == 404 => {
            warn!(
                machine_config = %mc_ref,
                "DriftAlert references non-existent MachineConfig"
            );
        }
        Err(e) => {
            return Err(OperatorError::Reconciliation(format!(
                "failed to get MachineConfig {mc_ref}: {e}"
            )));
        }
    }

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

/// Clean up resolved DriftAlerts for a MachineConfig that is no longer drifted.
async fn cleanup_drift_alerts(client: &Client, namespace: &str, mc_name: &str) {
    let alerts: Api<DriftAlert> = if namespace.is_empty() {
        Api::all(client.clone())
    } else {
        Api::namespaced(client.clone(), namespace)
    };

    // Use label selector to filter at API server instead of listing all alerts
    let lp = ListParams::default().labels(&format!("cfgd.io/machine-config={mc_name}"));

    let list = match alerts.list(&lp).await {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, "Failed to list DriftAlerts for cleanup");
            return;
        }
    };

    for alert in list {
        let alert_name = alert.name_any();
        if let Err(e) = alerts.delete(&alert_name, &Default::default()).await {
            warn!(
                name = %alert_name,
                error = %e,
                "Failed to delete resolved DriftAlert"
            );
        } else {
            info!(name = %alert_name, "Deleted resolved DriftAlert");
        }
    }
}

/// Error policy for DriftAlert reconciliation failures.
/// kube-rs Controller applies exponential backoff internally.
fn error_policy_da(
    _obj: Arc<DriftAlert>,
    error: &OperatorError,
    _ctx: Arc<ControllerContext>,
) -> Action {
    warn!(error = %error, "DriftAlert reconciliation error, requeuing");
    Action::requeue(std::time::Duration::from_secs(30))
}

// ---------------------------------------------------------------------------
// ConfigPolicy controller — validates MachineConfigs against policies
// ---------------------------------------------------------------------------

async fn reconcile_config_policy(
    obj: Arc<ConfigPolicy>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();

    info!(
        name = %name,
        required_modules = obj.spec.required_modules.len(),
        packages = obj.spec.packages.len(),
        settings = obj.spec.settings.len(),
        "Reconciling ConfigPolicy"
    );

    let machines: Api<MachineConfig> = if namespace.is_empty() {
        Api::all(ctx.client.clone())
    } else {
        Api::namespaced(ctx.client.clone(), &namespace)
    };

    let mc_list = machines.list(&ListParams::default()).await.map_err(|e| {
        OperatorError::Reconciliation(format!("failed to list MachineConfigs: {e}"))
    })?;

    let mut compliant_count: u32 = 0;
    let mut non_compliant_count: u32 = 0;

    for mc in &mc_list {
        if !matches_selector(&mc.spec, &obj.spec.target_selector) {
            continue;
        }

        if validate_policy_compliance(
            &mc.spec,
            &obj.spec.required_modules,
            &obj.spec.packages,
            &obj.spec.package_versions,
            &obj.spec.settings,
        ) {
            compliant_count += 1;
        } else {
            non_compliant_count += 1;
        }
    }

    let now = cfgd_core::utc_now_iso8601();
    let overall_status = if non_compliant_count == 0 {
        "True"
    } else {
        "False"
    };

    let policies: Api<ConfigPolicy> = if namespace.is_empty() {
        Api::all(ctx.client.clone())
    } else {
        Api::namespaced(ctx.client.clone(), &namespace)
    };

    let status = serde_json::json!({
        "status": ConfigPolicyStatus {
            compliant_count,
            non_compliant_count,
            conditions: vec![
                Condition {
                    condition_type: "Enforced".to_string(),
                    status: overall_status.to_string(),
                    reason: if non_compliant_count == 0 {
                        "AllCompliant".to_string()
                    } else {
                        "NonCompliantTargets".to_string()
                    },
                    message: format!(
                        "{} compliant, {} non-compliant",
                        compliant_count, non_compliant_count
                    ),
                    last_transition_time: now,
                },
            ],
        }
    });

    policies
        .patch_status(
            &name,
            &PatchParams::apply("cfgd-operator"),
            &Patch::Merge(status),
        )
        .await
        .map_err(|e| {
            OperatorError::Reconciliation(format!(
                "failed to update ConfigPolicy status for {name}: {e}"
            ))
        })?;

    info!(
        name = %name,
        compliant = compliant_count,
        non_compliant = non_compliant_count,
        "ConfigPolicy status updated"
    );

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

fn matches_selector(
    spec: &MachineConfigSpec,
    selector: &std::collections::BTreeMap<String, String>,
) -> bool {
    if selector.is_empty() {
        return true;
    }
    for (key, value) in selector {
        match key.as_str() {
            "hostname" => {
                if spec.hostname != *value {
                    return false;
                }
            }
            "profile" => {
                if spec.profile != *value {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

fn validate_policy_compliance(
    spec: &MachineConfigSpec,
    required_modules: &[String],
    packages: &[String],
    package_versions: &std::collections::BTreeMap<String, String>,
    settings: &std::collections::BTreeMap<String, String>,
) -> bool {
    // Check required modules are present in moduleRefs
    for module in required_modules {
        if !spec.module_refs.iter().any(|mr| mr.name == *module) {
            return false;
        }
    }
    for pkg in packages {
        if !spec.packages.contains(pkg) {
            return false;
        }
    }
    for (pkg, req_str) in package_versions {
        if !spec.packages.contains(pkg) {
            return false;
        }
        match spec.package_versions.get(pkg) {
            Some(reported) => {
                if !version_satisfies(reported, req_str) {
                    return false;
                }
            }
            None => return false,
        }
    }
    for (key, value) in settings {
        match spec.system_settings.get(key) {
            Some(v) if v == value => {}
            _ => return false,
        }
    }
    true
}

/// Error policy for ConfigPolicy reconciliation failures.
/// kube-rs Controller applies exponential backoff internally.
fn error_policy_cp(
    _obj: Arc<ConfigPolicy>,
    error: &OperatorError,
    _ctx: Arc<ControllerContext>,
) -> Action {
    warn!(error = %error, "ConfigPolicy reconciliation error, requeuing");
    Action::requeue(std::time::Duration::from_secs(30))
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mc_spec(hostname: &str, profile: &str) -> MachineConfigSpec {
        MachineConfigSpec {
            hostname: hostname.to_string(),
            profile: profile.to_string(),
            module_refs: vec![],
            packages: vec![],
            package_versions: Default::default(),
            files: vec![],
            system_settings: Default::default(),
        }
    }

    #[test]
    fn validate_spec_delegates_to_shared() {
        assert!(validate_spec(&mc_spec("", "default")).is_err());
        assert!(validate_spec(&mc_spec("host1", "default")).is_ok());
    }

    #[test]
    fn chrono_now_produces_rfc3339() {
        let now = cfgd_core::utc_now_iso8601();
        assert!(now.ends_with('Z'));
        assert!(now.contains('T'));
        assert_eq!(now.len(), 20); // "YYYY-MM-DDTHH:MM:SSZ"
    }

    #[test]
    fn matches_selector_empty_matches_all() {
        assert!(matches_selector(
            &mc_spec("any", "any"),
            &Default::default()
        ));
    }

    #[test]
    fn matches_selector_hostname() {
        let spec = mc_spec("host1", "default");
        let mut sel = std::collections::BTreeMap::new();
        sel.insert("hostname".to_string(), "host1".to_string());
        assert!(matches_selector(&spec, &sel));

        sel.insert("hostname".to_string(), "other".to_string());
        assert!(!matches_selector(&spec, &sel));
    }

    #[test]
    fn policy_compliance_all_packages_present() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec!["vim".to_string(), "git".to_string(), "curl".to_string()];
        let required = vec!["vim".to_string(), "git".to_string()];
        assert!(validate_policy_compliance(
            &spec,
            &[],
            &required,
            &Default::default(),
            &Default::default()
        ));
    }

    #[test]
    fn policy_compliance_missing_package() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec!["vim".to_string()];
        let required = vec!["vim".to_string(), "git".to_string()];
        assert!(!validate_policy_compliance(
            &spec,
            &[],
            &required,
            &Default::default(),
            &Default::default()
        ));
    }

    #[test]
    fn policy_compliance_settings() {
        let mut settings = std::collections::BTreeMap::new();
        settings.insert("key".to_string(), "value".to_string());

        let mut spec = mc_spec("h", "p");
        spec.system_settings = settings.clone();
        assert!(validate_policy_compliance(
            &spec,
            &[],
            &[],
            &Default::default(),
            &settings
        ));

        let mut wrong = std::collections::BTreeMap::new();
        wrong.insert("key".to_string(), "wrong".to_string());
        assert!(!validate_policy_compliance(
            &spec,
            &[],
            &[],
            &Default::default(),
            &wrong
        ));
    }

    #[test]
    fn policy_version_enforcement_satisfied() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec!["kubectl".to_string()];
        spec.package_versions
            .insert("kubectl".to_string(), "1.28.3".to_string());
        let mut reqs = std::collections::BTreeMap::new();
        reqs.insert("kubectl".to_string(), ">=1.28".to_string());
        assert!(validate_policy_compliance(
            &spec,
            &[],
            &[],
            &reqs,
            &Default::default()
        ));
    }

    #[test]
    fn policy_version_enforcement_not_satisfied() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec!["kubectl".to_string()];
        spec.package_versions
            .insert("kubectl".to_string(), "1.27.0".to_string());
        let mut reqs = std::collections::BTreeMap::new();
        reqs.insert("kubectl".to_string(), ">=1.28".to_string());
        assert!(!validate_policy_compliance(
            &spec,
            &[],
            &[],
            &reqs,
            &Default::default()
        ));
    }

    #[test]
    fn policy_version_enforcement_missing_version_report() {
        let mut spec = mc_spec("h", "p");
        spec.packages = vec!["kubectl".to_string()];
        let mut reqs = std::collections::BTreeMap::new();
        reqs.insert("kubectl".to_string(), ">=1.28".to_string());
        assert!(!validate_policy_compliance(
            &spec,
            &[],
            &[],
            &reqs,
            &Default::default()
        ));
    }

    #[test]
    fn policy_version_enforcement_missing_package() {
        let mut reqs = std::collections::BTreeMap::new();
        reqs.insert("kubectl".to_string(), ">=1.28".to_string());
        assert!(!validate_policy_compliance(
            &mc_spec("h", "p"),
            &[],
            &[],
            &reqs,
            &Default::default()
        ));
    }

    #[test]
    fn module_compliance_all_present() {
        let mut spec = mc_spec("h", "p");
        spec.module_refs = vec![
            crate::crds::ModuleRef {
                name: "corp-vpn".to_string(),
                required: true,
            },
            crate::crds::ModuleRef {
                name: "corp-certs".to_string(),
                required: true,
            },
        ];
        let required_modules = vec!["corp-vpn".to_string(), "corp-certs".to_string()];
        assert!(validate_policy_compliance(
            &spec,
            &required_modules,
            &[],
            &Default::default(),
            &Default::default()
        ));
    }

    #[test]
    fn module_compliance_missing_module() {
        let mut spec = mc_spec("h", "p");
        spec.module_refs = vec![crate::crds::ModuleRef {
            name: "corp-vpn".to_string(),
            required: true,
        }];
        let required_modules = vec!["corp-vpn".to_string(), "corp-certs".to_string()];
        assert!(!validate_policy_compliance(
            &spec,
            &required_modules,
            &[],
            &Default::default(),
            &Default::default()
        ));
    }

    #[test]
    fn module_compliance_no_requirements() {
        assert!(validate_policy_compliance(
            &mc_spec("h", "p"),
            &[],
            &[],
            &Default::default(),
            &Default::default()
        ));
    }
}

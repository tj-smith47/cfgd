use std::collections::BTreeMap;
use std::sync::Arc;

use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Resource, ResourceExt};
use tracing::{info, warn};

use crate::crds::{
    ClusterConfigPolicySpec, ConfigPolicy, ConfigPolicySpec, ConfigPolicyStatus, MachineConfig,
    MachineConfigSpec, MachineConfigStatus, ModuleRef, PackageRef,
};
use crate::errors::OperatorError;
use crate::metrics::PolicyLabels;
use cfgd_core::version_satisfies;

use super::{
    ControllerContext, FIELD_MANAGER_STATUS, build_condition, compliance_summary, emit_event,
    matches_selector, namespaced_api, record_reconcile_success,
};
pub(super) async fn reconcile_config_policy(
    obj: Arc<ConfigPolicy>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();

    info!(
        name = %name,
        required_modules = obj.spec.required_modules.len(),
        packages = obj.spec.packages.len(),
        settings = obj.spec.settings.len(),
        "reconciling ConfigPolicy"
    );

    let machines: Api<MachineConfig> = namespaced_api(&ctx.client, &namespace)?;

    let mc_list = machines.list(&ListParams::default()).await.map_err(|e| {
        OperatorError::Reconciliation(format!("failed to list MachineConfigs: {e}"))
    })?;

    // Filter MachineConfigs by target selector
    let targeted_mcs: Vec<&MachineConfig> = mc_list
        .iter()
        .filter(|mc| matches_selector(mc.metadata.labels.as_ref(), &obj.spec.target_selector))
        .collect();

    // Update each targeted MachineConfig's Compliant condition
    for mc in &targeted_mcs {
        let compliant = validate_policy_compliance(
            &mc.spec,
            mc.status.as_ref(),
            &obj.spec.required_modules,
            &obj.spec.packages,
            &obj.spec.settings,
        );
        let mc_name = mc.name_any();
        let mc_existing_conditions = mc
            .status
            .as_ref()
            .map(|s| s.conditions.as_slice())
            .unwrap_or(&[]);
        let now = cfgd_core::utc_now_iso8601();
        let (comp_status, comp_reason, comp_message) = if compliant {
            (
                "True",
                "PolicyCompliant",
                format!("Compliant with policy {}", name),
            )
        } else {
            (
                "False",
                "PolicyViolation",
                format!("Violates policy {}", name),
            )
        };
        let compliant_condition = build_condition(
            mc_existing_conditions,
            "Compliant",
            comp_status,
            comp_reason,
            &comp_message,
            &now,
            mc.meta().generation,
        );
        let mc_status_patch = serde_json::json!({
            "status": {
                "conditions": [compliant_condition]
            }
        });
        if let Err(e) = machines
            .patch_status(
                &mc_name,
                &PatchParams::apply(FIELD_MANAGER_STATUS),
                &Patch::Merge(mc_status_patch),
            )
            .await
        {
            warn!(name = %mc_name, error = %e, "failed to update Compliant condition on MachineConfig");
        }
    }

    // Evaluate compliance counts and emit violation events
    let targeted_mc_refs: Vec<MachineConfig> = targeted_mcs.into_iter().cloned().collect();
    let (compliant_count, non_compliant_count) = evaluate_policy_compliance(
        &ctx,
        &targeted_mc_refs,
        &obj.spec.packages,
        &obj.spec.required_modules,
        &obj.spec.settings,
        &name,
    )
    .await;

    let now = cfgd_core::utc_now_iso8601();
    let overall_status = if non_compliant_count == 0 {
        "True"
    } else {
        "False"
    };

    let policies: Api<ConfigPolicy> = namespaced_api(&ctx.client, &namespace)?;

    let existing_conditions = obj
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);

    let status = serde_json::json!({
        "status": ConfigPolicyStatus {
            compliant_count,
            non_compliant_count,
            conditions: vec![
                build_condition(
                    existing_conditions,
                    "Enforced", overall_status,
                    if non_compliant_count == 0 { "AllCompliant" } else { "NonCompliantTargets" },
                    &compliance_summary(compliant_count, non_compliant_count),
                    &now, obj.meta().generation,
                ),
            ],
        }
    });

    policies
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER_STATUS),
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
        "configPolicy status updated"
    );

    emit_policy_evaluation_events(
        &ctx,
        &obj.object_ref(&()),
        compliant_count,
        non_compliant_count,
    )
    .await;

    ctx.metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: name.clone(),
            namespace: namespace.clone(),
        })
        .set(i64::from(compliant_count));

    record_reconcile_success(&ctx, "config_policy", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}
pub(super) fn validate_policy_compliance(
    spec: &MachineConfigSpec,
    status: Option<&MachineConfigStatus>,
    required_modules: &[ModuleRef],
    packages: &[PackageRef],
    settings: &BTreeMap<String, serde_json::Value>,
) -> bool {
    for module in required_modules {
        if !spec.module_refs.iter().any(|mr| mr.name == module.name) {
            return false;
        }
    }
    for pkg in packages {
        if !spec.packages.iter().any(|p| p.name == pkg.name) {
            return false;
        }
        if let Some(req_str) = &pkg.version {
            let installed_versions = status.map(|s| &s.package_versions);
            match installed_versions.and_then(|pv| pv.get(&pkg.name)) {
                Some(reported) => {
                    if !version_satisfies(reported, req_str) {
                        return false;
                    }
                }
                None => return false,
            }
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
pub(super) async fn evaluate_policy_compliance(
    ctx: &ControllerContext,
    machine_configs: &[MachineConfig],
    required_packages: &[PackageRef],
    required_modules: &[ModuleRef],
    required_settings: &BTreeMap<String, serde_json::Value>,
    policy_name: &str,
) -> (u32, u32) {
    let mut compliant_count: u32 = 0;
    let mut non_compliant_count: u32 = 0;

    for mc in machine_configs {
        let compliant = validate_policy_compliance(
            &mc.spec,
            mc.status.as_ref(),
            required_modules,
            required_packages,
            required_settings,
        );
        if compliant {
            compliant_count += 1;
        } else {
            non_compliant_count += 1;
            let mc_name = mc.name_any();
            emit_event(
                &ctx.recorder,
                &mc.object_ref(&()),
                EventType::Warning,
                "PolicyViolation",
                format!("MachineConfig {} violates policy {}", mc_name, policy_name),
                "PolicyEvaluate",
            )
            .await;
        }
    }

    (compliant_count, non_compliant_count)
}

/// Emit standard post-evaluation events for a policy reconciler.
pub(super) async fn emit_policy_evaluation_events(
    ctx: &ControllerContext,
    obj_ref: &k8s_openapi::api::core::v1::ObjectReference,
    compliant_count: u32,
    non_compliant_count: u32,
) {
    emit_event(
        &ctx.recorder,
        obj_ref,
        EventType::Normal,
        "Evaluated",
        compliance_summary(compliant_count, non_compliant_count),
        "Evaluate",
    )
    .await;

    if non_compliant_count > 0 {
        emit_event(
            &ctx.recorder,
            obj_ref,
            EventType::Warning,
            "NonCompliantTargets",
            format!("{} non-compliant MachineConfigs", non_compliant_count),
            "Evaluate",
        )
        .await;
    }
}
pub(super) struct MergedPolicyRequirements {
    pub(super) packages: Vec<PackageRef>,
    pub(super) modules: Vec<ModuleRef>,
    pub(super) settings: BTreeMap<String, serde_json::Value>,
}

pub(super) fn merge_policy_requirements(
    cluster: &ClusterConfigPolicySpec,
    namespace_policies: &[&ConfigPolicySpec],
) -> MergedPolicyRequirements {
    let mut packages = cluster.packages.clone();
    let mut modules = cluster.required_modules.clone();
    let mut settings = BTreeMap::new();

    for ns in namespace_policies {
        for pkg in &ns.packages {
            if let Some(existing) = packages.iter_mut().find(|p| p.name == pkg.name) {
                if existing.version.is_none() {
                    existing.version = pkg.version.clone();
                }
            } else {
                packages.push(pkg.clone());
            }
        }
        for module in &ns.required_modules {
            if !modules.iter().any(|m| m.name == module.name) {
                modules.push(module.clone());
            }
        }
        settings.extend(ns.settings.clone());
    }

    for cluster_pkg in &cluster.packages {
        if let Some(ver) = &cluster_pkg.version
            && let Some(existing) = packages.iter_mut().find(|p| p.name == cluster_pkg.name)
        {
            existing.version = Some(ver.clone());
        }
    }

    settings.extend(cluster.settings.clone());

    MergedPolicyRequirements {
        packages,
        modules,
        settings,
    }
}

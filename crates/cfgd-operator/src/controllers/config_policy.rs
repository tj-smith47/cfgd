use std::collections::BTreeMap;
use std::sync::Arc;

use kube::api::{Api, Patch, PatchParams};
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
    machine_key, matches_selector, namespaced_api, record_reconcile_success, upsert_condition,
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

    // Filter MachineConfigs by target selector
    let targeted_mcs: Vec<Arc<MachineConfig>> = ctx
        .stores
        .machine_configs_in(&namespace)
        .await?
        .into_iter()
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
        // The condition is rebuilt from the object's own conditions every
        // reconcile, so an unchanged verdict produces a byte-identical entry —
        // patching it would be an etcd write and a watch fan-out per targeted
        // machine per minute, forever.
        if mc_existing_conditions.contains(&compliant_condition) {
            continue;
        }
        let mc_status_patch = serde_json::json!({
            "status": {
                "conditions": upsert_condition(mc_existing_conditions, compliant_condition)
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

    let already_reported = obj
        .status
        .as_ref()
        .map(|s| s.non_compliant_machines.as_slice())
        .unwrap_or(&[]);

    // Evaluate compliance counts and emit violation events
    let tally = evaluate_policy_compliance(
        &ctx,
        &targeted_mcs,
        &obj.spec.packages,
        &obj.spec.required_modules,
        &obj.spec.settings,
        &name,
        already_reported,
    )
    .await;

    let now = cfgd_core::utc_now_iso8601();
    let overall_status = if tally.non_compliant_count == 0 {
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

    let desired = ConfigPolicyStatus {
        compliant_count: tally.compliant_count,
        non_compliant_count: tally.non_compliant_count,
        non_compliant_machines: tally.non_compliant_machines,
        conditions: vec![build_condition(
            existing_conditions,
            "Enforced",
            overall_status,
            if tally.non_compliant_count == 0 {
                "AllCompliant"
            } else {
                "NonCompliantTargets"
            },
            &compliance_summary(tally.compliant_count, tally.non_compliant_count),
            &now,
            obj.meta().generation,
        )],
    };

    // `build_condition` carries the existing lastTransitionTime forward while
    // the condition's status holds, so an unchanged evaluation compares equal
    // and writes nothing.
    if obj.status.as_ref() != Some(&desired) {
        policies
            .patch_status(
                &name,
                &PatchParams::apply(FIELD_MANAGER_STATUS),
                &Patch::Merge(serde_json::json!({ "status": desired })),
            )
            .await
            .map_err(|e| {
                OperatorError::Reconciliation(format!(
                    "failed to update ConfigPolicy status for {name}: {e}"
                ))
            })?;

        info!(
            name = %name,
            compliant = tally.compliant_count,
            non_compliant = tally.non_compliant_count,
            "configPolicy status updated"
        );

        emit_policy_evaluation_events(
            &ctx,
            &obj.object_ref(&()),
            tally.compliant_count,
            tally.non_compliant_count,
        )
        .await;
    }

    ctx.metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: name.clone(),
            namespace: namespace.clone(),
        })
        .set(i64::from(tally.compliant_count));

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
/// Outcome of evaluating one policy against its targeted machines.
pub(super) struct ComplianceTally {
    pub(super) compliant_count: u32,
    pub(super) non_compliant_count: u32,
    /// `namespace/name` of every machine that failed, sorted — the value
    /// persisted as `status.nonCompliantMachines`.
    pub(super) non_compliant_machines: Vec<String>,
}

/// Count compliant/non-compliant machines, emitting a `PolicyViolation` Warning
/// only for machines that were not already recorded as violating.
///
/// `already_reported` is the policy's own persisted `nonCompliantMachines`, so
/// the transition memory survives an operator restart and is keyed per policy —
/// the MachineConfig's `Compliant` condition cannot serve, because several
/// policies may target one machine and each overwrites the other's verdict.
pub(super) async fn evaluate_policy_compliance(
    ctx: &ControllerContext,
    machine_configs: &[Arc<MachineConfig>],
    required_packages: &[PackageRef],
    required_modules: &[ModuleRef],
    required_settings: &BTreeMap<String, serde_json::Value>,
    policy_name: &str,
    already_reported: &[String],
) -> ComplianceTally {
    let mut compliant_count: u32 = 0;
    let mut non_compliant_machines: Vec<String> = Vec::new();

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
            continue;
        }

        let key = machine_key(mc);
        if !already_reported.iter().any(|k| k == &key) {
            emit_event(
                &ctx.recorder,
                &mc.object_ref(&()),
                EventType::Warning,
                "PolicyViolation",
                format!(
                    "MachineConfig {} violates policy {}",
                    mc.name_any(),
                    policy_name
                ),
                "PolicyEvaluate",
            )
            .await;
        }
        non_compliant_machines.push(key);
    }

    // Sorted so the persisted set is order-independent: the cache hands back
    // machines in hash order, and an unsorted list would compare unequal
    // between reconciles and force a write.
    non_compliant_machines.sort();

    ComplianceTally {
        compliant_count,
        non_compliant_count: u32::try_from(non_compliant_machines.len()).unwrap_or(u32::MAX),
        non_compliant_machines,
    }
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

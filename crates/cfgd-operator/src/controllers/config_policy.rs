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
    CONFIG_POLICY_FINALIZER, ControllerContext, FIELD_MANAGER_STATUS, add_finalizer,
    build_condition, compliance_summary, emit_event, machine_key, matches_selector, namespaced_api,
    record_reconcile_success, remove_finalizer, sort_and_cap_machines, upsert_condition,
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

    let finalizers = obj.metadata.finalizers.as_deref().unwrap_or(&[]);
    let has_finalizer = finalizers.iter().any(|f| f == CONFIG_POLICY_FINALIZER);

    let policies: Api<ConfigPolicy> = namespaced_api(&ctx.client, &namespace)?;

    if obj.metadata.deletion_timestamp.is_some() {
        if has_finalizer {
            info!(name = %name, "configPolicy being deleted, clearing its verdicts");
            clear_compliant_verdicts(&machines, &deletion_clear_targets(&obj, &targeted_mcs)).await;
            // The gauge loses its only writer with this reconcile, so an
            // unremoved series would export the deleted policy's last count
            // for the life of the process.
            ctx.metrics.devices_compliant.remove(&PolicyLabels {
                policy: name.clone(),
                namespace: namespace.clone(),
            });
            remove_finalizer(&policies, &name, finalizers, CONFIG_POLICY_FINALIZER).await?;
        }
        record_reconcile_success(&ctx, "config_policy", start);
        return Ok(Action::await_change());
    }

    if !has_finalizer {
        // The verdict this policy writes onto each machine outlives the policy
        // unless something clears it, and only a finalizer guarantees a last
        // reconcile in which to do that. The alternative — having the machine
        // controller ask whether a policy still exists — is the cross-controller
        // coupling that made the two rewrite each other's condition.
        add_finalizer(&policies, &name, finalizers, CONFIG_POLICY_FINALIZER).await?;
        info!(name = %name, "added finalizer to ConfigPolicy");
    }

    // Update each targeted MachineConfig's Compliant condition
    let mut verdicts: Vec<MachineVerdict<'_>> = Vec::with_capacity(targeted_mcs.len());
    for mc in &targeted_mcs {
        let compliant = validate_policy_compliance(
            &mc.spec,
            mc.status.as_ref(),
            &obj.spec.required_modules,
            &obj.spec.packages,
            &obj.spec.settings,
        );
        verdicts.push(MachineVerdict {
            machine: mc,
            compliant,
        });
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
    let tally = evaluate_policy_compliance(&ctx, &verdicts, &name, already_reported).await;

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
/// The machines whose verdict a deleted policy must retire: the selector match
/// at deletion time, plus every machine the policy's own status remembers as
/// non-compliant. The memory half covers a machine relabelled out of the
/// selector (or dropped by a `targetSelector` edit) after being judged, whose
/// stale `Compliant=False` nothing else would ever clear. A compliant machine
/// relabelled away is not in that memory and keeps its verdict until any policy
/// next evaluates it: the condition records no owner, so the full set of
/// machines ever judged is not derivable.
fn deletion_clear_targets(policy: &ConfigPolicy, targeted: &[Arc<MachineConfig>]) -> Vec<String> {
    let mut names: Vec<String> = targeted.iter().map(|mc| mc.name_any()).collect();
    let remembered = policy
        .status
        .as_ref()
        .map(|s| s.non_compliant_machines.as_slice())
        .unwrap_or(&[]);
    for key in remembered {
        // Keys are `namespace/name` (see `machine_key`); the Api is already
        // namespaced, so only the name half addresses the machine.
        let mc_name = key.split_once('/').map_or(key.as_str(), |(_, n)| n);
        if !names.iter().any(|n| n == mc_name) {
            names.push(mc_name.to_string());
        }
    }
    names
}

/// Reset the `Compliant` condition on every machine this policy had judged.
///
/// The policy controller owns that condition, so its deletion is the only event
/// that can retire the verdict: the machine controller carries whatever is there
/// forward verbatim, and asking it to look up whether the naming policy still
/// exists would put a policy read back into the machine reconcile. The value
/// written is the same triple the machine controller synthesizes for a machine
/// no policy has evaluated, so a machine another policy still targets is simply
/// re-judged on that policy's next pass rather than left in a third state.
///
/// Best effort per machine: a failure here must not block the finalizer, or a
/// deleted policy is stuck forever on one unreachable machine.
async fn clear_compliant_verdicts(machines: &Api<MachineConfig>, names: &[String]) {
    let now = cfgd_core::utc_now_iso8601();
    for mc_name in names {
        // The patch below replaces the whole conditions array, so it must be
        // built from the live object: a watch-cache copy can predate a
        // condition the machine controller wrote concurrently, and a merge
        // built from it would silently revert that write. One GET per machine
        // is the accepted cost: this loop runs only when a policy is deleted,
        // and the clear is already one PATCH per machine, so the read at most
        // doubles a per-machine fan-out the write side cannot avoid.
        let live = match machines.get_opt(mc_name).await {
            Ok(Some(mc)) => mc,
            // Already gone; there is no verdict left to retire.
            Ok(None) => continue,
            Err(e) => {
                warn!(name = %mc_name, error = %e, "failed to read MachineConfig before clearing its Compliant condition");
                continue;
            }
        };
        let existing = live
            .status
            .as_ref()
            .map(|s| s.conditions.as_slice())
            .unwrap_or(&[]);
        // Nothing to retire, and writing one would announce an evaluation that
        // never happened.
        if super::find_condition(existing, "Compliant").is_none() {
            continue;
        }
        let cleared = build_condition(
            existing,
            "Compliant",
            "Unknown",
            "NotEvaluated",
            "Awaiting policy evaluation",
            &now,
            live.meta().generation,
        );
        if existing.contains(&cleared) {
            continue;
        }
        let patch = serde_json::json!({
            "status": { "conditions": upsert_condition(existing, cleared) }
        });
        if let Err(e) = machines
            .patch_status(
                mc_name,
                &PatchParams::apply(FIELD_MANAGER_STATUS),
                &Patch::Merge(patch),
            )
            .await
        {
            warn!(name = %mc_name, error = %e, "failed to clear Compliant condition on MachineConfig");
        }
    }
}

/// Outcome of evaluating one policy against its targeted machines.
pub(super) struct ComplianceTally {
    pub(super) compliant_count: u32,
    /// The exact number of machines that failed, taken before the list beside it
    /// is capped. A caller aggregating several tallies accumulates THIS rather
    /// than counting the concatenated lists, which are already truncated.
    pub(super) non_compliant_count: u32,
    /// `namespace/name` of each machine that failed, sorted and then capped at
    /// [`crate::crds::MAX_NON_COMPLIANT_MACHINES`] — the value persisted as
    /// `status.nonCompliantMachines`.
    pub(super) non_compliant_machines: Vec<String>,
}

/// One machine paired with the verdict its policy reached about it.
///
/// The verdict travels with the machine rather than being recomputed here: the
/// ConfigPolicy controller already decides it to build the machine's `Compliant`
/// condition, and `validate_policy_compliance` walks every declared package,
/// module and setting per machine.
pub(super) struct MachineVerdict<'a> {
    pub(super) machine: &'a MachineConfig,
    pub(super) compliant: bool,
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
    verdicts: &[MachineVerdict<'_>],
    policy_name: &str,
    already_reported: &[String],
) -> ComplianceTally {
    let mut compliant_count: u32 = 0;
    let mut non_compliant_machines: Vec<String> = Vec::new();

    for verdict in verdicts {
        let mc = verdict.machine;
        if verdict.compliant {
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

    // The count is taken BEFORE the cap: it is the exact total, and only the
    // enumeration beside it is bounded.
    let non_compliant_count = u32::try_from(non_compliant_machines.len()).unwrap_or(u32::MAX);
    sort_and_cap_machines(&mut non_compliant_machines);

    ComplianceTally {
        compliant_count,
        non_compliant_count,
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

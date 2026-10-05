use std::collections::BTreeMap;
use std::sync::Arc;

use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Resource, ResourceExt};
use tracing::{info, warn};

use crate::crds::{
    ClusterConfigPolicySpec, Condition, ConfigPolicy, ConfigPolicySpec, ConfigPolicyStatus,
    MachineConfig, MachineConfigSpec, MachineConfigStatus, ModuleRef, PackageRef,
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

    let namespace_mcs: Vec<Arc<MachineConfig>> = ctx.stores.machine_configs_in(&namespace).await?;

    let finalizers = obj.metadata.finalizers.as_deref().unwrap_or(&[]);
    let has_finalizer = finalizers.iter().any(|f| f == CONFIG_POLICY_FINALIZER);

    let policies: Api<ConfigPolicy> = namespaced_api(&ctx.client, &namespace)?;

    // Every policy in force beside this one. The cache copy of this policy is
    // left out and the reconciled object is used instead, so a cache that lags
    // this policy's own create or spec edit cannot judge the machine against a
    // stale requirement.
    let mut in_force: Vec<Arc<ConfigPolicy>> = ctx
        .stores
        .config_policies_in(&namespace)
        .await?
        .into_iter()
        .filter(|p| p.name_any() != name)
        .collect();

    if obj.metadata.deletion_timestamp.is_some() {
        if has_finalizer {
            info!(name = %name, "configPolicy being deleted, clearing its verdicts");
            // Every machine in the namespace is re-judged from its live copy:
            // a cached copy can lag the verdict this policy just wrote, and
            // with no policy left to run another pass a skipped machine would
            // keep that verdict for good. Sorted so the reads go out in one
            // order whatever order the cache hands back.
            let mut names: Vec<String> = namespace_mcs.iter().map(|mc| mc.name_any()).collect();
            names.sort();
            clear_compliant_verdicts(&machines, &names, &in_force).await;
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

    in_force.push(Arc::clone(&obj));

    let targeted_mcs: Vec<&Arc<MachineConfig>> = namespace_mcs
        .iter()
        .filter(|mc| matches_selector(mc.metadata.labels.as_ref(), &obj.spec.target_selector))
        .collect();
    let verdicts: Vec<MachineVerdict<'_>> = targeted_mcs
        .iter()
        .map(|mc| MachineVerdict {
            machine: mc,
            compliant: validate_policy_compliance(
                &mc.spec,
                mc.status.as_ref(),
                &obj.spec.required_modules,
                &obj.spec.packages,
                &obj.spec.settings,
            ),
        })
        .collect();

    // Every machine in the namespace, targeted or not: one that left every
    // policy's selector still carries the verdict it was last given, and only
    // a policy pass can reset it.
    let now = cfgd_core::utc_now_iso8601();
    for mc in &namespace_mcs {
        let existing = mc
            .status
            .as_ref()
            .map(|s| s.conditions.as_slice())
            .unwrap_or(&[]);
        let Some(condition) = shared_compliant_condition(mc, &in_force, &now) else {
            continue;
        };
        let mc_name = mc.name_any();
        let mc_status_patch = serde_json::json!({
            "status": {
                "conditions": upsert_condition(existing, condition)
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
/// Every version the device reported for `package`, whichever manager holds it.
///
/// A policy names a package the way a person does — `kubectl` — while the
/// device reports it under the `<manager>/<package>` id
/// `cfgd_core::state::package_resource_id` composes, because two managers may
/// hold the same name at different versions. The ONE fold between the two
/// spellings, so a version requirement cannot silently fail to find the version
/// the machine reported. An exact key still wins on its own, which is what a row
/// written before the manager qualified the key reads as.
///
/// Several managers holding one package answer with ALL of their versions, and
/// the requirement must hold for every one: `PackageRef.version` is an arbitrary
/// semver requirement, not a floor, so a `<2.0` pin satisfied by the 1.x copy
/// while a 3.x copy sits beside it would call the machine compliant on exactly
/// the package the policy exists to forbid. Empty means no manager reported the
/// package at all.
fn reported_package_versions<'a>(status: &'a MachineConfigStatus, package: &str) -> Vec<&'a str> {
    if let Some(exact) = status.package_versions.get(package) {
        return vec![exact.as_str()];
    }
    status
        .package_versions
        .iter()
        .filter(|(id, _)| {
            cfgd_core::state::split_package_resource_id(id).is_some_and(|(_, name)| name == package)
        })
        .map(|(_, version)| version.as_str())
        .collect()
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
            let reported = status.map(|s| reported_package_versions(s, &pkg.name));
            // A machine that reported no version for a pinned package is not
            // compliant with the pin: nothing stands behind the claim.
            match reported.as_deref() {
                Some([]) | None => return false,
                Some(versions) => {
                    if !versions.iter().all(|v| version_satisfies(v, req_str)) {
                        return false;
                    }
                }
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
/// Recompute the `Compliant` condition on every machine in `names`, the ones
/// carrying a verdict when a policy is deleted, from the policies that
/// `remaining` holds.
///
/// The policy controller owns that condition, so its deletion is the only event
/// that can retire the verdict: the machine controller carries whatever is there
/// forward verbatim, and asking it to look up whether the naming policy still
/// exists would put a policy read back into the machine reconcile. A machine
/// another policy still targets gets the verdict that policy's own pass would
/// write. A machine no remaining policy targets gets the same triple the
/// machine controller synthesizes for a machine no policy has evaluated.
///
/// Best effort per machine: a failure here must not block the finalizer, or a
/// deleted policy is stuck forever on one unreachable machine.
async fn clear_compliant_verdicts(
    machines: &Api<MachineConfig>,
    names: &[String],
    remaining: &[Arc<ConfigPolicy>],
) {
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
        let Some(cleared) = shared_compliant_condition(&live, remaining, &now) else {
            continue;
        };
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

/// The `Compliant` condition `(status, reason, message)` for `mc` under every
/// policy in `policies` whose `targetSelector` matches it, or `None` when none
/// does.
///
/// A violation names the violated policies; otherwise the condition names every
/// policy that judged the machine. Names are sorted, so each policy's pass over
/// the same machine and the same policies produces the same text.
fn compliant_verdict(
    mc: &MachineConfig,
    policies: &[Arc<ConfigPolicy>],
) -> Option<(&'static str, &'static str, String)> {
    let mut judged: Vec<(String, bool)> = policies
        .iter()
        .filter(|p| matches_selector(mc.metadata.labels.as_ref(), &p.spec.target_selector))
        .map(|p| {
            let compliant = validate_policy_compliance(
                &mc.spec,
                mc.status.as_ref(),
                &p.spec.required_modules,
                &p.spec.packages,
                &p.spec.settings,
            );
            (p.name_any(), compliant)
        })
        .collect();
    if judged.is_empty() {
        return None;
    }
    judged.sort();
    let violated: Vec<&str> = judged
        .iter()
        .filter(|(_, compliant)| !compliant)
        .map(|(name, _)| name.as_str())
        .collect();
    Some(if violated.is_empty() {
        let all: Vec<&str> = judged.iter().map(|(name, _)| name.as_str()).collect();
        (
            "True",
            "PolicyCompliant",
            format!("Compliant with {}", naming_policies(&all)),
        )
    } else {
        (
            "False",
            "PolicyViolation",
            format!("Violates {}", naming_policies(&violated)),
        )
    })
}

/// The `Compliant` condition every policy pass writes onto `mc`, or `None`
/// when the machine already carries it.
///
/// A machine no policy in `in_force` targets is reset to the triple the
/// machine controller synthesizes for a machine no policy has evaluated, and
/// only when it carries a verdict to reset: writing one onto a machine that
/// never had one would announce an evaluation that never happened. The
/// condition is rebuilt from the machine's own conditions, so an unchanged
/// verdict compares equal and costs no write.
fn shared_compliant_condition(
    mc: &MachineConfig,
    in_force: &[Arc<ConfigPolicy>],
    now: &str,
) -> Option<Condition> {
    let existing = mc
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);
    let (status, reason, message) = match compliant_verdict(mc, in_force) {
        Some(verdict) => verdict,
        None if has_compliant_condition(mc) => (
            "Unknown",
            "NotEvaluated",
            "Awaiting policy evaluation".to_string(),
        ),
        None => return None,
    };
    let condition = build_condition(
        existing,
        "Compliant",
        status,
        reason,
        &message,
        now,
        mc.meta().generation,
    );
    (!existing.contains(&condition)).then_some(condition)
}

fn has_compliant_condition(mc: &MachineConfig) -> bool {
    mc.status
        .as_ref()
        .is_some_and(|s| super::find_condition(&s.conditions, "Compliant").is_some())
}

fn naming_policies(names: &[&str]) -> String {
    match names {
        [one] => format!("policy {one}"),
        _ => format!("policies {}", names.join(", ")),
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

use std::sync::Arc;

use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Resource, ResourceExt};
use tracing::info;

use crate::crds::{ClusterConfigPolicy, ClusterConfigPolicyStatus, ConfigPolicySpec};
use crate::errors::OperatorError;
use crate::metrics::PolicyLabels;

use super::config_policy::{
    MachineVerdict, emit_policy_evaluation_events, evaluate_policy_compliance,
    merge_policy_requirements, validate_policy_compliance,
};
use super::{
    CLUSTER_CONFIG_POLICY_FINALIZER, ControllerContext, FIELD_MANAGER_STATUS, add_finalizer,
    build_condition, compliance_summary, matches_selector, record_reconcile_success,
    remove_finalizer, sort_and_cap_machines,
};

// This controller writes nothing onto the machines it evaluates — its verdict
// lives entirely in its own status, which the API server removes with the
// object. But its `devices_compliant` gauge series is a process-global side
// effect the API server knows nothing about: without a guaranteed last
// reconcile to remove it from, a deleted policy exports its last count for the
// rest of the process's life. The finalizer below buys that last reconcile, at
// the same cost `config_policy.rs` already accepts: a deletion can hang while
// the operator is down.
pub(super) async fn reconcile_cluster_config_policy(
    obj: Arc<ClusterConfigPolicy>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();

    let finalizers = obj.metadata.finalizers.as_deref().unwrap_or(&[]);
    let has_finalizer = finalizers
        .iter()
        .any(|f| f == CLUSTER_CONFIG_POLICY_FINALIZER);
    let ccp_api: Api<ClusterConfigPolicy> = Api::all(ctx.client.clone());

    if obj.metadata.deletion_timestamp.is_some() {
        if has_finalizer {
            info!(name = %name, "clusterConfigPolicy being deleted, removing its devices_compliant series");
            // The gauge loses its only writer with this reconcile, so an
            // unremoved series would export the deleted policy's last count
            // for the life of the process.
            ctx.metrics.devices_compliant.remove(&PolicyLabels {
                policy: name.clone(),
                namespace: String::new(), // cluster-scoped
            });
            remove_finalizer(&ccp_api, &name, finalizers, CLUSTER_CONFIG_POLICY_FINALIZER).await?;
        }
        record_reconcile_success(&ctx, "cluster_config_policy", start);
        return Ok(Action::await_change());
    }

    if !has_finalizer {
        // The gauge's write path only runs from a normal reconcile; only a
        // finalizer guarantees a last one to remove it in.
        add_finalizer(&ccp_api, &name, finalizers, CLUSTER_CONFIG_POLICY_FINALIZER).await?;
        info!(name = %name, "added finalizer to ClusterConfigPolicy");
    }

    info!(
        name = %name,
        required_modules = obj.spec.required_modules.len(),
        packages = obj.spec.packages.len(),
        settings = obj.spec.settings.len(),
        "reconciling ClusterConfigPolicy"
    );

    // Namespaces come from the watch cache, filtered by namespace_selector.
    let matching_namespaces: Vec<String> = ctx
        .stores
        .all_namespaces()
        .await?
        .into_iter()
        .filter(|ns| matches_selector(ns.metadata.labels.as_ref(), &obj.spec.namespace_selector))
        .filter_map(|ns| ns.metadata.name.clone())
        .collect();

    let already_reported = obj
        .status
        .as_ref()
        .map(|s| s.non_compliant_machines.as_slice())
        .unwrap_or(&[]);

    let mut compliant_count: u32 = 0;
    // Accumulated from each namespace's own exact tally, never re-derived from
    // the concatenated list: every per-namespace list arrives already capped, so
    // counting the concatenation would understate a cluster whose violators
    // exceed the cap and contradict the exactness the CRD schema promises.
    let mut non_compliant_count: u32 = 0;
    let mut non_compliant_machines: Vec<String> = Vec::new();

    for ns_name in &matching_namespaces {
        let machines = ctx.stores.machine_configs_in(ns_name).await?;

        // ALL namespace-scoped ConfigPolicies take part in the merge — a
        // label-filtered read would silently drop policies from the result.
        let ns_policies = ctx.stores.config_policies_in(ns_name).await?;
        let ns_policy_specs: Vec<&ConfigPolicySpec> =
            ns_policies.iter().map(|cp| &cp.spec).collect();
        let merged = merge_policy_requirements(&obj.spec, &ns_policy_specs);

        let verdicts: Vec<MachineVerdict<'_>> = machines
            .iter()
            .map(|mc| MachineVerdict {
                machine: mc,
                compliant: validate_policy_compliance(
                    &mc.spec,
                    mc.status.as_ref(),
                    &merged.modules,
                    &merged.packages,
                    &merged.settings,
                ),
            })
            .collect();

        let tally = evaluate_policy_compliance(&ctx, &verdicts, &name, already_reported).await;
        compliant_count = compliant_count.saturating_add(tally.compliant_count);
        non_compliant_count = non_compliant_count.saturating_add(tally.non_compliant_count);
        non_compliant_machines.extend(tally.non_compliant_machines);
    }

    // Only the enumerated list is bounded; the count above is the exact total.
    // Capping again after the concatenation loses nothing a global cap would
    // have kept: a key is `namespace/name`, so one namespace's entries are
    // contiguous in the sorted union and its own smallest 500 are the only ones
    // that could fall inside a cluster-wide first 500.
    sort_and_cap_machines(&mut non_compliant_machines);

    let now = cfgd_core::utc_now_iso8601();
    let overall_status = if non_compliant_count == 0 {
        "True"
    } else {
        "False"
    };

    let existing_conditions = obj
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);

    let desired = ClusterConfigPolicyStatus {
        compliant_count,
        non_compliant_count,
        non_compliant_machines,
        conditions: vec![build_condition(
            existing_conditions,
            "Enforced",
            overall_status,
            if non_compliant_count == 0 {
                "AllCompliant"
            } else {
                "NonCompliantTargets"
            },
            &compliance_summary(compliant_count, non_compliant_count),
            &now,
            obj.meta().generation,
        )],
    };

    if obj.status.as_ref() != Some(&desired) {
        ccp_api
            .patch_status(
                &name,
                &PatchParams::apply(FIELD_MANAGER_STATUS),
                &Patch::Merge(serde_json::json!({ "status": desired })),
            )
            .await
            .map_err(|e| {
                OperatorError::Reconciliation(format!(
                    "failed to update ClusterConfigPolicy status for {name}: {e}"
                ))
            })?;

        info!(
            name = %name,
            compliant = compliant_count,
            non_compliant = non_compliant_count,
            "clusterConfigPolicy status updated"
        );

        emit_policy_evaluation_events(
            &ctx,
            &obj.object_ref(&()),
            compliant_count,
            non_compliant_count,
        )
        .await;
    }

    ctx.metrics
        .devices_compliant
        .get_or_create(&PolicyLabels {
            policy: name.clone(),
            namespace: String::new(), // cluster-scoped
        })
        .set(i64::from(compliant_count));

    record_reconcile_success(&ctx, "cluster_config_policy", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

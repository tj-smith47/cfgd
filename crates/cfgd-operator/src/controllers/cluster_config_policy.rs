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
    ControllerContext, FIELD_MANAGER_STATUS, build_condition, compliance_summary, matches_selector,
    record_reconcile_success, sort_and_cap_machines,
};
pub(super) async fn reconcile_cluster_config_policy(
    obj: Arc<ClusterConfigPolicy>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();

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
        compliant_count += tally.compliant_count;
        non_compliant_machines.extend(tally.non_compliant_machines);
    }

    // Exact total first; only the enumerated list beside it is bounded.
    let non_compliant_count = u32::try_from(non_compliant_machines.len()).unwrap_or(u32::MAX);
    sort_and_cap_machines(&mut non_compliant_machines);

    let now = cfgd_core::utc_now_iso8601();
    let overall_status = if non_compliant_count == 0 {
        "True"
    } else {
        "False"
    };

    let ccp_api: Api<ClusterConfigPolicy> = Api::all(ctx.client.clone());

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

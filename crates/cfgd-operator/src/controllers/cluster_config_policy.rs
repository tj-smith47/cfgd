use std::sync::Arc;

use k8s_openapi::api::core::v1::Namespace;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Resource, ResourceExt};
use tracing::info;

use crate::crds::{
    ClusterConfigPolicy, ClusterConfigPolicyStatus, ConfigPolicy, ConfigPolicySpec, MachineConfig,
};
use crate::errors::OperatorError;
use crate::metrics::PolicyLabels;

use super::config_policy::{
    emit_policy_evaluation_events, evaluate_policy_compliance, merge_policy_requirements,
};
use super::{
    ControllerContext, FIELD_MANAGER_STATUS, build_condition, compliance_summary, matches_selector,
    record_reconcile_success,
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

    // List all namespaces, filtering by namespace_selector if non-empty
    let ns_api: Api<Namespace> = Api::all(ctx.client.clone());
    let ns_list = ns_api
        .list(&ListParams::default())
        .await
        .map_err(|e| OperatorError::Reconciliation(format!("failed to list namespaces: {e}")))?;

    let matching_namespaces: Vec<String> = ns_list
        .items
        .iter()
        .filter(|ns| {
            let ns_labels = ns.metadata.labels.as_ref();
            matches_selector(ns_labels, &obj.spec.namespace_selector)
        })
        .filter_map(|ns| ns.metadata.name.clone())
        .collect();

    let mut compliant_count: u32 = 0;
    let mut non_compliant_count: u32 = 0;

    for ns_name in &matching_namespaces {
        let machines: Api<MachineConfig> = Api::namespaced(ctx.client.clone(), ns_name);
        let mc_list = machines.list(&ListParams::default()).await.map_err(|e| {
            OperatorError::Reconciliation(format!(
                "failed to list MachineConfigs in namespace {ns_name}: {e}"
            ))
        })?;

        // List ALL namespace-scoped ConfigPolicies for merging (C5 fix)
        let ns_policies_api: Api<ConfigPolicy> = Api::namespaced(ctx.client.clone(), ns_name);
        let cp_list = ns_policies_api
            .list(&ListParams::default())
            .await
            .map_err(|e| {
                OperatorError::Reconciliation(format!(
                    "failed to list ConfigPolicies in namespace {ns_name}: {e}"
                ))
            })?;

        let ns_policy_specs: Vec<&ConfigPolicySpec> =
            cp_list.items.iter().map(|cp| &cp.spec).collect();
        let merged = merge_policy_requirements(&obj.spec, &ns_policy_specs);

        let (c, nc) = evaluate_policy_compliance(
            &ctx,
            &mc_list.items,
            &merged.packages,
            &merged.modules,
            &merged.settings,
            &name,
        )
        .await;
        compliant_count += c;
        non_compliant_count += nc;
    }

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

    let status = serde_json::json!({
        "status": ClusterConfigPolicyStatus {
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

    ccp_api
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER_STATUS),
            &Patch::Merge(status),
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

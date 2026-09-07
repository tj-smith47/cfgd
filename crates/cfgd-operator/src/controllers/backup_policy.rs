use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;

use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Resource, ResourceExt};
use tracing::info;

use crate::crds::{
    BackupPolicy, BackupPolicyStatus, BackupPolicyUnitStatus, MAX_NON_COMPLIANT_MACHINES,
    MachineConfig, ScheduleOwner,
};
use crate::errors::OperatorError;

use super::{
    ControllerContext, FIELD_MANAGER_STATUS, build_condition, emit_event, matches_selector,
    namespaced_api, record_reconcile_success,
};

/// Why a row reports a unit instead of scheduling it. The machine's own profile
/// pinned the unit with `scheduleOwner: Local`, and a policy that stayed silent
/// about it would look like one that had applied.
const LOCALLY_PINNED: &str =
    "the machine pins this unit's schedule; this policy reports it and does not apply";

pub(super) async fn reconcile_backup_policy(
    obj: Arc<BackupPolicy>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    let start = std::time::Instant::now();
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();

    if obj.metadata.deletion_timestamp.is_some() {
        // Nothing to retire, so nothing to buy a last reconcile for: this
        // controller writes only its own status, and a deleted policy takes
        // that with it. The machine's schedules are the agent's to unwind.
        record_reconcile_success(&ctx, "backup_policy", start);
        return Ok(Action::await_change());
    }

    info!(
        name = %name,
        namespace = %namespace,
        units = obj.spec.units.len(),
        "reconciling BackupPolicy"
    );

    if let Err(errors) = obj.spec.validate() {
        let detail = errors.join("; ");
        emit_event(
            &ctx.recorder,
            &obj.object_ref(&()),
            EventType::Warning,
            "ReconcileError",
            format!("Reconciliation failed for {name}: {detail}"),
            "Reconcile",
        )
        .await;
        return Err(OperatorError::Reconciliation(format!(
            "BackupPolicy {name} is invalid: {detail}"
        )));
    }

    let machines = ctx.stores.machine_configs_in(&namespace).await?;
    let matched: Vec<&Arc<MachineConfig>> = machines
        .iter()
        .filter(|mc| matches_selector(mc.metadata.labels.as_ref(), &obj.spec.selector))
        .collect();
    let machines_matched = u32::try_from(matched.len()).unwrap_or(u32::MAX);

    // `status.units` is a structural-merge map keyed (hostname, name), so two
    // MachineConfigs naming one hostname describe one machine and collapse into
    // one row — the device is the identity, as it is for the agent, which
    // reconciles the MachineConfig whose hostname matches its own. A pin
    // reported by either object wins the collapse: claiming `cluster` for a
    // machine that pinned the unit is exactly the silent apply this controller
    // must never report. The map is keyed in the order the rows sort in, so
    // draining it needs no second sort.
    let mut rows: BTreeMap<(&str, &str), BackupPolicyUnitStatus> = BTreeMap::new();
    for machine in &matched {
        let pinned = machine.status.as_ref().map(|s| &s.backup_schedule_owners);
        for unit in &obj.spec.units {
            let locally_pinned = pinned
                .and_then(|owners| owners.get(&unit.name))
                .is_some_and(|owner| owner == ScheduleOwner::Local.label());
            // `lastRun` / `nextRun` stay absent: no device reports them, and a
            // guessed time reads as an observation.
            let row = if locally_pinned {
                BackupPolicyUnitStatus {
                    name: unit.name.clone(),
                    hostname: machine.spec.hostname.clone(),
                    owner: ScheduleOwner::Local.label().to_string(),
                    schedule: None,
                    retention: None,
                    last_run: None,
                    next_run: None,
                    message: Some(LOCALLY_PINNED.to_string()),
                }
            } else {
                BackupPolicyUnitStatus {
                    name: unit.name.clone(),
                    hostname: machine.spec.hostname.clone(),
                    owner: ScheduleOwner::Cluster.label().to_string(),
                    schedule: Some(unit.schedule.clone()),
                    retention: unit.retention,
                    last_run: None,
                    next_run: None,
                    message: None,
                }
            };
            match rows.entry((machine.spec.hostname.as_str(), unit.name.as_str())) {
                Entry::Vacant(slot) => {
                    slot.insert(row);
                }
                Entry::Occupied(mut slot) if locally_pinned => {
                    slot.insert(row);
                }
                Entry::Occupied(_) => {}
            }
        }
    }

    let locally_pinned_rows = rows
        .values()
        .filter(|row| row.owner == ScheduleOwner::Local.label())
        .count();
    let mut units: Vec<BackupPolicyUnitStatus> = rows.into_values().collect();
    let scheduled_rows = units.len() - locally_pinned_rows;
    units.truncate(MAX_NON_COMPLIANT_MACHINES);

    let (status, reason, message) = if matched.is_empty() {
        (
            "False",
            "NoMatchingMachines",
            format!("No MachineConfig in {namespace} matches this policy's selector"),
        )
    } else {
        let mut message = format!(
            "{} scheduled across {}",
            cfgd_core::pluralize(scheduled_rows, "backup unit"),
            cfgd_core::pluralize(matched.len(), "machine")
        );
        if locally_pinned_rows > 0 {
            message.push_str(&format!(
                ", {} pinned by the machine",
                cfgd_core::pluralize(locally_pinned_rows, "unit")
            ));
        }
        ("True", "Projected", message)
    };

    let now = cfgd_core::utc_now_iso8601();
    let existing_conditions = obj
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);
    let desired = BackupPolicyStatus {
        observed_generation: obj.meta().generation,
        units_summary: BackupPolicyStatus::summarize_units(&units),
        units,
        machines_matched,
        conditions: vec![build_condition(
            existing_conditions,
            "Applied",
            status,
            reason,
            &message,
            &now,
            obj.meta().generation,
        )],
    };

    // `build_condition` carries the existing lastTransitionTime forward while
    // the condition's status holds, so an unchanged projection compares equal
    // and writes nothing — the alternative is one etcd write and one watch
    // fan-out per policy per requeue, forever.
    if obj.status.as_ref() != Some(&desired) {
        let policies: Api<BackupPolicy> = namespaced_api(&ctx.client, &namespace)?;
        policies
            .patch_status(
                &name,
                &PatchParams::apply(FIELD_MANAGER_STATUS),
                &Patch::Merge(serde_json::json!({ "status": desired })),
            )
            .await
            .map_err(|e| {
                OperatorError::Reconciliation(format!(
                    "failed to update BackupPolicy status for {name}: {e}"
                ))
            })?;
        info!(
            name = %name,
            machines_matched = machines_matched,
            "backupPolicy status updated"
        );
    }

    record_reconcile_success(&ctx, "backup_policy", start);

    Ok(Action::requeue(std::time::Duration::from_secs(60)))
}

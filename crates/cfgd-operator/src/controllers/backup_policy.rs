use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::EventType;
use kube::{Resource, ResourceExt};
use tracing::info;

use crate::crds::{
    BackupPolicy, BackupPolicyStatus, BackupPolicyUnit, BackupPolicyUnitStatus,
    MAX_NON_COMPLIANT_MACHINES, MachineConfig, ScheduleOwner,
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

/// The same for a machine whose reported owner is a word no layer spells: the
/// policy cannot tell whether the machine took its schedule, so it reports the
/// unit rather than claiming to have scheduled it.
fn unreadable_owner_message(word: &str) -> String {
    format!(
        "the machine reported an unreadable schedule owner {word:?}; this policy does not apply"
    )
}

/// A row for a unit this policy schedules on `hostname`.
fn scheduled_row(unit: &BackupPolicyUnit, hostname: &str) -> BackupPolicyUnitStatus {
    BackupPolicyUnitStatus {
        name: unit.name.clone(),
        hostname: hostname.to_string(),
        owner: ScheduleOwner::Cluster.label().to_string(),
        schedule: Some(unit.schedule.clone()),
        retention: unit.retention,
        // `lastRun` / `nextRun` stay absent: no device reports them, and a
        // guessed time reads as an observation.
        last_run: None,
        next_run: None,
        message: None,
    }
}

/// A row for a unit `hostname` owns itself, carrying the reason the policy
/// declined to schedule it. It states no schedule and no retention: both would
/// be values this policy did not put on the machine.
fn reported_row(unit: &BackupPolicyUnit, hostname: &str, why: String) -> BackupPolicyUnitStatus {
    BackupPolicyUnitStatus {
        name: unit.name.clone(),
        hostname: hostname.to_string(),
        owner: ScheduleOwner::Local.label().to_string(),
        schedule: None,
        retention: None,
        last_run: None,
        next_run: None,
        message: Some(why),
    }
}

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
    // reconciles the MachineConfig whose hostname matches its own. A row that
    // reports rather than schedules wins the collapse, whichever object it came
    // from: claiming `cluster` for a machine that pinned the unit is exactly
    // the silent apply this controller must never report. The map is keyed in
    // the order the rows sort in, so draining it needs no second sort.
    let mut rows: BTreeMap<(&str, &str), BackupPolicyUnitStatus> = BTreeMap::new();
    // The machine whose reported owner no layer spells, and the first unit it
    // reported one for: one machine is enough to name in the event, and the
    // rest are a count.
    let mut unreadable: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for machine in &matched {
        let owners = machine.status.as_ref().map(|s| &s.backup_schedule_owners);
        let hostname = machine.spec.hostname.as_str();
        for unit in &obj.spec.units {
            // The map is device-reported and carries plain strings, so the word
            // is read through `ScheduleOwner`'s own case-insensitive parser
            // rather than compared against one spelling of it. A word no layer
            // spells reports the unit too: claiming `cluster` for a machine
            // whose answer this policy could not read is the silent apply the
            // whole surface exists to prevent.
            let reported = owners
                .and_then(|owners| owners.get(&unit.name))
                .map(|word| (word.as_str(), word.parse::<ScheduleOwner>()));
            let row = match reported {
                Some((_, Ok(ScheduleOwner::Local))) => {
                    reported_row(unit, hostname, LOCALLY_PINNED.to_string())
                }
                Some((word, Err(_))) => {
                    unreadable
                        .entry(hostname)
                        .or_insert((unit.name.as_str(), word));
                    reported_row(unit, hostname, unreadable_owner_message(word))
                }
                Some((_, Ok(ScheduleOwner::Cluster))) | None => scheduled_row(unit, hostname),
            };
            let reports_only = row.message.is_some();
            match rows.entry((hostname, unit.name.as_str())) {
                Entry::Vacant(slot) => {
                    slot.insert(row);
                }
                Entry::Occupied(mut slot) if reports_only => {
                    slot.insert(row);
                }
                Entry::Occupied(_) => {}
            }
        }
    }

    if let Some((hostname, (unit, word))) = unreadable.iter().next() {
        let mut detail = format!(
            "{hostname} reported an unreadable schedule owner {word:?} for backup unit {unit}; this policy does not apply it"
        );
        let others = unreadable.len() - 1;
        if others > 0 {
            detail.push_str(&format!(
                " ({} did the same)",
                cfgd_core::pluralize(others, "other machine")
            ));
        }
        emit_event(
            &ctx.recorder,
            &obj.object_ref(&()),
            EventType::Warning,
            "UnreadableScheduleOwner",
            detail,
            "Reconcile",
        )
        .await;
    }

    // Each clause counts the thing it names: a unit spans one row per machine,
    // so counting rows would call three machines running one unit three units.
    let mut scheduled_units: BTreeSet<&str> = BTreeSet::new();
    let mut scheduled_machines: BTreeSet<&str> = BTreeSet::new();
    let mut reported_units: BTreeSet<&str> = BTreeSet::new();
    let mut reporting_machines: BTreeSet<&str> = BTreeSet::new();
    for (&(hostname, unit), row) in &rows {
        if row.message.is_some() {
            reported_units.insert(unit);
            reporting_machines.insert(hostname);
        } else {
            scheduled_units.insert(unit);
            scheduled_machines.insert(hostname);
        }
    }

    let mut units: Vec<BackupPolicyUnitStatus> = rows.into_values().collect();
    units.truncate(MAX_NON_COMPLIANT_MACHINES);

    let (status, reason, message) = if matched.is_empty() {
        (
            "False",
            "NoMatchingMachines",
            format!("No MachineConfig in {namespace} matches this policy's selector"),
        )
    } else {
        // A clause counting nothing drops rather than reading "0 units pinned".
        let mut clauses: Vec<String> = Vec::with_capacity(2);
        if !scheduled_units.is_empty() {
            clauses.push(format!(
                "{} scheduled across {}",
                cfgd_core::pluralize(scheduled_units.len(), "backup unit"),
                cfgd_core::pluralize(scheduled_machines.len(), "machine")
            ));
        }
        if !reported_units.is_empty() {
            clauses.push(format!(
                "{} pinned on {}",
                cfgd_core::pluralize(reported_units.len(), "unit"),
                cfgd_core::pluralize(reporting_machines.len(), "machine")
            ));
        }
        ("True", "Projected", clauses.join("; "))
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

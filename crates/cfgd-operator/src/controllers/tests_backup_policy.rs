//! Reconcile-fn tests for `controllers/backup_policy.rs`.
#![cfg(test)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kube::runtime::controller::Action;

use super::ControllerStores;
use super::backup_policy::reconcile_backup_policy;
use super::test_fixtures::{backup_policy, backup_unit, machine_config};
use super::test_kube_harness::{
    ExpectedCall, MockKubeHarness, empty_stores, expect_event_post, seeded_store, unready_store,
};
use crate::crds::{
    BackupPolicyStatus, LabelSelector, MAX_NON_COMPLIANT_MACHINES, MachineConfig,
    MachineConfigStatus, ScheduleOwner,
};
use crate::metrics::ReconcileLabels;

const NS: &str = "cfgd-system";

fn backup_policy_status_path(name: &str) -> String {
    format!("/apis/cfgd.io/v1alpha1/namespaces/{NS}/backuppolicies/{name}/status")
}

/// Caches holding exactly these MachineConfigs — the state a reconcile reads
/// instead of listing.
fn stores_with(machine_configs: Vec<MachineConfig>) -> ControllerStores {
    ControllerStores {
        machine_configs: seeded_store(machine_configs),
        ..empty_stores()
    }
}

/// A machine whose device reported `owner` as the layer owning `unit`'s
/// schedule. The value is whatever the device wrote, not a parsed enum: the
/// wire carries a plain string.
fn reporting(mut mc: MachineConfig, unit: &str, owner: &str) -> MachineConfig {
    mc.status = Some(MachineConfigStatus {
        backup_schedule_owners: BTreeMap::from([(unit.to_string(), owner.to_string())]),
        ..Default::default()
    });
    mc
}

/// A machine whose device reported that it owns `unit`'s schedule itself.
fn pinning(mc: MachineConfig, unit: &str) -> MachineConfig {
    reporting(mc, unit, ScheduleOwner::Local.label())
}

/// The status body of the patch the reconcile sent, as the typed object the
/// controller built.
fn patched_status(body: &serde_json::Value) -> BackupPolicyStatus {
    serde_json::from_value(body["status"].clone()).expect("the patch carries a BackupPolicyStatus")
}

#[tokio::test]
async fn reconcile_backup_policy_projects_a_units_schedule_onto_every_selected_machine() {
    let policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "0 3 * * *")]);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .with_query_contains("fieldManager=cfgd-operator%2Fstatus")
                .returning_json(&policy),
        ],
        stores_with(vec![machine_config("mc-a", NS), machine_config("mc-b", NS)]),
    );

    let action = reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("a valid policy over a populated cache reconciles");
    assert_eq!(action, Action::requeue(std::time::Duration::from_secs(60)));

    let report = harness.finish().await;
    let status = patched_status(&report.captured[0].body_json());
    assert_eq!(status.machines_matched, 2);
    assert_eq!(status.units.len(), 2);
    for row in &status.units {
        assert_eq!(row.owner, ScheduleOwner::Cluster.label());
        assert_eq!(row.schedule.as_deref(), Some("0 3 * * *"));
        assert!(row.message.is_none(), "a projected row explains nothing");
    }
    // Sorted by (hostname, name), which the fixture's `<name>.test` hostnames
    // order.
    assert_eq!(status.units[0].hostname, "mc-a.test");
    assert_eq!(status.units[1].hostname, "mc-b.test");
    assert_eq!(status.units_summary.as_deref(), Some("dotfiles"));
    assert_eq!(status.conditions[0].condition_type, "Applied");
    assert_eq!(status.conditions[0].status, "True");
    assert_eq!(status.conditions[0].reason, "Projected");
    // One unit over two machines is two rows and one unit: the clause counts
    // the units it names, not the rows they span.
    assert_eq!(
        status.conditions[0].message,
        "1 backup unit scheduled across 2 machines"
    );
}

/// The guard against the one failure mode a fleet schedule must never have: a
/// machine that pinned the unit locally reads as scheduled by the cluster. The
/// word arrives as a plain string a device wrote, so every casing of it is the
/// same answer.
#[tokio::test]
async fn reconcile_backup_policy_reports_a_locally_pinned_unit_instead_of_claiming_to_apply() {
    let policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "0 3 * * *")]);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .returning_json(&policy),
        ],
        stores_with(vec![
            pinning(machine_config("mc-laptop", NS), "dotfiles"),
            // What serde writes for `spec.backups[].scheduleOwner`, and a
            // shouted spelling of it: the same pin either way.
            reporting(
                machine_config("mc-pascal", NS),
                "dotfiles",
                ScheduleOwner::Local.as_str(),
            ),
            reporting(machine_config("mc-shout", NS), "dotfiles", "LOCAL"),
            machine_config("mc-nuc", NS),
        ]),
    );

    reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("a pinned unit is reported, not an error");

    let report = harness.finish().await;
    let status = patched_status(&report.captured[0].body_json());
    for hostname in ["mc-laptop.test", "mc-pascal.test", "mc-shout.test"] {
        let pinned = status
            .units
            .iter()
            .find(|row| row.hostname == hostname)
            .unwrap_or_else(|| panic!("{hostname} still gets a row"));
        assert_eq!(
            pinned.owner,
            ScheduleOwner::Local.label(),
            "{hostname} pinned the unit whatever the casing it reported"
        );
        assert!(
            pinned.schedule.is_none(),
            "a pinned row states no schedule this policy did not set"
        );
        assert!(pinned.retention.is_none());
        let message = pinned.message.as_deref().expect("a pinned row says why");
        assert!(
            message.contains("pins this unit's schedule") && message.contains("does not apply"),
            "the message must name the pin and the declined apply: {message}"
        );
    }

    let projected = status
        .units
        .iter()
        .find(|row| row.hostname == "mc-nuc.test")
        .expect("the unpinned machine is still scheduled");
    assert_eq!(projected.owner, ScheduleOwner::Cluster.label());
    assert_eq!(projected.schedule.as_deref(), Some("0 3 * * *"));

    // The condition counts what was scheduled apart from what the machines
    // pinned, so a reader learns the split without walking `status.units`.
    assert_eq!(
        status.conditions[0].message,
        "1 backup unit scheduled across 1 machine; 1 unit pinned on 3 machines"
    );
}

/// A schedule owner no layer spells is the one case where the controller
/// cannot tell what the machine did, so it takes the only safe reading: report
/// the unit, apply nothing, and say so on the object as well as in the row.
#[tokio::test]
async fn reconcile_backup_policy_reports_a_machine_whose_schedule_owner_it_cannot_read() {
    let policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "0 3 * * *")]);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            expect_event_post(NS),
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .returning_json(&policy),
        ],
        stores_with(vec![
            reporting(machine_config("mc-laptop", NS), "dotfiles", "Locale"),
            machine_config("mc-nuc", NS),
        ]),
    );

    reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("an unreadable owner is reported, not an error");

    let report = harness.finish().await;
    let event = report.captured[0].body_json();
    let event_note = serde_json::to_string(&event).expect("the event serializes");
    assert!(
        event_note.contains("UnreadableScheduleOwner")
            && event_note.contains("mc-laptop.test")
            && event_note.contains("Locale"),
        "the warning names the machine and the word it could not read: {event_note}"
    );

    let status = patched_status(&report.captured[1].body_json());
    let unreadable = status
        .units
        .iter()
        .find(|row| row.hostname == "mc-laptop.test")
        .expect("the machine still gets a row");
    assert_eq!(
        unreadable.owner,
        ScheduleOwner::Local.label(),
        "a word this policy cannot read never reads as its own schedule applying"
    );
    assert!(unreadable.schedule.is_none());
    let message = unreadable.message.as_deref().expect("the row says why");
    assert!(
        message.contains("unreadable schedule owner")
            && message.contains("Locale")
            && message.contains("does not apply"),
        "the row names the value it could not read: {message}"
    );
    assert_eq!(
        status.conditions[0].message,
        "1 backup unit scheduled across 1 machine; 1 unit pinned on 1 machine"
    );
}

/// A unit spans one row per machine, so a message counting rows would call
/// three machines running one unit three units. Three machines, two units, one
/// of them pinned everywhere: six rows, and not one count in the message is a
/// six.
#[tokio::test]
async fn reconcile_backup_policy_counts_distinct_units_rather_than_projection_rows() {
    let policy = backup_policy(
        "nightly",
        NS,
        vec![
            backup_unit("dotfiles", "0 3 * * *"),
            backup_unit("notes", "6h"),
        ],
    );

    let machines: Vec<MachineConfig> = ["mc-a", "mc-b", "mc-c"]
        .into_iter()
        .map(|name| pinning(machine_config(name, NS), "dotfiles"))
        .collect();

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .returning_json(&policy),
        ],
        stores_with(machines),
    );

    reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("three machines reconcile");

    let report = harness.finish().await;
    let status = patched_status(&report.captured[0].body_json());
    assert_eq!(status.units.len(), 6, "three machines, two units each");
    assert_eq!(
        status.conditions[0].message,
        "1 backup unit scheduled across 3 machines; 1 unit pinned on 3 machines"
    );
}

/// `status.units` carries `maxItems: 500`, so the enumeration is what the cap
/// bounds and the matched count stays exact above it. Rows are machines times
/// units, so the cap is reachable at half that many machines.
#[tokio::test]
async fn reconcile_backup_policy_caps_the_unit_rows_but_not_the_matched_count() {
    let policy = backup_policy(
        "nightly",
        NS,
        vec![
            backup_unit("dotfiles", "0 3 * * *"),
            backup_unit("notes", "6h"),
        ],
    );

    let over_cap = MAX_NON_COMPLIANT_MACHINES / 2 + 1;
    let machines: Vec<MachineConfig> = (0..over_cap)
        .map(|i| machine_config(&format!("mc-{i:04}"), NS))
        .collect();

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .returning_json(&policy),
        ],
        stores_with(machines),
    );

    reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("a fleet over the cap reconciles");

    let report = harness.finish().await;
    let status = patched_status(&report.captured[0].body_json());
    assert_eq!(
        status.machines_matched,
        u32::try_from(over_cap).expect("the fixture fleet fits a u32"),
        "the matched count is the exact total and is never capped"
    );
    assert_eq!(
        status.units.len(),
        MAX_NON_COMPLIANT_MACHINES,
        "the enumeration is bounded at the cap"
    );
    assert_eq!(
        status.units[0].hostname, "mc-0000.test",
        "truncation follows the sort, so which rows fall outside is deterministic"
    );
    assert_eq!(
        status.conditions[0].message,
        format!("2 backup units scheduled across {over_cap} machines"),
        "the condition counts the whole fleet, not the truncated enumeration"
    );
}

#[tokio::test]
async fn reconcile_backup_policy_skips_a_machine_the_selector_does_not_match() {
    let mut policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "6h")]);
    policy.spec.selector = LabelSelector {
        match_labels: BTreeMap::from([("cfgd.io/profile".to_string(), "workstation".to_string())]),
        ..Default::default()
    };

    let mut selected = machine_config("mc-work", NS);
    selected.metadata.labels = Some(BTreeMap::from([(
        "cfgd.io/profile".to_string(),
        "workstation".to_string(),
    )]));

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .returning_json(&policy),
        ],
        stores_with(vec![selected, machine_config("mc-server", NS)]),
    );

    reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("a selector matching one machine reconciles");

    let report = harness.finish().await;
    let status = patched_status(&report.captured[0].body_json());
    assert_eq!(status.machines_matched, 1);
    assert_eq!(status.units.len(), 1);
    assert_eq!(status.units[0].hostname, "mc-work.test");
}

/// A unit the policy leaves unretained reports no retention at all: the row is
/// what the machine is asked to run, and a `0` there would read as a retention
/// the policy set rather than as the profile's own value standing.
#[tokio::test]
async fn reconcile_backup_policy_carries_an_absent_retention_rather_than_zero() {
    let policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "6h")]);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .returning_json(&policy),
        ],
        stores_with(vec![machine_config("mc-a", NS)]),
    );

    reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("a policy without a retention reconciles");

    let report = harness.finish().await;
    let body = report.captured[0].body_json();
    assert!(
        body["status"]["units"][0].get("retention").is_none(),
        "an unset retention must not be serialized at all: {}",
        body["status"]["units"][0]
    );
    assert!(patched_status(&body).units[0].retention.is_none());
}

/// Two MachineConfigs naming one hostname describe one machine, and
/// `status.units` is a merge map keyed (hostname, name) — two rows under one
/// key is not a shape the API server can hold.
#[tokio::test]
async fn two_machine_configs_naming_one_hostname_collapse_into_one_unit_row() {
    let policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "6h")]);

    let mut first = machine_config("mc-a", NS);
    first.spec.hostname = "nuc-01".to_string();
    let mut second = machine_config("mc-b", NS);
    second.spec.hostname = "nuc-01".to_string();

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .returning_json(&policy),
        ],
        // The second object is the one carrying the pin, so the collapse is
        // also proven to keep the pin rather than the first row it saw.
        stores_with(vec![first, pinning(second, "dotfiles")]),
    );

    reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("two objects for one machine reconcile");

    let report = harness.finish().await;
    let status = patched_status(&report.captured[0].body_json());
    assert_eq!(status.units.len(), 1, "one machine, one unit, one row");
    assert_eq!(status.units[0].hostname, "nuc-01");
    assert_eq!(status.units[0].owner, ScheduleOwner::Local.label());
    assert_eq!(
        status.machines_matched, 2,
        "the matched count stays exact even where the rows collapse"
    );
}

/// Steady state: the second pass over an unchanged machine set recomputes a
/// byte-identical status and must not write it. `build_condition` carries the
/// existing `lastTransitionTime` forward, which is what makes the comparison
/// hold across the two reconciles.
#[tokio::test]
async fn reconcile_backup_policy_writes_nothing_when_the_projection_is_unchanged() {
    let policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "0 3 * * *")]);
    let machines = vec![machine_config("mc-a", NS)];

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .returning_json(&policy),
        ],
        stores_with(machines.clone()),
    );
    reconcile_backup_policy(Arc::new(policy.clone()), ctx)
        .await
        .expect("the first pass writes the projection");
    let report = harness.finish().await;
    let written = patched_status(&report.captured[0].body_json());

    // The policy as the API server now holds it: what the first pass wrote.
    let mut settled = policy;
    settled.status = Some(written);

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(vec![], stores_with(machines));
    reconcile_backup_policy(Arc::new(settled), ctx)
        .await
        .expect("the second pass reconciles");

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "an unchanged projection must write nothing"
    );
}

#[tokio::test(start_paused = true)]
async fn reconcile_backup_policy_when_the_machine_config_cache_is_unpopulated_returns_error() {
    let policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "6h")]);

    let (machine_configs, _writer) = unready_store();
    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![],
        ControllerStores {
            machine_configs,
            ..empty_stores()
        },
    );

    let err = reconcile_backup_policy(Arc::new(policy), ctx.clone())
        .await
        .expect_err("an unpopulated cache must propagate");
    let msg = err.to_string();
    assert!(msg.contains("MachineConfig watch cache"), "{msg}");

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "nothing may be written from an unpopulated cache"
    );

    let successes = ctx
        .metrics
        .reconciliations_total
        .get_or_create(&ReconcileLabels {
            controller: "backup_policy".to_string(),
            result: "success".to_string(),
        })
        .get();
    assert_eq!(successes, 0, "a requeued reconcile is not a success");
}

#[tokio::test]
async fn reconcile_backup_policy_marks_a_selector_matching_nothing_as_not_applied() {
    let mut policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "6h")]);
    policy.spec.selector = LabelSelector {
        match_labels: BTreeMap::from([("cfgd.io/profile".to_string(), "nowhere".to_string())]),
        ..Default::default()
    };

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![
            ExpectedCall::patch_status(backup_policy_status_path("nightly"))
                .returning_json(&policy),
        ],
        stores_with(vec![machine_config("mc-a", NS)]),
    );

    reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("a policy matching nothing is reported, not an error");

    let report = harness.finish().await;
    let status = patched_status(&report.captured[0].body_json());
    assert_eq!(status.machines_matched, 0);
    assert!(status.units.is_empty());
    assert!(
        status.units_summary.is_none(),
        "an empty projection summarizes to nothing, not to an empty string"
    );
    assert_eq!(status.conditions[0].status, "False");
    assert_eq!(status.conditions[0].reason, "NoMatchingMachines");
}

/// An unreadable schedule is refused at the controller too, not only by the
/// admission webhook: a policy that reached etcd before the webhook was
/// installed must not be projected onto a fleet.
#[tokio::test]
async fn reconcile_backup_policy_refuses_a_schedule_no_machine_could_parse() {
    let policy = backup_policy(
        "nightly",
        NS,
        vec![backup_unit("dotfiles", "every so often")],
    );

    let (ctx, _registry, harness) = MockKubeHarness::with_stores(
        vec![expect_event_post(NS)],
        stores_with(vec![machine_config("mc-a", NS)]),
    );

    let err = reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect_err("an unparseable schedule must fail the reconcile");
    let msg = err.to_string();
    assert!(msg.contains("units[0].schedule"), "{msg}");

    let report = harness.finish().await;
    assert_eq!(
        report.captured.len(),
        1,
        "the invalid-spec path emits its event and writes no status"
    );
}

/// The policy writes nothing outside its own status, so a deletion has nothing
/// to retire — no finalizer, no cleanup, and no further reconcile to wait for.
#[tokio::test]
async fn reconcile_backup_policy_awaits_change_on_a_deleted_policy() {
    let mut policy = backup_policy("nightly", NS, vec![backup_unit("dotfiles", "6h")]);
    policy.metadata.deletion_timestamp = Some(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(k8s_openapi::jiff::Timestamp::now()),
    );

    let (ctx, _registry, harness) =
        MockKubeHarness::with_stores(vec![], stores_with(vec![machine_config("mc-a", NS)]));

    let action = reconcile_backup_policy(Arc::new(policy), ctx)
        .await
        .expect("a deleted policy reconciles cleanly");
    assert_eq!(action, Action::await_change());

    let report = harness.finish().await;
    assert!(
        report.captured.is_empty(),
        "a deleted policy writes nothing"
    );
}

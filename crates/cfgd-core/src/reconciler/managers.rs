//! The `cfgd:managers` owner group of the `Prerequisites` phase: one node per
//! package manager the run's desired state depends on, plus the tools cfgd's
//! own bootstrap cascades shell out to, wired into a dependency graph.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::config::MergedProfile;
use crate::modules::ResolvedModule;
use crate::providers::{PackageManager, ProviderRegistry, is_system_manager};

use super::types::{Action, ManagerAction};

/// The manager name a module package carries when its "install" is an inline
/// script rather than a manager command. It names no registry entry.
const SCRIPT_SENTINEL: &str = "script";

/// What the run has to do about one manager.
enum MemberState {
    /// Present already — refresh its index.
    Present,
    /// Absent, provisioned by the method its own cascade resolved to.
    Provision { via: String },
}

impl MemberState {
    /// The DAG id of the node this state mints for `manager`.
    fn node_id(&self, manager: &str) -> String {
        match self {
            MemberState::Present => ManagerAction::refresh_node(manager),
            MemberState::Provision { .. } => ManagerAction::provision_node(manager),
        }
    }
}

/// Everything the graph is built from, accumulated over one closure walk.
#[derive(Default)]
struct Graph {
    /// The managers in the closure and what the run does about each.
    members: BTreeMap<String, MemberState>,
    /// Tool -> the managers that named it. One node serves all of them.
    prerequisites: BTreeMap<String, BTreeSet<String>>,
    /// Manager -> the tools its provision waits on.
    needs: BTreeMap<String, Vec<String>>,
    /// Manager -> the manager its cascade installs through.
    prefers: BTreeMap<String, String>,
}

/// Plan the manager nodes for this run.
///
/// Membership is the effective desired package set — every manager the merged
/// profile or any resolved module names — **closed transitively over every
/// installer a `BootstrapPlan` names**: the system manager that installs a
/// missing prerequisite, and the manager a cascade installs through (`npm`,
/// `pipx` and `go` prefer brew, so brew joins even when no package names it).
/// Without the closure an edge either dangles or the install happens invisibly,
/// which is the unrendered bootstrap this phase exists to replace.
///
/// A sub-manager collapses onto its family's node: `brew-cask` has no bootstrap
/// of its own and answers `is_available()` with brew's, so provisioning it IS
/// provisioning brew, and two nodes would run `brew update` twice.
///
/// A manager that is absent and has no plan mints no node — nothing here could
/// carry it out, and the `Packages` phase already reports it as a skip naming
/// the manager.
///
/// The returned order is deterministic and topological: refreshes (which are
/// always roots), then prerequisites, then provisions in dependency order, each
/// tier sorted by name. Two runs against an unchanged host therefore plan
/// byte-identical actions, and a scheduler walking the list in order never
/// reaches a node before its dependencies.
pub(super) fn plan_managers(
    registry: &ProviderRegistry,
    profile: &MergedProfile,
    modules: &[ResolvedModule],
) -> Vec<Action> {
    // The one system manager every prerequisite in this run is installed from,
    // resolved once so two prerequisites can never name two installers on the
    // same host.
    let installer = prerequisite_installer(registry).map(|pm| pm.name().to_string());

    let mut queue: VecDeque<String> =
        crate::effective::effective_desired_packages(profile, modules)
            .into_iter()
            .filter(|ep| ep.manager != SCRIPT_SENTINEL)
            .map(|ep| node_manager(registry, &ep.manager).to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

    let mut graph = Graph::default();
    while let Some(name) = queue.pop_front() {
        if graph.members.contains_key(&name) {
            continue;
        }
        // A name the config declares but no provider backs: the `Packages`
        // phase reports it, and this phase has nothing to run for it.
        let Some(pm) = find_manager(registry, &name) else {
            continue;
        };
        if pm.is_available() {
            graph.members.insert(name, MemberState::Present);
            continue;
        }
        let Some(plan) = pm.bootstrap_plan() else {
            continue;
        };

        let missing: Vec<String> = plan
            .requires
            .iter()
            .filter(|tool| !crate::command_available(tool))
            .cloned()
            .collect();
        if !missing.is_empty() {
            // Nothing on this host installs the tool the cascade shells out to,
            // so the manager cannot be provisioned at all and mints no node
            // rather than one that must fail. `bootstrap_plan` answers `None`
            // on that path too; this is the planner's own guard against a plan
            // promising more than the host can carry out.
            let Some(installer) = installer.as_ref() else {
                continue;
            };
            for tool in &missing {
                graph
                    .prerequisites
                    .entry(tool.clone())
                    .or_default()
                    .insert(name.clone());
                queue.push_back(installer.clone());
            }
            graph.needs.insert(name.clone(), missing);
        }
        // A cascade's method names a registered manager whenever it installs
        // through one (`brew`, `apt`, `dnf`); a self-contained installer
        // (`rustup`, `homebrew installer`) names no registry entry and mints no
        // edge.
        if plan.method != name
            && let Some(preferred) = find_manager(registry, &plan.method)
        {
            let preferred = preferred.name().to_string();
            graph.prefers.insert(name.clone(), preferred.clone());
            queue.push_back(preferred);
        }
        graph
            .members
            .insert(name, MemberState::Provision { via: plan.method });
    }

    build_actions(&graph, installer.as_deref())
}

/// Assemble the nodes in topological order, wiring each edge to the id of the
/// node that satisfies it.
fn build_actions(graph: &Graph, installer: Option<&str>) -> Vec<Action> {
    let mut actions: Vec<Action> = Vec::new();

    for (manager, state) in &graph.members {
        if matches!(state, MemberState::Present) {
            actions.push(Action::Manager(ManagerAction::RefreshIndex {
                manager: manager.clone(),
            }));
        }
    }

    // A prerequisite waits on the index of the manager installing it, which is
    // available by construction and so always carries a refresh node.
    if let Some(installer) = installer {
        for (tool, required_by) in &graph.prerequisites {
            let depends_on = graph
                .members
                .get(installer)
                .map(|state| vec![state.node_id(installer)])
                .unwrap_or_default();
            actions.push(Action::Manager(ManagerAction::Prerequisite {
                tool: tool.clone(),
                installer: installer.to_string(),
                required_by: required_by.iter().cloned().collect(),
                depends_on,
            }));
        }
    }

    for manager in provision_order(graph) {
        let mut depends_on: Vec<String> = graph
            .needs
            .get(manager)
            .into_iter()
            .flatten()
            .map(|tool| ManagerAction::prereq_node(tool))
            .collect();
        if let Some(preferred) = graph.prefers.get(manager)
            && let Some(state) = graph.members.get(preferred)
        {
            depends_on.push(state.node_id(preferred));
        }
        let via = match graph.members.get(manager) {
            Some(MemberState::Provision { via }) => via.clone(),
            _ => String::new(),
        };
        actions.push(Action::Manager(ManagerAction::Provision {
            manager: manager.clone(),
            via,
            depends_on,
        }));
    }

    actions
}

/// The provisions in dependency order: a manager whose cascade installs through
/// another provisioned manager follows it. Ties break by name, so the order is
/// a function of the host rather than of iteration.
fn provision_order(graph: &Graph) -> Vec<&String> {
    let mut pending: Vec<&String> = graph
        .members
        .iter()
        .filter(|(_, state)| matches!(state, MemberState::Provision { .. }))
        .map(|(name, _)| name)
        .collect();
    let mut ordered: Vec<&String> = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let ready: Vec<&String> = pending
            .iter()
            .copied()
            .filter(|name| {
                graph
                    .prefers
                    .get(*name)
                    .is_none_or(|preferred| !pending.contains(&preferred))
            })
            .collect();
        // A cycle among provisions cannot be scheduled at all; take the
        // remainder in name order rather than spinning. Unreachable while
        // cascades install through system managers, which are never themselves
        // provisioned.
        let ready = if ready.is_empty() {
            pending.clone()
        } else {
            ready
        };
        ordered.extend(ready.iter().copied());
        pending.retain(|name| !ready.contains(name));
    }
    ordered
}

/// The manager whose node serves `name`: its family's parent when that parent
/// is itself registered, else the name as declared.
///
/// Only a registered parent collapses, so a user-defined manager whose name
/// happens to carry a hyphen keeps its own node instead of folding onto a
/// prefix that names nothing.
fn node_manager<'r>(registry: &'r ProviderRegistry, name: &'r str) -> &'r str {
    let family = crate::manager_family(name);
    if family != name && find_manager(registry, family).is_some() {
        family
    } else {
        name
    }
}

fn find_manager<'r>(registry: &'r ProviderRegistry, name: &str) -> Option<&'r dyn PackageManager> {
    registry
        .package_managers
        .iter()
        .find(|pm| pm.name() == name)
        .map(|pm| pm.as_ref())
}

/// The system manager a prerequisite is installed from on this host, in
/// registration order — the platform's own preference — or `None` when the host
/// has none, which is the refusal path.
fn prerequisite_installer(registry: &ProviderRegistry) -> Option<&dyn PackageManager> {
    registry
        .package_managers
        .iter()
        .map(|pm| pm.as_ref())
        .find(|pm| is_system_manager(pm.name()) && pm.is_available())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::PackageAction;
    use crate::reconciler::{PhaseName, format_action_description, format_plan_item};
    use crate::test_helpers::{MockPackageManager, ReconcilerTestHarness};

    /// A tool name no host has on `PATH`, so a `requires` naming it is always
    /// missing and the prerequisite arm is exercised on every platform.
    const ABSENT_TOOL: &str = "cfgd-absent-prerequisite-tool";

    /// The nodes `plan_managers` mints for `yaml` against `managers`, as their
    /// persisted ids.
    fn plan_ids(yaml: &str, managers: Vec<MockPackageManager>) -> Vec<String> {
        let mut builder = ReconcilerTestHarness::builder().profile_yaml(yaml);
        for pm in managers {
            builder = builder.with_package_manager(pm);
        }
        let harness = builder.build();
        plan_managers(&harness.registry, &harness.resolved.merged, &[])
            .iter()
            .map(format_action_description)
            .collect()
    }

    /// Every action `plan_managers` mints, for a test reading the node bodies
    /// rather than only their ids.
    fn plan_actions(yaml: &str, managers: Vec<MockPackageManager>) -> Vec<Action> {
        let mut builder = ReconcilerTestHarness::builder().profile_yaml(yaml);
        for pm in managers {
            builder = builder.with_package_manager(pm);
        }
        let harness = builder.build();
        plan_managers(&harness.registry, &harness.resolved.merged, &[])
    }

    #[test]
    fn membership_is_every_manager_the_desired_state_names() {
        let ids = plan_ids(
            "packages:\n  brew: [ripgrep]\n  cargo: [bat]\n",
            vec![
                MockPackageManager::new("brew"),
                MockPackageManager::new("cargo"),
                // Registered but unnamed by the profile: no node.
                MockPackageManager::new("npm"),
            ],
        );
        assert_eq!(
            ids,
            vec!["manager:refresh:brew", "manager:refresh:cargo"],
            "an available manager the desired state names refreshes, and only those"
        );
    }

    #[test]
    fn an_absent_manager_provisions_and_pulls_its_cascade_parent_into_the_closure() {
        let actions = plan_actions(
            "packages:\n  cargo: [bat]\n",
            vec![
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("brew"),
                // Named by no package — it joins only because cargo's cascade
                // installs through it.
                MockPackageManager::new("brew"),
            ],
        );
        let ids: Vec<String> = actions.iter().map(format_action_description).collect();
        assert_eq!(
            ids,
            vec!["manager:refresh:brew", "manager:provision:cargo"],
            "the cascade's installer joins the closure and refreshes first"
        );
        let Some(Action::Manager(ManagerAction::Provision {
            depends_on, via, ..
        })) = actions.last()
        else {
            panic!("the last node must be cargo's provision: {ids:?}");
        };
        assert_eq!(via, "brew");
        assert_eq!(
            depends_on,
            &vec!["manager:refresh:brew".to_string()],
            "a provision waits on the node that makes its installer usable"
        );
    }

    #[test]
    fn a_missing_required_tool_plans_a_prerequisite_from_the_system_manager() {
        let actions = plan_actions(
            "packages:\n  npm: [prettier]\n",
            vec![
                MockPackageManager::new("apt"),
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("brew")
                    .requiring(&[ABSENT_TOOL]),
                MockPackageManager::new("brew"),
            ],
        );
        let ids: Vec<String> = actions.iter().map(format_action_description).collect();
        assert_eq!(
            ids,
            vec![
                "manager:refresh:apt".to_string(),
                "manager:refresh:brew".to_string(),
                format!("manager:prereq:{ABSENT_TOOL}"),
                "manager:provision:npm".to_string(),
            ],
            "the missing tool becomes a node of its own, ahead of the provision needing it"
        );
        let Some(Action::Manager(ManagerAction::Prerequisite {
            installer,
            required_by,
            depends_on,
            ..
        })) = actions.get(2)
        else {
            panic!("the third node must be the prerequisite: {ids:?}");
        };
        assert_eq!(
            installer, "apt",
            "a prerequisite installs from a system manager"
        );
        assert_eq!(required_by, &vec!["npm".to_string()]);
        assert_eq!(
            depends_on,
            &vec!["manager:refresh:apt".to_string()],
            "an install waits on its installer's index"
        );
        let Some(Action::Manager(ManagerAction::Provision { depends_on, .. })) = actions.get(3)
        else {
            panic!("the fourth node must be npm's provision: {ids:?}");
        };
        assert_eq!(
            depends_on,
            &vec![
                format!("manager:prereq:{ABSENT_TOOL}"),
                "manager:refresh:brew".to_string(),
            ],
            "a provision waits on both its tool and its installer"
        );
    }

    #[test]
    fn a_missing_tool_no_system_manager_can_install_mints_no_node() {
        let ids = plan_ids(
            "packages:\n  npm: [prettier]\n",
            vec![
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("brew")
                    .requiring(&[ABSENT_TOOL]),
                MockPackageManager::new("brew"),
            ],
        );
        assert!(
            ids.is_empty(),
            "a manager this host cannot provision plans nothing rather than a node that must \
             fail, and its cascade's installer joins no closure it was never named by: {ids:?}"
        );
    }

    #[test]
    fn a_sub_manager_collapses_onto_its_familys_node() {
        let ids = plan_ids(
            "packages:\n  brew:\n    formulae: [ripgrep]\n    casks: [firefox]\n",
            vec![
                MockPackageManager::new("brew"),
                MockPackageManager::new("brew-cask"),
            ],
        );
        assert_eq!(
            ids,
            vec!["manager:refresh:brew"],
            "brew-cask is brew: one node, so `brew update` runs once"
        );
    }

    #[test]
    fn provisions_are_ordered_by_dependency_not_by_name() {
        let ids = plan_ids(
            "packages:\n  apk: [ripgrep]\n",
            vec![
                MockPackageManager::new("apk")
                    .unavailable()
                    .bootstrappable_via("brew"),
                MockPackageManager::new("brew")
                    .unavailable()
                    .bootstrappable_via("homebrew installer"),
            ],
        );
        assert_eq!(
            ids,
            vec!["manager:provision:brew", "manager:provision:apk"],
            "the installer is provisioned before the manager installed through it, \
             though its name sorts later"
        );
    }

    #[test]
    fn planning_twice_against_one_host_plans_the_same_nodes() {
        let yaml = "packages:\n  npm: [prettier]\n  cargo: [bat]\n  brew: [ripgrep]\n";
        let managers = || {
            vec![
                MockPackageManager::new("apt"),
                MockPackageManager::new("brew"),
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup"),
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("brew")
                    .requiring(&[ABSENT_TOOL]),
            ]
        };
        assert_eq!(
            plan_ids(yaml, managers()),
            plan_ids(yaml, managers()),
            "two runs against an unchanged host plan byte-identical nodes"
        );
    }

    #[test]
    fn a_manager_node_renders_what_it_will_do() {
        let actions = plan_actions(
            "packages:\n  npm: [prettier]\n",
            vec![
                MockPackageManager::new("apt"),
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("brew")
                    .requiring(&[ABSENT_TOOL]),
                MockPackageManager::new("brew"),
            ],
        );
        let items: Vec<String> = actions.iter().map(format_plan_item).collect();
        assert_eq!(
            items,
            vec![
                "refresh apt index".to_string(),
                "refresh brew index".to_string(),
                format!("apt install {ABSENT_TOOL} — required by npm"),
                "provision npm via brew".to_string(),
            ]
        );
    }

    #[test]
    fn the_prerequisites_phase_precedes_packages_and_owns_the_manager_nodes() {
        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("packages:\n  brew: [ripgrep]\n")
            .with_package_manager(MockPackageManager::new("brew"))
            .build();
        let plan = harness
            .plan_with_actions(
                Vec::new(),
                vec![PackageAction::Install {
                    manager: "brew".to_string(),
                    packages: vec!["ripgrep".to_string()],
                    origin: "profile".to_string(),
                }],
                Vec::new(),
            )
            .expect("plan");
        let phases: Vec<&PhaseName> = plan.phases.iter().map(|p| &p.name).collect();
        let index = |name: PhaseName| {
            phases
                .iter()
                .position(|p| **p == name)
                .unwrap_or_else(|| panic!("the plan must carry {name:?}: {phases:?}"))
        };
        assert!(
            index(PhaseName::Prerequisites) < index(PhaseName::Packages),
            "a manager is provisioned before the packages needing it: {phases:?}"
        );
        let phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Prerequisites)
            .expect("the phase exists whenever a manager node does");
        let owners: Vec<String> = phase
            .owned_actions()
            .map(|(owner, _)| owner.token())
            .collect();
        assert_eq!(
            owners,
            vec!["cfgd:managers".to_string()],
            "manager nodes belong to the cfgd:managers group"
        );
    }

    #[test]
    #[serial_test::serial]
    fn the_phases_groups_read_producer_before_consumer() {
        let tmp_home = tempfile::tempdir().expect("temp home");
        let _home = crate::with_test_home_guard(tmp_home.path());

        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("env:\n  - name: EDITOR\n    value: nvim\npackages:\n  brew: [ripgrep]\n")
            .with_package_manager(MockPackageManager::new("brew"))
            .build();
        let plan = harness.plan().expect("plan");
        let phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Prerequisites)
            .expect("the phase carries both the manager and the env work");

        let mut owners: Vec<String> = Vec::new();
        for (owner, _) in phase.owned_actions() {
            let token = owner.token();
            if owners.last() != Some(&token) {
                owners.push(token);
            }
        }
        assert_eq!(
            owners,
            vec![
                "cfgd:managers".to_string(),
                "cfgd:env".to_string(),
                "cfgd:session".to_string(),
            ],
            "the binaries are created, then where they live is published, then broadcast"
        );
    }

    #[test]
    fn provisioning_is_planned_once_not_as_a_package_bootstrap_too() {
        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("packages:\n  cargo: [bat]\n")
            .with_package_manager(
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup"),
            )
            .build();
        let plan = harness
            .plan_with_actions(
                Vec::new(),
                vec![PackageAction::Bootstrap {
                    manager: "cargo".to_string(),
                    method: "rustup".to_string(),
                    origin: "profile".to_string(),
                }],
                Vec::new(),
            )
            .expect("plan");
        let bootstraps: Vec<String> = plan
            .phases
            .iter()
            .flat_map(|p| p.owned_actions())
            .filter(|(_, action)| {
                matches!(action, Action::Package(PackageAction::Bootstrap { .. }))
            })
            .map(|(_, action)| format_action_description(action))
            .collect();
        assert!(
            bootstraps.is_empty(),
            "provisioning is a Prerequisites node now, planned nowhere else: {bootstraps:?}"
        );
        let provisions: Vec<String> = plan
            .phases
            .iter()
            .flat_map(|p| p.owned_actions())
            .map(|(_, action)| format_action_description(action))
            .filter(|desc| desc.starts_with("manager:provision:"))
            .collect();
        assert_eq!(
            provisions,
            vec!["manager:provision:cargo".to_string()],
            "and it is planned exactly once, as a manager node"
        );
    }
}

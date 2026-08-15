//! The `cfgd:managers` owner group of the `Prerequisites` phase: one node per
//! package manager the run's own work depends on, plus the tools cfgd's
//! bootstrap cascades shell out to, wired into a dependency graph.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::providers::{
    PackageAction, PackageManager, ProviderRegistry, SYSTEM_INSTALLABLE_TOOLS, is_system_manager,
};

use super::types::{Action, ManagerAction, ModuleAction, ModuleActionKind, PhaseName, Plan};

/// The manager name a module package carries when its "install" is an inline
/// script rather than a manager command. It names no registry entry.
const SCRIPT_SENTINEL: &str = "script";

/// What the run has to do about one manager.
enum MemberState {
    /// Present already — refresh its index.
    Present,
    /// Absent, provisioned by the method its own cascade resolved to.
    Provision { via: String },
    /// Absent and unprovisionable on this host, with the cause named.
    Refused { reason: String },
}

impl MemberState {
    /// The DAG id of the node this state mints for `manager`.
    fn node_id(&self, manager: &str) -> String {
        match self {
            MemberState::Present => ManagerAction::refresh_node(manager),
            MemberState::Provision { .. } => ManagerAction::provision_node(manager),
            MemberState::Refused { .. } => ManagerAction::refuse_node(manager),
        }
    }

    /// Whether this state ends with the manager usable. A node that depends on
    /// a manager only makes sense when it does.
    fn yields_a_usable_manager(&self) -> bool {
        matches!(self, MemberState::Present | MemberState::Provision { .. })
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
/// Membership starts from the managers this run's own work NAMES — a package
/// install planned in `Packages`, from the profile or from a module, or a
/// manager the run wanted and could not plan for at all — and is then closed
/// transitively over every installer a `BootstrapPlan` names: the system
/// manager that installs a missing prerequisite, and the manager a cascade
/// installs through (`npm`, `pipx` and `go` prefer brew, so brew joins even
/// when no package names it). Without the closure an edge either dangles or the
/// install happens invisibly, which is the unrendered bootstrap this phase
/// exists to replace.
///
/// Membership is deliberately NOT the desired package set: an index refresh is
/// only worth its network round trip when something in the run consumes the
/// index. A converged machine plans nothing here, so `cfgd apply` still reaches
/// its "nothing to do" answer and the daemon's reconcile tick does not run
/// `apt update` on every interval.
///
/// A sub-manager collapses onto its family's node: `brew-cask` has no bootstrap
/// of its own and answers `is_available()` with brew's, so provisioning it IS
/// provisioning brew, and two nodes would run `brew update` twice.
///
/// A manager that is absent with no cascade at all on this platform mints no
/// node — nothing here could carry it out, and the `Packages` phase already
/// reports it as a skip naming the manager. A manager whose cascade IS blocked
/// (its tool is missing and nothing available installs it) mints a refusal
/// naming the cause, because that is a thing cfgd would otherwise have done
/// silently.
///
/// The returned order is deterministic and topological: refreshes (which are
/// always roots), then prerequisites, then provisions in dependency order, then
/// the refusals, each tier sorted by name. Two runs against an unchanged host
/// therefore plan byte-identical actions, and a scheduler walking the list in
/// order never reaches a node before its dependencies.
pub fn plan_managers(
    registry: &ProviderRegistry,
    package_actions: &[PackageAction],
    module_routed: &[(PhaseName, Action)],
) -> Vec<Action> {
    // The one system manager every prerequisite in this run is installed from,
    // resolved once so two prerequisites can never name two installers on the
    // same host.
    let installer = prerequisite_installer(registry).map(|pm| pm.name().to_string());

    let mut queue: VecDeque<String> = wanted_managers(registry, package_actions, module_routed);

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
        // No cascade at all on this platform (apt on macOS, winget on Linux).
        // There is no cfgd decision to render: the manager was never a
        // candidate here, and `Packages` says so.
        let Some(plan) = pm.bootstrap_plan() else {
            continue;
        };

        let missing: Vec<String> = plan
            .requires
            .iter()
            .filter(|tool| !crate::command_available(tool))
            .cloned()
            .collect();
        // Judged against the REGISTRY rather than `PATH`, so the planner never
        // promises an install it has no registered manager to schedule.
        let unobtainable: Vec<&String> = missing
            .iter()
            .filter(|tool| {
                installer.is_none() || !SYSTEM_INSTALLABLE_TOOLS.contains(&tool.as_str())
            })
            .collect();
        if !unobtainable.is_empty() {
            let reason = blocked_reason(&unobtainable, installer.as_deref());
            graph.members.insert(name, MemberState::Refused { reason });
            continue;
        }
        if !missing.is_empty() {
            // `unobtainable` was empty, so an installer exists.
            if let Some(installer) = installer.as_ref() {
                for tool in &missing {
                    graph
                        .prerequisites
                        .entry(tool.clone())
                        .or_default()
                        .insert(name.clone());
                    queue.push_back(installer.clone());
                }
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

    refuse_provisions_with_no_usable_installer(&mut graph);
    drop_prerequisites_nothing_still_needs(&mut graph);
    build_actions(registry, &graph, installer.as_deref())
}

/// The managers this run's own work names, family-folded and deduplicated.
///
/// Two shapes count, and no others. A planned INSTALL — profile-level or a
/// module's — is a consumer of the manager's index and so earns a refresh. A
/// profile-level SKIP is a manager the run wanted and could not plan for, which
/// is where a refusal gets named. An uninstall consumes no index and a manager
/// nothing in the run touches has nothing to refresh for.
fn wanted_managers(
    registry: &ProviderRegistry,
    package_actions: &[PackageAction],
    module_routed: &[(PhaseName, Action)],
) -> VecDeque<String> {
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for action in package_actions {
        let manager = match action {
            PackageAction::Install { manager, .. } | PackageAction::Skip { manager, .. } => manager,
            PackageAction::Uninstall { .. } => continue,
        };
        wanted.insert(node_manager(registry, manager).to_string());
    }
    for (_, action) in module_routed {
        let Action::Module(ModuleAction {
            kind: ModuleActionKind::InstallPackages { resolved },
            ..
        }) = action
        else {
            continue;
        };
        for pkg in resolved {
            if pkg.manager == SCRIPT_SENTINEL {
                continue;
            }
            wanted.insert(node_manager(registry, &pkg.manager).to_string());
        }
    }
    wanted.into_iter().collect()
}

/// Why a cascade cannot run, in the words of the tools it is blocked on.
fn blocked_reason(tools: &[&String], installer: Option<&str>) -> String {
    let list = tools
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let is_are = if tools.len() == 1 { "is" } else { "are" };
    match installer {
        None => format!("{list} {is_are} missing and no system manager is available"),
        Some(installer) => {
            format!("{list} {is_are} missing and {installer} does not install it under that name")
        }
    }
}

/// Refuse every provision whose cascade installs through a manager this run
/// will not end up with, transitively.
///
/// `npm` installs through brew; if brew is itself refused — or is registered
/// with no cascade of its own — then `provision npm via brew` names a command
/// that cannot run. Refusing it here is the same statement the blocked-tool arm
/// makes, one link further down the chain.
fn refuse_provisions_with_no_usable_installer(graph: &mut Graph) {
    loop {
        let newly_refused: Vec<(String, String)> = graph
            .prefers
            .iter()
            .filter(|(manager, _)| {
                graph
                    .members
                    .get(*manager)
                    .is_some_and(|state| matches!(state, MemberState::Provision { .. }))
            })
            .filter(|(_, preferred)| {
                !graph
                    .members
                    .get(*preferred)
                    .is_some_and(MemberState::yields_a_usable_manager)
            })
            .map(|(manager, preferred)| {
                (
                    manager.clone(),
                    format!("it installs through {preferred}, which cannot be provisioned"),
                )
            })
            .collect();
        if newly_refused.is_empty() {
            return;
        }
        for (manager, reason) in newly_refused {
            graph
                .members
                .insert(manager, MemberState::Refused { reason });
        }
    }
}

/// Drop the prerequisite entries of managers that ended up refused, and any
/// tool left required by nobody — a run must not install a tool for a
/// provisioning it is not going to attempt.
fn drop_prerequisites_nothing_still_needs(graph: &mut Graph) {
    let refused: BTreeSet<String> = graph
        .members
        .iter()
        .filter(|(_, state)| matches!(state, MemberState::Refused { .. }))
        .map(|(name, _)| name.clone())
        .collect();
    if refused.is_empty() {
        return;
    }
    for manager in &refused {
        graph.needs.remove(manager);
    }
    for required_by in graph.prerequisites.values_mut() {
        required_by.retain(|manager| !refused.contains(manager));
    }
    graph
        .prerequisites
        .retain(|_, required_by| !required_by.is_empty());
}

/// Assemble the nodes in topological order, wiring each edge to the id of the
/// node that satisfies it.
fn build_actions(
    registry: &ProviderRegistry,
    graph: &Graph,
    installer: Option<&str>,
) -> Vec<Action> {
    let mut actions: Vec<Action> = Vec::new();

    for (manager, state) in &graph.members {
        // Only an already-present manager that keeps a local index gets a
        // refresh node. A manager this run is about to provision needs none:
        // its installer just fetched the index as part of installing it, so a
        // refresh would be a wasted network call for data that is already
        // current. A manager with no index at all (`cargo`, `npm`, `pipx` —
        // every install resolves against the remote) needs none either, and
        // planning one for it puts a line in the tree for work no manager
        // performs.
        let refreshes = find_manager(registry, manager).is_some_and(|pm| pm.has_index());
        if refreshes && matches!(state, MemberState::Present) {
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

    for (manager, via) in provision_order(graph) {
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
        actions.push(Action::Manager(ManagerAction::Provision {
            manager: manager.to_string(),
            via: via.to_string(),
            depends_on,
        }));
    }

    // Last: a refusal blocks nothing and carries no edge, so it reads after the
    // work the run will actually do.
    for (manager, state) in &graph.members {
        if let MemberState::Refused { reason } = state {
            actions.push(Action::Manager(ManagerAction::Refuse {
                manager: manager.clone(),
                reason: reason.clone(),
            }));
        }
    }

    actions
}

/// Drop the manager nodes the work left in `plan` no longer consumes, after
/// something else has pruned an already-planned plan.
///
/// [`plan_managers`] mints a node only for a manager the run's own work names,
/// and every later prune has to preserve that property or the phase promises
/// work nothing asked for. Three prunes reach a planner-built plan: the
/// daemon's per-module tick keeps one module's groups, the pending-decision
/// prune (`withhold_from_plan`) drops whatever awaits a source decision, and
/// `cfgd`'s own `filter_plan` (the widest caller — it runs after every
/// `--skip`-only pass, so a skip that removed a manager's `Provision` node
/// directly also drops the prerequisite tool that node alone needed). Left
/// unpruned, any of the three can leave an `apt update` behind with no install
/// left to read the index it refreshed.
///
/// A `RefreshIndex`/`Provision`/`Refuse` node survives when a surviving
/// install still names its manager — sub-manager folded onto its family, the
/// way the planner seeded it. A `Prerequisite` node is never seeded this way,
/// even when its own installer still has surviving package consumers: it
/// survives only when a node that is ITSELF surviving depends on it (the
/// `Provision`/`RefreshIndex` that named the tool as a requirement). A skip
/// that removes a manager's `Provision` node directly leaves its prerequisite
/// with no surviving dependent, so the tool that manager alone needed is
/// pruned along with it — the same silent, alert-free bookkeeping as a
/// purposeless refresh, never the stranded-install alert (that fires only for
/// a `Provision` a `--skip` pattern matched directly).
/// Every value a `Prerequisites` node's [`ManagerAction::filter_subject`] can
/// carry on this host — the vocabulary a `--phase prerequisites.<selector>` is
/// legal against.
///
/// Derived from the registry rather than listed at the CLI, and from the SAME
/// two rules the planner seeds nodes with: a manager is named by the node it
/// would be planned under ([`node_manager`]'s family collapse, which folds
/// `brew-cask` onto `brew` only when `brew` is itself registered), and a tool
/// is named by the bootstrap plan that shells out to it. A validator listing
/// families alone refused `--phase prerequisites.curl` — a spelling
/// [`ManagerAction::filter_subject`] is written to match and `--skip` already
/// accepts — so the two halves of one grammar disagreed about what the user
/// may type.
///
/// Vocabulary, not presence: a tool already installed plans no node, and a
/// selector naming it matches nothing and does nothing. That is the same
/// answer `prerequisites.brew` gives on a host with brew already current, and
/// it is what keeps this a spelling gate rather than a second planner.
pub fn prerequisite_selectors(registry: &ProviderRegistry) -> BTreeSet<String> {
    let mut selectors = BTreeSet::new();
    for pm in &registry.package_managers {
        selectors.insert(node_manager(registry, pm.name()).to_string());
        if let Some(plan) = pm.bootstrap_plan() {
            selectors.extend(plan.requires);
        }
    }
    selectors
}

pub fn prune_to_surviving_consumers(plan: &mut Plan) {
    let consumers = surviving_consumers(plan);
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut keep: BTreeSet<String> = BTreeSet::new();
    for phase in &plan.phases {
        for action in phase.actions() {
            let Action::Manager(node) = action else {
                continue;
            };
            let id = node.node_id();
            let directly_consumed = !matches!(node, ManagerAction::Prerequisite { .. })
                && consumers.contains(node.manager());
            if directly_consumed {
                keep.insert(id.clone());
            }
            edges.insert(id, node.depends_on().to_vec());
        }
    }
    if edges.is_empty() {
        return;
    }
    let mut queue: VecDeque<String> = keep.iter().cloned().collect();
    while let Some(id) = queue.pop_front() {
        for dependency in edges.get(&id).into_iter().flatten() {
            if keep.insert(dependency.clone()) {
                queue.push_back(dependency.clone());
            }
        }
    }
    for phase in &mut plan.phases {
        phase.retain_actions(|action| match action {
            Action::Manager(node) => keep.contains(&node.node_id()),
            _ => true,
        });
    }
    plan.phases.retain(|phase| !phase.is_empty());
}

/// The managers the package work still in `plan` would run a command through,
/// each recorded under the name it was declared with AND under its family, so
/// a `brew-cask` install keeps the `brew` node the planner folded it onto.
fn surviving_consumers(plan: &Plan) -> BTreeSet<String> {
    let mut consumers = BTreeSet::new();
    for phase in &plan.phases {
        for action in phase.actions() {
            match action {
                Action::Package(
                    PackageAction::Install { manager, .. } | PackageAction::Skip { manager, .. },
                ) => note_consumer(&mut consumers, manager),
                Action::Module(ModuleAction {
                    kind: ModuleActionKind::InstallPackages { resolved },
                    ..
                }) => {
                    for package in resolved {
                        note_consumer(&mut consumers, &package.manager);
                    }
                }
                _ => {}
            }
        }
    }
    consumers
}

fn note_consumer(consumers: &mut BTreeSet<String>, manager: &str) {
    if manager == SCRIPT_SENTINEL {
        return;
    }
    consumers.insert(manager.to_string());
    consumers.insert(crate::manager_family(manager).to_string());
}

/// The provisions in dependency order, each with the method it runs — read from
/// the same lookup that classified it, so a provision naming no method is not
/// representable. A manager whose cascade installs through another provisioned
/// manager follows it. Ties break by name, so the order is a function of the
/// host rather than of iteration.
fn provision_order(graph: &Graph) -> Vec<(&str, &str)> {
    let mut pending: Vec<(&str, &str)> = graph
        .members
        .iter()
        .filter_map(|(name, state)| match state {
            MemberState::Provision { via } => Some((name.as_str(), via.as_str())),
            MemberState::Present | MemberState::Refused { .. } => None,
        })
        .collect();
    let mut ordered: Vec<(&str, &str)> = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let ready: Vec<(&str, &str)> = pending
            .iter()
            .copied()
            .filter(|(name, _)| {
                graph.prefers.get(*name).is_none_or(|preferred| {
                    !pending.iter().any(|(pending, _)| pending == preferred)
                })
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
        pending.retain(|entry| !ready.contains(entry));
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

/// Fold each `Provision` node's declared `BootstrapPlan.creates_path_dirs`
/// into the dirs `plan_env` already recorded from past bootstraps, so an env
/// file this run writes already names a directory a manager it is about to
/// bootstrap will populate. Re-deriving `bootstrap_plan()` here rather than
/// carrying the dirs on `ManagerAction::Provision` keeps that type's shape —
/// consumed by every action-rendering and id-generation site — untouched;
/// two runs against the same host resolve the same manager to the same plan,
/// so re-deriving is exact, not an estimate.
pub(super) fn fold_provision_path_dirs<'a>(
    registry: &ProviderRegistry,
    actions: impl IntoIterator<Item = &'a Action>,
    recorded: Vec<String>,
) -> Vec<String> {
    let mut dirs = recorded;
    for action in actions {
        let Action::Manager(ManagerAction::Provision { manager, .. }) = action else {
            continue;
        };
        let Some(pm) = find_manager(registry, manager) else {
            continue;
        };
        let Some(plan) = pm.bootstrap_plan() else {
            continue;
        };
        for dir in plan.creates_path_dirs {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
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
    use crate::reconciler::types::{EnvAction, Owner, Phase};
    use crate::reconciler::{
        PhaseFilter, PhaseName, apply as reconciler_apply, env as reconciler_env,
        format_action_description, format_plan_item,
    };
    use crate::test_helpers::{MockPackageManager, ReconcilerTestHarness, test_printer};

    /// A tool name no host has on `PATH`, so a `requires` naming it is always
    /// missing, and one no system manager installs under that name, so the
    /// refusal arm is exercised on every platform.
    const ABSENT_TOOL: &str = "cfgd-absent-prerequisite-tool";

    /// A planned install per named manager — the consumer that earns a manager
    /// its node.
    fn installs(managers: &[&str]) -> Vec<PackageAction> {
        managers
            .iter()
            .map(|manager| PackageAction::Install {
                manager: (*manager).to_string(),
                packages: vec!["pkg".to_string()],
                origin: "profile".to_string(),
            })
            .collect()
    }

    /// One module install of `package` through `manager`, routed to `Packages`
    /// exactly as `plan_modules` routes it.
    fn module_install(manager: &str, package: &str) -> Vec<(PhaseName, Action)> {
        vec![(
            PhaseName::Packages,
            Action::Module(ModuleAction::with_origin(
                "mod",
                ModuleActionKind::InstallPackages {
                    resolved: vec![crate::modules::ResolvedPackage {
                        canonical_name: package.to_string(),
                        resolved_name: package.to_string(),
                        manager: manager.to_string(),
                        version: None,
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
                    }],
                },
                None,
            )),
        )]
    }

    /// Every action `plan_managers` mints for the given package work.
    fn plan_actions(
        package_actions: Vec<PackageAction>,
        managers: Vec<MockPackageManager>,
    ) -> Vec<Action> {
        plan_actions_with_modules(package_actions, Vec::new(), managers)
    }

    fn plan_actions_with_modules(
        package_actions: Vec<PackageAction>,
        module_routed: Vec<(PhaseName, Action)>,
        managers: Vec<MockPackageManager>,
    ) -> Vec<Action> {
        let mut builder = ReconcilerTestHarness::builder();
        for pm in managers {
            builder = builder.with_package_manager(pm);
        }
        let harness = builder.build();
        plan_managers(&harness.registry, &package_actions, &module_routed)
    }

    /// The nodes `plan_managers` mints, as their persisted ids.
    fn plan_ids(
        package_actions: Vec<PackageAction>,
        managers: Vec<MockPackageManager>,
    ) -> Vec<String> {
        plan_actions(package_actions, managers)
            .iter()
            .map(format_action_description)
            .collect()
    }

    #[test]
    fn membership_is_every_manager_the_runs_own_work_names() {
        let ids = plan_ids(
            installs(&["brew", "cargo"]),
            vec![
                MockPackageManager::new("brew"),
                MockPackageManager::new("cargo"),
                // Registered, available, and named by no work in this run: an
                // index nothing consumes is not worth a network round trip.
                MockPackageManager::new("npm"),
            ],
        );
        assert_eq!(
            ids,
            vec!["manager:refresh:brew", "manager:refresh:cargo"],
            "a manager this run installs through refreshes, and only those"
        );
    }

    #[test]
    fn a_manager_with_no_index_plans_no_refresh_node() {
        let ids = plan_ids(
            installs(&["brew", "cargo"]),
            vec![
                MockPackageManager::new("brew"),
                MockPackageManager::new("cargo").without_index(),
            ],
        );
        assert_eq!(
            ids,
            vec!["manager:refresh:brew"],
            "a manager that resolves its remote on every install has no index, so \
             planning a refresh for it puts a line in the tree — and an action in \
             `-o json` — for work no manager performs: {ids:?}"
        );
    }

    #[test]
    fn a_converged_run_plans_no_manager_nodes() {
        let ids = plan_ids(
            Vec::new(),
            vec![
                MockPackageManager::new("apt"),
                MockPackageManager::new("brew"),
            ],
        );
        assert!(
            ids.is_empty(),
            "a host with nothing left to install plans nothing, so `apply` keeps its \
             'nothing to do' answer and a daemon tick does not run `apt update` every \
             interval: {ids:?}"
        );
    }

    #[test]
    fn an_uninstall_alone_consumes_no_index() {
        let ids = plan_ids(
            vec![PackageAction::Uninstall {
                manager: "brew".to_string(),
                packages: vec!["ripgrep".to_string()],
                origin: "profile".to_string(),
            }],
            vec![MockPackageManager::new("brew")],
        );
        assert!(
            ids.is_empty(),
            "removing a package reads no index, so it earns no refresh: {ids:?}"
        );
    }

    #[test]
    fn a_module_package_is_a_consumer_of_its_managers_index() {
        let ids = plan_actions_with_modules(
            Vec::new(),
            module_install("brew", "ripgrep"),
            vec![MockPackageManager::new("brew")],
        )
        .iter()
        .map(format_action_description)
        .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["manager:refresh:brew"],
            "a module's install names its manager as surely as the profile's does"
        );
    }

    #[test]
    fn an_absent_manager_provisions_and_pulls_its_cascade_parent_into_the_closure() {
        let actions = plan_actions(
            installs(&["cargo"]),
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
    #[serial_test::serial]
    fn a_missing_required_tool_plans_a_prerequisite_from_the_system_manager() {
        // `curl` is the one tool a system manager installs under its own name,
        // so the prerequisite arm needs a host where it is genuinely absent.
        let _path_lock = crate::test_helpers::path_env_mutation_guard();
        let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let _path = crate::test_helpers::EnvVarGuard::set("PATH", "");

        let actions = plan_actions(
            installs(&["npm"]),
            vec![
                MockPackageManager::new("apt"),
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("brew")
                    .requiring(&["curl"]),
                MockPackageManager::new("brew"),
            ],
        );
        let ids: Vec<String> = actions.iter().map(format_action_description).collect();
        assert_eq!(
            ids,
            vec![
                "manager:refresh:apt".to_string(),
                "manager:refresh:brew".to_string(),
                "manager:prereq:curl".to_string(),
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
                "manager:prereq:curl".to_string(),
                "manager:refresh:brew".to_string(),
            ],
            "a provision waits on both its tool and its installer"
        );
        let items: Vec<String> = actions.iter().map(format_plan_item).collect();
        assert_eq!(
            items,
            vec![
                "refresh apt index".to_string(),
                "refresh brew index".to_string(),
                "apt install curl — required by npm".to_string(),
                "provision npm via brew".to_string(),
            ],
            "every node says what it will do, naming the method it runs"
        );
    }

    #[test]
    fn a_missing_tool_no_system_manager_can_install_is_refused_with_the_cause_named() {
        let actions = plan_actions(
            installs(&["npm"]),
            vec![
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
            vec!["manager:refuse:npm"],
            "a manager this host cannot provision is still a node, so the refusal is \
             visible in the phase the user is told to look at: {ids:?}"
        );
        assert_eq!(
            actions.iter().map(format_plan_item).collect::<Vec<_>>(),
            vec![format!(
                "cannot provision npm — {ABSENT_TOOL} is missing and no system manager is available"
            )],
            "and it names the cause rather than only the refusal"
        );
    }

    #[test]
    fn a_missing_tool_the_system_manager_does_not_install_names_that_installer() {
        let actions = plan_actions(
            installs(&["npm"]),
            vec![
                MockPackageManager::new("apt"),
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("brew")
                    .requiring(&[ABSENT_TOOL]),
                MockPackageManager::new("brew"),
            ],
        );
        assert_eq!(
            actions.iter().map(format_plan_item).collect::<Vec<_>>(),
            vec![format!(
                "cannot provision npm — {ABSENT_TOOL} is missing and apt does not install it \
                 under that name"
            )],
            "the host HAS a system manager; what it lacks is that manager's ability to \
             install this tool, and the refusal says which"
        );
    }

    #[test]
    fn a_cascade_through_an_unprovisionable_manager_is_refused() {
        let actions = plan_actions(
            installs(&["npm"]),
            vec![
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("brew"),
                // Registered, absent, and with no cascade of its own.
                MockPackageManager::new("brew").unavailable(),
            ],
        );
        let ids: Vec<String> = actions.iter().map(format_action_description).collect();
        assert_eq!(
            ids,
            vec!["manager:refuse:npm"],
            "a provision whose installer this run will never have is refused rather than \
             left promising `provision npm via brew` with no brew: {ids:?}"
        );
        assert_eq!(
            actions.iter().map(format_plan_item).collect::<Vec<_>>(),
            vec![
                "cannot provision npm — it installs through brew, which cannot be provisioned"
                    .to_string()
            ]
        );
    }

    #[test]
    fn a_manager_with_no_cascade_on_this_platform_mints_no_node() {
        let ids = plan_ids(
            installs(&["apt"]),
            vec![MockPackageManager::new("apt").unavailable()],
        );
        assert!(
            ids.is_empty(),
            "apt on macOS was never a candidate here; there is no cfgd decision to \
             render and `Packages` reports the skip: {ids:?}"
        );
    }

    #[test]
    fn a_sub_manager_collapses_onto_its_familys_node() {
        let ids = plan_ids(
            installs(&["brew", "brew-cask"]),
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
            installs(&["apk"]),
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
            plan_ids(installs(&["npm", "cargo", "brew"]), managers()),
            plan_ids(installs(&["npm", "cargo", "brew"]), managers()),
            "two runs against an unchanged host plan byte-identical nodes"
        );
    }

    #[test]
    fn the_prerequisites_phase_precedes_packages_and_owns_the_manager_nodes() {
        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("packages:\n  brew: [ripgrep]\n")
            .with_package_manager(MockPackageManager::new("brew"))
            .build();
        let plan = harness
            .plan_with_actions(Vec::new(), installs(&["brew"]), Vec::new())
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
        let plan = harness
            .plan_with_actions(Vec::new(), installs(&["brew"]), Vec::new())
            .expect("plan");
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
    fn provisioning_is_planned_once_as_a_manager_node() {
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
                vec![PackageAction::Install {
                    manager: "cargo".to_string(),
                    packages: vec!["bat".to_string()],
                    origin: "profile".to_string(),
                }],
                Vec::new(),
            )
            .expect("plan");
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

    #[test]
    fn fold_provision_path_dirs_adds_a_provisioned_managers_declared_dirs() {
        let actions = plan_actions(
            installs(&["cargo"]),
            vec![
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup")
                    .creating_dirs(&["/home/u/.cargo/bin"]),
            ],
        );
        let harness = ReconcilerTestHarness::builder()
            .with_package_manager(
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup")
                    .creating_dirs(&["/home/u/.cargo/bin"]),
            )
            .build();
        let recorded = vec!["/opt/homebrew/bin".to_string()];
        let dirs = fold_provision_path_dirs(&harness.registry, &actions, recorded.clone());
        assert_eq!(
            dirs,
            vec![
                "/opt/homebrew/bin".to_string(),
                "/home/u/.cargo/bin".to_string(),
            ],
            "the recorded dirs stay first; the provisioned manager's declared dir is appended"
        );
    }

    #[test]
    fn fold_provision_path_dirs_never_duplicates_an_already_recorded_dir() {
        let actions = plan_actions(
            installs(&["cargo"]),
            vec![
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup")
                    .creating_dirs(&["/home/u/.cargo/bin"]),
            ],
        );
        let harness = ReconcilerTestHarness::builder()
            .with_package_manager(
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup")
                    .creating_dirs(&["/home/u/.cargo/bin"]),
            )
            .build();
        let recorded = vec!["/home/u/.cargo/bin".to_string()];
        let dirs = fold_provision_path_dirs(&harness.registry, &actions, recorded.clone());
        assert_eq!(dirs, recorded, "a dir already recorded is not repeated");
    }

    #[test]
    fn fold_provision_path_dirs_leaves_recorded_dirs_untouched_when_nothing_provisions() {
        let harness = ReconcilerTestHarness::builder()
            .with_package_manager(MockPackageManager::new("brew"))
            .build();
        let recorded = vec!["/opt/homebrew/bin".to_string()];
        let dirs =
            fold_provision_path_dirs(&harness.registry, &Vec::<Action>::new(), recorded.clone());
        assert_eq!(dirs, recorded);
    }
    /// A minimal one-phase plan built directly from manager and package
    /// actions, bypassing `plan_managers` — the two `prune_to_surviving_consumers`
    /// tests below construct a plan shape by hand rather than through a real
    /// planning pass, so each can isolate exactly one node's presence.
    fn one_phase_plan(
        prereq_and_provision_actions: Vec<Action>,
        package_actions: Vec<Action>,
    ) -> Plan {
        let profile = Owner::profile("test");
        let mut phases = vec![Phase::from_actions(
            PhaseName::Prerequisites,
            &profile,
            prereq_and_provision_actions,
        )];
        if !package_actions.is_empty() {
            phases.push(Phase::from_actions(
                PhaseName::Packages,
                &profile,
                package_actions,
            ));
        }
        Plan {
            phases,
            warnings: Vec::new(),
        }
    }

    fn pkg_install(manager: &str, package: &str) -> Action {
        Action::Package(PackageAction::Install {
            manager: manager.to_string(),
            packages: vec![package.to_string()],
            origin: "profile".to_string(),
        })
    }

    #[test]
    fn prune_to_surviving_consumers_drops_a_prerequisite_whose_sole_dependent_is_gone() {
        // Mirrors the state left behind by `--skip prerequisites.npm`: the
        // skip removes npm's `Provision` node directly (it matched the
        // pattern), but leaves the npm package install alone (a different
        // pattern), so npm is still a "surviving consumer" by
        // `surviving_consumers`'s own test. The curl prerequisite npm alone
        // needed has no surviving dependent left to keep it alive and must be
        // pruned along with the provision that named it — the doc comment's
        // claim on `prune_to_surviving_consumers`, previously untested.
        let curl_prereq = Action::Manager(ManagerAction::Prerequisite {
            tool: "curl".to_string(),
            installer: "apt".to_string(),
            required_by: vec!["npm".to_string()],
            depends_on: vec![ManagerAction::refresh_node("apt")],
        });
        let mut plan = one_phase_plan(vec![curl_prereq], vec![pkg_install("npm", "typescript")]);

        prune_to_surviving_consumers(&mut plan);

        assert!(
            !plan
                .phases
                .iter()
                .any(|p| p.name == PhaseName::Prerequisites),
            "the prerequisite's sole dependent (npm's provision) is absent from the plan, \
             so it must be pruned and the now-empty Prerequisites phase dropped with it: {:?}",
            plan.phases
        );
    }

    #[test]
    fn prune_to_surviving_consumers_keeps_a_prerequisite_another_manager_still_depends_on() {
        // The companion case: curl is required by both npm and pipx. npm's
        // provision is gone (as if `--skip prerequisites.npm` ran), but
        // pipx's provision — which also depends on the curl prerequisite —
        // survives because pipx still has a package consumer. The shared
        // prerequisite must survive through pipx's edge even though npm's is
        // gone.
        let curl_prereq = Action::Manager(ManagerAction::Prerequisite {
            tool: "curl".to_string(),
            installer: "apt".to_string(),
            required_by: vec!["npm".to_string(), "pipx".to_string()],
            depends_on: vec![ManagerAction::refresh_node("apt")],
        });
        let pipx_provision = Action::Manager(ManagerAction::Provision {
            manager: "pipx".to_string(),
            via: "pipx installer".to_string(),
            depends_on: vec![ManagerAction::prereq_node("curl")],
        });
        let mut plan = one_phase_plan(
            vec![curl_prereq, pipx_provision],
            vec![pkg_install("pipx", "black")],
        );

        prune_to_surviving_consumers(&mut plan);

        let prereq_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Prerequisites)
            .expect("pipx's provision survives, and the prerequisite it depends on with it");
        assert_eq!(
            prereq_phase.action_count(),
            2,
            "both the curl prerequisite and pipx's provision survive: {:?}",
            prereq_phase.actions().collect::<Vec<_>>()
        );
    }

    #[test]
    fn every_named_manager_gets_a_managers_phase_action() {
        let actions = plan_actions(
            installs(&["brew", "cargo"]),
            vec![
                MockPackageManager::new("brew"),
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup"),
            ],
        );
        let ids: Vec<String> = actions.iter().map(format_action_description).collect();
        assert_eq!(
            ids,
            vec![
                "manager:refresh:brew".to_string(),
                "manager:provision:cargo".to_string(),
            ],
            "every manager this run names earns a Prerequisites-phase action, present or absent: {ids:?}"
        );
    }

    #[test]
    fn a_present_manager_refreshes_its_index_and_does_not_provision() {
        let actions = plan_actions(installs(&["brew"]), vec![MockPackageManager::new("brew")]);
        let ids: Vec<String> = actions.iter().map(format_action_description).collect();
        assert_eq!(
            ids,
            vec!["manager:refresh:brew".to_string()],
            "a present manager refreshes its index and mints no provision node: {ids:?}"
        );
    }

    #[test]
    fn a_provisioned_manager_emits_no_second_index_refresh() {
        let actions = plan_actions(
            installs(&["cargo"]),
            vec![
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup"),
            ],
        );
        let ids: Vec<String> = actions.iter().map(format_action_description).collect();
        assert_eq!(
            ids,
            vec!["manager:provision:cargo".to_string()],
            "a provisioned manager's own index refresh is implied by provisioning it — a \
             second, separate refresh node would run `cargo update` against a manager that \
             was not on PATH a moment before: {ids:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_missing_prerequisite_is_installed_from_a_system_manager() {
        let _path_lock = crate::test_helpers::path_env_mutation_guard();
        let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let _path = crate::test_helpers::EnvVarGuard::set("PATH", "");

        let actions = plan_actions(
            installs(&["npm"]),
            vec![
                MockPackageManager::new("apt"),
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("brew")
                    .requiring(&["curl"]),
                MockPackageManager::new("brew"),
            ],
        );
        let Some(Action::Manager(ManagerAction::Prerequisite {
            installer, tool, ..
        })) = actions
            .iter()
            .find(|a| format_action_description(a) == "manager:prereq:curl")
        else {
            panic!(
                "a missing prerequisite tool must plan a Prerequisite node: {:?}",
                actions
                    .iter()
                    .map(format_action_description)
                    .collect::<Vec<_>>()
            );
        };
        assert_eq!(tool, "curl");
        assert_eq!(
            installer, "apt",
            "decision (a): a prerequisite installs from a system manager, never from the \
             cascade's own installer"
        );
    }

    #[test]
    fn a_missing_prerequisite_with_no_system_manager_fails_the_manager_by_name() {
        let actions = plan_actions(
            installs(&["npm"]),
            vec![
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
            vec!["manager:refuse:npm".to_string()],
            "with no system manager registered to install the missing tool, the manager \
             itself fails by name rather than the plan silently dropping it: {ids:?}"
        );
        let Some(Action::Manager(ManagerAction::Refuse { manager, reason })) = actions.first()
        else {
            panic!("the node must be a Refuse action: {ids:?}");
        };
        assert_eq!(manager, "npm");
        assert!(
            reason.contains(ABSENT_TOOL),
            "the refusal names the missing tool: {reason}"
        );
    }

    #[test]
    fn provision_failure_fails_only_that_managers_packages() {
        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("packages:\n  cargo: [bat]\n  npm: [typescript]\n")
            .with_package_manager(
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup"),
            )
            .with_package_manager(
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("nvm")
                    .bootstrap_succeeds(),
            )
            .build();
        let plan = harness
            .plan_with_actions(Vec::new(), installs(&["cargo", "npm"]), Vec::new())
            .expect("plan");
        let result = harness.apply(&plan, &test_printer()).expect("apply");

        let cargo_install = result
            .action_results
            .iter()
            .find(|r| r.description.starts_with("package:cargo:install"))
            .expect("cargo's install is still planned even though its provision fails");
        assert!(
            !cargo_install.success,
            "cargo's package install must fail because cargo's provision never became available"
        );

        let npm_install = result
            .action_results
            .iter()
            .find(|r| r.description.starts_with("package:npm:install"))
            .expect("npm's install must still be planned");
        assert!(
            npm_install.success,
            "npm's own provision succeeded independently, so its packages install fine \
             despite cargo's provision failing: {:?}",
            npm_install.error
        );
    }

    #[test]
    #[serial_test::serial]
    fn managers_phase_precedes_env_in_plan_order() {
        let tmp_home = tempfile::tempdir().expect("temp home");
        let _home = crate::with_test_home_guard(tmp_home.path());

        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("env:\n  - name: EDITOR\n    value: nvim\npackages:\n  brew: [ripgrep]\n")
            .with_package_manager(MockPackageManager::new("brew"))
            .build();
        let plan = harness
            .plan_with_actions(Vec::new(), installs(&["brew"]), Vec::new())
            .expect("plan");
        let phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Prerequisites)
            .expect("the phase carries both the manager and the env work");

        let managers_index = phase
            .owned_actions()
            .position(|(owner, _)| owner.token() == "cfgd:managers")
            .expect("the manager node is present");
        let env_index = phase
            .owned_actions()
            .position(|(owner, _)| owner.token() == "cfgd:env")
            .expect("the env write is present");
        assert!(
            managers_index < env_index,
            "a manager must be provisioned before the env file that publishes where its \
             binaries live is written: managers at {managers_index}, env at {env_index}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn env_file_carries_bootstrapped_path_dirs_without_regeneration() {
        let tmp_home = tempfile::tempdir().expect("temp home");
        let _home = crate::with_test_home_guard(tmp_home.path());

        let cargo_mgr = || {
            MockPackageManager::new("cargo")
                .unavailable()
                .bootstrappable_via("rustup")
                .bootstrap_succeeds()
                .creating_dirs(&["/opt/cargo-bin"])
        };

        // What the plan folds in before cargo is ever bootstrapped.
        let manager_actions = plan_actions(installs(&["cargo"]), vec![cargo_mgr()]);
        let plan_registry = ReconcilerTestHarness::builder()
            .with_package_manager(cargo_mgr())
            .build()
            .registry;
        let path_dirs_at_plan =
            fold_provision_path_dirs(&plan_registry, &manager_actions, Vec::new());
        assert_eq!(
            path_dirs_at_plan,
            vec!["/opt/cargo-bin".to_string()],
            "the not-yet-provisioned manager's declared dir is already folded into the \
             plan before it is bootstrapped"
        );

        // What a real apply actually records once cargo is bootstrapped.
        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("packages:\n  cargo: [bat]\n")
            .with_package_manager(cargo_mgr())
            .build();
        let plan = harness
            .plan_with_actions(Vec::new(), installs(&["cargo"]), Vec::new())
            .expect("plan");
        harness.apply(&plan, &test_printer()).expect("apply");
        let path_dirs_now = reconciler_env::recorded_manager_path_dirs(
            harness.state_store(),
            &harness.resolved_profile().merged,
            &[],
        );

        assert!(
            !reconciler_apply::path_dirs_changed(&path_dirs_now, &path_dirs_at_plan),
            "what a real bootstrap recorded matches what planning already folded in, so \
             the post-apply regeneration path this proves dead never has anything to react \
             to: now={path_dirs_now:?} at_plan={path_dirs_at_plan:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_prerequisite_installer_outside_the_package_set_joins_the_phase() {
        // `curl` is the one tool a system manager installs under its own
        // name, so the missing-tool arm needs a host where it is genuinely
        // absent — same fixture as
        // `a_missing_required_tool_plans_a_prerequisite_from_the_system_manager`,
        // whose real `PATH` this must not race with.
        let _path_lock = crate::test_helpers::path_env_mutation_guard();
        let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let _path = crate::test_helpers::EnvVarGuard::set("PATH", "");

        let actions = plan_actions(
            installs(&["npm"]),
            vec![
                MockPackageManager::new("apt"),
                MockPackageManager::new("npm")
                    .unavailable()
                    .bootstrappable_via("brew")
                    .requiring(&["curl"]),
                MockPackageManager::new("brew"),
            ],
        );
        let ids: Vec<String> = actions.iter().map(format_action_description).collect();
        assert!(
            ids.contains(&"manager:refresh:apt".to_string()),
            "apt is named by no package action, yet the prerequisite it installs pulls it \
             into the phase's closure: {ids:?}"
        );
    }

    #[test]
    fn a_cascade_preferred_manager_outside_the_package_set_joins_the_phase() {
        let actions = plan_actions(
            installs(&["cargo"]),
            vec![
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("brew"),
                MockPackageManager::new("brew"),
            ],
        );
        let ids: Vec<String> = actions.iter().map(format_action_description).collect();
        assert!(
            ids.contains(&"manager:refresh:brew".to_string()),
            "brew is named by no package action, yet cargo's cascade installer pulls it \
             into the phase's closure: {ids:?}"
        );
    }

    #[test]
    fn provision_failure_fails_downstream_provisions_with_the_root_cause() {
        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("packages:\n  cargo: [bat]\n")
            .with_package_manager(
                MockPackageManager::new("brew")
                    .unavailable()
                    .bootstrappable_via("official installer"),
            )
            .with_package_manager(
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("brew"),
            )
            .build();
        let plan = harness
            .plan_with_actions(Vec::new(), installs(&["cargo"]), Vec::new())
            .expect("plan");
        let result = harness.apply(&plan, &test_printer()).expect("apply");

        let brew_provision = result
            .action_results
            .iter()
            .find(|r| r.description == "manager:provision:brew")
            .expect("brew's provision must be planned as its own node");
        assert!(
            !brew_provision.success,
            "brew never becomes available, so its own provision fails"
        );

        let cargo_provision = result
            .action_results
            .iter()
            .find(|r| r.description == "manager:provision:cargo")
            .expect("cargo's provision must be planned, waiting on brew's");
        assert!(
            !cargo_provision.success,
            "a provision whose installer's own provision failed cannot itself run"
        );
        let error = cargo_provision.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("brew"),
            "the transitive failure names brew — the root cause — not cargo's own node: {error}"
        );
    }

    #[test]
    fn phase_packages_with_an_unprovisioned_manager_fails_by_name_with_guidance() {
        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("packages:\n  cargo: [bat]\n")
            .with_package_manager(
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup"),
            )
            .build();
        let plan = harness
            .plan_with_actions(Vec::new(), installs(&["cargo"]), Vec::new())
            .expect("plan");
        let phase_filter = PhaseFilter::Phase(PhaseName::Packages);
        let result = harness
            .apply_with_filter(&plan, &test_printer(), Some(&phase_filter))
            .expect("apply");

        let cargo_install = result
            .action_results
            .iter()
            .find(|r| r.description.starts_with("package:cargo:install"))
            .expect("the install is still planned under --phase packages");
        assert!(
            !cargo_install.success,
            "a run that skips Prerequisites cannot have provisioned cargo, so the install fails"
        );
        let error = cargo_install.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("cargo") && error.contains("cfgd apply --phase prerequisites.managers"),
            "the failure names the manager and points at the owner-group-scoped fix, not the coarse phase: {error}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn planned_env_content_carries_declared_path_dirs_of_provisioned_managers() {
        let tmp_home = tempfile::tempdir().expect("temp home");
        let _home = crate::with_test_home_guard(tmp_home.path());

        let harness = ReconcilerTestHarness::builder()
            .profile_yaml("packages:\n  cargo: [bat]\n")
            .with_package_manager(
                MockPackageManager::new("cargo")
                    .unavailable()
                    .bootstrappable_via("rustup")
                    .creating_dirs(&["/opt/cargo-bin"]),
            )
            .build();
        let plan = harness
            .plan_with_actions(Vec::new(), installs(&["cargo"]), Vec::new())
            .expect("plan");
        let phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Prerequisites)
            .expect("the manager and env work are both planned");
        let env_write = phase
            .owned_actions()
            .find_map(|(_, action)| match action {
                Action::Env(EnvAction::WriteEnvFile { content, .. }) => Some(content),
                _ => None,
            })
            .expect("cargo names a package, which earns the env write its non-empty path_dirs");
        assert!(
            env_write.contains("/opt/cargo-bin"),
            "the manager's declared PATH dir is folded into the planned env content \
             BEFORE it is ever provisioned: {env_write}"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn prerequisite_obtainability_agrees_with_the_planners_installer_predicate() {
        let _path_lock = crate::test_helpers::path_env_mutation_guard();
        let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();

        // No system manager anywhere on PATH: neither predicate can obtain curl.
        {
            let _path = crate::test_helpers::EnvVarGuard::set("PATH", "");
            let registry = ReconcilerTestHarness::builder()
                .with_package_manager(MockPackageManager::new("apt").unavailable())
                .build()
                .registry;
            assert_eq!(
                prerequisite_installer(&registry).is_some(),
                crate::providers::prerequisite_obtainable("curl"),
                "with no system manager reachable, both predicates must refuse curl"
            );
            assert!(!crate::providers::prerequisite_obtainable("curl"));
        }

        // A real `apt` on PATH: both predicates must agree curl is obtainable.
        {
            let (_bin_dir, _shim) = crate::test_helpers::install_named_path_shim("apt", 0, "", "");
            let registry = ReconcilerTestHarness::builder()
                .with_package_manager(MockPackageManager::new("apt"))
                .build()
                .registry;
            assert_eq!(
                prerequisite_installer(&registry).is_some(),
                crate::providers::prerequisite_obtainable("curl"),
                "with a system manager on PATH, both predicates must agree curl is obtainable"
            );
            assert!(crate::providers::prerequisite_obtainable("curl"));
        }
    }
}

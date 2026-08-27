//! The concurrent dispatcher: per-manager lanes, serving the `Packages` phase
//! behind Rule P's tier barrier and the `Prerequisites` phase's `cfgd:managers`
//! group as a DAG.
//!
//! Every other phase mutates shared user state and stays a sequential walk.
//! Package installs do not: two managers driving two different binaries have
//! nothing to contend over, and the run's wall-clock cost is dominated by them.
//! Provisioning the managers themselves is the same shape one step earlier —
//! `apt update` and a `rustup` install contend over nothing either — except
//! that its ordering is a graph rather than a tier: a node runs once every node
//! its plan named ([`Slot::depends_on`]) has completed.
//!
//! The shape is a coordinator plus workers, and the reason is SQLite. The
//! reconciler's `StateStore` owns a `rusqlite::Connection`, which is `Send` and
//! not `Sync`, so `&Reconciler` cannot be shared with a worker at all — and
//! that constraint is also the design: every journal row, file backup and
//! bootstrap record is written on the coordinator thread, at a dispatch point
//! or at a collection point. A worker returns results and nothing else. A
//! worker that needs to READ package state sends the coordinator a message and
//! waits for the answer, over the same channel it will later report its result
//! on, so there is exactly one writer by construction rather than by discipline.
//!
//! What holds actions back, in the order the dispatcher applies it:
//!
//! 1. **The tier barrier** — no action is dispatched until every action in the
//!    tier above it has *completed*. An empty tier is released and drained in
//!    the same instant.
//! 2. **Module `depends`, and a node's own edges** — a module's package work
//!    waits for every action of its transitive dependencies, and a
//!    `Prerequisites` node waits for every node its plan named. A node whose
//!    dependency FAILED never runs at all: it settles as a failure naming the
//!    ancestor, because what it was waiting to be handed does not exist —
//!    unless the run is aborting, where nothing that never began is reported
//!    at all and the shortfall is the rollup's to name, for a dependent and a
//!    sibling alike.
//! 3. **The per-family lane** — at most one action per manager FAMILY is in
//!    flight, so the maximum parallelism is the number of distinct families.
//!    `brew`, `brew-tap` and `brew-cask` are one binary and share a lane; the
//!    key is [`Slot::lane`], never the registered name.
//! 4. **The owner's turn** — an owner already holding a lane takes a second one
//!    only after every other owner with ready work has taken one. So a module
//!    declaring brew and apt work takes brew while another module holds apt,
//!    and then says it is waiting on apt — while a lone owner still fills every
//!    lane in the phase.
//! 5. **The serial sub-gate** — any action whose manager is registered and not
//!    currently available drains the phase. Evaluated at dispatch time, because
//!    a manager provisioned earlier in the same phase becomes available
//!    mid-run. The predicate is keyed on the manager's own state rather than on
//!    which action kind names it, so it does NOT apply to a `Prerequisites`
//!    node: a node whose whole job is to MAKE its manager available is
//!    unavailable by definition — left in, it would drain the one phase whose
//!    purpose is that provisioning runs concurrently.
//!
//! ## The caller must not hold `path_env_mutation_guard()` across `apply()`
//!
//! `dispatch_lanes` spawns worker threads that read the process `PATH`
//! (a package manager resolving its own binary, `git`, a script interpreter),
//! each guarded by `cfgd_core::test_helpers::path_env_read_guard()` at the
//! actual point of spawn. That guard's thread-locals are per-thread, so a
//! freshly spawned worker carries neither flag and takes a REAL read lock on
//! `PATH_ENV_LOCK`. If the thread that called `apply()` already holds the
//! exclusive `path_env_mutation_guard()` (a test fixture driving `apply()`
//! from inside a `CwdGuard`/`PathShimGuard` window) that thread goes on to
//! park in `inbox.recv()` waiting for the very worker that is blocked behind
//! its own write guard — deadlock, with no timeout. `dispatch_lanes`
//! asserts against exactly that precondition before spawning anything, so the
//! violation fails fast instead of hanging.
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use crate::config::{ResolvedProfile, ScriptShell};
use crate::errors::{PackageError, Result, StateError};
use crate::modules::ResolvedModule;
use crate::output::{LaneOutput, Printer};
use crate::providers::{ActionNote, NoteSink, PackageAction, PackageStateStore, ProviderRegistry};

use super::apply::ActionOutcome;
use super::format::{
    action_display_subject, format_action_description, parse_resource_from_description,
};
use super::live_tree::{Held, PhaseTree, Wait};
use super::packages::{ModuleInstallContext, PackageExec, action_manager};
use super::types::{
    Action, ManagerAction, ModuleAction, ModuleActionKind, Owner, PhaseName, ReconcileContext, Tier,
};

/// The run-scoped inputs every dispatched action needs, none of which change
/// between actions.
pub(super) struct LaneRun<'x> {
    pub(super) printer: &'x Printer,
    pub(super) apply_id: i64,
    /// The phase being dispatched — the journal's `phase` column. Read from the
    /// plan rather than assumed, because two phases now dispatch through here
    /// and a row filed under the wrong one is a row no phase query finds.
    pub(super) phase: &'x PhaseName,
    pub(super) config_dir: &'x std::path::Path,
    pub(super) resolved: &'x ResolvedProfile,
    pub(super) module_actions: &'x [ResolvedModule],
    pub(super) context: ReconcileContext,
    pub(super) shell_override: Option<ScriptShell>,
    pub(super) abort: &'x crate::AbortFlag,
    /// Base of the run-wide plan-position counter for this phase.
    pub(super) plan_index_base: usize,
    /// Depth an action's line — and so its lane's window — renders at.
    pub(super) action_depth: usize,
}

/// Where a finished action goes: through the caller's settle, which journals
/// it and files its result row, and then into the phase's tree, which settles
/// the row it has been running in.
///
/// One type rather than two parameters because every collection point must do
/// both, in that order: an outcome that reached the tree without being settled
/// would render a line the run's own results never counted.
pub(super) struct LaneCollector<'a, 'p, 'g> {
    /// Returns the line to render, and `None` when the caller kept it — off a
    /// TTY the phase's tree is written at close, in plan order, and nothing
    /// settles here.
    settle: &'a mut dyn FnMut(&'p Owner, &'p Action, LaneCollected) -> Option<ActionOutcome>,
    tree: &'a mut PhaseTree<'p, 'g>,
}

impl<'a, 'p, 'g> LaneCollector<'a, 'p, 'g> {
    pub(super) fn new(
        settle: &'a mut dyn FnMut(&'p Owner, &'p Action, LaneCollected) -> Option<ActionOutcome>,
        tree: &'a mut PhaseTree<'p, 'g>,
    ) -> Self {
        Self { settle, tree }
    }

    fn finished(&mut self, owner: &'p Owner, action: &'p Action, collected: LaneCollected) {
        if let Some(outcome) = (self.settle)(owner, action, collected) {
            self.tree.settled(owner, action, outcome);
        }
    }
}

/// One finished action, as the coordinator collected it.
pub(super) struct LaneCollected {
    pub(super) journal_id: Option<i64>,
    pub(super) result: Result<super::apply::ActionRun>,
    pub(super) elapsed: Duration,
    pub(super) notes: Vec<ActionNote>,
    /// The lane's captured child output. Empty whenever a live window already
    /// showed it, and under `Verbosity::Quiet`.
    pub(super) body: Vec<String>,
}

/// What a lane sends the coordinator. One channel for both directions of
/// traffic, because the coordinator must be able to service a state read while
/// it is waiting for the next completion — two channels would need a select
/// `std::sync::mpsc` does not have.
enum LaneMessage {
    ResolvedPrefix {
        manager: String,
        reply: Sender<Result<Option<(String, bool)>>>,
    },
    RecordResolvedPrefix {
        manager: String,
        prefix: String,
        is_fallback: bool,
        reply: Sender<Result<()>>,
    },
    Finished(Box<LaneFinished>),
}

struct LaneFinished {
    slot: usize,
    result: Result<super::apply::ActionRun>,
    elapsed: Duration,
    notes: Vec<ActionNote>,
    body: Vec<String>,
    bootstrapped: Vec<super::packages::BootstrapRecord>,
}

/// A worker's view of the one SQLite connection: a request, and a wait for the
/// coordinator's answer.
///
/// Built inside the worker so no `&dyn PackageStateStore` ever crosses a thread
/// boundary — the trait carries no `Send + Sync` bound, deliberately, and this
/// preserves that.
struct LaneStateProxy {
    tx: Sender<LaneMessage>,
}

impl LaneStateProxy {
    fn unreachable(reason: &str) -> crate::errors::CfgdError {
        StateError::LaneUnreachable {
            reason: reason.to_string(),
        }
        .into()
    }
}

impl PackageStateStore for LaneStateProxy {
    fn resolved_prefix(&self, manager: &str) -> Result<Option<(String, bool)>> {
        let (reply, answers) = channel();
        self.tx
            .send(LaneMessage::ResolvedPrefix {
                manager: manager.to_string(),
                reply,
            })
            .map_err(|_| Self::unreachable("the coordinator stopped reading"))?;
        answers
            .recv()
            .map_err(|_| Self::unreachable("the coordinator sent no answer"))?
    }

    fn record_resolved_prefix(&self, manager: &str, prefix: &str, is_fallback: bool) -> Result<()> {
        let (reply, answers) = channel();
        self.tx
            .send(LaneMessage::RecordResolvedPrefix {
                manager: manager.to_string(),
                prefix: prefix.to_string(),
                is_fallback,
                reply,
            })
            .map_err(|_| Self::unreachable("the coordinator stopped reading"))?;
        answers
            .recv()
            .map_err(|_| Self::unreachable("the coordinator sent no answer"))?
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum SlotState {
    Waiting,
    Running,
    Done,
}

/// One action's standing in the dispatch.
struct Slot<'p> {
    owner: &'p Owner,
    action: &'p Action,
    /// Position in the phase's plan order — the value that becomes the
    /// journal's `action_index`, not the order this slot will finish in.
    plan_index: usize,
    tier: Tier,
    /// The manager this action drives, by its registered name. `None` for an
    /// action that runs no manager command and therefore contends with nothing.
    /// The LANE it occupies is [`Slot::lane`], which is this name's family.
    manager: Option<String>,
    /// The module that owns it, for the `depends` edges.
    module: Option<String>,
    /// The DAG node this action IS, when the phase's ordering is a graph
    /// (`Prerequisites`). `None` for package work, whose ordering is the tier
    /// barrier and module `depends` instead — and the one bit the drain rule
    /// and the failure cascade both read to tell the two gatings apart.
    node: Option<String>,
    /// The nodes this one must follow, as [`ManagerAction::node_id`] values.
    /// Read off the plan rather than re-derived, so the scheduler cannot
    /// disagree with the edges the preview showed. Empty for package work.
    depends_on: &'p [String],
    /// Whether this action INSTALLS through a manager that registers sources
    /// for its family (`brew-tap`). The dispatcher offers these ahead of the
    /// tier barrier and holds the family's other installs behind them: a
    /// formula may only exist in the tap being added by this very run, and
    /// tier order alone would run a module's brew installs before a
    /// profile-declared tap. Install-shaped actions only — that reason does
    /// not apply to a removal, so an untap neither crosses the barrier nor
    /// holds its family. Always `false` for a `Prerequisites` node, whose
    /// ordering is the DAG's.
    registers_sources: bool,
    state: SlotState,
}

/// The ONE wait-line grammar: `<head> · waiting on <thing>`.
///
/// One sentence for every cardinality — the tier in flight for a blocked
/// group, the family lane for a blocked package action, the node ahead of it
/// for a blocked manager node — because they are the same statement at
/// different levels and reading them side by side is the point.
///
/// The head is what is being held: the blocked action's own display subject,
/// in the row that action will start in. It is omitted entirely for a line
/// standing for a whole GROUP, whose heading sits directly above it and is
/// already the head — a group line naming its owner would print the same token
/// twice, one line apart.
fn wait_subject(head: Option<&str>, thing: &str) -> String {
    match head {
        Some(head) => format!("{head} · waiting on {thing}"),
        None => format!("waiting on {thing}"),
    }
}

/// The tier currently in flight, given each tier's count of actions that have
/// not yet completed.
///
/// An **empty** tier is never in flight: it is released and drained in the same
/// instant, having nothing to dispatch. That is the whole reason this is stated
/// as "the tier in flight" rather than "the nearest undrained tier above" — the
/// latter is a property of the plan and would have a run with no module package
/// work announce that it is waiting on modules.
fn tier_in_flight(pending_per_tier: [usize; 2]) -> Option<Tier> {
    Tier::ALL
        .into_iter()
        .find(|tier| pending_per_tier[tier_index(*tier)] > 0)
}

fn tier_index(tier: Tier) -> usize {
    match tier {
        Tier::Modules => 0,
        Tier::Rest => 1,
    }
}

/// A group's standing in the tier barrier, as the wait lines see it.
struct GroupWait<'p> {
    owner: &'p Owner,
    tier: Tier,
    /// The group still has an undispatched action held BEHIND the tier
    /// barrier. A source registration does not count: it is offered across
    /// the barrier, gets its own specific row when blocked, and counting it
    /// here would render its wait twice — once as the coarse tier row and
    /// once as the per-slot row.
    pending: bool,
}

/// One line per group blocked behind the tier in flight, paired with the owner
/// it belongs to — the pairing is what the bar bookkeeping needs, since a
/// group's line is replaced rather than reopened.
///
/// A group in the tier that is in flight is not blocked and renders nothing, so
/// no group can ever name its own tier; under `0 → 1` a module group is
/// dispatched first and is never blocked at all.
fn tier_waits<'g>(groups: &[GroupWait<'g>], in_flight: Option<Tier>) -> Vec<(&'g Owner, String)> {
    let Some(in_flight) = in_flight else {
        return Vec::new();
    };
    let Some(word) = in_flight.wait_word() else {
        return Vec::new();
    };
    groups
        .iter()
        .filter(|g| g.pending && g.tier > in_flight)
        .map(|g| (g.owner, wait_subject(None, word)))
        .collect()
}

/// Every module a module transitively depends on.
fn transitive_depends(modules: &[ResolvedModule]) -> HashMap<&str, HashSet<&str>> {
    let direct: HashMap<&str, &[String]> = modules
        .iter()
        .map(|m| (m.name.as_str(), m.depends.as_slice()))
        .collect();
    let mut closure: HashMap<&str, HashSet<&str>> = HashMap::new();
    for module in modules {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = module.depends.iter().map(String::as_str).collect();
        // Iterative rather than recursive, and guarded by `seen`: the module
        // resolver rejects cycles, but a dispatcher that hung on one would be a
        // far worse failure than one that simply stops expanding.
        while let Some(next) = stack.pop() {
            if !seen.insert(next) {
                continue;
            }
            if let Some(deps) = direct.get(next) {
                stack.extend(deps.iter().map(String::as_str));
            }
        }
        closure.insert(module.name.as_str(), seen);
    }
    closure
}

/// Whether an action for `manager` drains the phase.
///
/// The predicate is "registered and not currently available", keyed on the
/// manager's own state rather than on which action kind names it: a module
/// install can reach an unavailable manager just as a profile install can,
/// and both must serialize around it the same way. An UNREGISTERED name —
/// the `script` pseudo-manager, or a typo — is never unavailable in this
/// sense and so drains nothing.
fn drains_phase(registry: &ProviderRegistry, manager: &str) -> bool {
    registry
        .package_managers()
        .iter()
        .any(|pm| pm.name() == manager && !pm.is_available())
}

impl<'p> Slot<'p> {
    fn drains(&self, registry: &ProviderRegistry) -> bool {
        // A `Prerequisites` node is exempt: the gate asks "is this manager
        // missing", and a provision's answer is yes until the moment it
        // succeeds. Draining on it would serialize the whole graph — see the
        // module doc's rule 5.
        if self.node.is_some() {
            return false;
        }
        self.manager
            .as_deref()
            .is_some_and(|m| drains_phase(registry, m))
    }

    /// The lane this action occupies: the manager FAMILY, not the registered
    /// name. `brew`, `brew-tap` and `brew-cask` are three managers over one
    /// binary and one prefix, so keying on the name would run three concurrent
    /// `brew` processes — which is exactly what one-operation-per-manager
    /// exists to prevent. Display, the journal and the sub-gate all keep the
    /// registered name; only the mutual exclusion is per family.
    ///
    /// A provision lanes on its `via`, not on the manager it delivers: the
    /// command that runs is the METHOD's (`provision npm via apt` is an
    /// `apt-get install`), and two provisions mediated by the same system
    /// manager laned on their own names hold that manager's lock against each
    /// other — `provision pipx via apt` died on the dpkg lock the npm
    /// provision's own apt-get was holding. A standalone method ("homebrew
    /// installer", "rustup") lanes on its phrase, which collides with nothing.
    /// Ordering against the delivered manager's later work needs no lane: a
    /// package install depends on its manager's provision by DAG edge.
    fn lane(&self) -> Option<&str> {
        if let Action::Manager(ManagerAction::Provision { via, .. }) = self.action {
            return Some(crate::manager_family(via));
        }
        self.manager.as_deref().map(crate::manager_family)
    }
}

/// Whether `action` INSTALLS through a manager that registers sources for its
/// family — the derivation behind [`Slot::registers_sources`].
///
/// Install-shaped arms only: the hoist-and-hold exists because a formula may
/// only exist in the repository the tap adds, and that reason does not apply
/// to a removal — an untap hoisted across the barrier would run BEFORE the
/// installs that still resolve through the tap it removes. A `Prerequisites`
/// node is excluded by shape (neither arm matches), keeping its ordering the
/// DAG's.
fn registers_family_sources(action: &Action, registry: &ProviderRegistry) -> bool {
    if !matches!(
        action,
        Action::Package(PackageAction::Install { .. })
            | Action::Module(ModuleAction {
                kind: ModuleActionKind::InstallPackages { .. },
                ..
            })
    ) {
        return false;
    }
    action_manager(action).is_some_and(|manager| {
        registry
            .package_managers()
            .iter()
            .any(|pm| pm.name() == manager && pm.registers_family_sources())
    })
}

/// Whether every action of `slot`'s module's transitive dependencies has
/// completed.
fn depends_satisfied(
    slots: &[Slot<'_>],
    index: usize,
    deps: &HashMap<&str, HashSet<&str>>,
) -> bool {
    let Some(module) = slots[index].module.as_deref() else {
        return true;
    };
    let Some(needs) = deps.get(module) else {
        return true;
    };
    if needs.is_empty() {
        return true;
    }
    !slots.iter().any(|s| {
        s.state != SlotState::Done
            && s.module
                .as_deref()
                .is_some_and(|owner| needs.contains(owner))
    })
}

/// Whether every node `slots[index]` must follow has completed.
///
/// An edge naming a node that is not in this dispatch counts as satisfied. A
/// phase filter selects actions, not sub-graphs, so a user who asked for one
/// half of the phase can leave an edge pointing at a node that was never going
/// to run — and a dispatcher that waited for it would stall the run the user
/// asked for rather than perform it.
///
/// A FAILED dependency is `Done` like any other, which is why this predicate
/// says nothing about failure: [`fail_dependents`] settles every node
/// downstream of a failure before the next dispatch pass, so a slot whose
/// dependency failed is already `Done` and is never offered here.
fn dag_satisfied(slots: &[Slot<'_>], index: usize) -> bool {
    blocking_node(slots, index).is_none()
}

/// The node `slots[index]` must wait for, and the one its wait line NAMES:
/// the last of its unsatisfied edges to finish.
///
/// The last, not the first in flight — a line naming a blocker that clears
/// while the node stays put has named something that was not in the way. What
/// finishes last is not knowable from here, so it is ordered by how far each
/// blocker still is from done: one that has not started finishes after one
/// already running, and a tie goes to the later of them in plan order. Edges
/// are direct, so a node behind a chain names the node immediately ahead of
/// it and that node names the one ahead of IT — each line stating the next
/// thing that has to happen rather than three lines repeating the root.
///
/// Also the whole of [`dag_satisfied`], so the dispatcher's gate and the
/// renderer's attribution can never disagree about whether a node is held.
fn blocking_node<'s, 'p>(slots: &'s [Slot<'p>], index: usize) -> Option<&'s Slot<'p>> {
    slots[index]
        .depends_on
        .iter()
        .filter_map(|dependency| {
            slots.iter().find(|s| {
                s.state != SlotState::Done && s.node.as_deref() == Some(dependency.as_str())
            })
        })
        .max_by_key(|slot| (slot.state == SlotState::Waiting, slot.plan_index))
}

/// What a node is CALLED on the line of a node it takes down.
///
/// A prerequisite is named by its TOOL rather than by the manager running the
/// install: `apt install curl` failing is curl not arriving, and curl is what
/// the dependent was waiting for. Everything else is named by its manager.
fn node_subject(action: &Action) -> Option<&str> {
    match action {
        Action::Manager(ManagerAction::Prerequisite { tool, .. }) => Some(tool.as_str()),
        Action::Manager(node) => Some(node.manager()),
        _ => None,
    }
}

/// Why a dispatch stopped with planned work still unanswered.
///
/// Both arms report every outstanding slot, because an action that neither the
/// exit code nor the tree ever hears about is a shortfall the run walks away
/// from green. A cooperative ABORT is deliberately not one of them: there the
/// run's own status is `Aborted` and the rollup names the shortfall as
/// `{applied} of {planned}`, so the dispatcher reports only what it began.
enum Unrun {
    /// `pick_next` left work `Waiting` with nothing running to unblock it — a
    /// coordinator invariant failure.
    Stalled,
    /// The inbox disconnected: every worker handle was dropped without a
    /// `Finished` message ever landing, which the lane's own panic guard
    /// cannot see.
    Lost,
}

impl Unrun {
    fn error(&self, manager: String) -> crate::errors::CfgdError {
        match self {
            Unrun::Stalled => PackageError::LaneStalled { manager }.into(),
            Unrun::Lost => PackageError::LaneLost { manager }.into(),
        }
    }
}

/// Answer every slot the dispatch never settled, in plan order.
///
/// The ONE rule for outstanding work, applied to dependents and siblings alike:
/// a slot still `Waiting` because nothing ever offered it a lane and a slot
/// still `Running` because its worker vanished are both actions the run
/// planned and did not complete, and each gets a line of its own naming why.
/// Marked `Done` as it goes, so a second exit path cannot report one twice.
fn settle_unrun<'p>(
    slots: &mut [Slot<'p>],
    reason: &Unrun,
    journal_ids: &mut HashMap<usize, Option<i64>>,
    collect: &mut LaneCollector<'_, 'p, '_>,
) {
    for (index, slot) in slots.iter_mut().enumerate() {
        if slot.state == SlotState::Done {
            continue;
        }
        slot.state = SlotState::Done;
        let manager = slot.manager.clone().unwrap_or_default();
        collect.finished(
            slot.owner,
            slot.action,
            LaneCollected {
                journal_id: journal_ids.remove(&index).flatten(),
                result: Err(reason.error(manager)),
                elapsed: Duration::ZERO,
                notes: Vec::new(),
                body: Vec::new(),
            },
        );
    }
}

/// Settle every node downstream of the failure at `root` as a failure of its
/// own, without running any of them.
///
/// Attribution is the ROOT rather than the nearest dependency: a `provision
/// npm` held up by a missing curl says curl, because the reader's next move is
/// to fix curl and three lines each blaming the line above it say the same
/// thing three times without ever naming what to do. Sweeps run in slot order —
/// the plan's own order — and repeat until nothing more is reachable, so a
/// chain of any depth is settled in one call and always in the same sequence.
fn fail_dependents<'p>(
    slots: &mut [Slot<'p>],
    root: usize,
    collect: &mut LaneCollector<'_, 'p, '_>,
) {
    let root_action = slots[root].action;
    let Some(root_node) = slots[root].node.clone() else {
        return;
    };
    let cause = node_subject(root_action)
        .unwrap_or(root_node.as_str())
        .to_string();
    let mut failed: Vec<String> = vec![root_node];
    loop {
        let mut progressed = false;
        for slot in slots.iter_mut() {
            if slot.state != SlotState::Waiting
                || !slot
                    .depends_on
                    .iter()
                    .any(|dependency| failed.iter().any(|f| f == dependency))
            {
                continue;
            }
            // Marked `Done` rather than left `Waiting`: an uncollected slot is
            // what the stall check exists to catch, and this one is not
            // stalled — it has been answered.
            slot.state = SlotState::Done;
            if let Some(node) = slot.node.clone() {
                failed.push(node);
            }
            progressed = true;
            collect.finished(
                slot.owner,
                slot.action,
                LaneCollected {
                    journal_id: None,
                    result: Err(PackageError::DependencyFailed {
                        dependency: cause.clone(),
                    }
                    .into()),
                    elapsed: Duration::ZERO,
                    notes: Vec::new(),
                    body: Vec::new(),
                },
            );
        }
        if !progressed {
            return;
        }
    }
}

/// What the dispatcher holds while it decides.
struct DispatchState<'a> {
    lanes_busy: &'a HashSet<String>,
    /// Occupancy count per owner token, not a set: an owner can hold two
    /// lanes at once (a module declaring both brew and apt work), and the
    /// fairness rule at `:31-35` must keep treating it as busy until BOTH
    /// finish, not just the first.
    owners_busy: &'a HashMap<String, usize>,
    /// An action whose manager is not currently available is in flight, so the
    /// phase is drained until it completes.
    draining: bool,
    running: usize,
}

/// The lanes whose next occupant must be a source registration: a family
/// with a dispatchable tap waiting refuses its installs until the tap has
/// run, because a formula may only exist in the repository the tap adds.
/// Judged on depends/dag alone — fail OPEN: a tap held by a module
/// dependency edge does not hold its family, or the edge and the family
/// could each be waiting on the other.
///
/// Shared by [`pick_next`] (the hold itself) and [`held_waits`] (the row
/// saying so), so the dispatcher and the renderer can never disagree about
/// which families are held.
fn source_held_lanes<'s>(
    slots: &'s [Slot<'_>],
    deps: &HashMap<&str, HashSet<&str>>,
) -> HashSet<&'s str> {
    slots
        .iter()
        .enumerate()
        .filter(|(index, slot)| {
            slot.state == SlotState::Waiting
                && slot.registers_sources
                && depends_satisfied(slots, *index, deps)
                && dag_satisfied(slots, *index)
        })
        .filter_map(|(_, slot)| slot.lane())
        .collect()
}

/// The next action to dispatch, or `None` when nothing may start right now.
fn pick_next(
    slots: &[Slot<'_>],
    registry: &ProviderRegistry,
    deps: &HashMap<&str, HashSet<&str>>,
    state: &DispatchState<'_>,
) -> Option<usize> {
    if state.draining {
        return None;
    }
    let pending = [
        pending_in(slots, Tier::Modules),
        pending_in(slots, Tier::Rest),
    ];
    let in_flight = tier_in_flight(pending)?;

    // An action its own owner is already busy for. Held back until every other
    // owner with ready work has taken a lane, and dispatched after that rather
    // than idling a free lane — so a module declaring brew and apt work does
    // not take both and leave another module's only manager standing idle,
    // while a lone owner still fills every lane in the phase.
    let mut owner_busy: Option<usize> = None;

    let source_held = source_held_lanes(slots, deps);

    // Two passes over plan order: source registrations first, across BOTH
    // tiers — the tier barrier orders module work before profile work, and a
    // profile-declared tap serviced in tier order would run after the module
    // brew installs whose formulas it delivers. Everything else keeps the
    // tier gate.
    let scan = slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.registers_sources)
        .chain(
            slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| !slot.registers_sources),
        );
    for (index, slot) in scan {
        if slot.state != SlotState::Waiting {
            continue;
        }
        if !slot.registers_sources {
            if slot.tier != in_flight {
                continue;
            }
            if slot.lane().is_some_and(|l| source_held.contains(l)) {
                continue;
            }
        }
        if !depends_satisfied(slots, index, deps) {
            continue;
        }
        if !dag_satisfied(slots, index) {
            continue;
        }
        if slot.drains(registry) {
            // Mutual exclusion, not an ordering: a draining action waits for an
            // empty phase, and nothing may start while it runs — otherwise a
            // later action would resolve a binary through a PATH the bootstrap
            // has not finished populating. Returning here rather than falling
            // through to `owner_busy` is what makes it a hard stop.
            if slot.registers_sources && slot.tier != in_flight {
                // A barrier-crossing tap whose manager needs bootstrapping
                // must not quiesce the tiers ahead of it: its own family is
                // already held (`source_held`), so only that family waits,
                // and the tap still starts only on a quiet phase.
                if state.running == 0 {
                    return Some(index);
                }
                continue;
            }
            return (state.running == 0).then_some(index);
        }
        if slot.lane().is_some_and(|l| state.lanes_busy.contains(l)) {
            continue;
        }
        if state.owners_busy.contains_key(&slot.owner.token()) {
            owner_busy.get_or_insert(index);
            continue;
        }
        return Some(index);
    }
    owner_busy
}

fn pending_in(slots: &[Slot<'_>], tier: Tier) -> usize {
    slots
        .iter()
        .filter(|s| s.tier == tier && s.state != SlotState::Done)
        .count()
}

/// The scheduler state a wait line is derived from.
struct WaitInputs<'a, 'p> {
    slots: &'a [Slot<'p>],
    groups: &'a [(&'p Owner, Tier)],
    deps: &'a HashMap<&'a str, HashSet<&'a str>>,
    lanes_busy: &'a HashSet<String>,
}

/// Every distinct owner in the phase, in the order the dispatch offers them,
/// each paired with the earliest tier it has work in.
///
/// The MINIMUM rather than the first slot's tier, for the same reason
/// `Tier::of` is written to make one action's tier unambiguous: an owner is
/// released when its earliest work is, so an owner that somehow spanned two
/// tiers would otherwise be told it is waiting on a tier it is already running
/// in.
fn groups_of<'p>(slots: &[Slot<'p>]) -> Vec<(&'p Owner, Tier)> {
    let mut order: Vec<&'p Owner> = Vec::new();
    let mut earliest: HashMap<String, Tier> = HashMap::new();
    for slot in slots {
        let token = slot.owner.token();
        match earliest.get_mut(&token) {
            Some(tier) => *tier = (*tier).min(slot.tier),
            None => {
                order.push(slot.owner);
                earliest.insert(token, slot.tier);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|owner| earliest.get(&owner.token()).map(|tier| (owner, *tier)))
        .collect()
}

impl super::Reconciler<'_> {
    /// Run one phase's concurrent actions across per-manager lanes, handing each
    /// finished action to `collect` on this thread, in completion order.
    ///
    /// Returns the abort exit code when a signal stopped the dispatch.
    pub(super) fn dispatch_lanes<'p>(
        &self,
        dispatch: &[(&'p Owner, &'p Action, usize)],
        run: &LaneRun<'_>,
        collect: &mut LaneCollector<'_, 'p, '_>,
    ) -> Option<u8> {
        // See the module doc's "caller must not hold `path_env_mutation_guard()`"
        // section: a worker's own `path_env_read_guard()` would block forever
        // behind this thread's write guard once this thread parks in
        // `inbox.recv()` below. Fail fast here rather than hang there.
        #[cfg(any(test, feature = "test-helpers"))]
        debug_assert!(
            !crate::test_helpers::path_env_exclusive_guard_held(),
            "dispatch_lanes() called while this thread already holds \
             path_env_mutation_guard() — a lane worker's path_env_read_guard() \
             would deadlock behind it once this thread parks in inbox.recv(). \
             Release the mutation guard before calling apply()."
        );

        let mut slots: Vec<Slot<'p>> = dispatch
            .iter()
            .map(|(owner, action, plan_index)| Slot {
                owner,
                action,
                plan_index: *plan_index,
                tier: Tier::of(owner),
                manager: action_manager(action).map(str::to_string),
                module: match action {
                    Action::Module(ModuleAction { module_name, .. }) => Some(module_name.clone()),
                    _ => None,
                },
                node: match action {
                    Action::Manager(node) => Some(node.node_id()),
                    _ => None,
                },
                depends_on: match action {
                    Action::Manager(node) => node.depends_on(),
                    _ => &[],
                },
                registers_sources: registers_family_sources(action, self.registry),
                state: SlotState::Waiting,
            })
            .collect();
        let deps = transitive_depends(run.module_actions);
        let groups = groups_of(&slots);
        let registry = self.registry;

        let mut lanes_busy: HashSet<String> = HashSet::new();
        let mut owners_busy: HashMap<String, usize> = HashMap::new();
        // The slot of the draining action in flight, if any. Recorded by slot
        // rather than recomputed at collection, because a Prerequisites
        // `Provision` node's whole point is that its manager IS available by
        // the time it finishes.
        let mut draining: Option<usize> = None;
        let mut running: usize = 0;
        let mut aborted: Option<u8> = None;
        // Set when the loop leaves planned work unanswered for a reason that is
        // not a cooperative abort. Settled once, below the loop, so the two
        // exits that can do it cannot answer a slot differently — or twice.
        let mut unrun: Option<Unrun> = None;
        let mut journal_ids: HashMap<usize, Option<i64>> = HashMap::new();

        let (tx, inbox) = channel::<LaneMessage>();
        // `None` once no `Waiting` slot remains (or the run aborts): the
        // coordinator will never clone a sender again, so it drops its own
        // handle and lets `inbox.recv()` disconnect instead of blocking
        // forever behind a worker that dies without sending `Finished` — see
        // the drop below and the `Err(_)` arm at the bottom of this loop.
        let mut tx: Option<Sender<LaneMessage>> = Some(tx);

        std::thread::scope(|scope| {
            loop {
                // Cooperative cancellation is checked before anything NEW is
                // dispatched; whatever is already in flight finishes, so no
                // action is left half-applied.
                if aborted.is_none()
                    && let Some(code) = run.abort.aborted()
                {
                    aborted = Some(code);
                }

                if aborted.is_none() {
                    while let Some(index) = pick_next(
                        &slots,
                        registry,
                        &deps,
                        &DispatchState {
                            lanes_busy: &lanes_busy,
                            owners_busy: &owners_busy,
                            draining: draining.is_some(),
                            running,
                        },
                    ) {
                        let action = slots[index].action;
                        let owner = slots[index].owner;
                        let action_index = run.plan_index_base + slots[index].plan_index;
                        let journal_id = self.begin_package_journal(run, action, action_index);
                        // The dispatch-time read the script-install arm needs.
                        // Done here rather than in the worker because it is a
                        // SQLite read, and current because a bootstrap has
                        // always been collected before anything that could
                        // observe its directories is dispatched.
                        let path_dirs = super::all_recorded_path_dirs(self.state);
                        // The action's row goes RUNNING, in the place the tree
                        // has been showing it — which is where its line will
                        // settle too, so nothing about it moves. The row names
                        // the action alone: its owner is the heading above it.
                        let lane = collect.tree.dispatched(owner, action);
                        let Some(worker_tx) = tx.clone() else {
                            // Unreachable: `pick_next` returns an index only
                            // while some slot is still `Waiting`, which is
                            // exactly the condition under which the
                            // coordinator still holds `tx` (see the drop
                            // below). No slot state has been mutated yet, so
                            // breaking here costs nothing but a redundant
                            // dispatch pass rather than panicking.
                            break;
                        };

                        slots[index].state = SlotState::Running;
                        running += 1;
                        if slots[index].drains(registry) {
                            draining = Some(index);
                        }
                        if let Some(lane) = slots[index].lane() {
                            lanes_busy.insert(lane.to_string());
                        }
                        *owners_busy.entry(slots[index].owner.token()).or_insert(0) += 1;
                        let manager = slots[index].manager.clone().unwrap_or_default();
                        journal_ids.insert(index, journal_id);
                        // Captured on the coordinator's thread and re-installed
                        // as the worker's first statement, the same shape as
                        // `spawn_blocking_with_test_home`: `TEST_HOME_OVERRIDE`
                        // is a thread-local, so a `scope.spawn` worker — a
                        // fresh thread — does not inherit it, and a test's
                        // `Packages` action would otherwise resolve `~` against
                        // the developer's real `$HOME`.
                        let test_home = crate::test_home_override();
                        scope.spawn(move || {
                            let _test_home_guard =
                                test_home.as_deref().map(crate::with_test_home_guard);
                            // Held across `run_one_action` below, which resolves
                            // a package manager's own binary, `git`, and script
                            // interpreters — all PATH reads. See the module
                            // doc's "caller must not hold
                            // `path_env_mutation_guard()`" section: this is the
                            // read half of the lock that debug_assert checks
                            // for at the top of this function. Compiled out of
                            // release builds, like every other PATH-reading
                            // call site.
                            #[cfg(any(test, feature = "test-helpers"))]
                            let _path_guard = crate::test_helpers::path_env_read_guard();
                            // The whole worker, not just the action body, must
                            // be panic-safe: `run_one_action` already
                            // catch_unwinds the action itself, but
                            // `lane.finish()` runs OUTSIDE that boundary. Two
                            // separate guards, not one wrapping both calls,
                            // because `finished` (and the bootstrap PATH rows
                            // inside it) must survive a panic in
                            // `lane.finish()` — folding both calls into one
                            // `catch_unwind` would drop `finished` along with
                            // the unwind and lose them even though
                            // `run_one_action` already returned successfully.
                            // Either guard tripping still sends a `Finished`
                            // message, so `running` always decrements and the
                            // coordinator never blocks in `inbox.recv()`
                            // waiting on a worker that died silently.
                            let started = Instant::now();
                            let panic_manager = manager.clone();
                            let action_tx = worker_tx.clone();
                            let executed =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    run_one_action(
                                        registry, run, action, &lane, &action_tx, &manager,
                                        &path_dirs,
                                    )
                                }));
                            let message = match executed {
                                Ok(finished) => {
                                    let body = std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| lane.finish()),
                                    )
                                    .unwrap_or_default();
                                    LaneFinished {
                                        slot: index,
                                        result: finished.result,
                                        elapsed: finished.elapsed,
                                        notes: finished.notes,
                                        body,
                                        bootstrapped: finished.bootstrapped,
                                    }
                                }
                                Err(_) => LaneFinished {
                                    slot: index,
                                    result: Err(PackageError::LanePanicked {
                                        manager: panic_manager,
                                    }
                                    .into()),
                                    elapsed: started.elapsed(),
                                    notes: Vec::new(),
                                    body: Vec::new(),
                                    bootstrapped: Vec::new(),
                                },
                            };
                            let _ = worker_tx.send(LaneMessage::Finished(Box::new(message)));
                        });
                    }
                    collect.tree.waiting(&held_waits(&WaitInputs {
                        slots: &slots,
                        groups: &groups,
                        deps: &deps,
                        lanes_busy: &lanes_busy,
                    }));
                }
                // Nothing new dispatches after an abort, so no refresh follows
                // it: every row still describing work that has not started is
                // a claim about work that never will, and the tree retires all
                // of them when it closes.

                // Nothing will ever clone `tx` again once no slot remains
                // `Waiting` (or the run has aborted, which stops new
                // dispatch outright). Dropping the coordinator's own handle
                // here is what lets `inbox.recv()` below actually
                // disconnect if `running` is ever wrong — a worker killed by
                // something `catch_unwind` cannot see — instead of blocking
                // on a channel only the coordinator itself still holds open.
                if tx.is_some()
                    && (aborted.is_some() || !slots.iter().any(|s| s.state == SlotState::Waiting))
                {
                    tx = None;
                }

                if running == 0 {
                    // `pick_next` returned nothing runnable and no worker is
                    // in flight to unblock it — a coordinator invariant
                    // failure (a family lane held by a slot that will never
                    // finish, an owner-fairness rule with no other owner
                    // left to alternate to). An abort in progress already
                    // explains an empty `running`; this is the OTHER way to
                    // get here, and every slot still `Waiting` never enters
                    // `collect` on its own, so without `settle_unrun` it
                    // vanishes from both the exit code (`results` never sees
                    // it) and the rendered tree (`recorded` never sees it) —
                    // a run that reached none of its plan would otherwise
                    // print `✓ Apply complete` and exit 0.
                    if aborted.is_none() {
                        unrun = Some(Unrun::Stalled);
                    }
                    break;
                }

                match inbox.recv() {
                    Ok(LaneMessage::ResolvedPrefix { manager, reply }) => {
                        let _ = reply.send(self.state.resolved_prefix(&manager));
                    }
                    Ok(LaneMessage::RecordResolvedPrefix {
                        manager,
                        prefix,
                        is_fallback,
                        reply,
                    }) => {
                        let _ = reply.send(self.state.record_resolved_prefix(
                            &manager,
                            &prefix,
                            is_fallback,
                        ));
                    }
                    Ok(LaneMessage::Finished(done)) => {
                        let index = done.slot;
                        slots[index].state = SlotState::Done;
                        running -= 1;
                        if draining == Some(index) {
                            draining = None;
                        }
                        if let Some(lane) = slots[index].lane() {
                            lanes_busy.remove(lane);
                        }
                        if let std::collections::hash_map::Entry::Occupied(mut entry) =
                            owners_busy.entry(slots[index].owner.token())
                        {
                            *entry.get_mut() -= 1;
                            if *entry.get() == 0 {
                                entry.remove();
                            }
                        }
                        self.persist_bootstraps(done.bootstrapped);
                        let failed = done.result.is_err();
                        collect.finished(
                            slots[index].owner,
                            slots[index].action,
                            LaneCollected {
                                journal_id: journal_ids.remove(&index).flatten(),
                                result: done.result,
                                elapsed: done.elapsed,
                                notes: done.notes,
                                body: done.body,
                            },
                        );
                        // Settled BEFORE the next dispatch pass, so a node whose
                        // dependency just failed is never offered a lane: the
                        // thing it was waiting to be handed does not exist, and
                        // running it anyway is the silent bootstrap this phase
                        // replaces.
                        //
                        // Not once the run is ABORTING, though: from there
                        // nothing further will be dispatched at all, so a
                        // cascade would put a failure line under the dependents
                        // of this one node while every other action the abort
                        // stopped renders nothing — two rules for the same
                        // "planned, never began" fact inside one run. An
                        // aborted run reports what it began and leaves the
                        // shortfall to the rollup, for dependents and siblings
                        // alike.
                        if failed {
                            // Re-read rather than reuse the sample taken at the
                            // top of this iteration: that read happened before
                            // this action ran, and what the cascade needs to
                            // know is whether the run is stopping NOW.
                            if aborted.is_none() {
                                aborted = run.abort.aborted();
                            }
                            if aborted.is_none() {
                                fail_dependents(&mut slots, index, collect);
                            }
                        }
                    }
                    // Reachable only once the coordinator has dropped its own
                    // `tx` above AND every worker's clone is also gone
                    // without a `Finished` ever landing — a worker killed by
                    // something the panic guard in the spawned closure
                    // cannot see. Whatever is still outstanding never ran to
                    // completion and is reported as such, for the same reason
                    // the stall above is: this is not an abort, so nothing
                    // else in the run will account for it.
                    Err(_) => {
                        if aborted.is_none() {
                            unrun = Some(Unrun::Lost);
                        }
                        break;
                    }
                }
            }

            // Every outstanding slot is answered before the tree closes, so
            // the row a stalled action has been waiting in settles as the
            // failure it is rather than being retired unexplained.
            if let Some(reason) = &unrun {
                settle_unrun(&mut slots, reason, &mut journal_ids, collect);
            }
        });

        aborted
    }

    /// Open one action's journal row at its dispatch point. A coordinator-only
    /// write, like every other SQLite access in the phase.
    fn begin_package_journal(
        &self,
        run: &LaneRun<'_>,
        action: &Action,
        action_index: usize,
    ) -> Option<i64> {
        let description = format_action_description(action);
        let (action_type, resource_id) = parse_resource_from_description(&description);
        self.state
            .journal_begin(
                run.apply_id,
                action_index,
                run.phase.as_str(),
                &action_type,
                &resource_id,
                None,
            )
            .ok()
    }
}

/// What the scheduler is holding back, and why — the complete set, in the
/// order the tree wants the lines on screen.
///
/// Recomputed rather than incrementally patched: a wait line is a statement
/// about the scheduler's current state, and the state is small.
///
/// A free function over the dispatch state and nothing else, so a test can
/// drive it from synthetic slots without a state store, an apply, or a
/// terminal — what a wait line SAYS is decided here, and where it is drawn is
/// [`super::live_tree`]'s.
fn held_waits<'p>(inputs: &WaitInputs<'_, 'p>) -> Held<'p> {
    let slots = inputs.slots;
    let pending = [
        pending_in(slots, Tier::Modules),
        pending_in(slots, Tier::Rest),
    ];
    let in_flight = tier_in_flight(pending);

    let waits: Vec<GroupWait<'_>> = inputs
        .groups
        .iter()
        .map(|(owner, tier)| GroupWait {
            owner,
            tier: *tier,
            pending: slots.iter().any(|s| {
                s.state == SlotState::Waiting
                    && !s.registers_sources
                    && s.owner.token() == owner.token()
            }),
        })
        .collect();
    // Sealing counts EVERY waiting slot, source registrations included: the
    // tap's group must stay open to gain and settle the tap's own row, even
    // when the tap is the only work the group has left.
    let pending_owners: Vec<String> = inputs
        .groups
        .iter()
        .filter(|(owner, _)| {
            slots
                .iter()
                .any(|s| s.state == SlotState::Waiting && s.owner.token() == owner.token())
        })
        .map(|(owner, _)| owner.token())
        .collect();
    // A `Vec` throughout, never a `HashMap`: the order these are built in IS
    // the top-to-bottom order the tree draws new rows in, and collecting
    // through a `HashMap` reshuffled every fresh set per process.
    let mut rows: Vec<Wait<'p>> = tier_waits(&waits, in_flight)
        .into_iter()
        .map(|(owner, subject)| Wait {
            owner,
            action: None,
            subject,
        })
        .collect();

    let source_held = source_held_lanes(slots, inputs.deps);

    // `slots` is walked in plan order, so the rows built from it keep it.
    for (index, slot) in slots.iter().enumerate() {
        if slot.state != SlotState::Waiting {
            continue;
        }
        // A source registration is exempt from the tier filter for the same
        // reason `pick_next` offers it across the barrier: a profile-declared
        // tap can be blocked while the module tier is in flight, and filtering
        // it out here would leave the one action holding a whole family absent
        // from the live region for the length of its wait.
        if Some(slot.tier) != in_flight && !slot.registers_sources {
            continue;
        }
        if !depends_satisfied(slots, index, inputs.deps) {
            continue;
        }
        // What holds this slot, in the order the dispatcher applies: an edge
        // it has not cleared, else the family lane something else is running
        // in. A node behind an edge is NOT waiting on its lane — it may not
        // even have one yet — so naming the lane there would name a blocker
        // that is not in the way, and saying nothing at all would leave the
        // node absent from the live region for the whole of its wait.
        let on = match blocking_node(slots, index) {
            Some(blocker) => {
                // Only a manager node carries edges, so a blocker always has a
                // name. If that ever stops holding, say nothing rather than
                // "waiting on " with the claim's object missing.
                let named = node_subject(blocker.action);
                debug_assert!(
                    named.is_some(),
                    "a node is blocked by an action with no subject to name"
                );
                let Some(on) = named else {
                    continue;
                };
                on.to_string()
            }
            None => {
                // An owner mid-action in another lane still gets this line:
                // one row per in-flight action AND one per blocked action is
                // the whole point of the grammar, and the window it describes
                // — a module holding brew while another holds apt — is the one
                // the wait line exists for.
                // A busy lane, or one held for a dispatchable tap: an install
                // refused because its family's next occupant must be a source
                // registration is waiting on that family exactly as it would
                // be on a running action — with nothing in `lanes_busy` at
                // all, its wait was otherwise invisible. The tap itself is
                // exempt from the second half: it IS the hold, and would name
                // itself as its own blocker.
                let Some(lane) = slot.lane().filter(|lane| {
                    inputs.lanes_busy.contains(*lane)
                        || (!slot.registers_sources && source_held.contains(*lane))
                }) else {
                    continue;
                };
                // The lane, not the registered name: an action for `brew-cask`
                // held back by a running `brew` is waiting on brew.
                lane.to_string()
            }
        };
        rows.push(Wait {
            owner: slot.owner,
            action: Some(slot.action),
            subject: wait_subject(Some(&action_display_subject(slot.action).to_string()), &on),
        });
    }
    Held {
        waits: rows,
        pending_owners,
    }
}

/// What a worker hands back.
struct LaneWorkerResult {
    result: Result<super::apply::ActionRun>,
    elapsed: Duration,
    notes: Vec<ActionNote>,
    bootstrapped: Vec<super::packages::BootstrapRecord>,
}

/// One action, executed on a worker thread with no `&Reconciler` in reach.
fn run_one_action(
    registry: &ProviderRegistry,
    run: &LaneRun<'_>,
    action: &Action,
    lane: &dyn LaneOutput,
    tx: &Sender<LaneMessage>,
    manager: &str,
    path_dirs: &[String],
) -> LaneWorkerResult {
    let started = Instant::now();
    let proxy = LaneStateProxy { tx: tx.clone() };
    // A note produced here belongs to THIS action, so the sink is this
    // action's: a shared one would attach a lane's caveat to whichever
    // action happened to be collected next.
    let notes = NoteSink::default();
    // Built OUTSIDE the panic guard, and only borrowed inside it: a bootstrap
    // this exec already performed lands on disk (and on this process's PATH
    // registry) before the panic that might follow it, so `exec` — and the
    // records inside it — must survive the unwind for `take_bootstrapped()`
    // below to still see them. Moving construction inside the guarded
    // closure, as it once did, drops `exec` along with the panic and loses
    // every bootstrap the action performed just before failing.
    let exec = PackageExec::new(registry, &proxy, run.printer, &notes).in_lane(lane);
    let executed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match action {
        Action::Package(pkg) => exec.apply_package_action(pkg),
        Action::Manager(node) => exec.apply_manager_action(node),
        Action::Module(
            module @ ModuleAction {
                kind: ModuleActionKind::InstallPackages { resolved },
                ..
            },
        ) => exec.install_module_packages(
            module,
            resolved,
            &ModuleInstallContext {
                config_dir: run.config_dir,
                resolved: run.resolved,
                module_actions: run.module_actions,
                context: run.context,
                shell_override: run.shell_override,
                abort: run.abort,
                path_dirs,
            },
        ),
        // The dispatched set holds only package and manager work by
        // construction (`phase_for_module_kind` routes module work, and the
        // caller partitions the phase), so this arm exists for totality.
        other => Ok(super::apply::ActionRun::new(
            format_action_description(other),
            false,
        )),
    }));
    let result = match executed {
        Ok(result) => result,
        Err(_) => Err(PackageError::LanePanicked {
            manager: manager.to_string(),
        }
        .into()),
    };
    LaneWorkerResult {
        result,
        elapsed: started.elapsed(),
        notes: notes.take(),
        // Read unconditionally: `exec` was never moved into the guarded
        // closure, so it — and whatever it recorded before a panic — is
        // still here to drain whether or not the action itself panicked.
        bootstrapped: exec.take_bootstrapped(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::SectionGuard;
    use crate::output::lane::LaneHandle;

    /// The wait lines the live region would show, in the order given, each
    /// paired with the group it belongs under — the sentence no longer names
    /// its owner, so the pairing is the only thing that says whose line it is.
    fn subjects(groups: &[GroupWait<'_>], in_flight: Option<Tier>) -> Vec<(String, String)> {
        tier_waits(groups, in_flight)
            .into_iter()
            .map(|(owner, subject)| (owner.token(), subject))
            .collect()
    }

    fn group<'p>(owner: &'p Owner, tier: Tier, pending: bool) -> GroupWait<'p> {
        GroupWait {
            owner,
            tier,
            pending,
        }
    }

    #[test]
    fn blocked_group_renders_one_wait_line() {
        // Cardinality is the assertion: a group with four waiting actions is
        // one group waiting on one thing, and four copies of that sentence
        // would be noise.
        let profile = Owner::profile("work");
        let managers = Owner::cfgd("managers");
        let subjects = subjects(
            &[
                group(&managers, Tier::Rest, true),
                group(&profile, Tier::Rest, true),
            ],
            Some(Tier::Modules),
        );

        assert_eq!(
            subjects,
            vec![
                (
                    "cfgd:managers".to_string(),
                    "waiting on modules".to_string()
                ),
                ("profile:work".to_string(), "waiting on modules".to_string()),
            ]
        );
    }

    #[test]
    fn a_module_group_never_renders_a_wait_line() {
        // Tier 0 is dispatched first, so no group can ever name its own tier.
        let nvim = Owner::module("nvim");
        assert!(subjects(&[group(&nvim, Tier::Modules, true)], Some(Tier::Modules)).is_empty());
    }

    #[test]
    fn wait_line_subject_names_the_tier_in_flight() {
        let profile = Owner::profile("work");
        let managers = Owner::cfgd("managers");
        let groups = [
            group(&managers, Tier::Rest, true),
            group(&profile, Tier::Rest, true),
        ];

        // Module work in flight: the Rest tier waits on it.
        assert_eq!(tier_in_flight([2, 1]), Some(Tier::Modules));
        assert_eq!(
            subjects(&groups, tier_in_flight([2, 1])),
            vec![
                (
                    "cfgd:managers".to_string(),
                    "waiting on modules".to_string()
                ),
                ("profile:work".to_string(), "waiting on modules".to_string()),
            ]
        );

        // Nothing is ever blocked behind the last tier.
        assert_eq!(tier_in_flight([0, 1]), Some(Tier::Rest));
        assert!(subjects(&groups, tier_in_flight([0, 1])).is_empty());
        assert_eq!(tier_in_flight([0, 0]), None);
    }

    #[test]
    fn wait_line_skips_an_empty_tier() {
        // A plan with only profile-owned package work and no module package
        // work: tier 0 is released and drained in the same instant, so it is
        // never in flight and `waiting on modules` is never said.
        let profile = Owner::profile("work");
        let groups = [group(&profile, Tier::Rest, true)];

        assert_eq!(tier_in_flight([0, 1]), Some(Tier::Rest));
        assert!(
            subjects(&groups, tier_in_flight([0, 1])).is_empty(),
            "an empty tier is never in flight, and nothing is ever blocked behind the last tier"
        );
    }

    #[test]
    fn a_group_with_nothing_left_to_dispatch_renders_no_wait_line() {
        let profile = Owner::profile("work");
        assert!(
            subjects(&[group(&profile, Tier::Rest, false)], Some(Tier::Modules)).is_empty(),
            "a group whose actions are all dispatched is not waiting"
        );
    }

    #[test]
    fn both_wait_line_cardinalities_share_one_grammar() {
        // The blocked-action line and the blocked-group line are the same
        // sentence at two levels; they are built by one function so they
        // cannot drift apart.
        assert_eq!(
            wait_subject(Some("apt install git"), "apt"),
            "apt install git · waiting on apt",
            "the blocked-action cardinality names what is held"
        );
        assert_eq!(
            wait_subject(None, Tier::Modules.wait_word().unwrap_or_default()),
            "waiting on modules",
            "the blocked-group cardinality is headed by the group's own heading"
        );
    }

    /// One waiting slot: an owner, a tier, and the manager whose lane it wants.
    fn slot<'p>(owner: &'p Owner, tier: Tier, manager: &str, action: &'p Action) -> Slot<'p> {
        Slot {
            owner,
            action,
            plan_index: 0,
            tier,
            manager: Some(manager.to_string()),
            module: owner
                .token()
                .strip_prefix("module:")
                .map(ToString::to_string),
            node: None,
            depends_on: &[],
            registers_sources: false,
            state: SlotState::Waiting,
        }
    }

    fn busy(managers: &[&str]) -> HashSet<String> {
        managers.iter().map(|m| (*m).to_string()).collect()
    }

    /// What the scheduler is holding back, for one arrangement of slots.
    fn held<'p>(
        slots: &[Slot<'p>],
        groups: &[(&'p Owner, Tier)],
        deps: &HashMap<&str, HashSet<&str>>,
        lanes_busy: &HashSet<String>,
    ) -> Held<'p> {
        held_waits(&WaitInputs {
            slots,
            groups,
            deps,
            lanes_busy,
        })
    }

    /// Every wait line, in the order the tree would draw new rows in.
    fn lines(held: &Held<'_>) -> Vec<String> {
        held.waits.iter().map(|w| w.subject.clone()).collect()
    }

    /// Every wait line paired with the group whose heading it sits under.
    fn rows(held: &Held<'_>) -> Vec<(String, String)> {
        held.waits
            .iter()
            .map(|w| (w.owner.token(), w.subject.clone()))
            .collect()
    }

    fn install(manager: &str, package: &str) -> Action {
        Action::Package(crate::providers::PackageAction::Install {
            manager: manager.to_string(),
            packages: vec![package.to_string()],
            origin: "local".to_string(),
        })
    }

    fn probe_action() -> Action {
        install("brew", "neovim")
    }

    #[test]
    fn a_dispatchable_tap_is_offered_across_the_tier_barrier_and_holds_its_family() {
        // A profile-declared tap delivers the repositories a module's
        // formulas resolve from, and the tier barrier alone would run the
        // module's brew installs first — so the tap leapfrogs the barrier,
        // and its family refuses other work until it has run.
        let nvim = Owner::module("nvim");
        let profile = Owner::profile("work");
        let formula = install("brew", "neovim");
        let unrelated = install("apt", "git");
        let tap = install("brew-tap", "acme/tools");
        let mut slots = vec![
            slot(&nvim, Tier::Modules, "brew", &formula),
            slot(&nvim, Tier::Modules, "apt", &unrelated),
            slot(&profile, Tier::Rest, "brew-tap", &tap),
        ];
        slots[2].registers_sources = true;
        let registry = ProviderRegistry::new();
        let deps = HashMap::new();
        let no_lanes = HashSet::new();
        let no_owners = HashMap::new();

        assert_eq!(
            pick_next(
                &slots,
                &registry,
                &deps,
                &DispatchState {
                    lanes_busy: &no_lanes,
                    owners_busy: &no_owners,
                    draining: false,
                    running: 0,
                },
            ),
            Some(2),
            "the tap dispatches ahead of the tier in flight"
        );

        // The tap's owner is mid-action elsewhere: its family stays held —
        // the module's brew install may not overtake it — while another
        // family's work proceeds.
        let profile_busy = HashMap::from([(profile.token(), 1usize)]);
        let held = DispatchState {
            lanes_busy: &no_lanes,
            owners_busy: &profile_busy,
            draining: false,
            running: 1,
        };
        assert_eq!(
            pick_next(&slots, &registry, &deps, &held),
            Some(1),
            "an unrelated family is not held by the tap"
        );
        slots[1].state = SlotState::Done;
        assert_eq!(
            pick_next(&slots, &registry, &deps, &held),
            Some(2),
            "with nothing else ready the owner-held tap is the pick, never the formula"
        );
    }

    #[test]
    fn a_draining_tap_quiets_only_its_own_family_not_the_phase() {
        // The tap's manager needs bootstrapping, so it starts only on a quiet
        // phase — but quiescing the tiers ahead of it would serialize work the
        // tap cannot affect. Its own family is already held; the rest of the
        // phase keeps moving.
        let nvim = Owner::module("nvim");
        let profile = Owner::profile("work");
        let formula = install("brew", "neovim");
        let unrelated = install("apt", "git");
        let tap = install("brew-tap", "acme/tools");
        let mut slots = vec![
            slot(&nvim, Tier::Modules, "brew", &formula),
            slot(&nvim, Tier::Modules, "apt", &unrelated),
            slot(&profile, Tier::Rest, "brew-tap", &tap),
        ];
        slots[2].registers_sources = true;
        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(
            crate::test_helpers::MockPackageManager::new("brew-tap").unavailable(),
        ));
        let deps = HashMap::new();
        let no_lanes = HashSet::new();
        let no_owners = HashMap::new();

        assert_eq!(
            pick_next(
                &slots,
                &registry,
                &deps,
                &DispatchState {
                    lanes_busy: &no_lanes,
                    owners_busy: &no_owners,
                    draining: false,
                    running: 1,
                },
            ),
            Some(1),
            "module-tier work outside the tap's family proceeds while the draining tap waits"
        );
        assert_eq!(
            pick_next(
                &slots,
                &registry,
                &deps,
                &DispatchState {
                    lanes_busy: &no_lanes,
                    owners_busy: &no_owners,
                    draining: false,
                    running: 0,
                },
            ),
            Some(2),
            "on a quiet phase the draining tap is the pick"
        );
    }

    #[test]
    fn only_an_install_derives_the_source_registration_flag() {
        // The hoist-and-hold exists so installs can resolve from the tap
        // being added; an untap delivers no repository, so it gets neither
        // the hoist nor the hold.
        let mut registry = ProviderRegistry::new();
        registry.add_package_manager(Box::new(
            crate::test_helpers::MockPackageManager::new("brew-tap").registering_family_sources(),
        ));

        assert!(registers_family_sources(
            &install("brew-tap", "acme/tools"),
            &registry
        ));
        let untap = Action::Package(PackageAction::Uninstall {
            manager: "brew-tap".to_string(),
            packages: vec!["acme/tools".to_string()],
            origin: "local".to_string(),
        });
        assert!(
            !registers_family_sources(&untap, &registry),
            "an untap neither crosses the tier barrier nor holds its family"
        );
        assert!(
            !registers_family_sources(&install("brew", "neovim"), &registry),
            "an install through a non-registering manager stays behind the barrier"
        );
    }

    #[test]
    fn an_install_held_for_a_dispatchable_tap_names_its_family_lane() {
        // Nothing is running, so `lanes_busy` is empty — the hold is the tap
        // itself, and without this row the formula would be absent from the
        // live region for the whole of its wait.
        let nvim = Owner::module("nvim");
        let profile = Owner::profile("work");
        let formula = install("brew", "neovim");
        let tap = install("brew-tap", "acme/tools");
        let mut slots = vec![
            slot(&nvim, Tier::Modules, "brew", &formula),
            slot(&profile, Tier::Rest, "brew-tap", &tap),
        ];
        slots[1].registers_sources = true;
        let groups = groups_of(&slots);

        let held = held(&slots, &groups, &HashMap::new(), &HashSet::new());

        assert!(
            rows(&held).contains(&(
                "module:nvim".to_string(),
                "brew install neovim · waiting on brew".to_string()
            )),
            "a source-held install says what its family is waiting on: {:?}",
            rows(&held)
        );
        assert!(
            !rows(&held)
                .iter()
                .any(|(owner, subject)| owner == "profile:work" && subject.contains("brew-tap")),
            "the tap is the hold, never its own blocker: {:?}",
            rows(&held)
        );
    }

    #[test]
    fn a_barrier_crossing_tap_blocked_by_a_running_family_gets_a_row() {
        // The tap sits in the Rest tier while modules are in flight; the tier
        // filter must not hide the one action holding the whole brew family.
        let nvim = Owner::module("nvim");
        let profile = Owner::profile("work");
        let running = probe_action();
        let tap = install("brew-tap", "acme/tools");
        let mut slots = vec![
            slot(&nvim, Tier::Modules, "brew", &running),
            slot(&profile, Tier::Rest, "brew-tap", &tap),
        ];
        slots[0].state = SlotState::Running;
        slots[1].registers_sources = true;
        let groups = groups_of(&slots);

        let held = held(&slots, &groups, &HashMap::new(), &busy(&["brew"]));

        assert!(
            rows(&held).contains(&(
                "profile:work".to_string(),
                "brew-tap install acme/tools · waiting on brew".to_string()
            )),
            "the tier filter does not hide a barrier-crossing tap: {:?}",
            rows(&held)
        );
        // One row per wait: the tap crossed the barrier, so its owner is not
        // also "waiting on modules" — the coarse tier row would say the same
        // wait twice.
        assert!(
            !rows(&held).contains(&("profile:work".to_string(), "waiting on modules".to_string())),
            "the coarse tier row never doubles the tap's specific row: {:?}",
            rows(&held)
        );
        assert!(
            held.pending_owners.contains(&"profile:work".to_string()),
            "the tap's group stays open to settle the tap's own row"
        );
    }

    #[test]
    fn a_blocked_action_gets_a_bar_while_its_own_owner_holds_another_lane() {
        // The spec's worked example, at the rendering level: `nvim` is running
        // on brew and waiting on apt, which `tmux` holds. Suppressing the line
        // because its owner is busy would delete it in exactly the window the
        // grammar exists for.
        let nvim = Owner::module("nvim");
        let tmux = Owner::module("tmux");
        let running = probe_action();
        let blocked = install("apt", "git");
        let mut slots = vec![
            slot(&nvim, Tier::Modules, "brew", &running),
            slot(&nvim, Tier::Modules, "apt", &blocked),
            slot(&tmux, Tier::Modules, "apt", &running),
        ];
        slots[0].state = SlotState::Running;
        slots[2].state = SlotState::Running;
        let groups = groups_of(&slots);

        let held = held(&slots, &groups, &HashMap::new(), &busy(&["brew", "apt"]));

        assert_eq!(
            rows(&held),
            vec![(
                "module:nvim".to_string(),
                "apt install git · waiting on apt".to_string()
            )]
        );
    }

    #[test]
    fn a_blocked_sub_manager_action_names_the_family_holding_the_lane() {
        // `brew-cask` and `brew` are one binary, so the line has to say what is
        // actually in the way rather than repeating the action's own name.
        let profile = Owner::profile("work");
        let running = probe_action();
        let cask = install("brew-cask", "firefox");
        let mut slots = vec![
            slot(&profile, Tier::Rest, "brew", &running),
            slot(&profile, Tier::Rest, "brew-cask", &cask),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);

        let held = held(&slots, &groups, &HashMap::new(), &busy(&["brew"]));

        assert_eq!(
            lines(&held),
            vec!["brew-cask install firefox · waiting on brew"]
        );
    }

    #[test]
    fn every_blocked_action_gets_its_own_line_in_plan_order() {
        // Formulae, taps and casks all queue behind one `brew` process. Each is
        // a row of its own, because a row is where that action will START — the
        // wait is the first state of the line the action then runs and settles
        // in, so collapsing three waits into one would leave two actions with
        // nowhere to appear. Naming the blocked action rather than its owner is
        // what makes three lines behind one lane readable.
        let profile = Owner::profile("work");
        let running = probe_action();
        let tap = install("brew-tap", "homebrew/cask-fonts");
        let cask = install("brew-cask", "firefox");
        let apt = install("apt", "git");
        let mut slots = vec![
            slot(&profile, Tier::Rest, "brew", &running),
            slot(&profile, Tier::Rest, "brew-tap", &tap),
            slot(&profile, Tier::Rest, "brew-cask", &cask),
            slot(&profile, Tier::Rest, "apt", &apt),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);

        let held = held(&slots, &groups, &HashMap::new(), &busy(&["brew", "apt"]));

        assert_eq!(
            lines(&held),
            vec![
                "brew-tap install homebrew/cask-fonts · waiting on brew",
                "brew-cask install firefox · waiting on brew",
                "apt install git · waiting on apt",
            ]
        );
    }

    #[test]
    fn an_action_waiting_on_its_dependency_gets_no_manager_line() {
        // It is not waiting on a manager — its module is waiting on another
        // module, and saying "waiting on apt" would name the wrong thing.
        let nvim = Owner::module("nvim");
        let base = Owner::module("base");
        let running = probe_action();
        let blocked = install("apt", "git");
        let mut slots = vec![
            slot(&base, Tier::Modules, "apt", &running),
            slot(&nvim, Tier::Modules, "apt", &blocked),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);
        let deps = HashMap::from([("nvim", HashSet::from(["base"]))]);

        let held = held(&slots, &groups, &deps, &busy(&["apt"]));

        assert!(lines(&held).is_empty(), "{:?}", lines(&held));
    }

    /// One node of the `cfgd:managers` DAG, built the way the dispatcher
    /// builds it: id and edges are read off the action rather than restated
    /// here, so a test cannot describe a graph the planner could not emit.
    fn node_slot<'p>(owner: &'p Owner, action: &'p Action, plan_index: usize) -> Slot<'p> {
        let Action::Manager(node) = action else {
            panic!("node_slot takes a manager action")
        };
        Slot {
            owner,
            action,
            plan_index,
            tier: Tier::of(owner),
            manager: crate::reconciler::packages::action_manager(action).map(str::to_string),
            module: None,
            node: Some(node.node_id()),
            depends_on: node.depends_on(),
            registers_sources: false,
            state: SlotState::Waiting,
        }
    }

    fn provision(manager: &str, via: &str, depends_on: &[String]) -> Action {
        Action::Manager(ManagerAction::Provision {
            manager: manager.to_string(),
            via: via.to_string(),
            declared: None,
            batched: vec![],
            depends_on: depends_on.to_vec(),
        })
    }

    #[test]
    fn provisions_sharing_a_bootstrap_mediator_share_its_lane() {
        // `provision npm via apt` and `provision pipx via apt` both run
        // apt-get, so they must hold ONE lane or they race for the dpkg lock
        // — laned on their own names, the pipx provision died on the lock the
        // npm provision's apt-get was still holding. The registered name
        // stays on the slot for display/journal/drains; only the mutual
        // exclusion follows the mediator.
        let managers = Owner::cfgd("managers");
        let npm = provision("npm", "apt", &[]);
        let pipx = provision("pipx", "apt", &[]);
        let brew = provision("brew", "homebrew installer", &[]);
        let slots = [
            node_slot(&managers, &npm, 0),
            node_slot(&managers, &pipx, 1),
            node_slot(&managers, &brew, 2),
        ];
        assert_eq!(slots[0].lane(), Some("apt"));
        assert_eq!(slots[0].lane(), slots[1].lane());
        assert_eq!(
            slots[2].lane(),
            Some("homebrew installer"),
            "a standalone method lanes on its phrase, colliding with nothing"
        );
        assert_eq!(
            slots[0].manager.as_deref(),
            Some("npm"),
            "the slot keeps the registered name for everything but the lane"
        );
    }

    #[test]
    fn an_edge_blocked_node_names_the_last_thing_that_has_to_finish() {
        // Two blockers: one already running and LATER in the plan, one not
        // started and earlier. The line names the unstarted one, because that
        // is what the node is still behind once the running one clears — an
        // attribution by "first in flight" would name brew and then have to
        // take it back a second later.
        let managers = Owner::cfgd("managers");
        let pipx = provision("pipx", "brew", &[]);
        let brew = provision("brew", "curl", &[]);
        let poetry = provision(
            "poetry",
            "pipx",
            &[
                ManagerAction::provision_node("brew"),
                ManagerAction::provision_node("pipx"),
            ],
        );
        let mut slots = vec![
            node_slot(&managers, &pipx, 0),
            node_slot(&managers, &brew, 1),
            node_slot(&managers, &poetry, 2),
        ];
        slots[1].state = SlotState::Running;
        let groups = groups_of(&slots);

        let held = held(&slots, &groups, &HashMap::new(), &busy(&["brew"]));

        // The head is the node's own display subject, not the owner token:
        // every node here belongs to `cfgd:managers`, whose heading is already
        // one line above, so a token would print the same six characters on
        // every line and name none of them. The pipx provision earns the first
        // line: it lanes on its mediator, since `brew install pipx` cannot run
        // while another brew process holds the lane.
        assert_eq!(
            lines(&held),
            vec![
                "provision pipx via brew · waiting on brew",
                "provision poetry via pipx · waiting on pipx",
            ],
            "a node held by an edge is in the live region for the whole of its wait"
        );
    }

    #[test]
    fn an_edge_to_a_node_absent_from_the_plan_is_satisfied() {
        // A `--skip` on the dependency prunes its node out of the plan
        // entirely — `filter_plan` never rewrites a surviving node's
        // `depends_on`, so the edge still names an id no slot carries. The
        // dispatcher must treat that as already satisfied rather than wait
        // on a node that will never arrive.
        let managers = Owner::cfgd("managers");
        let poetry = provision("poetry", "pipx", &[ManagerAction::provision_node("brew")]);
        let slots = vec![node_slot(&managers, &poetry, 0)];

        assert!(
            dag_satisfied(&slots, 0),
            "an edge to a node the plan no longer carries must not block dispatch"
        );
    }

    #[test]
    fn two_unstarted_edges_are_ranked_by_plan_order_not_dispatch_order() {
        // Neither blocker has started, so the tie breaks on the PLAN: the node
        // planned later is the one still ahead of the dependent once the other
        // is done. Two other rankings would answer brew and are excluded here
        // — the edges are declared brew-first, and the slots are held in an
        // order (pipx, brew) that is not their plan order, which is the shape
        // a tier-partitioned dispatch produces.
        let managers = Owner::cfgd("managers");
        let brew = provision("brew", "curl", &[]);
        let pipx = provision("pipx", "brew", &[]);
        let poetry = provision(
            "poetry",
            "pipx",
            &[
                ManagerAction::provision_node("brew"),
                ManagerAction::provision_node("pipx"),
            ],
        );
        let slots = vec![
            node_slot(&managers, &pipx, 1),
            node_slot(&managers, &brew, 0),
            node_slot(&managers, &poetry, 2),
        ];
        let groups = groups_of(&slots);

        let held = held(&slots, &groups, &HashMap::new(), &busy(&[]));

        assert!(
            lines(&held).contains(&"provision poetry via pipx · waiting on pipx".to_string()),
            "{:?}",
            lines(&held)
        );
    }

    #[test]
    fn a_blocking_prerequisite_is_named_by_its_tool() {
        // `apt install curl` is curl arriving, and curl is what the dependent
        // is waiting for — naming apt would name the installer of the thing
        // rather than the thing.
        let managers = Owner::cfgd("managers");
        let curl = Action::Manager(ManagerAction::Prerequisite {
            tool: "curl".to_string(),
            installer: "apt".to_string(),
            required_by: vec!["brew".to_string()],
            depends_on: Vec::new(),
        });
        let curl_node = match &curl {
            Action::Manager(node) => node.node_id(),
            _ => panic!("built a manager action"),
        };
        let brew = provision("brew", "curl", &[curl_node]);
        let mut slots = vec![
            node_slot(&managers, &curl, 0),
            node_slot(&managers, &brew, 1),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);

        let held = held(&slots, &groups, &HashMap::new(), &busy(&["apt"]));

        assert_eq!(
            lines(&held),
            vec!["provision brew via curl · waiting on curl"]
        );
    }

    #[test]
    fn an_edge_blocked_node_does_not_name_the_lane_it_has_not_reached_for() {
        // The node's own manager is busy AND an edge is unsatisfied. The edge
        // is what the dispatcher checks first, so it is what the line says;
        // naming the lane would name a blocker the node is not yet behind.
        let managers = Owner::cfgd("managers");
        let brew = provision("brew", "curl", &[]);
        let brew_cask = Action::Manager(ManagerAction::Provision {
            manager: "brew-cask".to_string(),
            via: "brew".to_string(),
            declared: None,
            batched: vec![],
            depends_on: vec![ManagerAction::provision_node("brew")],
        });
        let mut slots = vec![
            node_slot(&managers, &brew, 0),
            node_slot(&managers, &brew_cask, 1),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);

        let held = held(&slots, &groups, &HashMap::new(), &busy(&["brew"]));

        assert_eq!(
            lines(&held),
            vec!["provision brew-cask via brew · waiting on brew"],
            "one blocker, one line"
        );
    }

    #[test]
    fn a_groups_line_is_withdrawn_once_the_tier_in_flight_advances() {
        // The group's line is held while Modules is in flight, and withdrawn
        // outright — never left stale — once nothing above the group is left.
        let nvim = Owner::module("nvim");
        let managers = Owner::cfgd("managers");
        let profile = Owner::profile("work");
        let action = probe_action();
        let mut slots = vec![
            slot(&nvim, Tier::Modules, "apt", &action),
            slot(&managers, Tier::Rest, "pipx", &action),
            slot(&profile, Tier::Rest, "brew", &action),
        ];
        let groups = groups_of(&slots);
        let deps = HashMap::new();
        let idle = busy(&[]);

        assert_eq!(
            rows(&held(&slots, &groups, &deps, &idle)),
            vec![
                (
                    "cfgd:managers".to_string(),
                    "waiting on modules".to_string()
                ),
                ("profile:work".to_string(), "waiting on modules".to_string()),
            ]
        );

        slots[0].state = SlotState::Done;
        assert!(
            lines(&held(&slots, &groups, &deps, &idle)).is_empty(),
            "nothing is ever blocked behind the last tier"
        );
    }

    #[test]
    fn an_action_line_is_withdrawn_once_its_lane_frees() {
        let profile = Owner::profile("work");
        let running = probe_action();
        let blocked = install("apt", "git");
        let mut slots = vec![
            slot(&profile, Tier::Rest, "apt", &running),
            slot(&profile, Tier::Rest, "apt", &blocked),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);
        let deps = HashMap::new();

        assert_eq!(
            lines(&held(&slots, &groups, &deps, &busy(&["apt"]))),
            vec!["apt install git · waiting on apt"]
        );

        slots[0].state = SlotState::Done;
        assert!(
            lines(&held(&slots, &groups, &deps, &busy(&[]))).is_empty(),
            "a wait line outlives neither the wait nor the action"
        );
    }

    /// The order the tree would append new group rows in.
    fn group_row_order(held: &Held<'_>) -> Vec<String> {
        held.waits
            .iter()
            .filter(|w| w.action.is_none())
            .map(|w| w.owner.token())
            .collect()
    }

    #[test]
    fn group_wait_rows_are_built_in_slot_order_not_hash_order() {
        // Owners are deliberately non-alphabetical: a `HashMap`-driven creation
        // loop reshuffles this every process, and the top-to-bottom order new
        // rows are drawn in has to match `groups_of`'s plan order — the same
        // order the phase tree shows — every run, not just this one.
        let busy_owner = Owner::module("busy");
        let zeta = Owner::module("zeta");
        let alpha = Owner::module("alpha");
        let mid = Owner::module("mid");
        let action = probe_action();
        let mut slots = vec![
            slot(&busy_owner, Tier::Modules, "brew", &action),
            slot(&zeta, Tier::Rest, "brew", &action),
            slot(&alpha, Tier::Rest, "brew", &action),
            slot(&mid, Tier::Rest, "brew", &action),
        ];
        // Keeps `Modules` in flight, so the three `Rest`-tier groups are all
        // blocked and all render a wait line in the same refresh.
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);

        for _ in 0..5 {
            assert_eq!(
                group_row_order(&held(&slots, &groups, &HashMap::new(), &busy(&[]))),
                vec!["module:zeta", "module:alpha", "module:mid"],
                "wait rows must be built in slot/plan order every run"
            );
        }
    }

    /// The order the tree would append new action rows in, named by the group
    /// each belongs under — the sentences are the actions' own, and what this
    /// asserts is which group's rows come first.
    fn action_row_order(held: &Held<'_>) -> Vec<String> {
        held.waits
            .iter()
            .filter(|w| w.action.is_some())
            .map(|w| w.owner.token())
            .collect()
    }

    #[test]
    fn action_wait_rows_are_built_in_slot_order_not_hash_order() {
        let holder = Owner::module("holder");
        let zeta = Owner::module("zeta");
        let alpha = Owner::module("alpha");
        let mid = Owner::module("mid");
        let action = probe_action();
        let mut slots = vec![
            slot(&holder, Tier::Rest, "brew", &action),
            slot(&zeta, Tier::Rest, "brew", &action),
            slot(&alpha, Tier::Rest, "brew", &action),
            slot(&mid, Tier::Rest, "brew", &action),
        ];
        // `holder` occupies the `brew` lane, so `zeta`/`alpha`/`mid` are all
        // blocked behind it and all render a wait line in the same refresh.
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);

        for _ in 0..5 {
            assert_eq!(
                action_row_order(&held(&slots, &groups, &HashMap::new(), &busy(&["brew"]))),
                vec!["module:zeta", "module:alpha", "module:mid"],
                "wait rows must be built in slot order every run"
            );
        }
    }

    #[test]
    fn packages_phase_runs_lanes_concurrently_when_all_managers_are_present() {
        // Three independent, already-present managers, each holding its lane
        // open long enough for the other two to be seen in theirs. Nothing
        // here shares a lane, so a dispatcher that ran them is one that had
        // all three installs in flight at once; a sequential loop peaks at
        // one however fast the machine is.
        let delay = Duration::from_millis(200);
        let witness = crate::test_helpers::ConcurrencyWitness::new();
        let mock = |name: &str| {
            crate::test_helpers::MockPackageManager::new(name)
                .with_install_delay(delay)
                .with_concurrency_witness(witness.clone())
        };
        let harness = crate::test_helpers::ReconcilerTestHarness::builder()
            .with_package_manager(mock("brew"))
            .with_package_manager(mock("cargo"))
            .with_package_manager(mock("npm"))
            .build();

        let pkg_actions = vec![
            crate::providers::PackageAction::Install {
                manager: "brew".to_string(),
                packages: vec!["neovim".to_string()],
                origin: "local".to_string(),
            },
            crate::providers::PackageAction::Install {
                manager: "cargo".to_string(),
                packages: vec!["ripgrep".to_string()],
                origin: "local".to_string(),
            },
            crate::providers::PackageAction::Install {
                manager: "npm".to_string(),
                packages: vec!["typescript".to_string()],
                origin: "local".to_string(),
            },
        ];
        let plan = harness
            .plan_with_actions(Vec::new(), pkg_actions, Vec::new())
            .expect("plan should succeed");

        let printer = crate::test_helpers::test_printer();
        let result = harness
            .apply(&plan, &printer)
            .expect("apply should succeed");

        assert_eq!(
            result.status,
            crate::state::ApplyStatus::Success,
            "all three installs should succeed: {result:?}"
        );
        assert_eq!(
            witness.peak(),
            3,
            "three independent managers should install concurrently; \
             a peak of one is a serial walk"
        );
    }

    #[test]
    fn settling_outstanding_work_answers_every_slot_once_and_in_plan_order() {
        // The `Lost` exit is the one a test cannot stage — it needs a worker
        // the panic guard never sees — so the rule it shares with the stall
        // exit is pinned here instead: a slot still RUNNING when the dispatch
        // ends is outstanding exactly like one still waiting, each is answered
        // once, and a second sweep finds nothing left to blame twice.
        let owner = Owner::profile("work");
        let first = probe_action();
        let second = probe_action();
        let done = probe_action();
        let mut slots = vec![
            slot(&owner, Tier::Rest, "brew", &first),
            slot(&owner, Tier::Rest, "cargo", &second),
            slot(&owner, Tier::Rest, "npm", &done),
        ];
        slots[0].state = SlotState::Running;
        slots[2].state = SlotState::Done;
        let mut journal_ids: HashMap<usize, Option<i64>> = HashMap::from([(0, Some(7))]);

        let mut answered: Vec<(String, Option<i64>)> = Vec::new();
        // No section, so the tree is inert and every outcome is the caller's
        // to record — which is what this test reads.
        let printer = crate::test_helpers::test_printer();
        let mut tree = PhaseTree::new(&printer, None, None, 0, 0);
        // Scoped so the closure's borrow of `answered` ends before it is read.
        {
            let mut record = |_owner: &Owner, _action: &Action, collected: LaneCollected| {
                let message = collected
                    .result
                    .as_ref()
                    .err()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                answered.push((message, collected.journal_id));
                None
            };
            settle_unrun(
                &mut slots,
                &Unrun::Lost,
                &mut journal_ids,
                &mut LaneCollector::new(&mut record, &mut tree),
            );
        }

        assert_eq!(
            answered.len(),
            2,
            "the completed slot must not be answered again: {answered:?}"
        );
        assert!(
            answered[0].0.contains("brew lane ended") && answered[1].0.contains("cargo lane ended"),
            "answered out of plan order, or with the wrong reason: {answered:?}"
        );
        assert_eq!(
            answered[0].1,
            Some(7),
            "a running slot's open journal row must be closed by its answer"
        );
        assert!(
            slots.iter().all(|s| s.state == SlotState::Done),
            "an answered slot stays answered"
        );

        let mut again = 0;
        {
            let mut count = |_owner: &Owner, _action: &Action, _collected: LaneCollected| {
                again += 1;
                None
            };
            settle_unrun(
                &mut slots,
                &Unrun::Stalled,
                &mut journal_ids,
                &mut LaneCollector::new(&mut count, &mut tree),
            );
        }
        assert_eq!(again, 0, "a second exit path must not report a slot twice");
    }

    /// Concern 3 of the elision re-review: `settle_unrun`/`fail_dependents`
    /// reach `PhaseTree::settled` only through `LaneCollector::finished`, and
    /// every existing swept-row test either called `tree.settled` directly
    /// (bypassing `fail_dependents` itself) or ran off a TTY, where
    /// `settles_in_place` is false and `tree.settled` is never reached at
    /// all. Drives the real `fail_dependents` against a LIVE tree whose head
    /// (`actions[0]`, `apt`) is pinned `Running` for as long as the caller
    /// holds the returned handle — `actions[1]` is `brew`, which runs (and
    /// fails); `actions[2]`/`actions[3]` are `npm`/`pnpm`, which never run at
    /// all and are swept by `fail_dependents`.
    fn swept_by_fail_dependents<'a>(
        printer: &'a Printer,
        section: &'a SectionGuard<'a>,
        owner: &'a Owner,
        actions: &'a [Action],
    ) -> (PhaseTree<'a, 'a>, LaneHandle<'a>) {
        let mut tree = PhaseTree::new(printer, Some(section), None, section.depth + 1, 30);
        let running = tree.dispatched(owner, &actions[0]);
        // brew actually ran (and failed) rather than being swept, so it is
        // dispatched first like `apt` — its own row keeps its line and is not
        // counted by `held_unseen()`. Only the dependents it takes down with
        // it (`npm`, `pnpm`) never ran at all.
        tree.dispatched(owner, &actions[1]).finish();

        let mut slots = vec![
            node_slot(owner, &actions[1], 1),
            node_slot(owner, &actions[2], 2),
            node_slot(owner, &actions[3], 3),
        ];
        // Mirrors `apply.rs`'s live settle closure closely enough to prove
        // the wiring: every finished action becomes a `Fail`-role outcome
        // carrying the real error `fail_dependents` produced.
        let mut record = |_owner: &Owner, action: &Action, collected: LaneCollected| {
            let subject = action_display_subject(action).to_string();
            let mut outcome = ActionOutcome::for_test(&subject, Duration::ZERO);
            outcome.role = crate::output::Role::Fail;
            outcome.duration = None;
            outcome.detail = collected.result.err().map(|e| e.to_string());
            Some(outcome)
        };
        {
            let mut collect = LaneCollector::new(&mut record, &mut tree);
            // brew's own failure, exactly as the coordinator reports it
            // before sweeping what depended on it.
            collect.finished(
                slots[0].owner,
                slots[0].action,
                LaneCollected {
                    journal_id: None,
                    result: Err(PackageError::BootstrapFailed {
                        manager: "brew".to_string(),
                        message: "brew still not available after bootstrap".to_string(),
                    }
                    .into()),
                    elapsed: Duration::ZERO,
                    notes: Vec::new(),
                    body: Vec::new(),
                },
            );
            fail_dependents(&mut slots, 0, &mut collect);
        }
        let done_count = slots[1..]
            .iter()
            .filter(|s| s.state == SlotState::Done)
            .count();
        assert_eq!(
            done_count, 2,
            "the whole dependent chain must be answered by one sweep"
        );
        (tree, running)
    }

    fn prerequisites_sweep_actions() -> [Action; 4] {
        [
            provision("apt", "system", &[]),
            provision("brew", "curl", &[]),
            provision("npm", "brew", &[ManagerAction::provision_node("brew")]),
            provision("pnpm", "npm", &[ManagerAction::provision_node("npm")]),
        ]
    }

    #[test]
    fn fail_dependents_holds_swept_rows_for_commit_on_a_live_tree() {
        // While `apt` (the group's head) is still Running, nothing can commit
        // — so the two rows `fail_dependents` just swept are genuinely HELD,
        // not merely en route to an instant commit.
        let (printer, _buf) = crate::output::Printer::for_test_with_live_bars();
        let section = printer.section_phase(&PhaseName::Prerequisites.section_label());
        let managers = Owner::cfgd("managers");
        let actions = prerequisites_sweep_actions();

        let (tree, running) = swept_by_fail_dependents(&printer, &section, &managers, &actions);

        let rows = tree.drawn_rows();
        assert!(
            rows.iter()
                .any(|line| line.contains("2 settled rows held for commit")),
            "the swept dependents were not counted while genuinely held: {rows:?}"
        );
        drop(running);
    }

    #[test]
    fn fail_dependents_commits_swept_rows_once_in_dispatch_order_on_a_live_tree() {
        let (printer, buf) = crate::output::Printer::for_test_live_scrollback();
        let section = printer.section_phase(&PhaseName::Prerequisites.section_label());
        let managers = Owner::cfgd("managers");
        let actions = prerequisites_sweep_actions();

        let (mut tree, running) = swept_by_fail_dependents(&printer, &section, &managers, &actions);

        // Release the head and let the tree drain what it was holding.
        running.finish();
        tree.settled(
            &managers,
            &actions[0],
            ActionOutcome::for_test("provision apt via system", Duration::ZERO),
        );
        tree.finish();

        let scrollback = crate::test_helpers::captured_text(&buf);
        for subject in [
            "provision brew via curl",
            "provision npm via brew",
            "provision pnpm via npm",
        ] {
            assert_eq!(
                scrollback.matches(subject).count(),
                1,
                "{subject} did not reach the scrollback exactly once: {scrollback}"
            );
        }
        let at = |needle: &str| {
            scrollback
                .find(needle)
                .unwrap_or_else(|| panic!("no {needle:?} in {scrollback}"))
        };
        assert!(
            at("provision apt via system") < at("provision brew via curl"),
            "the sweep committed ahead of the head that was blocking it: {scrollback}"
        );
        assert!(
            at("provision brew via curl") < at("provision npm via brew"),
            "the sweep did not commit in dispatch order: {scrollback}"
        );
        assert!(
            at("provision npm via brew") < at("provision pnpm via npm"),
            "the sweep did not commit in dispatch order: {scrollback}"
        );
    }
}

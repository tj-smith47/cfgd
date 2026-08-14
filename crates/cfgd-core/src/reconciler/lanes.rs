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
//!    ancestor, because what it was waiting to be handed does not exist.
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
use crate::output::{LaneOutput, Printer, WaitBar};
use crate::providers::{ActionNote, NoteSink, PackageStateStore, ProviderRegistry};

use super::format::{
    action_display_subject, format_action_description, parse_resource_from_description,
};
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

/// One finished action, as the coordinator collected it.
pub(super) struct LaneCollected {
    pub(super) journal_id: Option<i64>,
    pub(super) result: Result<(String, bool)>,
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
    result: Result<(String, bool)>,
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
    state: SlotState,
}

/// The ONE wait-line grammar: `<owner token> · waiting on <thing>`.
///
/// One sentence for both cardinalities — the manager for a blocked action, the
/// tier in flight for a blocked group — because they are the same statement at
/// two levels and reading them side by side is the point.
fn wait_subject(owner: &Owner, thing: &str) -> String {
    format!("{} · waiting on {}", owner.token(), thing)
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
    /// The group still has an action that has not been dispatched.
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
        .map(|g| (g.owner, wait_subject(g.owner, word)))
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
        .package_managers
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
    fn lane(&self) -> Option<&str> {
        self.manager.as_deref().map(crate::manager_family)
    }
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
    slots[index].depends_on.iter().all(|dependency| {
        !slots
            .iter()
            .any(|s| s.state != SlotState::Done && s.node.as_deref() == Some(dependency.as_str()))
    })
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

/// Settle every node downstream of the failure at `root` as a failure of its
/// own, without running any of them.
///
/// Attribution is the ROOT rather than the nearest dependency: a `provision
/// npm` held up by a missing curl says curl, because the reader's next move is
/// to fix curl and three lines each blaming the line above it say the same
/// thing three times without ever naming what to do. Sweeps run in slot order —
/// the plan's own order — and repeat until nothing more is reachable, so a
/// chain of any depth is settled in one call and always in the same sequence.
fn fail_dependents(
    slots: &mut [Slot<'_>],
    root: usize,
    collect: &mut dyn FnMut(&Action, LaneCollected),
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
            collect(
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

    for (index, slot) in slots.iter().enumerate() {
        if slot.state != SlotState::Waiting || slot.tier != in_flight {
            continue;
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

/// The wait bars currently on screen, each keyed by what replaces it: a group's
/// by its owner token, an action's by the `(owner token, lane)` pair its
/// sentence is built from.
///
/// That pair and not the slot index, because the pair IS the sentence: one
/// owner with formulae, taps and casks all held behind a running `brew` has
/// three blocked slots and one thing to say, and three byte-identical live
/// lines would be three claims where there is one fact. Cardinality is still
/// per blocked action wherever the actions differ — an owner blocked on `apt`
/// and on `brew` keeps two bars, because those are two sentences.
struct WaitBars<'p> {
    groups: HashMap<String, WaitBar<'p>>,
    actions: HashMap<(String, String), WaitBar<'p>>,
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
        collect: &mut dyn FnMut(&Action, LaneCollected),
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
        let mut journal_ids: HashMap<usize, Option<i64>> = HashMap::new();
        let mut bars = WaitBars {
            groups: HashMap::new(),
            actions: HashMap::new(),
        };

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
                        let action_index = run.plan_index_base + slots[index].plan_index;
                        let journal_id = self.begin_package_journal(run, action, action_index);
                        // The bar names its owner as well as its action: the
                        // phase's tree is not on screen until phase close, so a
                        // bare action text names nothing.
                        let subject = format!(
                            "{} · {}",
                            slots[index].owner.token(),
                            action_display_subject(action)
                        );
                        // The dispatch-time read the script-install arm needs.
                        // Done here rather than in the worker because it is a
                        // SQLite read, and current because a bootstrap has
                        // always been collected before anything that could
                        // observe its directories is dispatched.
                        let path_dirs = super::all_recorded_path_dirs(self.state);
                        let lane = run.printer.lane_at(run.action_depth, subject);
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
                    refresh_wait_bars(
                        run.printer,
                        &WaitInputs {
                            slots: &slots,
                            groups: &groups,
                            deps: &deps,
                            lanes_busy: &lanes_busy,
                        },
                        &mut bars,
                    );
                } else {
                    // Nothing new will dispatch after an abort, so every wait
                    // line still on screen is a claim about work that will
                    // never start. Cleared once here rather than per collected
                    // action, because the refresh that would retire them is
                    // itself skipped while aborting.
                    bars.groups.clear();
                    bars.actions.clear();
                }

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
                    // `collect` on its own, so without this loop it vanishes
                    // from both the exit code (`results` never sees it) and
                    // the rendered tree (`recorded` never sees it) — a run
                    // that reached none of its plan would otherwise print
                    // `✓ Apply complete` and exit 0. `bars.groups` /
                    // `bars.actions` are cleared for the same reason the
                    // abort branch above clears them: no further
                    // `refresh_wait_bars` call follows this `break`, so a
                    // wait line for a slot this loop just failed would
                    // otherwise be the last thing drawn for it.
                    if aborted.is_none() {
                        bars.groups.clear();
                        bars.actions.clear();
                        for slot in slots.iter_mut().filter(|s| s.state == SlotState::Waiting) {
                            slot.state = SlotState::Done;
                            let manager = slot.manager.clone().unwrap_or_default();
                            collect(
                                slot.action,
                                LaneCollected {
                                    journal_id: None,
                                    result: Err(PackageError::LaneStalled { manager }.into()),
                                    elapsed: Duration::ZERO,
                                    notes: Vec::new(),
                                    body: Vec::new(),
                                },
                            );
                        }
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
                        collect(
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
                        if failed {
                            fail_dependents(&mut slots, index, collect);
                        }
                    }
                    // Reachable only once the coordinator has dropped its own
                    // `tx` above AND every worker's clone is also gone
                    // without a `Finished` ever landing — a worker killed by
                    // something the panic guard in the spawned closure
                    // cannot see. Whatever is still `Waiting` or `Running` is
                    // left uncollected; the caller reports the run as not
                    // having reached that work, the same as any other
                    // aborted dispatch.
                    Err(_) => break,
                }
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

/// Bring the live region's wait lines in step with the dispatch.
///
/// Recomputed rather than incrementally patched: a wait line is a statement
/// about the scheduler's current state, and the state is small.
///
/// A free function taking the printer rather than a `Reconciler` method: what
/// decides which bars exist is the dispatch state, and nothing else — so a test
/// can drive it from synthetic slots without a state store or an apply.
fn refresh_wait_bars<'x>(
    printer: &'x Printer,
    inputs: &WaitInputs<'_, '_>,
    bars: &mut WaitBars<'x>,
) {
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
            pending: slots
                .iter()
                .any(|s| s.state == SlotState::Waiting && s.owner.token() == owner.token()),
        })
        .collect();
    // A `Vec`, not a `HashMap`: `tier_waits` already hands back `groups_of`
    // order, and a bar not yet in `bars.groups` is `multi_progress.add`-ed
    // (via `printer.wait_bar`) at the point it is first inserted below — so
    // the iteration order here IS the top-to-bottom order new wait lines are
    // drawn in. Collecting through a `HashMap` first discarded that order and
    // redrew every fresh set of bars in `RandomState` order, reshuffled per
    // process.
    let wanted: Vec<(String, String)> = tier_waits(&waits, in_flight)
        .into_iter()
        .map(|(owner, subject)| (owner.token(), subject))
        .collect();
    let wanted_tokens: HashSet<&str> = wanted.iter().map(|(token, _)| token.as_str()).collect();
    bars.groups
        .retain(|token, _| wanted_tokens.contains(token.as_str()));
    for (token, subject) in wanted {
        match bars.groups.get(&token) {
            // Replaced, never appended to: the chain `modules` → the
            // group's own bars is one line changing what it says, not two
            // lines accumulating.
            Some(bar) => bar.set_subject(&subject),
            None => {
                bars.groups.insert(token, printer.wait_bar(&subject));
            }
        }
    }

    // Same reasoning as `wanted` above: `slots` is walked in plan order, so
    // building `blocked` as a `Vec` keeps that order for the bars inserted
    // from it.
    let mut blocked: Vec<((String, String), String)> = Vec::new();
    // Several slots can collapse onto the same `(owner, lane)` line — dedup
    // on first sight so the bar is inserted exactly once; `wait_subject` is a
    // pure function of `(owner, lane)`, so every later occurrence would say
    // the same thing anyway.
    let mut blocked_seen: HashSet<(String, String)> = HashSet::new();
    for (index, slot) in slots.iter().enumerate() {
        if slot.state != SlotState::Waiting || Some(slot.tier) != in_flight {
            continue;
        }
        if !depends_satisfied(slots, index, inputs.deps) {
            continue;
        }
        // A node still waiting on an edge is not waiting on its lane, and
        // saying so would name the wrong blocker. What such a node IS waiting
        // on is the node ahead of it — attribution the renderer owns.
        if !dag_satisfied(slots, index) {
            continue;
        }
        // An owner mid-action in another lane still gets this line: one
        // bar per in-flight action AND one per blocked action is the whole
        // point of the grammar, and the window it describes — a module
        // holding brew while another holds apt — is the one the wait line
        // exists for.
        let Some(lane) = slot.lane() else {
            continue;
        };
        if inputs.lanes_busy.contains(lane) {
            // The lane, not the registered name: an action for `brew-cask`
            // held back by a running `brew` is waiting on brew. Keying on
            // `(owner, lane)` collapses the several actions of one owner that
            // are all held behind the same binary into the one line they all
            // say.
            let key = (slot.owner.token(), lane.to_string());
            if blocked_seen.insert(key.clone()) {
                blocked.push((key, wait_subject(slot.owner, lane)));
            }
        }
    }
    let blocked_keys: HashSet<&(String, String)> = blocked.iter().map(|(k, _)| k).collect();
    bars.actions.retain(|key, _| blocked_keys.contains(key));
    for (key, subject) in blocked {
        match bars.actions.get(&key) {
            Some(bar) => bar.set_subject(&subject),
            None => {
                bars.actions.insert(key, printer.wait_bar(&subject));
            }
        }
    }
}

/// What a worker hands back.
struct LaneWorkerResult {
    result: Result<(String, bool)>,
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
        Action::Package(pkg) => exec.apply_package_action(pkg).map(|desc| (desc, true)),
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
        other => Ok((format_action_description(other), false)),
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

    /// The wait lines the live region would show, in the order given.
    fn subjects(groups: &[GroupWait<'_>], in_flight: Option<Tier>) -> Vec<String> {
        tier_waits(groups, in_flight)
            .into_iter()
            .map(|(_, subject)| subject)
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
                "cfgd:managers · waiting on modules",
                "profile:work · waiting on modules",
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
                "cfgd:managers · waiting on modules",
                "profile:work · waiting on modules",
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
        let nvim = Owner::module("nvim");
        assert_eq!(
            wait_subject(&nvim, "apt"),
            "module:nvim · waiting on apt",
            "the blocked-action cardinality"
        );
        assert_eq!(
            wait_subject(&nvim, Tier::Modules.wait_word().unwrap_or_default()),
            "module:nvim · waiting on modules",
            "the blocked-group cardinality"
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
            state: SlotState::Waiting,
        }
    }

    fn busy(managers: &[&str]) -> HashSet<String> {
        managers.iter().map(|m| (*m).to_string()).collect()
    }

    /// Drive one refresh. Takes the printer by reference so the bars it opens
    /// borrow it for the whole test rather than for one call.
    fn refresh<'p>(
        printer: &'p Printer,
        slots: &[Slot<'_>],
        groups: &[(&Owner, Tier)],
        deps: &HashMap<&str, HashSet<&str>>,
        lanes_busy: &HashSet<String>,
        bars: &mut WaitBars<'p>,
    ) {
        refresh_wait_bars(
            printer,
            &WaitInputs {
                slots,
                groups,
                deps,
                lanes_busy,
            },
            bars,
        );
    }

    /// Every wait line currently on screen, sorted so the assertion does not
    /// depend on `HashMap` iteration order.
    ///
    /// The theme's pending glyph is stripped — that the glyph comes from the
    /// theme is `the_glyph_comes_from_the_theme_not_the_call_site`'s assertion,
    /// and repeating it here would make every subject assertion below depend on
    /// the default theme's icon set.
    fn on_screen(bars: &WaitBars<'_>) -> Vec<String> {
        let glyph = format!("{} ", crate::output::Theme::default().icon_pending);
        let mut lines: Vec<String> = bars
            .groups
            .values()
            .chain(bars.actions.values())
            .map(|bar| {
                let line = bar.subject();
                line.strip_prefix(&glyph).unwrap_or(&line).to_string()
            })
            .collect();
        lines.sort();
        lines
    }

    fn empty_bars<'p>() -> WaitBars<'p> {
        WaitBars {
            groups: HashMap::new(),
            actions: HashMap::new(),
        }
    }

    fn probe_action() -> Action {
        Action::Package(crate::providers::PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["neovim".to_string()],
            origin: "local".to_string(),
        })
    }

    #[test]
    fn a_blocked_action_gets_a_bar_while_its_own_owner_holds_another_lane() {
        // The spec's worked example, at the rendering level: `nvim` is running
        // on brew and waiting on apt, which `tmux` holds. Suppressing the line
        // because its owner is busy would delete it in exactly the window the
        // grammar exists for.
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let nvim = Owner::module("nvim");
        let tmux = Owner::module("tmux");
        let action = probe_action();
        let mut slots = vec![
            slot(&nvim, Tier::Modules, "brew", &action),
            slot(&nvim, Tier::Modules, "apt", &action),
            slot(&tmux, Tier::Modules, "apt", &action),
        ];
        slots[0].state = SlotState::Running;
        slots[2].state = SlotState::Running;
        let groups = groups_of(&slots);
        let mut bars = empty_bars();

        refresh(
            &printer,
            &slots,
            &groups,
            &HashMap::new(),
            &busy(&["brew", "apt"]),
            &mut bars,
        );

        assert_eq!(on_screen(&bars), vec!["module:nvim · waiting on apt"]);
    }

    #[test]
    fn a_blocked_sub_manager_action_names_the_family_holding_the_lane() {
        // `brew-cask` and `brew` are one binary, so the line has to say what is
        // actually in the way rather than repeating the action's own name.
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let profile = Owner::profile("work");
        let action = probe_action();
        let mut slots = vec![
            slot(&profile, Tier::Rest, "brew", &action),
            slot(&profile, Tier::Rest, "brew-cask", &action),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);
        let mut bars = empty_bars();

        refresh(
            &printer,
            &slots,
            &groups,
            &HashMap::new(),
            &busy(&["brew"]),
            &mut bars,
        );

        assert_eq!(on_screen(&bars), vec!["profile:work · waiting on brew"]);
    }

    #[test]
    fn one_owners_actions_behind_one_family_share_a_single_line() {
        // Formulae, taps and casks all queue behind one `brew` process, so the
        // owner has three blocked actions and exactly one thing to say. Keyed
        // by slot the live region would repeat `waiting on brew` three times,
        // character for character. A second lane still earns its own line —
        // the collapse is of identical sentences, not of blocked actions.
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let profile = Owner::profile("work");
        let action = probe_action();
        let mut slots = vec![
            slot(&profile, Tier::Rest, "brew", &action),
            slot(&profile, Tier::Rest, "brew-tap", &action),
            slot(&profile, Tier::Rest, "brew-cask", &action),
            slot(&profile, Tier::Rest, "apt", &action),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);
        let mut bars = empty_bars();

        refresh(
            &printer,
            &slots,
            &groups,
            &HashMap::new(),
            &busy(&["brew", "apt"]),
            &mut bars,
        );

        assert_eq!(
            on_screen(&bars),
            vec![
                "profile:work · waiting on apt",
                "profile:work · waiting on brew",
            ]
        );
        assert_eq!(bars.actions.len(), 2, "no line is drawn twice");
    }

    #[test]
    fn an_action_waiting_on_its_dependency_gets_no_manager_line() {
        // It is not waiting on a manager — its module is waiting on another
        // module, and saying "waiting on apt" would name the wrong thing.
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let nvim = Owner::module("nvim");
        let base = Owner::module("base");
        let action = probe_action();
        let mut slots = vec![
            slot(&base, Tier::Modules, "apt", &action),
            slot(&nvim, Tier::Modules, "apt", &action),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);
        let deps = HashMap::from([("nvim", HashSet::from(["base"]))]);
        let mut bars = empty_bars();

        refresh(&printer, &slots, &groups, &deps, &busy(&["apt"]), &mut bars);

        assert!(on_screen(&bars).is_empty(), "{:?}", on_screen(&bars));
    }

    #[test]
    fn a_groups_line_is_removed_once_the_tier_in_flight_advances() {
        // The group's line is drawn while Modules is in flight, and removed
        // outright — never left stale — once nothing above the group is left.
        let (printer, _buf) = Printer::for_test_with_live_bars();
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
        let mut bars = empty_bars();

        refresh(&printer, &slots, &groups, &deps, &idle, &mut bars);
        assert_eq!(
            on_screen(&bars),
            vec![
                "cfgd:managers · waiting on modules",
                "profile:work · waiting on modules",
            ]
        );

        slots[0].state = SlotState::Done;
        refresh(&printer, &slots, &groups, &deps, &idle, &mut bars);
        assert!(
            on_screen(&bars).is_empty(),
            "nothing is ever blocked behind the last tier"
        );
        assert_eq!(bars.groups.len(), 0, "the line is removed, not left stale");
    }

    #[test]
    fn an_action_line_is_dropped_once_its_lane_frees() {
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let profile = Owner::profile("work");
        let action = probe_action();
        let mut slots = vec![
            slot(&profile, Tier::Rest, "apt", &action),
            slot(&profile, Tier::Rest, "apt", &action),
        ];
        slots[0].state = SlotState::Running;
        let groups = groups_of(&slots);
        let deps = HashMap::new();
        let mut bars = empty_bars();

        refresh(&printer, &slots, &groups, &deps, &busy(&["apt"]), &mut bars);
        assert_eq!(on_screen(&bars), vec!["profile:work · waiting on apt"]);

        slots[0].state = SlotState::Done;
        refresh(&printer, &slots, &groups, &deps, &busy(&[]), &mut bars);
        assert!(
            bars.actions.is_empty(),
            "a wait line outlives neither the wait nor the action"
        );
    }

    /// The order new group bars are `printer.wait_bar`-ed in, read back through
    /// `WaitBar::seq` since indicatif exposes no public draw-position API.
    fn group_bar_creation_order(bars: &WaitBars<'_>) -> Vec<String> {
        let mut by_seq: Vec<(String, u64)> = bars
            .groups
            .iter()
            .map(|(token, bar)| (token.clone(), bar.seq()))
            .collect();
        by_seq.sort_by_key(|(_, seq)| seq.to_owned());
        by_seq.into_iter().map(|(token, _)| token).collect()
    }

    #[test]
    fn group_wait_bars_are_created_in_slot_order_not_hash_order() {
        // Owners are deliberately non-alphabetical: a `HashMap`-driven creation
        // loop reshuffles this every process, and the top-to-bottom order new
        // bars are drawn in has to match `groups_of`'s plan order — the same
        // order the phase tree shows — every run, not just this one.
        let (printer, _buf) = Printer::for_test_with_live_bars();
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
            let mut bars = empty_bars();
            refresh(
                &printer,
                &slots,
                &groups,
                &HashMap::new(),
                &busy(&[]),
                &mut bars,
            );
            assert_eq!(
                group_bar_creation_order(&bars),
                vec!["module:zeta", "module:alpha", "module:mid"],
                "wait bars must be created in slot/plan order every run"
            );
        }
    }

    /// The order new action bars are `printer.wait_bar`-ed in, same reasoning
    /// as `group_bar_creation_order` but for the `(owner, lane)`-keyed map.
    fn action_bar_creation_order(bars: &WaitBars<'_>) -> Vec<String> {
        let mut by_seq: Vec<(String, u64)> = bars
            .actions
            .iter()
            .map(|((owner, _lane), bar)| (owner.clone(), bar.seq()))
            .collect();
        by_seq.sort_by_key(|(_, seq)| seq.to_owned());
        by_seq.into_iter().map(|(owner, _)| owner).collect()
    }

    #[test]
    fn action_wait_bars_are_created_in_slot_order_not_hash_order() {
        let (printer, _buf) = Printer::for_test_with_live_bars();
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
            let mut bars = empty_bars();
            refresh(
                &printer,
                &slots,
                &groups,
                &HashMap::new(),
                &busy(&["brew"]),
                &mut bars,
            );
            assert_eq!(
                action_bar_creation_order(&bars),
                vec!["module:zeta", "module:alpha", "module:mid"],
                "wait bars must be created in slot order every run"
            );
        }
    }
}

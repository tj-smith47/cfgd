//! The `Packages` phase's dispatcher: per-manager lanes behind Rule P's tier
//! barrier.
//!
//! Every other phase mutates shared user state and stays a sequential walk.
//! Package installs do not: two managers driving two different binaries have
//! nothing to contend over, and the run's wall-clock cost is dominated by them.
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
//! 2. **Tier B is serial**, in plan order: a bootstrap can depend on another
//!    bootstrap, which is the one intra-tier edge in the phase.
//! 3. **Module `depends`** — a module's package work waits for every action of
//!    its transitive dependencies.
//! 4. **The per-manager lane** — at most one action per manager is in flight,
//!    so the maximum parallelism is the number of distinct managers.
//! 5. **The owner's turn** — an owner already holding a lane takes a second one
//!    only after every other owner with ready work has taken one. So a module
//!    declaring brew and apt work takes brew while another module holds apt,
//!    and then says it is waiting on apt — while a lone owner still fills every
//!    lane in the phase.
//! 6. **The serial sub-gate** — any action whose manager is registered and not
//!    currently available drains the phase. Evaluated at dispatch time, because
//!    a manager bootstrapped earlier in the same phase becomes available
//!    mid-run.
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
    Action, ModuleAction, ModuleActionKind, Owner, PhaseName, ReconcileContext, Tier,
};

/// The run-scoped inputs every package action needs, none of which change
/// between actions.
pub(super) struct PackageRun<'x> {
    pub(super) printer: &'x Printer,
    pub(super) apply_id: i64,
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
fn tier_in_flight(pending_per_tier: [usize; 3]) -> Option<Tier> {
    Tier::ALL
        .into_iter()
        .find(|tier| pending_per_tier[tier_index(*tier)] > 0)
}

fn tier_index(tier: Tier) -> usize {
    match tier {
        Tier::Modules => 0,
        Tier::Bootstraps => 1,
        Tier::Rest => 2,
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
/// no group can ever name its own tier; under `0 → B → 1` a module group is
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
/// The predicate is "registered and not currently available", not "is a
/// `Bootstrap` action": a module install on an unavailable manager bootstraps it
/// inline, with no planned bootstrap action anywhere in the plan, and a gate
/// keyed on the action variant would miss that entirely. An UNREGISTERED name —
/// the `script` pseudo-manager, or a typo — bootstraps nothing and so drains
/// nothing.
fn drains_phase(registry: &ProviderRegistry, manager: &str) -> bool {
    registry
        .package_managers
        .iter()
        .any(|pm| pm.name() == manager && !pm.is_available())
}

impl<'p> Slot<'p> {
    fn drains(&self, registry: &ProviderRegistry) -> bool {
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

/// What the dispatcher holds while it decides.
struct DispatchState<'a> {
    lanes_busy: &'a HashSet<String>,
    owners_busy: &'a HashSet<String>,
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
        pending_in(slots, Tier::Bootstraps),
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
        // Tier B is dispatched serially in plan order: a bootstrap can depend
        // on another bootstrap (pipx's installs brew first), which is the one
        // intra-tier edge in the phase. So the FIRST waiting bootstrap is the
        // only candidate, and it waits for an empty phase.
        if in_flight == Tier::Bootstraps {
            return (state.running == 0).then_some(index);
        }
        if !depends_satisfied(slots, index, deps) {
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
        if state.owners_busy.contains(&slot.owner.token()) {
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
/// by its owner token, an action's by its slot.
struct WaitBars<'p> {
    groups: HashMap<String, WaitBar<'p>>,
    actions: HashMap<usize, WaitBar<'p>>,
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
    /// Run the `Packages` phase across per-manager lanes, handing each finished
    /// action to `collect` on this thread, in completion order.
    ///
    /// Returns the abort exit code when a signal stopped the dispatch.
    pub(super) fn dispatch_package_lanes<'p>(
        &self,
        dispatch: &[(&'p Owner, &'p Action, usize)],
        run: &PackageRun<'_>,
        collect: &mut dyn FnMut(&Action, LaneCollected),
    ) -> Option<u8> {
        let mut slots: Vec<Slot<'p>> = dispatch
            .iter()
            .map(|(owner, action, plan_index)| Slot {
                owner,
                action,
                plan_index: *plan_index,
                tier: Tier::of(owner, action),
                manager: action_manager(action).map(str::to_string),
                module: match action {
                    Action::Module(ModuleAction { module_name, .. }) => Some(module_name.clone()),
                    _ => None,
                },
                state: SlotState::Waiting,
            })
            .collect();
        let deps = transitive_depends(run.module_actions);
        let groups = groups_of(&slots);
        let registry = self.registry;

        let mut lanes_busy: HashSet<String> = HashSet::new();
        let mut owners_busy: HashSet<String> = HashSet::new();
        // The slot of the draining action in flight, if any. Recorded by slot
        // rather than recomputed at collection, because a bootstrap's whole
        // point is that its manager IS available by the time it finishes.
        let mut draining: Option<usize> = None;
        let mut running: usize = 0;
        let mut aborted: Option<u8> = None;
        let mut journal_ids: HashMap<usize, Option<i64>> = HashMap::new();
        let mut bars = WaitBars {
            groups: HashMap::new(),
            actions: HashMap::new(),
        };

        let (tx, inbox) = channel::<LaneMessage>();

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
                        let tx = tx.clone();

                        slots[index].state = SlotState::Running;
                        running += 1;
                        if slots[index].drains(registry) {
                            draining = Some(index);
                        }
                        if let Some(lane) = slots[index].lane() {
                            lanes_busy.insert(lane.to_string());
                        }
                        owners_busy.insert(slots[index].owner.token());
                        let manager = slots[index].manager.clone().unwrap_or_default();
                        journal_ids.insert(index, journal_id);
                        scope.spawn(move || {
                            let finished = run_one_action(
                                registry, run, action, &lane, &tx, &manager, &path_dirs,
                            );
                            let body = lane.finish();
                            let _ = tx.send(LaneMessage::Finished(Box::new(LaneFinished {
                                slot: index,
                                result: finished.result,
                                elapsed: finished.elapsed,
                                notes: finished.notes,
                                body,
                                bootstrapped: finished.bootstrapped,
                            })));
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
                }

                if running == 0 {
                    if aborted.is_none() && slots.iter().any(|s| s.state == SlotState::Waiting) {
                        tracing::warn!(
                            "package dispatch stalled with actions still waiting; \
                             they will be reported as not applied"
                        );
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
                        owners_busy.remove(&slots[index].owner.token());
                        bars.actions.remove(&index);
                        self.persist_bootstraps(done.bootstrapped);
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
                    }
                    // Every worker holds a sender and the coordinator holds
                    // one, so this is unreachable while anything is running.
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
        run: &PackageRun<'_>,
        action: &Action,
        action_index: usize,
    ) -> Option<i64> {
        let description = format_action_description(action);
        let (action_type, resource_id) = parse_resource_from_description(&description);
        self.state
            .journal_begin(
                run.apply_id,
                action_index,
                PhaseName::Packages.as_str(),
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
        pending_in(slots, Tier::Bootstraps),
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
    let wanted: HashMap<String, String> = tier_waits(&waits, in_flight)
        .into_iter()
        .map(|(owner, subject)| (owner.token(), subject))
        .collect();
    bars.groups.retain(|token, _| wanted.contains_key(token));
    for (token, subject) in wanted {
        match bars.groups.get(&token) {
            // Replaced, never appended to: the chain `modules` →
            // `bootstraps` → the group's own bars is one line changing what
            // it says, not three lines accumulating.
            Some(bar) => bar.set_subject(&subject),
            None => {
                bars.groups.insert(token, printer.wait_bar(&subject));
            }
        }
    }

    let mut blocked: HashMap<usize, String> = HashMap::new();
    for (index, slot) in slots.iter().enumerate() {
        if slot.state != SlotState::Waiting || Some(slot.tier) != in_flight {
            continue;
        }
        if !depends_satisfied(slots, index, inputs.deps) {
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
            // held back by a running `brew` is waiting on brew.
            blocked.insert(index, wait_subject(slot.owner, lane));
        }
    }
    bars.actions.retain(|index, _| blocked.contains_key(index));
    for (index, subject) in blocked {
        match bars.actions.get(&index) {
            Some(bar) => bar.set_subject(&subject),
            None => {
                bars.actions.insert(index, printer.wait_bar(&subject));
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
    run: &PackageRun<'_>,
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
    let executed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let exec = PackageExec::new(registry, &proxy, run.printer, &notes).in_lane(lane);
        let result = match action {
            Action::Package(pkg) => exec.apply_package_action(pkg).map(|desc| (desc, true)),
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
            // The phase holds only package work by construction
            // (`phase_for_module_kind`), so this arm exists for totality.
            other => Ok((format_action_description(other), false)),
        };
        (result, exec.take_bootstrapped())
    }));
    let (result, bootstrapped) = match executed {
        Ok(pair) => pair,
        Err(_) => (
            Err(PackageError::LanePanicked {
                manager: manager.to_string(),
            }
            .into()),
            Vec::new(),
        ),
    };
    LaneWorkerResult {
        result,
        elapsed: started.elapsed(),
        notes: notes.take(),
        bootstrapped,
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
                group(&managers, Tier::Bootstraps, true),
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
            group(&managers, Tier::Bootstraps, true),
            group(&profile, Tier::Rest, true),
        ];

        // Module work in flight: both lower tiers wait on it.
        assert_eq!(tier_in_flight([2, 1, 1]), Some(Tier::Modules));
        assert_eq!(
            subjects(&groups, tier_in_flight([2, 1, 1])),
            vec![
                "cfgd:managers · waiting on modules",
                "profile:work · waiting on modules",
            ]
        );

        // The line is REPLACED as the tier in flight changes, and the group
        // that was in flight is no longer blocked by anything.
        assert_eq!(tier_in_flight([0, 1, 1]), Some(Tier::Bootstraps));
        assert_eq!(
            subjects(&groups, tier_in_flight([0, 1, 1])),
            vec!["profile:work · waiting on bootstraps"]
        );

        // Nothing is ever blocked behind the last tier.
        assert_eq!(tier_in_flight([0, 0, 1]), Some(Tier::Rest));
        assert!(subjects(&groups, tier_in_flight([0, 0, 1])).is_empty());
        assert_eq!(tier_in_flight([0, 0, 0]), None);
    }

    #[test]
    fn wait_line_skips_an_empty_tier() {
        // A plan with a bootstrap and a profile install and no module package
        // work: tier 0 is released and drained in the same instant, so it is
        // never in flight and `waiting on modules` is never said.
        let profile = Owner::profile("work");
        let groups = [group(&profile, Tier::Rest, true)];

        assert_eq!(tier_in_flight([0, 1, 1]), Some(Tier::Bootstraps));
        assert_eq!(
            subjects(&groups, tier_in_flight([0, 1, 1])),
            vec!["profile:work · waiting on bootstraps"],
            "an empty tier is never in flight and is never named"
        );
    }

    #[test]
    fn a_group_with_nothing_left_to_dispatch_renders_no_wait_line() {
        let profile = Owner::profile("work");
        assert!(
            subjects(
                &[group(&profile, Tier::Rest, false)],
                Some(Tier::Bootstraps)
            )
            .is_empty(),
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
    fn a_groups_line_is_replaced_as_the_tier_in_flight_advances() {
        // One line changing what it says, not two lines accumulating — and it
        // is removed outright once nothing above the group is left.
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let nvim = Owner::module("nvim");
        let managers = Owner::cfgd("managers");
        let profile = Owner::profile("work");
        let action = probe_action();
        let mut slots = vec![
            slot(&nvim, Tier::Modules, "apt", &action),
            slot(&managers, Tier::Bootstraps, "pipx", &action),
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
        assert_eq!(
            on_screen(&bars),
            vec!["profile:work · waiting on bootstraps"],
            "the group that was blocked keeps ONE line, re-labelled"
        );
        assert_eq!(bars.groups.len(), 1, "replaced, never appended to");

        slots[1].state = SlotState::Done;
        refresh(&printer, &slots, &groups, &deps, &idle, &mut bars);
        assert!(
            on_screen(&bars).is_empty(),
            "nothing is ever blocked behind the last tier"
        );
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
}

//! Withholding the resources a source decision covers.
//!
//! `docs/sources.md` gives a source-delivered item three decision states, and
//! only one of them reaches the machine: an item **awaiting** a decision is
//! recorded and notified but not applied, an item the operator **declined** is
//! excluded from reconciliation, and only an **accepted** one is included in
//! the next reconcile. The type below is how every planning path keeps that
//! promise — the daemon's auto-apply tick, `cfgd plan`, and `cfgd apply` in all
//! its modes translate the same rows through the same vocabulary and prune the
//! same plan shape, so no path can execute an item another path withholds.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::{self, MergedProfile, ResolvedProfile};
use crate::errors::Result;
use crate::state::{PendingDecision, StateStore};
use crate::to_posix_string;

use super::{Action, Plan, SystemAction, action_resource_info};

/// Every resource a merged profile declares, in decision vocabulary.
///
/// The one derivation of the dot-notation paths the source-decision workflow
/// mints (`packages.<mgr>.<pkg>`, `files.<target>`, `env.<NAME>`,
/// `system.<key>`). Both halves of the workflow read it: the daemon hashes a
/// source's paths to notice what it newly delivers, and [`DecisionScope`] reads
/// the LOCAL layer's paths so a decision about a source's item can never
/// withhold the operator's own declaration of the same resource.
pub fn declared_decision_paths(merged: &MergedProfile) -> HashSet<String> {
    let mut resources = HashSet::new();

    let pkgs = &merged.packages;
    if let Some(ref brew) = pkgs.brew {
        for f in &brew.formulae {
            resources.insert(format!("packages.brew.{}", f));
        }
        for c in &brew.casks {
            resources.insert(format!("packages.brew.{}", c));
        }
    }
    if let Some(ref apt) = pkgs.apt {
        for p in &apt.packages {
            resources.insert(format!("packages.apt.{}", p));
        }
    }
    if let Some(ref cargo) = pkgs.cargo {
        for p in &cargo.packages {
            resources.insert(format!("packages.cargo.{}", p));
        }
    }
    for p in &pkgs.pipx {
        resources.insert(format!("packages.pipx.{}", p));
    }
    for p in &pkgs.dnf {
        resources.insert(format!("packages.dnf.{}", p));
    }
    if let Some(ref npm) = pkgs.npm {
        for p in &npm.global {
            resources.insert(format!("packages.npm.{}", p));
        }
    }

    for file in &merged.files.managed {
        resources.insert(format!("files.{}", to_posix_string(&file.target)));
    }

    for ev in &merged.env {
        resources.insert(format!("env.{}", ev.name));
    }

    for k in merged.system.keys() {
        resources.insert(format!("system.{}", k));
    }

    resources
}

/// What one source actually delivered into a composed profile.
///
/// The decision workflow's question is "which resources did source X put in
/// front of me", and only the COMPOSED profile can answer it: composition tags
/// every layer it builds with the source that supplied it, so the source's own
/// layers merge back into the same shape a profile resolution produces. Reading
/// the local profile instead answers a different question entirely — it names
/// the subscriber's own declarations, so no source-delivered item is ever
/// reachable by a decision and every local one is minted as if a source had
/// sent it.
///
/// A tier the subscriber has not opted into never becomes a layer
/// (`accept_recommended: false` keeps the recommended tier out, `opt_in` gates
/// the optional profiles), so it is absent here too: an item cfgd will not
/// apply mints no pending row and adds no noise to a plan.
pub fn source_delivered_profile(resolved: &ResolvedProfile, source_name: &str) -> MergedProfile {
    let layers: Vec<config::ProfileLayer> = resolved
        .layers
        .iter()
        .filter(|l| l.source == source_name)
        .cloned()
        .collect();
    config::merge_layers(&layers)
}

/// The layer name composition gives the subscriber's own profile.
const LOCAL_LAYER: &str = "local";

/// The gate every decision passes before it can withhold anything.
///
/// A decision row names a source and a resource path, and nothing more. Two
/// facts about the run decide whether that row may still take a resource off
/// the machine, and neither of them is in the row:
///
/// - **The source must still be subscribed.** A source the operator has dropped
///   leaves rows nobody can answer — `cfgd decide` names a source that is gone
///   — so its decisions, pending or rejected and however old, withhold nothing.
///   That is what stops a rejection becoming a permanent invisible block on a
///   path, and it makes the rows earlier versions auto-resolved as `rejected`
///   on removal inert again without a migration. Subscription is read from the
///   config rather than from what composition actually merged, so a transient
///   cache miss cannot un-withhold an undecided item for a run.
/// - **The operator must not declare it themselves.** A local declaration is
///   the operator's own intent, at composition's highest priority. Withholding
///   it because a source offers the same path — the same `~/.zshrc`, the same
///   package — would let a decision about the source's copy settle the fate of
///   work the operator wrote.
#[derive(Debug, Default)]
pub struct DecisionScope {
    subscribed: HashSet<String>,
    local: HashSet<String>,
}

impl DecisionScope {
    /// Build the scope from the currently subscribed source names and the
    /// composed profile whose local layers say what the operator declares.
    pub fn new<I, S>(subscribed: I, resolved: &ResolvedProfile) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            subscribed: subscribed.into_iter().map(Into::into).collect(),
            local: declared_decision_paths(&source_delivered_profile(resolved, LOCAL_LAYER)),
        }
    }

    /// Whether a decision raised by `source` over `resource` still withholds it.
    pub fn withholds(&self, source: &str, resource: &str) -> bool {
        self.subscribed.contains(source) && !self.local.contains(resource)
    }
}

/// The decisions withholding something from this run, split by the state that
/// withholds them.
///
/// Both halves prune, and both are rendered: a resource missing from a plan is
/// explained by a row the operator can see, whichever state that row is in.
#[derive(Debug, Default)]
pub struct WithheldDecisions {
    /// Rows awaiting `cfgd decide`.
    pub pending: Vec<PendingDecision>,
    /// Rows the operator already declined.
    pub rejected: Vec<PendingDecision>,
}

impl WithheldDecisions {
    /// Read the withholding rows from `store` and keep the ones `scope` still
    /// admits.
    ///
    /// Fails rather than degrading: this is the gate deciding what reaches the
    /// machine, so a store that cannot be read must stop the run. Treating the
    /// error as "nothing is withheld" would apply every undecided item on a
    /// locked or corrupt database, silently.
    pub fn read(store: &StateStore, scope: &DecisionScope) -> Result<Self> {
        let mut out = Self::default();
        for decision in store.withheld_decisions()? {
            if !scope.withholds(&decision.source, &decision.resource) {
                continue;
            }
            if decision.resolved_at.is_some() {
                out.rejected.push(decision);
            } else {
                out.pending.push(decision);
            }
        }
        Ok(out)
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.rejected.is_empty()
    }

    /// The resource path of every withholding row, in decision vocabulary.
    ///
    /// For a caller that must fold its own paths in beside them — the daemon
    /// adds the items a policy declined outright, which record no row.
    pub fn resource_paths(&self) -> impl Iterator<Item = String> + '_ {
        self.pending
            .iter()
            .chain(self.rejected.iter())
            .map(|d| d.resource.clone())
    }
}

/// The resources a source decision withholds, in the plan's own vocabulary.
///
/// The ONE place that knows both grammars. A decision row carries the
/// dot-notation path the source-decision workflow mints; a planned action
/// carries the resource id `action_resource_info` mints. The two never agree
/// as strings — `packages.cargo.bat` against `cargo:bat,ripgrep`,
/// `files.~/.zshrc` against `/home/u/.zshrc` — so the translation happens here,
/// once, per arm:
///
/// | decision path | what it withholds |
/// |---|---|
/// | `files.<target>` | a `File` action on that target, and the same target inside a module's `DeployFiles` batch — profile files and module files are separate surfaces that can name one path, and withholding only the profile one would still write it. The decision keeps the DECLARED spelling, the planner expands `~`, so the path is expanded and folded to `/` here to meet the id |
/// | `packages.<mgr>.<pkg>` | that one package inside a batch — a `PackageAction::Install`/`Uninstall` for `<mgr>` or a module's `InstallPackages` (matched on its resolved name). The batch keeps its other packages and is dropped only when it empties. `packages.brew.<pkg>` also matches the `brew-cask` manager: the decision vocabulary folds casks into `brew` and cannot tell a cask from a formula. A `Bootstrap` or `Skip` names no package and is never withheld — a bootstrap installs the package MANAGER, which every still-decided package in the batch needs |
/// | `env.<NAME>` | every `Env` action. There is no per-variable action to withhold: one `WriteEnvFile` renders every declared variable into one file, `InjectSourceLine` loads that file and `RefreshLiveSession` mirrors it — so the env surface is withheld as the unit it is generated as, and a decided variable waits with the undecided one rather than an undecided one reaching the machine. That includes the post-apply regeneration: a manager bootstrapped in a withholding tick does not get its PATH dir into `~/.cfgd.env` until the decision clears (the next non-withholding tick plans env unconditionally and converges it) |
/// | `system.<configurator>` | every `System` action for that configurator. The decision names a whole `spec.system.<configurator>` block, one level above the `<configurator>:<key>` id an individual drift carries |
///
/// No pending row can withhold a `Secret` or `Script` action as a whole, and a
/// `Module` action is withheld only by the batch arms above — the packages a
/// module installs and the files it deploys leave its batches one entry at a
/// time, while the module action itself (and every `RunScript` or `Skip`)
/// stays. Those four prefixes are the only paths the source-decision workflow
/// mints, so nothing else is reachable from a decision.
///
/// Two consequences a reader of a pruned run should expect. A withheld
/// resource leaves the plan entirely, so it is absent from the drift surface
/// too: `cfgd status` shows the decision awaiting review, not a drift row for
/// the resource behind it. And an id derived from a batch moves when a decision
/// toggles — the surviving half of a batch renders `cargo:ripgrep` while it is
/// withheld and `cargo:bat,ripgrep` once it is accepted — so drift rows and
/// journal entries for a partially-withheld batch are keyed to the shape the
/// run applied, not to a stable per-resource id.
#[derive(Debug, Default)]
pub struct DecisionExclusions {
    files: HashSet<String>,
    packages: HashMap<String, HashSet<String>>,
    env: HashSet<String>,
    system: HashSet<String>,
}

impl DecisionExclusions {
    /// Translate pending decision paths into the action vocabulary.
    ///
    /// `expand` is the caller's `~` expansion — the daemon passes the same
    /// injectable one its planning hooks use, so a test that redirects home
    /// redirects this too. The id-producing sides (`modules/resolve.rs`,
    /// `files/plan.rs`) still call the free `expand_tilde`; the two are
    /// identical in production, and a hooks impl that expanded differently
    /// would stop this set matching the ids those sites mint.
    pub fn from_decision_paths<I, E>(paths: I, expand: E) -> Self
    where
        I: IntoIterator<Item = String>,
        E: Fn(&Path) -> PathBuf,
    {
        let mut out = Self::default();
        for path in paths {
            let unmatched = |detail: &str| {
                tracing::warn!(
                    decision = %path,
                    "pending decision {detail} — it cannot be withheld from the plan"
                );
            };
            if let Some(target) = path.strip_prefix("files.") {
                if target.is_empty() {
                    unmatched("names no file");
                    continue;
                }
                out.files.insert(to_posix_string(expand(Path::new(target))));
            } else if let Some(rest) = path.strip_prefix("packages.") {
                match rest.split_once('.') {
                    Some((manager, package)) if !manager.is_empty() && !package.is_empty() => {
                        out.packages
                            .entry(manager.to_string())
                            .or_default()
                            .insert(package.to_string());
                    }
                    _ => unmatched("names no manager and package"),
                }
            } else if let Some(name) = path.strip_prefix("env.") {
                if name.is_empty() {
                    unmatched("names no variable");
                    continue;
                }
                out.env.insert(name.to_string());
            } else if let Some(key) = path.strip_prefix("system.") {
                if key.is_empty() {
                    unmatched("names no configurator");
                    continue;
                }
                out.system.insert(key.to_string());
            } else {
                unmatched("is in no known resource vocabulary");
            }
        }
        out
    }

    /// Every withholding decision, in the action vocabulary.
    ///
    /// Unresolved and `rejected` rows both withhold — see
    /// [`StateStore::withheld_decisions`] for why one query answers both — so a
    /// caller reaching for this never has to ask which state a row is in. It
    /// takes the same [`WithheldDecisions`] the preview renders, so the rows
    /// naming what is missing and the rows removing it are one list.
    ///
    /// `~` is expanded through the free [`crate::expand_tilde`] — the same one
    /// `files/plan.rs` and `modules/resolve.rs` mint their targets with, and
    /// the one a test home guard redirects.
    pub fn from_withheld(withheld: &WithheldDecisions) -> Self {
        Self::from_decision_paths(withheld.resource_paths(), crate::expand_tilde)
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
            && self.packages.is_empty()
            && self.env.is_empty()
            && self.system.is_empty()
    }

    /// Whether the whole action is withheld. A batching action never is — its
    /// entries leave one at a time, through [`Self::withholds_package`] and
    /// [`Self::withholds_file`], and the action goes only when they empty it.
    pub fn withholds_action(&self, action: &Action) -> bool {
        match action {
            // Read through `action_resource_info` so the file id this compares
            // against is the same derivation the drift row and the journal use.
            Action::File(_) => self.files.contains(&action_resource_info(action).1),
            Action::Env(_) => self.withholds_env_surface(),
            // Matched on the typed field rather than on the rendered
            // `<configurator>:<key>` id: splitting an id back apart at the match
            // site would be a second grammar living outside this type.
            Action::System(SystemAction::SetValue { configurator, .. })
            | Action::System(SystemAction::Skip { configurator, .. }) => {
                self.system.contains(configurator)
            }
            _ => false,
        }
    }

    /// Whether the env surface is withheld.
    ///
    /// The one predicate behind both halves of the guarantee: the `Env` arm of
    /// [`Self::withholds_action`], which prunes the planned env actions, and
    /// `Reconciler::withholding_env_surface`, which stops apply rebuilding that
    /// same surface from the declared set after the phases run. A second match
    /// site would be a second answer.
    pub fn withholds_env_surface(&self) -> bool {
        !self.env.is_empty()
    }

    /// Whether one file inside a module's deploy batch is withheld. Compared on
    /// the same expanded, `/`-folded spelling the `files.` arm stored, which is
    /// what a `ResolvedFile` target already carries.
    pub fn withholds_file(&self, target: &Path) -> bool {
        self.files.contains(&to_posix_string(target))
    }

    /// Whether one package inside a batch is withheld.
    pub fn withholds_package(&self, manager: &str, package: &str) -> bool {
        self.names_package(manager, package)
            // Casks are minted as `packages.brew.<name>`, but the planner
            // installs them through the `brew-cask` manager.
            || (manager == "brew-cask" && self.names_package("brew", package))
    }

    fn names_package(&self, manager: &str, package: &str) -> bool {
        self.packages
            .get(manager)
            .is_some_and(|packages| packages.contains(package))
    }
}

/// Prune every action `exclusions` withholds out of `plan`, returning how many
/// actions left it.
///
/// The one prune, shared by the daemon's tick and by `cfgd plan` / `cfgd apply`:
/// a resource awaiting a decision leaves the plan itself rather than being
/// discounted afterwards, so the action count, the preview, the `-o json`
/// payload, the drift rows a run records and the actions it executes all
/// describe one set. A phase emptied by the prune is dropped, so no surface
/// renders a header over nothing.
///
/// Pruning alone is only half the guarantee for the env surface — a caller that
/// applies must also build its `Reconciler` with
/// [`withholding_env_surface`](super::Reconciler::withholding_env_surface), fed
/// from [`DecisionExclusions::withholds_env_surface`], because apply regenerates
/// that surface from the DECLARED set after the phases run.
pub fn withhold_from_plan(plan: &mut Plan, exclusions: &DecisionExclusions) -> usize {
    if exclusions.is_empty() {
        return 0;
    }
    let before = plan.total_actions();
    for phase in &mut plan.phases {
        phase.retain_actions_and_batches(
            |action| !exclusions.withholds_action(action),
            |manager, package| !exclusions.withholds_package(manager, package),
            |target| !exclusions.withholds_file(target),
        );
    }
    plan.phases.retain(|p| !p.is_empty());
    let withheld = before.saturating_sub(plan.total_actions());
    if withheld > 0 {
        tracing::info!(
            actions = withheld,
            "withheld action(s) whose resource awaits a source decision"
        );
    }
    withheld
}

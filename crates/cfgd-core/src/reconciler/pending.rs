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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::{
    self, AutoApplyPolicyConfig, CfgdConfig, LOCAL_LAYER, LayerPolicy, MergedProfile, PolicyAction,
    ResolvedProfile,
};
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

    // The package half walks the SAME enumeration the reconciler plans from
    // (`manager_names` → `desired_packages_for_spec`), so a manager added to
    // the planner is covered here by construction — a hand-kept second list is
    // how six managers minted decisions while the other nine installed a
    // source's items without one.
    let pkgs = &merged.packages;
    for manager in pkgs.manager_names() {
        let Some(decision_manager) = decision_manager_name(&manager) else {
            continue;
        };
        for pkg in config::desired_packages_for_spec(&manager, pkgs) {
            resources.insert(format!("packages.{}.{}", decision_manager, pkg));
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

/// The manager segment a package's decision path carries, from the planner's
/// manager name.
///
/// Casks fold into `brew`: the decision vocabulary cannot tell a cask from a
/// formula, and [`DecisionExclusions::withholds_package`] already meets the
/// planner's `brew-cask` batch through the same fold — splitting them now
/// would orphan every recorded `packages.brew.<cask>` row. Everything else
/// passes through verbatim, EXCEPT a manager whose own name contains a `.`:
/// the path grammar splits on the first dot, so such a name cannot round-trip
/// into [`DecisionExclusions`] and the row it minted could never withhold the
/// item it names. Minting nothing keeps the grammar honest, and the contract
/// holds anyway: [`undecidable_source_batches`] withholds the manager's
/// source-delivered packages fail-closed, no row required. Only a custom
/// manager can carry such a name; every built-in is dot-free.
fn decision_manager_name(manager: &str) -> Option<&str> {
    if manager.contains('.') {
        return None;
    }
    Some(if manager == "brew-cask" {
        "brew"
    } else {
        manager
    })
}

/// A source-delivered package batch no decision row can ever name: the custom
/// manager's own name contains a `.`, which the decision path grammar splits
/// on. Ask-before-install still holds — the batch is withheld from the plan
/// fail-closed — but there is nothing `cfgd decide` could record, so the run
/// warns instead of listing a row, and the way out is renaming the manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndecidableBatch {
    pub source: String,
    pub manager: String,
    pub packages: Vec<String>,
}

impl UndecidableBatch {
    /// The warning line the run header renders and the `-o json` payload's
    /// `warnings` carries — the same surface the withheld rows are named on.
    pub fn warning(&self) -> String {
        format!(
            "custom manager '{}' cannot carry source decisions — its name contains '.', which the decision path grammar splits on. Withheld from this run until the manager is renamed: {} (from {})",
            self.manager,
            self.packages.join(", "),
            self.source
        )
    }
}

/// The batches [`UndecidableBatch`] describes, for every subscribed source.
///
/// A package the operator ALSO declares locally under the same manager is
/// theirs, not the source's — it stays in the plan, mirroring the
/// [`DecisionScope`] guard that keeps local declarations out of every other
/// withhold.
pub fn undecidable_source_batches<'a, I>(
    resolved: &ResolvedProfile,
    sources: I,
) -> Vec<UndecidableBatch>
where
    I: IntoIterator<Item = &'a str>,
{
    let local = local_profile(resolved);
    let mut out = Vec::new();
    for source in sources {
        let delivered = source_delivered_profile(resolved, source);
        for manager in delivered.packages.manager_names() {
            if !manager.contains('.') {
                continue;
            }
            let local_pkgs: HashSet<String> =
                config::desired_packages_for_spec(&manager, &local.packages)
                    .into_iter()
                    .collect();
            let packages: Vec<String> =
                config::desired_packages_for_spec(&manager, &delivered.packages)
                    .into_iter()
                    .filter(|p| !local_pkgs.contains(p))
                    .collect();
            if !packages.is_empty() {
                out.push(UndecidableBatch {
                    source: source.to_string(),
                    manager,
                    packages,
                });
            }
        }
    }
    out
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
    config::merge_layers(&source_delivered_layers(resolved, source_name))
}

/// The layers one source contributed to a composed profile.
///
/// Kept separate from [`source_delivered_profile`] because merging throws away
/// the one fact the auto-apply policy needs: which TIER each layer arrived
/// under. Composition builds a layer per tier
/// (`<source>/locked`, `<source>/required`, `<source>/recommended`, an opt-in
/// optional profile, the subscriber's overrides, the source's standard
/// profiles) and tags each with a [`LayerPolicy`], so the layer that carried an
/// item is what says whether `newRecommended`, `newOptional` or
/// `lockedConflict` governs it.
pub fn source_delivered_layers(
    resolved: &ResolvedProfile,
    source_name: &str,
) -> Vec<config::ProfileLayer> {
    resolved
        .layers
        .iter()
        .filter(|l| l.source == source_name)
        .cloned()
        .collect()
}

/// What the subscriber's own layers declare, as one merged profile.
///
/// The local half of [`DecisionScope`], named so every caller reaches the same
/// layer set. A caller that resolves more than the layers carry — the CLI folds
/// Brewfile / `package.json` / apt-list entries into a profile's packages after
/// merging — resolves them into THIS profile before handing it over, or its own
/// declarations are invisible to the guard.
pub fn local_profile(resolved: &ResolvedProfile) -> MergedProfile {
    source_delivered_profile(resolved, LOCAL_LAYER)
}

/// One spelling for a decision path, whichever side of the guard minted it.
///
/// A `files.` path carries whatever spelling its profile declared, and the two
/// sides of the guard are written by different people: a source may deliver
/// `files./home/u/.zshrc` while the subscriber declares `files.~/.zshrc`. The
/// prune already expands `~` to meet the planner's ids, so the guard expands
/// too — comparing raw strings would admit a decision the operator's own
/// declaration should have refused, and the exclusion would then match (and
/// remove) that very declaration's action. No other prefix carries a path.
fn normalized_decision_path(path: &str) -> String {
    match path.strip_prefix("files.") {
        Some(target) if !target.is_empty() => {
            format!(
                "files.{}",
                to_posix_string(crate::expand_tilde(Path::new(target)))
            )
        }
        _ => path.to_string(),
    }
}

/// Which sources a run knows the subscriber still has.
///
/// The one answer to "can this row still mean anything", shared by everything
/// that reads a decision: the planning gate ([`DecisionScope`]) requires it
/// before withholding, and `cfgd status` / `cfgd decide` list rows on it alone.
/// A row whose source is gone is unanswerable — `cfgd decide` would act against
/// a source that no longer exists — so no surface should show it and no plan
/// should obey it.
#[derive(Debug)]
pub enum Subscriptions {
    /// The config parsed, so the list is authoritative and a row naming
    /// anything outside it is inert.
    Known(HashSet<String>),
    /// The config did not parse (`cfgd apply --module x` against a broken
    /// `cfgd.yaml` falls back to a module-only run). An empty subscription list
    /// would then be a fabrication that releases every undecided item, so every
    /// row stands until a run can read the real list.
    Unverified,
}

impl Subscriptions {
    pub fn known<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Known(names.into_iter().map(Into::into).collect())
    }

    /// Whether a row raised by `source` can still be answered.
    pub fn answers(&self, source: &str) -> bool {
        match self {
            Self::Known(names) => names.contains(source),
            Self::Unverified => true,
        }
    }

    /// Keep only the rows a read surface should list.
    pub fn answerable(&self, rows: Vec<PendingDecision>) -> Vec<PendingDecision> {
        rows.into_iter()
            .filter(|d| self.answers(&d.source))
            .collect()
    }
}

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
#[derive(Debug)]
pub struct DecisionScope {
    subscribed: Subscriptions,
    local: HashSet<String>,
}

impl DecisionScope {
    /// Build the scope from the currently subscribed source names and the
    /// [`local_profile`] saying what the operator declares.
    pub fn new<I, S>(subscribed: I, local: &MergedProfile) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            subscribed: Subscriptions::known(subscribed),
            local: Self::local_paths(local),
        }
    }

    /// The scope for a run that could not read its config, and so cannot say
    /// which sources are still subscribed. Fail-closed: every row withholds.
    pub fn unverified(local: &MergedProfile) -> Self {
        Self {
            subscribed: Subscriptions::Unverified,
            local: Self::local_paths(local),
        }
    }

    fn local_paths(local: &MergedProfile) -> HashSet<String> {
        declared_decision_paths(local)
            .iter()
            .map(|p| normalized_decision_path(p))
            .collect()
    }

    /// Whether a decision raised by `source` over `resource` still withholds it.
    pub fn withholds(&self, source: &str, resource: &str) -> bool {
        self.subscribed.answers(source) && !self.local.contains(&normalized_decision_path(resource))
    }
}

/// The decisions withholding something from this run, split by the state that
/// withholds them.
///
/// The two row-backed halves prune AND are rendered: a resource missing from a
/// plan is explained by a row the operator can see, whichever state that row is
/// in. The third prunes silently — an auto-apply policy of `Reject`/`Ignore` is
/// a standing instruction, already written in the config the operator is
/// reading, so `docs/sources.md` gives it no row and no line.
#[derive(Debug, Default)]
pub struct WithheldDecisions {
    /// Rows awaiting `cfgd decide`.
    pub pending: Vec<PendingDecision>,
    /// Rows the operator already declined.
    pub rejected: Vec<PendingDecision>,
    /// Paths an auto-apply policy declined outright, which record no row.
    pub declined: HashSet<String>,
    /// Source batches no row can name (dotted custom manager) — withheld
    /// fail-closed and warned about on the run header instead of listed.
    pub undecidable: Vec<UndecidableBatch>,
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

    /// Fold in the paths an auto-apply policy declined outright.
    ///
    /// Kept on the same value the rows are read into so every consumer prunes
    /// from one list: the daemon's tick and `cfgd plan` / `cfgd apply` all
    /// withhold a `Reject`-tier item, and a manual apply cannot launder onto
    /// the machine what the daemon declines.
    pub fn with_policy_declined(mut self, declined: HashSet<String>) -> Self {
        // A declined item is silent on every rendered surface by contract —
        // `docs/sources.md` gives `Reject`/`Ignore` no row and no line, because
        // the instruction is already in the config being read. That leaves no
        // way to answer "why is this item nowhere", so the paths are named at
        // debug level: visible to an operator who goes looking, invisible to
        // the one who did not ask.
        if !declined.is_empty() && tracing::enabled!(tracing::Level::DEBUG) {
            let mut paths: Vec<&str> = declined.iter().map(String::as_str).collect();
            paths.sort_unstable();
            tracing::debug!(
                resources = %paths.join(", "),
                "auto-apply policy declines these source items; they are withheld from the plan and record no decision"
            );
        }
        self.declined = declined;
        self
    }

    /// Fold in the items a `Notify` policy has classified but no store has
    /// recorded yet.
    ///
    /// The window this closes is the one between a source delivering an item
    /// and a row existing for it. `Notify` is the DEFAULT tier disposition, so
    /// without this a `cfgd plan` or `cfgd apply` reaching the item first would
    /// plan and install it, it would become a managed resource, and the daemon
    /// would never ask — the standing contract is ask-before-install, on every
    /// path. An unrecorded mint therefore withholds exactly as a recorded
    /// pending row does, and is rendered beside them.
    ///
    /// A mint whose row this run already read is skipped, so a path that MINTS
    /// before reading (`cfgd decide` answering an item, through
    /// [`mint_decisions`]) does not list the same item twice. The scope gate
    /// applies here too: an item the
    /// operator also declares themselves is theirs, not the source's, and no
    /// classification of the source's copy may withhold it.
    /// Fold in the batches no decision can name — see [`UndecidableBatch`].
    /// They ride the same value the rows do so every consumer prunes (and
    /// warns) from one list, exactly like the policy-declined paths.
    pub fn with_undecidable(mut self, batches: Vec<UndecidableBatch>) -> Self {
        self.undecidable = batches;
        self
    }

    pub fn with_unrecorded(mut self, mints: &[DecisionMint], scope: &DecisionScope) -> Self {
        for mint in mints {
            if !scope.withholds(&mint.source, &mint.resource) {
                continue;
            }
            let already_read = self
                .pending
                .iter()
                .chain(self.rejected.iter())
                .any(|d| d.source == mint.source && d.resource == mint.resource);
            if already_read {
                continue;
            }
            self.pending.push(mint.as_row());
        }
        self
    }

    /// Release the rows this run's classification auto-accepted.
    ///
    /// The rows were read before the classification resolved them (or, on a
    /// read-only path, were never resolved at all), so they still sit in
    /// `pending` and would withhold an item the machine already runs. Pruning
    /// here is what lets `cfgd plan` — which writes nothing — preview the item
    /// as included, and keeps the writing paths' plans identical to that
    /// preview. Only `pending` is touched: a rejection is the operator's
    /// standing answer and no installed state overrides it.
    pub fn with_auto_accepted(mut self, accepted: &[AutoAccepted]) -> Self {
        self.pending.retain(|row| {
            !accepted
                .iter()
                .any(|a| a.source == row.source && a.resource == row.resource)
        });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
            && self.rejected.is_empty()
            && self.declined.is_empty()
            && self.undecidable.is_empty()
    }

    /// The resource path of everything this run withholds, in decision
    /// vocabulary — the rows and the policy-declined paths that have none.
    pub fn resource_paths(&self) -> impl Iterator<Item = String> + '_ {
        self.pending
            .iter()
            .chain(self.rejected.iter())
            .map(|d| d.resource.clone())
            .chain(self.declined.iter().cloned())
    }
}

/// The auto-apply setting a run honours, straight from the config.
///
/// The daemon may override it from its own flags; every other path reads it
/// here so a policy that declines an item in the daemon declines it in
/// `cfgd plan` and `cfgd apply` too.
pub fn configured_auto_apply(cfg: &CfgdConfig) -> bool {
    cfg.spec
        .daemon
        .as_ref()
        .and_then(|d| d.reconcile.as_ref())
        .map(|r| r.auto_apply)
        .unwrap_or(false)
}

/// A row an auto-apply policy wants minted for review.
#[derive(Debug, Clone)]
pub struct DecisionMint {
    pub source: String,
    pub resource: String,
    pub tier: String,
    /// Why the item is still being asked about even though something is
    /// installed — the version-conflict annotation
    /// (`installed 13.0, source wants ^14`). Data on the row: it rides the
    /// summary, so `status` / `decide` list it and the `-o json` payloads
    /// carry it, recorded or not.
    pub annotation: Option<String>,
}

impl DecisionMint {
    /// The `pending_decisions.summary` text the row carries.
    pub fn summary(&self) -> String {
        match &self.annotation {
            Some(annotation) => format!(
                "{} {} (from {}) — {}",
                self.tier, self.resource, self.source, annotation
            ),
            None => format!("{} {} (from {})", self.tier, self.resource, self.source),
        }
    }

    /// The row this mint stands for, before anything records it.
    ///
    /// A run that classifies an item but does not own the store — `cfgd plan`
    /// is read-only — still has to WITHHOLD it and name it on the surface the
    /// operator reads, or a `Notify`-tier item would be installed by whichever
    /// path reached it before the row existed. The row is real in every field
    /// the operator sees; `id` is `0` because no store has assigned one yet,
    /// which is also how a reader of the `-o json` payload tells an
    /// unanswered-and-unrecorded item from a recorded one.
    pub fn as_row(&self) -> PendingDecision {
        PendingDecision {
            id: 0,
            source: self.source.clone(),
            resource: self.resource.clone(),
            tier: self.tier.to_string(),
            action: DECISION_ACTION_INSTALL.to_string(),
            summary: self.summary(),
            created_at: crate::utc_now_iso8601(),
            resolved_at: None,
            resolution: None,
        }
    }
}

/// The `pending_decisions.action` value every minted row carries: the decision
/// is whether to put the item ON the machine.
pub const DECISION_ACTION_INSTALL: &str = "install";

/// What this run's own package planning observed, threaded into the
/// source-decision classification so a source-delivered package the machine
/// already runs can be accepted without asking.
///
/// Built by the SAME enumeration the planner diffs desired state against —
/// `docs/sources.md` promises one derivation of "installed", so the
/// classification never shells out a second time or invents a second
/// package-listing vocabulary. A manager absent from this value was never
/// enumerated (unavailable, probe failed, or the run planned no packages), and
/// the classification fails closed: nothing auto-accepts on a guess.
#[derive(Debug, Default)]
pub struct ActualPackages {
    managers: HashMap<String, ObservedManager>,
}

#[derive(Debug, Default)]
struct ObservedManager {
    /// Installed identity → version, when the enumeration carries one.
    installed: HashMap<String, Option<String>>,
    /// Raw desired entry → the identity the planner diffed it under.
    identities: HashMap<String, String>,
}

impl ActualPackages {
    /// Record what `manager`'s enumeration listed as installed. Recording an
    /// EMPTY enumeration still marks the manager as observed — "nothing
    /// installed" is an answer, "never asked" is not.
    pub fn record_enumeration<I>(&mut self, manager: &str, installed: I)
    where
        I: IntoIterator<Item = (String, Option<String>)>,
    {
        self.managers
            .entry(manager.to_string())
            .or_default()
            .installed
            .extend(installed);
    }

    /// Record the identity the planner diffed a desired `entry` under, so the
    /// classification agrees with the planner about presence by construction —
    /// a manager that maps `example.com/tool@v1.2.3` to `tool` answers for the
    /// entry, not just the raw string.
    pub fn record_identity(&mut self, manager: &str, entry: &str, identity: &str) {
        self.managers
            .entry(manager.to_string())
            .or_default()
            .identities
            .insert(entry.to_string(), identity.to_string());
    }
}

impl ObservedManager {
    fn verdict_for(&self, entry: &str) -> InstallVerdict {
        let identity = self
            .identities
            .get(entry)
            .map(String::as_str)
            .unwrap_or(entry);
        if let Some(version) = self.installed.get(identity) {
            // Present exactly as the planner diffs it: the plan holds no work
            // for this entry, so accepting it blesses converged state and
            // installs nothing.
            if self.installed.contains_key(entry) {
                // Listed verbatim — any `@` is part of the NAME here (brew's
                // `python@3.12` is a formula, not a version pin).
                return InstallVerdict::AutoAccept {
                    reason: installed_reason(version),
                };
            }
            if let Some(spec) = embedded_version_spec(entry) {
                return match version {
                    Some(v) if crate::version_satisfies(v, &spec) => InstallVerdict::AutoAccept {
                        reason: format!(
                            "installed {} satisfies {}",
                            crate::escape_control_chars(v),
                            spec
                        ),
                    },
                    Some(v) => InstallVerdict::Conflict {
                        annotation: format!(
                            "installed {}, source wants {}",
                            crate::escape_control_chars(v),
                            spec
                        ),
                    },
                    // Satisfaction cannot be judged without a version, so the
                    // question stands — never auto-accept on a guess.
                    None => InstallVerdict::Conflict {
                        annotation: format!("installed (version unknown), source wants {}", spec),
                    },
                };
            }
            return InstallVerdict::AutoAccept {
                reason: installed_reason(version),
            };
        }
        // Absent by planner identity — but when the entry pins a version, the
        // BARE name is what the machine actually lists (`tool@^14` installs
        // and lists as `tool`), so the pin is judged against it. A satisfying
        // installed version answers the source's ask and auto-accepts; the
        // plan may still converge the pin, which is what accepting means. A
        // mismatch or an unknowable version stays pending, with the reason as
        // row data. The bare name resolves through the planner's identity
        // mapping so a case-insensitive manager's listing (`tool`) answers
        // for the source's spelling (`Tool@^14`).
        if let Some(spec) = embedded_version_spec(entry)
            && let Some((name, _)) = entry.rsplit_once('@')
        {
            let name = self
                .identities
                .get(name)
                .map(String::as_str)
                .unwrap_or(name);
            match self.installed.get(name) {
                Some(Some(v)) if crate::version_satisfies(v, &spec) => {
                    return InstallVerdict::AutoAccept {
                        reason: format!(
                            "installed {} satisfies {}",
                            crate::escape_control_chars(v),
                            spec
                        ),
                    };
                }
                Some(Some(v)) => {
                    return InstallVerdict::Conflict {
                        annotation: format!(
                            "installed {}, source wants {}",
                            crate::escape_control_chars(v),
                            spec
                        ),
                    };
                }
                Some(None) => {
                    return InstallVerdict::Conflict {
                        annotation: format!("installed (version unknown), source wants {}", spec),
                    };
                }
                None => {}
            }
        }
        InstallVerdict::Undetermined
    }
}

fn installed_reason(version: &Option<String>) -> String {
    match version {
        // Manager-reported strings reach terminal-rendered row summaries, so
        // control characters render escaped rather than repainting the line.
        Some(v) => format!("already installed ({})", crate::escape_control_chars(v)),
        None => "already installed".to_string(),
    }
}

/// The version requirement a package entry embeds, when it embeds one.
///
/// The grammar is deliberately narrow, because `@` is also a legal NAME
/// character (brew's `python@3.12`, npm's `@scope/name`): the trailing segment
/// counts as a spec only when it announces itself with a range operator
/// (`^14`, `>=2.1`, `~1.4`, `*`) or a `v`-prefixed version (`v1.2.3`), and
/// parses as a semver requirement after the `v` is stripped. Anything else is
/// part of the package's name and carries no satisfaction semantics.
fn embedded_version_spec(entry: &str) -> Option<String> {
    let (name, raw) = entry.rsplit_once('@')?;
    if name.is_empty() || raw.is_empty() {
        return None;
    }
    let looks_like_spec = raw.starts_with(['^', '~', '>', '<', '=', '*'])
        || (raw.starts_with(['v', 'V']) && raw[1..].starts_with(|c: char| c.is_ascii_digit()));
    if !looks_like_spec {
        return None;
    }
    let normalized = raw.strip_prefix(['v', 'V']).unwrap_or(raw);
    semver::VersionReq::parse(normalized).ok()?;
    Some(normalized.to_string())
}

/// What the installed state says about one delivered resource.
enum InstallVerdict {
    /// The machine already satisfies the item — accept without asking.
    AutoAccept { reason: String },
    /// Something IS installed but does not answer the source's ask — stays
    /// pending, with the reason as data on the row.
    Conflict { annotation: String },
    /// Not a package, not installed, or never enumerated — the question
    /// stands unchanged.
    Undetermined,
}

/// Judge one delivered resource against the run's observed package state.
///
/// Packages only, by contract: a `files.` / `env.` / `system.` path always
/// comes back [`InstallVerdict::Undetermined`] — an existing file matching
/// source content is NOT consent to manage it.
fn manual_install_verdict(resource: &str, actual: &ActualPackages) -> InstallVerdict {
    let Some(rest) = resource.strip_prefix("packages.") else {
        return InstallVerdict::Undetermined;
    };
    let Some((manager, entry)) = rest.split_once('.') else {
        return InstallVerdict::Undetermined;
    };
    // The decision vocabulary folds casks into `brew`, so both planner
    // batches answer for a `packages.brew.<pkg>` path.
    let candidates: &[&str] = if manager == "brew" {
        &["brew", "brew-cask"]
    } else {
        &[manager]
    };
    let mut best = InstallVerdict::Undetermined;
    for name in candidates {
        let Some(observed) = actual.managers.get(*name) else {
            continue;
        };
        match observed.verdict_for(entry) {
            v @ InstallVerdict::AutoAccept { .. } => return v,
            v @ InstallVerdict::Conflict { .. } => best = v,
            InstallVerdict::Undetermined => {}
        }
    }
    best
}

/// An item the classification accepted because the machine already satisfies
/// it — the auto-accept half of a [`SourcePolicyReview`].
///
/// Read-only paths use it to release the withhold; writing paths record it
/// through [`mint_decisions`] as an already-resolved row whose resolution says
/// it was accepted by installed state, not by the operator's hand.
#[derive(Debug, Clone)]
pub struct AutoAccepted {
    pub source: String,
    pub resource: String,
    pub tier: String,
    /// Why no question was owed — the provenance the resolved row carries.
    pub reason: String,
}

impl AutoAccepted {
    /// The `pending_decisions.summary` text the resolved row carries.
    pub fn summary(&self) -> String {
        format!(
            "{} {} (from {}) — auto-accepted: {}",
            self.tier, self.resource, self.source, self.reason
        )
    }
}

/// What the auto-apply policy makes of everything the subscribed sources
/// currently deliver.
///
/// One classification, consumed WHOLE by every path that plans: `declined`
/// prunes silently, `to_mint` withholds the item and names it as pending. What
/// differs between the paths is only who may WRITE it — `cfgd apply` and the
/// daemon own their store and record the rows through [`mint_decisions`],
/// `cfgd plan` is read-only and rides [`WithheldDecisions::with_unrecorded`]
/// instead. Computing the disposition from the same inputs on every path is
/// what keeps a manual apply from installing an item the daemon would have
/// asked about.
#[derive(Debug, Default)]
pub struct SourcePolicyReview {
    /// Resource paths the policy declines outright — `Reject` and `Ignore`.
    pub declined: HashSet<String>,
    /// Rows to record for the operator to answer.
    pub to_mint: Vec<DecisionMint>,
    /// Items the machine already satisfies — released from withholding, and
    /// recorded as resolved rows by whichever writing path runs first.
    pub auto_accepted: Vec<AutoAccepted>,
    /// Pending rows whose installed-state annotation moved — re-recorded so
    /// the row the operator reads carries the current conflict, without
    /// counting as a fresh notification.
    pub annotation_refresh: Vec<DecisionMint>,
    /// `(source, hash)` for every source whose delivered set changed.
    pub changed_hashes: Vec<(String, String)>,
    /// Batches no decision can name (dotted custom manager) — withheld
    /// fail-closed, warned about, never minted and never hashed.
    pub undecidable: Vec<UndecidableBatch>,
}

impl SourcePolicyReview {
    /// The subset of this review an ANSWERING run may record: rows for exactly
    /// the items it is answering, and NO source hashes.
    ///
    /// A hash stamp marks the source's whole delivered set as seen, so the
    /// daemon's next tick would find the source "unchanged" and never send the
    /// notification still owed for the items this run did not touch. Leaving
    /// the hash unstamped is safe on the other side too:
    /// [`review_source_policy`] re-asks an answered item only on a real hash
    /// change, never on the first stamped observation.
    pub fn narrowed_to(&self, targets: &DecisionTargets<'_>) -> SourcePolicyReview {
        SourcePolicyReview {
            declined: HashSet::new(),
            to_mint: self
                .to_mint
                .iter()
                .filter(|m| targets.covers(m))
                .cloned()
                .collect(),
            // An answer records only what it answers; an auto-accepted item
            // and a refreshed annotation are the classification's writes, not
            // the operator's, and stay with the full-review paths.
            auto_accepted: Vec::new(),
            annotation_refresh: Vec::new(),
            changed_hashes: Vec::new(),
            // Nothing recordable: no row can name these batches, so an
            // answering run has nothing to narrow to.
            undecidable: Vec::new(),
        }
    }
}

/// Which classified items an answering run is recording — `cfgd decide`'s
/// narrowing over a [`SourcePolicyReview`]. The daemon and `cfgd apply` record
/// the whole review; an answer records only what it answers, through
/// [`SourcePolicyReview::narrowed_to`].
#[derive(Debug, Clone, Copy)]
pub enum DecisionTargets<'a> {
    /// `decide accept --all`: every item the listing surfaces offer.
    All,
    /// `decide accept --source <name>`: the named source's items.
    Source(&'a str),
    /// `decide accept <resource>`: the one item the plan's instruction names.
    Resource(&'a str),
}

impl DecisionTargets<'_> {
    fn covers(&self, mint: &DecisionMint) -> bool {
        match self {
            Self::All => true,
            Self::Source(name) => mint.source == *name,
            Self::Resource(path) => mint.resource == *path,
        }
    }
}

/// Classify what every subscribed source delivers against the auto-apply policy.
///
/// Writes nothing. `auto_apply` is the effective setting — off, the policy has
/// no say at all (`docs/sources.md`: with `autoApply: false` no row is ever
/// created and source items simply apply), so nothing is declined and nothing
/// is minted.
pub fn review_source_policies(
    store: &StateStore,
    cfg: &CfgdConfig,
    resolved: &ResolvedProfile,
    auto_apply: bool,
    actual: &ActualPackages,
) -> Result<SourcePolicyReview> {
    let mut review = SourcePolicyReview::default();
    if !auto_apply || cfg.spec.sources.is_empty() {
        return Ok(review);
    }
    let default_policy = AutoApplyPolicyConfig::default();
    let policy = cfg
        .spec
        .daemon
        .as_ref()
        .and_then(|d| d.reconcile.as_ref())
        .and_then(|r| r.policy.as_ref())
        .unwrap_or(&default_policy);

    for source in &cfg.spec.sources {
        let one = review_source_policy(
            store,
            &source.name,
            &DeliveredItems::for_source(resolved, &source.name),
            policy,
            actual,
        )?;
        review.declined.extend(one.declined);
        review.to_mint.extend(one.to_mint);
        review.auto_accepted.extend(one.auto_accepted);
        review.annotation_refresh.extend(one.annotation_refresh);
        review.changed_hashes.extend(one.changed_hashes);
    }
    // Computed with the review rather than beside it so every surface that
    // classifies (plan, apply, the daemon's tick) inherits the batches — and
    // their fail-closed withhold — under exactly the conditions the
    // classification itself runs.
    review.undecidable =
        undecidable_source_batches(resolved, cfg.spec.sources.iter().map(|s| s.name.as_str()));
    Ok(review)
}

/// Classify what ONE source delivers against the auto-apply policy.
///
/// The per-source half of [`review_source_policies`], for a caller holding a
/// source's delivered items directly rather than a whole config.
pub fn review_source_policy(
    store: &StateStore,
    source_name: &str,
    delivered: &DeliveredItems,
    policy: &AutoApplyPolicyConfig,
    actual: &ActualPackages,
) -> Result<SourcePolicyReview> {
    let mut review = SourcePolicyReview::default();
    let current_hash = delivered.resource_hash();

    let previous_hash = store
        .source_config_hash(source_name)?
        .map(|h| h.config_hash);
    let config_changed = previous_hash.as_deref() != Some(&current_hash);
    // A change is a PREVIOUS observation disagreeing; the first observation is
    // not one. Rows can exist before any hash does — `cfgd decide` records the
    // item it answers and stamps nothing — and treating None → Some as "the
    // source moved" would re-mint, and so re-ask, the very item just answered.
    let source_changed = previous_hash.is_some() && config_changed;

    // The old resource set is not stored, only its hash — the items still
    // being asked about stand in for it. Those rows were minted in this very
    // vocabulary, so the comparison below needs no translation. The
    // managed-resource table is deliberately NOT consulted: its rows live in
    // the state vocabulary (`package` + `<mgr>/<pkg>`), which never matches a
    // decision path — and an installed item that has no row is an item nobody
    // was ever asked about, exactly the one the `Notify` arm below still owes
    // a question.
    let rows = store.pending_decisions_for_source(source_name)?;
    let mut known: HashSet<String> = HashSet::new();
    if previous_hash.is_some() {
        for d in &rows {
            known.insert(d.resource.clone());
        }
    }

    // The rows already asked about answer to installed state too — this is
    // the manual-install path `docs/sources.md` promises: the operator
    // installs a pending package by hand, and the next classification finds
    // it present and accepts the row instead of leaving the question open.
    // Judged under the item's CURRENT policy action, because a row whose tier
    // the policy now declines is withheld by `declined` recomputation the
    // moment its row resolves — releasing it here would launder a
    // `Reject`-tier item onto the machine through its own leftover row.
    for row in &rows {
        let Some(tier) = delivered.tier_for(&row.resource) else {
            // The source no longer delivers it; a stale row is not consent.
            continue;
        };
        let action = policy_action_for(policy, tier);
        if matches!(action, PolicyAction::Reject | PolicyAction::Ignore) {
            continue;
        }
        match manual_install_verdict(&row.resource, actual) {
            InstallVerdict::AutoAccept { reason } => review.auto_accepted.push(AutoAccepted {
                source: source_name.to_string(),
                resource: row.resource.clone(),
                tier: row.tier.clone(),
                reason,
            }),
            InstallVerdict::Conflict { annotation } => {
                let refreshed = DecisionMint {
                    source: source_name.to_string(),
                    resource: row.resource.clone(),
                    tier: row.tier.clone(),
                    annotation: Some(annotation),
                };
                // Re-recorded only when the summary actually moved, so a
                // steady conflict does not rewrite the row every tick.
                if refreshed.summary() != row.summary {
                    review.annotation_refresh.push(refreshed);
                }
            }
            InstallVerdict::Undetermined => {}
        }
    }

    for (resource, tier) in delivered.iter().filter(|(r, _)| !known.contains(*r)) {
        let action = policy_action_for(policy, tier);
        match action {
            // Included in the plan normally.
            PolicyAction::Accept => {}
            PolicyAction::Reject | PolicyAction::Ignore => {
                // The ITEM is skipped, not merely the decision about it: the
                // four policy actions are one series of dispositions, and "skip
                // silently" next to "don't apply" and "automatically apply" can
                // only mean the item does not reach the machine. Recomputed on
                // every run — declining records nothing, so there is no row to
                // carry the disposition to the next one.
                review.declined.insert(resource.clone());
            }
            // Minted when the source's delivered set changed OR when this item
            // has never been asked about. The second half is what makes a
            // policy flip work: an item a `Reject` policy declined has no row,
            // so flipping to `Notify` must ask about it even though the source
            // itself has not moved. Any row — including a rejection — still
            // suppresses re-minting until the source changes, so an answer is
            // not re-asked every tick.
            PolicyAction::Notify => {
                if source_changed || !store.has_decision(source_name, resource)? {
                    match manual_install_verdict(resource, actual) {
                        // Already on the machine as delivered: no question is
                        // owed, and the writing path records the resolved row
                        // directly instead of asking one tick and answering
                        // the next.
                        InstallVerdict::AutoAccept { reason } => {
                            review.auto_accepted.push(AutoAccepted {
                                source: source_name.to_string(),
                                resource: resource.clone(),
                                tier: tier.to_string(),
                                reason,
                            })
                        }
                        InstallVerdict::Conflict { annotation } => {
                            review.to_mint.push(DecisionMint {
                                source: source_name.to_string(),
                                resource: resource.clone(),
                                tier: tier.to_string(),
                                annotation: Some(annotation),
                            })
                        }
                        InstallVerdict::Undetermined => review.to_mint.push(DecisionMint {
                            source: source_name.to_string(),
                            resource: resource.clone(),
                            tier: tier.to_string(),
                            annotation: None,
                        }),
                    }
                }
            }
        }
    }

    if config_changed {
        review
            .changed_hashes
            .push((source_name.to_string(), current_hash));
    }
    Ok(review)
}

/// The policy action governing an item delivered at `tier` — one lookup,
/// shared by the never-asked classification and the existing-row walk so the
/// two cannot disagree about what a tier's disposition is.
fn policy_action_for<'a>(policy: &'a AutoApplyPolicyConfig, tier: &str) -> &'a PolicyAction {
    match tier {
        TIER_OPTIONAL => &policy.new_optional,
        TIER_LOCKED => &policy.locked_conflict,
        _ => &policy.new_recommended,
    }
}

/// Record the rows a [`SourcePolicyReview`] asked for, returning how many were
/// minted per source.
///
/// The WRITE half of the review, shared by the daemon's tick, by `cfgd apply`
/// (after its confirmation — a declined run declines its writes), and by
/// `cfgd decide` when it answers an item no run has recorded yet — decide
/// hands over [`SourcePolicyReview::narrowed_to`] its targets, an answer
/// recording only what it answers — so a row exists by whichever of them runs
/// first instead of the item waiting on the
/// next daemon tick. Idempotent by construction — `review_source_policy` only mints
/// what has never been asked about (or what a changed source re-asks), and the
/// upsert refreshes an unresolved row rather than duplicating it.
///
/// A row that will not record is logged and skipped rather than failing the
/// run: the caller withholds the item either way (an unrecorded mint still
/// rides [`WithheldDecisions::with_unrecorded`]), so a store that rejects the
/// write costs the operator the ability to answer, never the protection.
///
/// The hashes are stored AFTER the rows, so a failure between the two re-asks
/// on the next run instead of marking the source seen for items nobody
/// recorded.
pub fn mint_decisions(store: &StateStore, review: &SourcePolicyReview) -> Vec<(String, u32)> {
    let mut minted_per_source: Vec<(String, u32)> = Vec::new();
    for mint in &review.to_mint {
        if let Err(e) = store.upsert_pending_decision(
            &mint.source,
            &mint.resource,
            &mint.tier,
            DECISION_ACTION_INSTALL,
            &mint.summary(),
        ) {
            tracing::warn!(error = %e, "failed to record pending decision");
            continue;
        }
        match minted_per_source
            .iter_mut()
            .find(|(s, _)| *s == mint.source)
        {
            Some((_, count)) => *count += 1,
            None => minted_per_source.push((mint.source.clone(), 1)),
        }
    }

    // A refreshed annotation rewrites the row the operator already knows
    // about; it is not a new question, so it never counts toward the
    // notification totals above.
    for mint in &review.annotation_refresh {
        if let Err(e) = store.upsert_pending_decision(
            &mint.source,
            &mint.resource,
            &mint.tier,
            DECISION_ACTION_INSTALL,
            &mint.summary(),
        ) {
            tracing::warn!(error = %e, "failed to refresh pending decision annotation");
        }
    }

    // Auto-accepted items land as already-resolved rows: `status` can show
    // WHY the item was accepted (installed state, not the operator's hand),
    // and `withheld_decisions` releases the resource by construction because
    // the newest row's resolution is not a rejection.
    for accepted in &review.auto_accepted {
        if let Err(e) = store.record_auto_accepted_decision(
            &accepted.source,
            &accepted.resource,
            &accepted.tier,
            DECISION_ACTION_INSTALL,
            &accepted.summary(),
        ) {
            tracing::warn!(error = %e, "failed to record auto-accepted decision");
        }
    }

    for (source_name, hash) in &review.changed_hashes {
        if let Err(e) = store.set_source_config_hash(source_name, hash) {
            tracing::warn!(error = %e, "failed to store source config hash");
        }
    }

    minted_per_source
}

/// Whether a run may sweep the decision rows out of the store it opened.
///
/// One rule, read by `cfgd apply` and by the daemon's tick, and it is SEMANTIC:
/// ownership follows what the resolved config path IS, not how it was spelled
/// on the command line. The machine's own config — the file at the default
/// config location, however it was named (`--config
/// ~/.config/cfgd/cfgd.yaml` is the same file the bare default resolves, and
/// every installed service unit bakes exactly that `--config` into its
/// invocation) — owns the default store's rows. A FOREIGN config pointed at
/// the default store does not: its subscription list belongs to a different
/// machine picture, and the rows it would delete are another config's,
/// unrecoverably. Bringing its own state dir makes any config authoritative,
/// because then the store it sweeps is the one that config owns.
///
/// `scope` is the RUN'S OWN scope, and it bounds which default location
/// counts: the store a run opens is resolved from that same scope, so only
/// that scope's default config speaks for it. Accepting the other scope's
/// default would let `cfgd --config /etc/cfgd/cfgd.yaml apply` — a user-scope
/// run — sweep the per-user store with the system picture's subscription list.
///
/// Withholding is unaffected either way — a row whose source this run does not
/// subscribe to is inert through [`Subscriptions`], swept or not.
pub fn owns_decision_store(
    config_path: &Path,
    has_state_dir_override: bool,
    scope: crate::Scope,
) -> bool {
    has_state_dir_override || is_machines_own_config(config_path, scope)
}

/// Whether `config_path` names the machine's own config for a run at `scope`:
/// a discovery-named config file (`cfgd.yaml` / `cfgd.toml`) sitting in that
/// scope's default config directory. Falls back to canonical comparison so a
/// symlinked config root (macOS `/tmp`, a stow-managed `~/.config`) still
/// matches its own default.
fn is_machines_own_config(config_path: &Path, scope: crate::Scope) -> bool {
    let named = crate::expand_tilde(config_path);
    if !named
        .file_name()
        .is_some_and(|name| name == config::CONFIG_FILENAME || name == config::CONFIG_FILENAME_TOML)
    {
        return false;
    }
    let Some(parent) = named.parent() else {
        return false;
    };
    let default_dir = crate::default_config_dir_for(scope);
    parent == default_dir
        || matches!(
            (parent.canonicalize(), default_dir.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        )
}

/// Hash a resource set so a source's delivered items can be compared against
/// the last run's without storing them.
pub fn hash_resources(resources: &HashSet<String>) -> String {
    let mut sorted: Vec<&String> = resources.iter().collect();
    sorted.sort();
    let combined: String = sorted.iter().map(|r| format!("{}\n", r)).collect();
    crate::sha256_hex(combined.as_bytes())
}

/// The tier a source offered an item at, as the `pending_decisions.tier` column
/// spells it and as `docs/sources.md` names the policy key that governs it.
pub const TIER_LOCKED: &str = "locked";
pub const TIER_RECOMMENDED: &str = "recommended";
pub const TIER_OPTIONAL: &str = "optional";

/// The tier of the layer an item arrived on.
///
/// `Required` covers both `policy.locked` and `policy.required` — composition
/// gives them one [`LayerPolicy`] because they share the enforcement the
/// subscriber cannot override, and `lockedConflict` is the policy key
/// `docs/sources.md` gives that enforcement. `Local` cannot appear on a
/// source's layer; it maps to the recommended default rather than inventing a
/// fifth disposition.
fn tier_of(policy: &LayerPolicy) -> &'static str {
    match policy {
        LayerPolicy::Required => TIER_LOCKED,
        LayerPolicy::Optional => TIER_OPTIONAL,
        LayerPolicy::Recommended | LayerPolicy::Local => TIER_RECOMMENDED,
    }
}

/// How strongly a tier is enforced, for the case where one source delivers an
/// item on more than one layer: the strongest tier decides, because the item
/// IS locked if any layer locked it.
fn tier_rank(tier: &str) -> u8 {
    match tier {
        TIER_LOCKED => 2,
        TIER_RECOMMENDED => 1,
        _ => 0,
    }
}

/// Every resource one source delivers, each tagged with its policy tier.
///
/// The auto-apply policy has a key per tier (`newRecommended`, `newOptional`,
/// `lockedConflict`), so the classification cannot read a merged profile alone
/// — merging erases which layer, and therefore which tier, carried an item.
/// This is the input [`review_source_policy`] classifies, and it is built from
/// the source's own layers so `newOptional` governs exactly the items the
/// subscriber opted into and nothing else.
#[derive(Debug, Default)]
pub struct DeliveredItems {
    tiers: BTreeMap<String, &'static str>,
}

impl DeliveredItems {
    /// What `source_name` delivered into a composed profile.
    pub fn for_source(resolved: &ResolvedProfile, source_name: &str) -> Self {
        Self::from_layers(&source_delivered_layers(resolved, source_name))
    }

    /// Tag every resource on `layers` with the tier of the layer carrying it.
    pub fn from_layers(layers: &[config::ProfileLayer]) -> Self {
        let mut tiers: BTreeMap<String, &'static str> = BTreeMap::new();
        for layer in layers {
            let tier = tier_of(&layer.policy);
            for resource in
                declared_decision_paths(&config::merge_layers(std::slice::from_ref(layer)))
            {
                match tiers.get(&resource) {
                    Some(existing) if tier_rank(existing) >= tier_rank(tier) => {}
                    _ => {
                        tiers.insert(resource, tier);
                    }
                }
            }
        }
        Self { tiers }
    }

    /// `(resource, tier)` for everything the source delivers, in path order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &'static str)> + '_ {
        self.tiers.iter().map(|(r, t)| (r, *t))
    }

    /// The tier `resource` is delivered at, if this source still delivers it.
    pub fn tier_for(&self, resource: &str) -> Option<&'static str> {
        self.tiers.get(resource).copied()
    }

    /// The hash the change detector compares against the last run's.
    pub fn resource_hash(&self) -> String {
        hash_resources(&self.tiers.keys().cloned().collect())
    }

    pub fn is_empty(&self) -> bool {
        self.tiers.is_empty()
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
/// | `packages.<mgr>.<pkg>` | that one package inside a batch — a `PackageAction::Install`/`Uninstall` for `<mgr>` or a module's `InstallPackages` (matched on its resolved name). The batch keeps its other packages and is dropped only when it empties. `packages.brew.<pkg>` also matches the `brew-cask` manager: the decision vocabulary folds casks into `brew` and cannot tell a cask from a formula. Every other manager — a brew tap under `brew-tap`, a custom manager under its own name — mints under the exact name its planned batch carries, so the match here is verbatim. A `Bootstrap` or `Skip` names no package and is never withheld — a bootstrap installs the package MANAGER, which every still-decided package in the batch needs |
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
    /// One line per [`UndecidableBatch`] folded in — pushed onto the plan's
    /// warnings by [`withhold_from_plan`], so the surface naming the absence
    /// is the same one the prune runs through.
    warnings: Vec<String>,
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
        Self::from_withheld_with(withheld, crate::expand_tilde)
    }

    /// [`Self::from_withheld`] with the caller's own `~` expansion — the
    /// daemon passes its hooks' injectable one. Also the ONE place the
    /// undecidable batches enter the exclusion set: their manager names never
    /// fit a decision path, so they ride the [`WithheldDecisions`] value
    /// directly and fold into the package map (and the warning list) here.
    pub fn from_withheld_with<E>(withheld: &WithheldDecisions, expand: E) -> Self
    where
        E: Fn(&Path) -> PathBuf,
    {
        let mut out = Self::from_decision_paths(withheld.resource_paths(), expand);
        for batch in &withheld.undecidable {
            out.packages
                .entry(batch.manager.clone())
                .or_default()
                .extend(batch.packages.iter().cloned());
            out.warnings.push(batch.warning());
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
            && self.packages.is_empty()
            && self.env.is_empty()
            && self.system.is_empty()
            && self.warnings.is_empty()
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
    // An undecidable batch has no row to explain its absence, so the warning
    // lands on the plan itself: the run header renders `plan.warnings` and
    // the `-o json` payload carries them, which is exactly where the operator
    // reads why something they declared is not in the run.
    plan.warnings.extend(exclusions.warnings.iter().cloned());
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

#[cfg(test)]
mod verdict_tests {
    use super::*;

    fn observed(
        manager: &str,
        installed: &[(&str, Option<&str>)],
        identities: &[(&str, &str)],
    ) -> ActualPackages {
        let mut actual = ActualPackages::default();
        actual.record_enumeration(
            manager,
            installed
                .iter()
                .map(|(name, v)| (name.to_string(), v.map(str::to_string))),
        );
        for (entry, identity) in identities {
            actual.record_identity(manager, entry, identity);
        }
        actual
    }

    #[test]
    fn version_spec_grammar_admits_operators_and_v_prefixed_versions_only() {
        // `@` is a legal NAME character; the suffix is a spec only when it
        // announces itself. These edges are the grammar's whole point — a
        // widening would silently turn manager-native names into pins.
        assert_eq!(embedded_version_spec("tool@^14").as_deref(), Some("^14"));
        assert_eq!(
            embedded_version_spec("tool@>=2.1").as_deref(),
            Some(">=2.1")
        );
        assert_eq!(embedded_version_spec("tool@~1.4").as_deref(), Some("~1.4"));
        assert_eq!(embedded_version_spec("tool@*").as_deref(), Some("*"));
        assert_eq!(
            embedded_version_spec("tool@v1.2.3").as_deref(),
            Some("1.2.3"),
            "a v-prefixed version is a pin, normalized without the v"
        );
        assert_eq!(embedded_version_spec("tool@V2").as_deref(), Some("2"));
        assert_eq!(
            embedded_version_spec("python@3.12"),
            None,
            "bare digits after @ are a NAME (brew's python@3.12 formula)"
        );
        assert_eq!(
            embedded_version_spec("tool@1.2.3"),
            None,
            "same rule regardless of how version-shaped the suffix looks"
        );
        assert_eq!(
            embedded_version_spec("@scope/name"),
            None,
            "npm scope: nothing before the @, so no name to pin"
        );
        assert_eq!(
            embedded_version_spec("tool@"),
            None,
            "trailing @ pins nothing"
        );
        assert_eq!(
            embedded_version_spec("tool@v"),
            None,
            "v alone is not a version"
        );
        assert_eq!(
            embedded_version_spec("tool@vanilla"),
            None,
            "v must introduce a digit"
        );
        assert_eq!(embedded_version_spec("plain"), None);
    }

    #[test]
    fn a_verbatim_listed_at_name_auto_accepts_as_a_name() {
        // brew's `python@3.12` is a formula whose NAME contains `@`: the
        // enumeration lists the entry verbatim, so it auto-accepts as an
        // installed name — never judged as a `3.12` version pin.
        let actual = observed(
            "brew",
            &[("python@3.12", Some("3.12.7"))],
            &[("python@3.12", "python@3.12")],
        );
        match manual_install_verdict("packages.brew.python@3.12", &actual) {
            InstallVerdict::AutoAccept { reason } => {
                assert_eq!(reason, "already installed (3.12.7)")
            }
            InstallVerdict::Conflict { annotation } => {
                panic!("a formula name must not be read as a pin: {annotation}")
            }
            InstallVerdict::Undetermined => panic!("listed verbatim must auto-accept"),
        }
    }

    #[test]
    fn a_bare_v_pin_keeps_caret_semantics() {
        // `docs/sources.md`: `tool@v1.2.3` means `^1.2.3` (the semver crate's
        // default, matching cargo/npm convention), so installed 1.4.0
        // satisfies the pin.
        let actual = observed("cargo", &[("tool", Some("1.4.0"))], &[]);
        match manual_install_verdict("packages.cargo.tool@v1.2.3", &actual) {
            InstallVerdict::AutoAccept { reason } => {
                assert_eq!(reason, "installed 1.4.0 satisfies 1.2.3")
            }
            _ => panic!("caret semantics: 1.4.0 satisfies ^1.2.3"),
        }
    }

    #[test]
    fn a_bare_name_pin_lookup_folds_through_the_planner_identity() {
        // Case-insensitive managers list the folded identity (`tool`) while
        // the source may spell the pin `Tool@^14`; the bare name resolves
        // through the identity mapping the planner recorded for it.
        let actual = observed(
            "winget",
            &[("tool", Some("13.0"))],
            &[("Tool@^14", "tool@^14"), ("Tool", "tool")],
        );
        match manual_install_verdict("packages.winget.Tool@^14", &actual) {
            InstallVerdict::Conflict { annotation } => {
                assert_eq!(annotation, "installed 13.0, source wants ^14")
            }
            InstallVerdict::AutoAccept { reason } => {
                panic!("13.0 does not satisfy ^14: {reason}")
            }
            InstallVerdict::Undetermined => {
                panic!("the folded listing must answer for the source's spelling")
            }
        }
    }

    #[test]
    fn a_control_character_in_a_manager_version_renders_escaped() {
        // The version string comes from manager output and lands in a
        // terminal-rendered row summary; a raw escape sequence could repaint
        // the line the operator acts on.
        let actual = observed("cargo", &[("tool", Some("13.0\x1b[2K"))], &[]);
        match manual_install_verdict("packages.cargo.tool@^14", &actual) {
            InstallVerdict::Conflict { annotation } => {
                assert_eq!(annotation, "installed 13.0\\x1b[2K, source wants ^14");
            }
            _ => panic!("a mismatch stays a conflict"),
        }
    }
}

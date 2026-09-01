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
///
/// The key view over [`declared_decision_fingerprints`], never a second walk.
pub fn declared_decision_paths(merged: &MergedProfile) -> HashSet<String> {
    declared_decision_fingerprints(merged).into_keys().collect()
}

/// Every declared resource in decision vocabulary, each with a fingerprint of
/// the entry the path names.
///
/// The path alone says WHICH item a source delivers; the fingerprint says WHAT
/// it currently declares for it, which is the difference between `env.EDITOR`
/// meaning `nvim` and meaning `vim`. `review_source_policy` re-asks on the
/// fingerprint moving, so the two must be derived in ONE walk: a second
/// enumeration would eventually mint a path whose content nothing fingerprints
/// (never re-asked) or fingerprint a path nothing mints (asked about forever).
pub fn declared_decision_fingerprints(merged: &MergedProfile) -> BTreeMap<String, String> {
    let mut resources = BTreeMap::new();

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
            resources.insert(
                format!("packages.{}.{}", decision_manager, pkg),
                entry_fingerprint(&pkg),
            );
        }
    }

    for file in &merged.files.managed {
        resources.insert(
            format!("files.{}", to_posix_string(&file.target)),
            entry_fingerprint(file),
        );
    }

    for ev in &merged.env {
        resources.insert(format!("env.{}", ev.name), entry_fingerprint(ev));
    }

    for (key, value) in &merged.system {
        resources.insert(format!("system.{}", key), entry_fingerprint(value));
    }

    resources
}

/// The fingerprint of one declared entry: a digest of its JSON serialization.
///
/// JSON rather than the source YAML because the entry is compared against what
/// a PAST run recorded for the same path, and only the typed form is stable
/// across the formatting, key order and comments an upstream commit may move
/// without changing what it declares. An entry that will not serialize
/// fingerprints as empty, which reads as "unchanged" — the safe direction: it
/// costs a re-ask nobody needed rather than silently applying a changed item.
fn entry_fingerprint<T: serde::Serialize>(entry: &T) -> String {
    let canonical = serde_json::to_string(entry).unwrap_or_default();
    crate::sha256_hex(canonical.as_bytes())
}

/// What one decision path actually asks the operator to accept.
///
/// A decision row's stored `summary` restates the row's own coordinates
/// (`recommended env.EDITOR (from team)`), which the surface rendering it has
/// already said twice over: the owner heading names the source, and the row's
/// own subject names the tier and the resource. What it never says is the one
/// thing the operator is being asked about — the CONTENT the source wants to
/// put on the machine. This recovers that from the profile the source
/// delivered, in the same vocabulary [`declared_decision_paths`] mints the
/// path with, so a path and its content can never describe different entries.
///
/// `None` means the content is unrecoverable — the resource is no longer
/// declared, the grammar does not parse, or a file's own bytes cannot be read
/// — and the caller falls back to the persisted summary rather than inventing
/// a shape. The file arm deliberately reports SIZE and mode, never the body:
/// a decision list is a scannable index, not a diff.
pub fn decision_resource_content(
    merged: &MergedProfile,
    resource: &str,
    config_dir: &Path,
) -> Option<String> {
    let (kind, rest) = resource.split_once('.')?;
    match kind {
        "env" => merged
            .env
            .iter()
            .find(|ev| ev.name == rest)
            .map(|ev| format!("{}={}", ev.name, ev.value)),
        "packages" => {
            let (decision_manager, package) = rest.split_once('.')?;
            merged.packages.manager_names().iter().find_map(|manager| {
                if decision_manager_name(manager) != Some(decision_manager) {
                    return None;
                }
                config::desired_packages_for_spec(manager, &merged.packages)
                    .iter()
                    .any(|p| p == package)
                    .then(|| format!("{manager} install {package}"))
            })
        }
        "files" => merged
            .files
            .managed
            .iter()
            .find(|f| to_posix_string(&f.target) == rest)
            .and_then(|f| managed_file_content(f, &merged.files, config_dir)),
        "system" => merged.system.get(rest).map(yaml_one_line),
        _ => None,
    }
}

/// The size-and-mode line one managed file entry stands for.
///
/// Both halves are best-effort and the row degrades rather than lying: an
/// unreadable source yields `None` (the caller falls back to the summary), and
/// a file with no declared mode simply says nothing about one.
fn managed_file_content(
    file: &config::ManagedFileSpec,
    files: &config::FilesSpec,
    config_dir: &Path,
) -> Option<String> {
    // The SAME resolution the plan action takes, so the row and the write it
    // describes can never name two different files.
    let path = crate::resolve_managed_file_source(&file.source, config_dir)?;
    let lines = count_lines(&path)?;
    let mode = file.permissions.as_deref().or_else(|| {
        files
            .permissions
            .get(&to_posix_string(&file.target))
            .map(String::as_str)
    });
    let counted = crate::pluralize(lines, "line");
    Some(match mode {
        Some(mode) => format!("{counted}, mode {mode}"),
        None => counted,
    })
}

/// Lines in `path`, counted over a STREAM.
///
/// A decision list can carry many `files.*` rows and each one is a whole file
/// the row states the size of; reading each into memory to count `\n` costs the
/// full byte length of every one of them for a number that needs none of it.
/// A file with no trailing newline still ends a line, which is what the final
/// `+ 1` accounts for; an empty file is zero lines rather than one.
fn count_lines(path: &Path) -> Option<usize> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut lines = 0usize;
    let mut ended_with_newline = true;
    loop {
        let buf = reader.fill_buf().ok()?;
        if buf.is_empty() {
            break;
        }
        lines += buf.iter().filter(|b| **b == b'\n').count();
        ended_with_newline = buf.last() == Some(&b'\n');
        let consumed = buf.len();
        reader.consume(consumed);
    }
    Some(lines + usize::from(!ended_with_newline))
}

/// A system setting rendered as ONE line, whatever its YAML shape.
///
/// A plain scalar renders as the operator wrote it; anything structured falls
/// back to compact JSON, which is the only always-one-line rendering of an
/// arbitrary YAML value — a decision row has one line to spend and a block
/// scalar would break the list it sits in.
fn yaml_one_line(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => "null".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "<unrenderable>".to_string()),
    }
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
    /// What the rows above would actually put on the machine, when the caller
    /// could resolve it. Display-only, and carried here so the run header's
    /// withheld rows read the SAME derivation `cfgd decide` and `cfgd status`
    /// render from rather than a fourth wording of the same fact.
    pub contents: DecisionContents,
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

    /// Attach what the withheld rows would put on the machine.
    ///
    /// Resolved by the caller, which already holds the composed profile the
    /// content is recovered from; a run that cannot resolve one simply does not
    /// call this and every row falls back to its persisted summary.
    pub fn with_contents(mut self, contents: DecisionContents) -> Self {
        self.contents = contents;
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
    /// The fingerprint of what the source declares for this item, recorded on
    /// the row so the next classification can tell "already answered" from
    /// "answered when it said something else". `None` only where the mint is
    /// built without the delivered item in hand.
    pub content_hash: Option<String>,
}

/// The glue between a decision summary's coordinates and its annotation.
///
/// Written once because it is JOINED here and SPLIT by
/// [`decision_row_annotation`]: a summary is the only place the annotation is
/// persisted, so the two spellings have to be the same one.
const SUMMARY_ANNOTATION_GLUE: &str = " — ";

/// The version-conflict annotation a stored row carries, if any.
///
/// A surface that renders a decision's CONTENT instead of its summary
/// (`decide`, `status`) still has to say WHY an installed package is being
/// asked about, and the summary is where that fact lives once the mint is
/// gone.
pub fn decision_row_annotation(summary: &str) -> Option<&str> {
    summary.split_once(SUMMARY_ANNOTATION_GLUE).map(|(_, a)| a)
}

/// What each pending decision is actually asking the operator to accept.
///
/// A row's persisted `summary` restates its own coordinates, which every render
/// has already said: the owner heading names the source and the subject names
/// the tier and the resource. The CONTENT is the one thing neither says, and it
/// lives in the profile the source delivered — so the LOOKUP is built ONCE by
/// the caller, from the desired state it already resolved, and handed to every
/// surface that lists a decision. Deriving it inside a renderer would mean a
/// second config parse per surface, and three surfaces each deriving their own
/// is how one screen comes to describe an item differently from the next.
///
/// A resource whose content cannot be recovered — no longer declared, an
/// unreadable file — keeps the persisted summary rather than rendering an
/// empty detail.
#[derive(Debug, Default)]
pub struct DecisionContents {
    contents: HashMap<(String, String), String>,
    /// The owner token that would WIN each item whose accepting source is not
    /// the one the merge ends up honouring. See [`outranking_owner`].
    outranked: HashMap<(String, String), String>,
}

impl DecisionContents {
    /// One [`source_delivered_profile`] per SOURCE, not per row: the merge
    /// clones every layer that source contributed, and a per-row derivation
    /// pays it once per item the source delivered.
    ///
    /// `owners` is the ownership record the machine will honour, built by the
    /// caller through [`merged_entry_owners`] from the same resolution and the
    /// same modules the run planned with. A caller passing
    /// `ResolvedProfile::merged.entry_owners` alone simply annotates no
    /// module-owned entry, rather than claiming a profile layer wins one.
    pub fn for_decisions(
        resolved: &ResolvedProfile,
        decisions: &[PendingDecision],
        config_dir: &Path,
        owners: &config::EntryOwners,
    ) -> Self {
        let mut by_source: BTreeMap<&str, Vec<&PendingDecision>> = BTreeMap::new();
        for d in decisions {
            by_source.entry(&d.source).or_default().push(d);
        }
        let mut contents = HashMap::new();
        let mut outranked = HashMap::new();
        for (source, items) in by_source {
            let delivered = source_delivered_profile(resolved, source);
            for item in items {
                let key = (source.to_string(), item.resource.clone());
                if let Some(content) =
                    decision_resource_content(&delivered, &item.resource, config_dir)
                {
                    contents.insert(key.clone(), content);
                }
                if let Some(winner) = outranking_owner(owners, source, &item.resource) {
                    outranked.insert(key, winner);
                }
            }
        }
        Self {
            contents,
            outranked,
        }
    }

    /// The ONE composition of a decision row, for every surface that lists one:
    /// `cfgd decide`, `cfgd status`, and the plan/apply run header.
    ///
    /// The subject names the tier and the resource (`Recommended env.EDITOR`);
    /// the detail says what the source would put on the machine
    /// (`EDITOR=vim`), carrying the version-conflict annotation the persisted
    /// summary is the only home for.
    ///
    /// A row whose content could not be recovered renders the SUBJECT ALONE.
    /// The stored `summary` is deliberately never the fallback: it restates the
    /// tier, the resource and the source, all three of which the subject and
    /// the `source:<name>` owner heading above it have already said — printing
    /// it produced `Optional env.GONE — optional env.GONE (from team-config)`,
    /// which is the duplication this composer exists to remove.
    pub fn decision_row(&self, item: &PendingDecision) -> (String, Option<String>) {
        let subject = format!("{} {}", title_cased_tier(&item.tier), item.resource);
        let key = (item.source.clone(), item.resource.clone());
        let content = self.contents.get(&key);
        let annotation = decision_row_annotation(&item.summary);
        let detail = match (content, annotation) {
            (Some(content), Some(annotation)) => {
                Some(format!("{content}{SUMMARY_ANNOTATION_GLUE}{annotation}"))
            }
            (Some(content), None) => Some(content.clone()),
            // The annotation is real information about an installed package
            // that the subject cannot carry, so it stands alone when the
            // content is gone — unlike the coordinates the summary restates.
            (None, Some(annotation)) => Some(annotation.to_string()),
            (None, None) => None,
        };
        let Some(winner) = self.outranked.get(&key) else {
            return (subject, detail);
        };
        // Parenthesised and last, because it is a fact ABOUT the value to its
        // left rather than a second value: accepting the item still records the
        // answer, and this is why the apply that follows writes nothing.
        let outranked = format!("outranked by {winner}");
        (
            subject,
            Some(match detail {
                Some(detail) => format!("{detail} ({outranked})"),
                None => outranked,
            }),
        )
    }
}

/// The ownership record the machine will actually honour: the layer merge's own
/// claims, with every resolved module folded in on top.
///
/// The order is the env engine's, not a second opinion about precedence — a
/// module's entries overwrite a profile-layer claim there, so they overwrite
/// one here.
pub fn merged_entry_owners(
    resolved: &ResolvedProfile,
    modules: &[crate::modules::ResolvedModule],
) -> config::EntryOwners {
    let mut owners = resolved.merged.entry_owners.clone();
    for module in modules {
        owners.claim(
            &super::Owner::module(&module.name).token(),
            &module.env,
            &module.aliases,
        );
    }
    owners
}

/// Which per-entry ownership record, if any, can answer "who wins this
/// resource" for a decision path's KIND.
///
/// Only one kind has an answer, and the reason is structural rather than
/// missing work: `spec.env` merges last-writer-wins and the merge RECORDS the
/// writer ([`config::EntryOwners`]), so a losing entry is knowable. The other
/// three keep no per-entry owner and could not without changing what the merge
/// means — a manager's package list merges as a UNION (no entry displaces
/// another), and `files`/`system` entries are keyed by target with the winning
/// value written straight into the merged spec, nothing recording which layer
/// put it there.
enum OwnershipRecord {
    /// The merge records a per-entry owner, so an outranked item can be named.
    PerEntry,
    /// No per-entry owner exists for this kind, and the merge's shape is why.
    NoneByDesign,
    /// A kind nobody has classified: neither named nor knowably unnameable.
    Unclassified,
}

fn ownership_record(kind: &str) -> OwnershipRecord {
    match kind {
        "env" => OwnershipRecord::PerEntry,
        // A manager's package list merges as a union: no entry displaces another.
        "packages" => OwnershipRecord::NoneByDesign,
        // A managed file's winning value is written straight into the merged
        // spec, with nothing recording which layer put it there.
        "files" => OwnershipRecord::NoneByDesign,
        // Same for a system setting, keyed by its `<configurator>.<key>`.
        "system" => OwnershipRecord::NoneByDesign,
        _ => OwnershipRecord::Unclassified,
    }
}

/// The owner token that will win `resource` when it is NOT the source being
/// asked about — the fact that turns an accepted decision into an apply with
/// nothing to write.
///
/// `docs/sources.md` ranks the layers by priority and puts a module's env above
/// a profile's, so a source can be outranked by a higher-priority source, by
/// the operator's own local layer, or by a module. Answering `None` is the
/// honest default: it says nothing rather than claiming this source wins.
fn outranking_owner(owners: &config::EntryOwners, source: &str, resource: &str) -> Option<String> {
    let (kind, rest) = resource.split_once('.')?;
    let OwnershipRecord::PerEntry = ownership_record(kind) else {
        return None;
    };
    let winner = owners.env.get(rest)?;
    // `PATH` records every contributing layer, so "did this source win" is a
    // membership question rather than an equality one: a source whose entries
    // are IN the folded value is not outranked by the layers beside it.
    let mine = super::Owner::source(source).token();
    (!winner.split_whitespace().any(|token| token == mine)).then(|| winner.clone())
}

/// The tier word as it opens a decision row's subject, TitleCased to match
/// every other subject-opening word on the same dashboard.
///
/// Display only: `PendingDecision.tier` is the raw `spec.policy` key the user
/// wrote and the token every `-o json` reader and `cfgd decide` matches on, so
/// it is never rewritten at the source. It lives here beside the tier constants
/// rather than in one surface's helpers because all three surfaces that name a
/// decision read it — the run header rendered a raw `recommended` while
/// `decide` and `status` rendered `Recommended`, two spellings of one row.
/// Generic rather than a three-arm match over the known tiers, so a policy key
/// cfgd does not recognise still renders as itself instead of vanishing.
pub fn title_cased_tier(tier: &str) -> String {
    crate::sentence_case(tier)
}

/// The ONE instruction for closing an unanswered decision, spelled the same by
/// the run header, `cfgd decide` and `cfgd status`.
///
/// Three surfaces a single take shows back to back said `Run \`cfgd decide
/// accept/reject\` to answer` and `Use \`cfgd decide accept \<resource\>\` … to
/// resolve` — two verbs and two nouns for one operation on one object.
pub const MSG_ANSWER_DECISIONS: &str =
    "Run `cfgd decide accept <resource>` or `cfgd decide reject <resource>` to answer";

/// [`MSG_ANSWER_DECISIONS`] with the bulk form folded in, for a surface that
/// knows how many decisions are waiting.
///
/// `cfgd decide` printed the per-resource instruction and the bulk instruction
/// as two hints whose first thirty characters were identical, so the second read
/// as a wrapped continuation of the first. One line carries both, and the bulk
/// half appears only where it can do something a single answer cannot — with one
/// item pending, `--all` and naming the resource are the same operation.
pub fn answer_decisions_hint(pending: usize) -> String {
    if pending > 1 {
        format!("{MSG_ANSWER_DECISIONS}; `cfgd decide accept --all` answers every item")
    } else {
        MSG_ANSWER_DECISIONS.to_string()
    }
}

/// The same, for a decision already declined: the answer exists and reversing
/// it is a different instruction, so it is a different constant rather than a
/// second wording of [`MSG_ANSWER_DECISIONS`].
pub const MSG_INCLUDE_DECLINED_DECISIONS: &str =
    "Run `cfgd decide accept <resource>` to include it";

/// Which surface a decisions section is on, and so what its title's
/// annotation says beside the count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionsTitleScope {
    /// A listing of the decisions themselves (`cfgd decide`, `cfgd status`).
    Listing,
    /// A plan or apply preview, which lists them to say what it left out.
    NotInThisPlan,
}

/// The Pending Decisions section title, carrying its own count.
///
/// The count is the section's annotation, never a row: rendered as a status it
/// wore the decision glyph and the decision indent, so a single withheld item
/// listed as two `⊙` lines — a tally of the row directly beneath it. The plan's
/// qualifier joins it in the same parenthetical (`Pending Decisions (1 item,
/// not included in this plan)`): `cfgd plan` hand-built a title with the
/// qualifier and no count while `cfgd decide` one screen below rendered the
/// count and no qualifier, so one section wore two titles in one take.
pub fn pending_decisions_title(count: usize, scope: DecisionsTitleScope) -> String {
    decisions_title("Pending Decisions", count, scope)
}

/// The Declined Decisions section title — the same annotation as its pending
/// sibling, over the rows already answered `reject`.
pub fn declined_decisions_title(count: usize, scope: DecisionsTitleScope) -> String {
    decisions_title("Declined Decisions", count, scope)
}

fn decisions_title(noun: &str, count: usize, scope: DecisionsTitleScope) -> String {
    let items = crate::pluralize(count, "item");
    match scope {
        DecisionsTitleScope::Listing => format!("{noun} ({items})"),
        DecisionsTitleScope::NotInThisPlan => {
            format!("{noun} ({items}, not included in this plan)")
        }
    }
}

/// A decision list grouped by the source that raised each row, alphabetically.
///
/// The grouping every listing surface renders as `source:<name>` owner
/// sections — the heading is what says WHOSE the rows are, which is why no row
/// carries a `by <source>` of its own.
pub fn decisions_by_source(rows: &[PendingDecision]) -> BTreeMap<&str, Vec<&PendingDecision>> {
    let mut by_source: BTreeMap<&str, Vec<&PendingDecision>> = BTreeMap::new();
    for d in rows {
        by_source.entry(&d.source).or_default().push(d);
    }
    by_source
}

impl DecisionMint {
    /// The `pending_decisions.summary` text the row carries.
    pub fn summary(&self) -> String {
        match &self.annotation {
            Some(annotation) => format!(
                "{} {} (from {}){SUMMARY_ANNOTATION_GLUE}{}",
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
            content_hash: self.content_hash.clone(),
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
    /// `(source, resource, fingerprint)` for every row that predates
    /// fingerprinting, or that was recorded by a path with no fingerprint to
    /// hand. The first observation of an item's content is not a change to it,
    /// so the fingerprint is written onto the existing row and no question is
    /// asked.
    pub fingerprint_backfill: Vec<(String, String, String)>,
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
    /// [`review_source_policy`] judges each item on its own fingerprint, so a
    /// source hash it never wrote costs it nothing.
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
            fingerprint_backfill: Vec::new(),
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
        review.fingerprint_backfill.extend(one.fingerprint_backfill);
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

    // The unresolved rows, for the manual-install walk below. The
    // managed-resource table is deliberately NOT consulted: its rows live in
    // the state vocabulary (`package` + `<mgr>/<pkg>`), which never matches a
    // decision path — and an installed item that has no row is an item nobody
    // was ever asked about, exactly the one the `Notify` arm below still owes
    // a question.
    let rows = store.pending_decisions_for_source(source_name)?;

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
                    content_hash: delivered
                        .content_hash_for(&row.resource)
                        .map(str::to_string),
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

    for (resource, tier) in delivered.iter() {
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
            // Asked about per ITEM, judged on the item's own fingerprint —
            // never on the source's delivered SET. A whole-source gate got
            // both halves wrong: an unrelated upstream commit adding one item
            // re-asked every answer the operator had already given, while an
            // item whose declared value changed under an unchanged set (EDITOR
            // moving from nvim to vim) was applied without ever asking. So:
            // no row means never asked; a row whose fingerprint differs means
            // the operator answered a different item than the one now
            // delivered; a row whose fingerprint agrees is answered, whatever
            // else the source moved. An item a `Reject` policy declined has no
            // row, which is what makes a policy flip to `Notify` ask about it.
            //
            // A row with no fingerprint recorded is the item's FIRST
            // observation under this rule, not a change to it — the
            // fingerprint is written onto the row and nothing is asked, which
            // is what keeps an answer given before the column existed (or
            // through `cfgd decide`, which records the item it answers) from
            // being re-asked once.
            PolicyAction::Notify => {
                let current = delivered.content_hash_for(resource).unwrap_or_default();
                let must_ask = match store.latest_decision_content_hash(source_name, resource)? {
                    None => true,
                    Some(None) => {
                        review.fingerprint_backfill.push((
                            source_name.to_string(),
                            resource.clone(),
                            current.to_string(),
                        ));
                        false
                    }
                    Some(Some(recorded)) => recorded != current,
                };
                if must_ask {
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
                                content_hash: Some(current.to_string()),
                            })
                        }
                        InstallVerdict::Undetermined => review.to_mint.push(DecisionMint {
                            source: source_name.to_string(),
                            resource: resource.clone(),
                            tier: tier.to_string(),
                            annotation: None,
                            content_hash: Some(current.to_string()),
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
/// what has never been asked about (or what the source now declares
/// differently from the row's fingerprint), and the
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
            mint.content_hash.as_deref(),
        ) {
            // tracing-ok: the decision ROW could not be written; the decision itself renders from the plan
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
            mint.content_hash.as_deref(),
        ) {
            // tracing-ok: same, for the annotation column
            tracing::warn!(error = %e, "failed to refresh pending decision annotation");
        }
    }

    // Stamping an existing row with the fingerprint of what it was already
    // asked about — no question, no notification, and no new row.
    for (source_name, resource, hash) in &review.fingerprint_backfill {
        if let Err(e) = store.set_decision_content_hash(source_name, resource, hash) {
            // tracing-ok: same, for the per-item fingerprint
            tracing::warn!(error = %e, "failed to record decision content hash");
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
            // tracing-ok: same, for an auto-accepted row
            tracing::warn!(error = %e, "failed to record auto-accepted decision");
        }
    }

    for (source_name, hash) in &review.changed_hashes {
        if let Err(e) = store.set_source_config_hash(source_name, hash) {
            // tracing-ok: same, for the source hash
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
    items: BTreeMap<String, DeliveredItem>,
}

/// One delivered resource: the tier its layer carried it at, and the
/// fingerprint of what that layer declares for it.
#[derive(Debug)]
struct DeliveredItem {
    tier: &'static str,
    content_hash: String,
}

impl DeliveredItems {
    /// What `source_name` delivered into a composed profile.
    pub fn for_source(resolved: &ResolvedProfile, source_name: &str) -> Self {
        Self::from_layers(&source_delivered_layers(resolved, source_name))
    }

    /// Tag every resource on `layers` with the tier of the layer carrying it.
    ///
    /// One source can deliver a path on more than one layer; the strongest
    /// tier decides, and the fingerprint travels with it — the item the
    /// operator is asked about is the one that tier's layer declares.
    pub fn from_layers(layers: &[config::ProfileLayer]) -> Self {
        let mut items: BTreeMap<String, DeliveredItem> = BTreeMap::new();
        for layer in layers {
            let tier = tier_of(&layer.policy);
            for (resource, content_hash) in
                declared_decision_fingerprints(&config::merge_layers(std::slice::from_ref(layer)))
            {
                match items.get(&resource) {
                    Some(existing) if tier_rank(existing.tier) >= tier_rank(tier) => {}
                    _ => {
                        items.insert(resource, DeliveredItem { tier, content_hash });
                    }
                }
            }
        }
        Self { items }
    }

    /// `(resource, tier)` for everything the source delivers, in path order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &'static str)> + '_ {
        self.items.iter().map(|(r, i)| (r, i.tier))
    }

    /// The tier `resource` is delivered at, if this source still delivers it.
    pub fn tier_for(&self, resource: &str) -> Option<&'static str> {
        self.items.get(resource).map(|i| i.tier)
    }

    /// What the source currently declares for `resource`, as the fingerprint a
    /// decision row records so a later run can tell the item apart from itself.
    pub fn content_hash_for(&self, resource: &str) -> Option<&str> {
        self.items.get(resource).map(|i| i.content_hash.as_str())
    }

    /// The hash the change detector compares against the last run's.
    pub fn resource_hash(&self) -> String {
        hash_resources(&self.items.keys().cloned().collect())
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
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
/// | `packages.<mgr>.<pkg>` | that one package inside a batch — a `PackageAction::Install`/`Uninstall` for `<mgr>` or a module's `InstallPackages` (matched on its resolved name). The batch keeps its other packages and is dropped only when it empties. `packages.brew.<pkg>` also matches the `brew-cask` manager: the decision vocabulary folds casks into `brew` and cannot tell a cask from a formula. Every other manager — a brew tap under `brew-tap`, a custom manager under its own name — mints under the exact name its planned batch carries, so the match here is verbatim. A `Skip` names no package and is never withheld |
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
                // tracing-ok: a decision path that matches no planned resource; no row exists for it to restate
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

    /// Whether a recorded drift row names a resource these exclusions withheld
    /// from the plan — the keep-side twin of the prune. [`withhold_from_plan`]
    /// runs BEFORE the daemon's complement-resolve reads the plan, so a
    /// withheld resource's action is already gone when the tick asks "what did
    /// this plan not re-find"; without this predicate the very rows the
    /// pending decision is about are resolved blind, though the tick
    /// deliberately did not judge them. Matches every id grammar a drift
    /// writer mints for the four exclusion vocabularies, both producers'
    /// spellings per type.
    pub fn withholds_recorded_row(&self, resource_type: &str, resource_id: &str) -> bool {
        match resource_type {
            // The daemon's file rows carry the same expanded, `/`-folded id
            // the exclusion set stores (`withholds_action` compares the two
            // directly).
            "file" => self.files.contains(resource_id),
            // The CLI's module-file rows: `<module>/<target>` with the
            // target's leading separator trimmed. The tail is folded
            // unconditionally before matching, the exclusion set's own
            // `to_posix_string` fold — keep and prune then answer alike for
            // a Unix target carrying a legal `\`.
            "module" => resource_id.split_once('/').is_some_and(|(_, tail)| {
                let tail = crate::posixify_text(tail);
                self.files
                    .iter()
                    .any(|f| f.trim_start_matches('/') == tail.as_ref())
            }),
            // Every `package` grammar. A `provision:`/`refuse:` row is kept
            // while ANY package on that manager is withheld: withholding the
            // manager's last consumer prunes its provision node too, and the
            // finding stands until the decision lands. Otherwise one withheld
            // member reshapes a batch, so both the per-package and the batch
            // spelling are judged member by member.
            "package" => {
                if let Some(manager) = resource_id
                    .strip_prefix("provision:")
                    .or_else(|| resource_id.strip_prefix("refuse:"))
                {
                    return self.packages.contains_key(manager)
                        || (manager == "brew-cask" && self.packages.contains_key("brew"));
                }
                resource_id.split_once(':').is_some_and(|(manager, rest)| {
                    rest.split(',').any(|p| self.withholds_package(manager, p))
                })
            }
            // Both system grammars — the CLI's `<cfg>.<key>` and the daemon's
            // `<cfg>:<key>`; a configurator is withheld whole.
            "system" => resource_id
                .split_once(['.', ':'])
                .is_some_and(|(configurator, _)| self.system.contains(configurator)),
            // The env surface is withheld as a unit, so every per-item and
            // per-file spelling under it is a row the tick did not judge.
            "env-var" | "alias" | "env" | "env-rc" | "env-session" => self.withholds_env_surface(),
            _ => false,
        }
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
    // A manager node exists to serve the installs below it. Withholding the
    // last of them withholds the refresh with them, before the count is taken,
    // so the header never names a number the run disagrees with.
    super::managers::prune_to_surviving_consumers(plan);
    let withheld = before.saturating_sub(plan.total_actions());
    if withheld > 0 {
        tracing::debug!(
            actions = withheld,
            "withheld actions whose resource awaits a source decision"
        );
    }
    withheld
}

#[cfg(test)]
mod outranked_tests {
    use super::*;

    fn row(source: &str, resource: &str) -> PendingDecision {
        PendingDecision {
            id: 1,
            source: source.to_string(),
            resource: resource.to_string(),
            tier: TIER_RECOMMENDED.to_string(),
            action: DECISION_ACTION_INSTALL.to_string(),
            summary: format!("recommended {resource} (from {source})"),
            created_at: "2026-05-14T10:00:00Z".to_string(),
            resolved_at: None,
            resolution: None,
            content_hash: None,
        }
    }

    fn owners(entries: &[(&str, &str)]) -> config::EntryOwners {
        let mut owners = config::EntryOwners::default();
        for (name, owner) in entries {
            owners.claim_env_names(owner, [*name]);
        }
        owners
    }

    /// A composition in which `source` delivered one env var: the layer is what
    /// `source_delivered_profile` reads the row's CONTENT out of, so a fixture
    /// without one exercises the content-less arm instead of this one.
    fn resolved_with_source_env(source: &str, name: &str, value: &str) -> ResolvedProfile {
        let env = vec![config::EnvVar {
            name: name.to_string(),
            value: value.to_string(),
            platforms: vec![],
        }];
        let spec = config::ProfileSpec {
            env: env.clone(),
            ..Default::default()
        };
        let merged = MergedProfile {
            env,
            ..Default::default()
        };
        ResolvedProfile {
            layers: vec![config::ProfileLayer {
                source: source.to_string(),
                profile_name: "team".to_string(),
                priority: 500,
                policy: config::LayerPolicy::Recommended,
                spec,
            }],
            merged,
        }
    }

    /// Accepting an item whose value the merge will discard still records the
    /// answer — and the row has to say why the apply that follows writes
    /// nothing, or the operator reads a decision that did nothing as a bug.
    #[test]
    fn an_env_item_a_higher_layer_wins_says_who_wins_it() {
        let resolved = resolved_with_source_env("team", "PAGER", "less");
        let decisions = [row("team", "env.PAGER")];
        let contents = DecisionContents::for_decisions(
            &resolved,
            &decisions,
            Path::new("/nonexistent"),
            &owners(&[("PAGER", "module:nvim")]),
        );
        let (subject, detail) = contents.decision_row(&decisions[0]);
        assert_eq!(subject, "Recommended env.PAGER");
        assert_eq!(
            detail.as_deref(),
            Some("PAGER=less (outranked by module:nvim)"),
            "the row states the value AND who displaces it"
        );
    }

    /// The negative half: the source that wins its own entry is annotated with
    /// nothing. Without this the annotation would read as decoration on every
    /// env row instead of as a warning about this one.
    #[test]
    fn an_env_item_its_own_source_wins_carries_no_annotation() {
        let resolved = resolved_with_source_env("team", "PAGER", "less");
        let decisions = [row("team", "env.PAGER")];
        let contents = DecisionContents::for_decisions(
            &resolved,
            &decisions,
            Path::new("/nonexistent"),
            &owners(&[("PAGER", "source:team")]),
        );
        assert_eq!(
            contents.decision_row(&decisions[0]).1.as_deref(),
            Some("PAGER=less")
        );
    }

    /// The two decision-section titles have ONE builder. `cfgd plan` hand-built
    /// `Pending Decisions (not included in this plan)` while `cfgd decide` one
    /// screen below rendered `Pending Decisions (1 item)` — the same section,
    /// the same rows, two annotations in one take. Every production literal in
    /// both crates is walked; one outside this file fails.
    #[test]
    fn every_decisions_section_title_comes_from_the_one_builder() {
        let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let cli = core.join("../../cfgd/src");
        let mut offenders = Vec::new();
        let mut files = Vec::new();
        for root in [core, cli] {
            rust_files(&root, &mut files);
        }
        assert!(files.len() > 100, "the walk reached {} files", files.len());
        for path in files {
            if path.file_name().is_some_and(|n| n == "pending.rs")
                || path.file_name().is_some_and(|n| n == "tests.rs")
                || path.components().any(|c| c.as_os_str() == "tests")
            {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (n, line) in body.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if code.contains("\"Pending Decisions") || code.contains("\"Declined Decisions") {
                    offenders.push(format!("{}:{}: {code}", path.display(), n + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a decisions section title composes through `pending_decisions_title` / \
             `declined_decisions_title`:\n{}",
            offenders.join("\n")
        );

        assert_eq!(
            pending_decisions_title(1, DecisionsTitleScope::NotInThisPlan),
            "Pending Decisions (1 item, not included in this plan)"
        );
        assert_eq!(
            declined_decisions_title(2, DecisionsTitleScope::Listing),
            "Declined Decisions (2 items)"
        );
    }

    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Every kind `decision_resource_content` recognizes is classified by
    /// [`ownership_record`], so the next decision path added trips here rather
    /// than silently never being annotated. The kinds are read out of the
    /// producer's own match arms — a new arm there is a new row here.
    #[test]
    fn every_decision_kind_states_whether_it_has_an_ownership_record() {
        let source = include_str!("pending.rs");
        let body = source
            .split_once("pub fn decision_resource_content(")
            .and_then(|(_, rest)| rest.split_once("\n}\n"))
            .map(|(body, _)| body)
            .expect("decision_resource_content must be findable in its own file");
        let kinds: Vec<&str> = body
            .lines()
            .filter_map(|line| line.trim().strip_prefix('"'))
            .filter_map(|rest| rest.split_once("\" =>"))
            .map(|(kind, _)| kind)
            .collect();
        assert!(
            kinds.len() >= 4,
            "the scan found no decision kinds, so it proves nothing: {kinds:?}"
        );
        let unclassified: Vec<&str> = kinds
            .iter()
            .copied()
            .filter(|kind| matches!(ownership_record(kind), OwnershipRecord::Unclassified))
            .collect();
        assert!(
            unclassified.is_empty(),
            "a decision kind must say whether a per-entry owner exists for it, \
             so an outranked item of that kind is either named or knowably \
             unnameable:\n{unclassified:?}"
        );
    }
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

    #[test]
    fn a_mints_annotation_round_trips_out_of_its_summary() {
        let mint = DecisionMint {
            source: "team".to_string(),
            resource: "packages.brew.curl".to_string(),
            tier: TIER_RECOMMENDED.to_string(),
            annotation: Some("installed 7.1, source wants ^8".to_string()),
            content_hash: None,
        };
        assert_eq!(
            decision_row_annotation(&mint.summary()),
            Some("installed 7.1, source wants ^8"),
            "the join and the split must be the same glue"
        );

        let plain = DecisionMint {
            annotation: None,
            ..mint
        };
        assert_eq!(
            decision_row_annotation(&plain.summary()),
            None,
            "a summary carrying no annotation must not invent one"
        );
    }
}

#[cfg(test)]
mod fingerprint_gate_tests {
    use super::*;
    use crate::config::{AutoApplyPolicyConfig, LayerPolicy};
    use crate::test_helpers::test_state;

    /// One source offering the named environment variables at the recommended
    /// tier — the surface the whole-source gate got wrong, because an env
    /// entry's value changes without its path changing.
    fn env_delivery(vars: &[(&str, &str)]) -> DeliveredItems {
        let layer = config::ProfileLayer {
            source: "acme".to_string(),
            profile_name: "offered".to_string(),
            priority: 500,
            policy: LayerPolicy::Recommended,
            spec: config::ProfileSpec {
                env: vars
                    .iter()
                    .map(|(name, value)| config::EnvVar {
                        name: name.to_string(),
                        value: value.to_string(),
                        platforms: vec![],
                    })
                    .collect(),
                ..Default::default()
            },
        };
        DeliveredItems::from_layers(&[layer])
    }

    fn classify(store: &StateStore, delivered: &DeliveredItems) -> SourcePolicyReview {
        review_source_policy(
            store,
            "acme",
            delivered,
            &AutoApplyPolicyConfig::default(),
            &ActualPackages::default(),
        )
        .expect("classification reads the test store")
    }

    fn minted(review: &SourcePolicyReview) -> Vec<&str> {
        review.to_mint.iter().map(|m| m.resource.as_str()).collect()
    }

    /// The two variables asked about once, then answered — an accepted one and
    /// a rejected one, both carrying the fingerprint of what they answered.
    fn store_with_two_answers() -> (StateStore, DeliveredItems) {
        let store = test_state();
        let delivered = env_delivery(&[("EDITOR", "nvim"), ("SHELL", "zsh")]);
        let review = classify(&store, &delivered);
        assert_eq!(minted(&review), vec!["env.EDITOR", "env.SHELL"]);
        mint_decisions(&store, &review);
        store.resolve_decision("env.EDITOR", "accepted").unwrap();
        store.resolve_decision("env.SHELL", "rejected").unwrap();
        (store, delivered)
    }

    /// Every resource a decision still withholds — an acceptance releases its
    /// resource, a rejection and an unanswered question both keep it.
    fn withheld(store: &StateStore) -> Vec<String> {
        let mut resources: Vec<String> = store
            .withheld_decisions()
            .unwrap()
            .into_iter()
            .map(|d| d.resource)
            .collect();
        resources.sort();
        resources
    }

    #[test]
    fn a_new_item_leaves_the_already_answered_ones_alone() {
        // The reported bug: an upstream commit adds an unrelated variable, and
        // the whole-source hash re-asked every answer the operator had given.
        let (store, _) = store_with_two_answers();
        let grown = env_delivery(&[("EDITOR", "nvim"), ("SHELL", "zsh"), ("PAGER", "less")]);

        let review = classify(&store, &grown);
        assert_eq!(
            minted(&review),
            vec!["env.PAGER"],
            "only the newcomer owes a question"
        );
        assert!(
            review.fingerprint_backfill.is_empty(),
            "the answered rows are already fingerprinted"
        );

        mint_decisions(&store, &review);
        assert_eq!(
            withheld(&store),
            vec!["env.PAGER".to_string(), "env.SHELL".to_string()],
            "the acceptance still releases its resource and the rejection \
             still withholds its own; only the newcomer joins them"
        );
    }

    #[test]
    fn a_changed_value_reasks_exactly_that_item() {
        // The other half the whole-source hash missed: the delivered SET is
        // identical, so nothing about the source moved except what one of its
        // items says.
        let (store, _) = store_with_two_answers();
        let changed = env_delivery(&[("EDITOR", "vim"), ("SHELL", "zsh")]);

        let review = classify(&store, &changed);
        assert_eq!(
            minted(&review),
            vec!["env.EDITOR"],
            "the item the operator accepted now says something else"
        );
    }

    #[test]
    fn a_row_with_no_fingerprint_is_stamped_rather_than_reasked() {
        // A row answered before the column existed, or recorded by a path with
        // no delivered item in hand. The first observation of its content is
        // not a change to it.
        let store = test_state();
        store
            .upsert_pending_decision(
                "acme",
                "env.EDITOR",
                TIER_RECOMMENDED,
                DECISION_ACTION_INSTALL,
                "recommended env.EDITOR (from acme)",
                None,
            )
            .unwrap();
        store.resolve_decision("env.EDITOR", "accepted").unwrap();

        let delivered = env_delivery(&[("EDITOR", "nvim")]);
        let review = classify(&store, &delivered);
        assert!(
            review.to_mint.is_empty(),
            "an answered item is not re-asked on its first fingerprinting"
        );
        assert_eq!(
            review
                .fingerprint_backfill
                .iter()
                .map(|(s, r, _)| (s.as_str(), r.as_str()))
                .collect::<Vec<_>>(),
            vec![("acme", "env.EDITOR")],
            "the row learns which version of the item it answered"
        );

        mint_decisions(&store, &review);
        let again = classify(&store, &delivered);
        assert!(
            again.to_mint.is_empty() && again.fingerprint_backfill.is_empty(),
            "the stamped row is settled: nothing to ask, nothing to write"
        );
        assert!(
            withheld(&store).is_empty(),
            "and the backfill never touched the answer: the item stays accepted"
        );
    }

    #[test]
    fn an_unresolved_row_is_reminted_only_when_its_item_changed() {
        // A pending question is not re-asked (and so not re-notified) tick
        // after tick; a pending question about an item that has since changed
        // is, because the row on screen describes something the source no
        // longer delivers.
        let store = test_state();
        let delivered = env_delivery(&[("EDITOR", "nvim")]);
        let first = classify(&store, &delivered);
        assert_eq!(minted(&first), vec!["env.EDITOR"]);
        mint_decisions(&store, &first);

        let unchanged = classify(&store, &delivered);
        assert!(
            unchanged.to_mint.is_empty() && unchanged.fingerprint_backfill.is_empty(),
            "an unanswered question is asked once, not every tick"
        );

        let changed = classify(&store, &env_delivery(&[("EDITOR", "vim")]));
        assert_eq!(
            minted(&changed),
            vec!["env.EDITOR"],
            "the pending row must describe what the source delivers now"
        );
        mint_decisions(&store, &changed);
        let rows = store.pending_decisions_for_source("acme").unwrap();
        assert_eq!(rows.len(), 1, "refreshed in place, never duplicated");
    }
}

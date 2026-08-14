use std::str::FromStr;

use serde::Serialize;

use crate::config::ScriptEntry;
use crate::providers::{FileAction, PackageAction, SecretAction};
use crate::state::ApplyStatus;
use crate::to_posix_string;

/// Whether the reconciler is running in CLI apply mode or daemon reconcile mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileContext {
    Apply,
    Reconcile,
}

/// Ordered reconciliation phases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PhaseName {
    PreScripts,
    /// Everything the rest of the run consumes but no user document declares:
    /// the package managers themselves (`cfgd:managers`), the generated env
    /// file that publishes where their binaries live (`cfgd:env`), and the live
    /// session broadcast (`cfgd:session`) — in that producer-before-consumer
    /// order.
    Prerequisites,
    Modules,
    Packages,
    System,
    Files,
    Secrets,
    PostScripts,
}

impl PhaseName {
    pub fn as_str(&self) -> &str {
        match self {
            PhaseName::PreScripts => "pre-scripts",
            PhaseName::Prerequisites => "prerequisites",
            PhaseName::Modules => "modules",
            PhaseName::Packages => "packages",
            PhaseName::System => "system",
            PhaseName::Files => "files",
            PhaseName::Secrets => "secrets",
            PhaseName::PostScripts => "post-scripts",
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            PhaseName::PreScripts => "Pre-Scripts",
            PhaseName::Prerequisites => "Prerequisites",
            PhaseName::Modules => "Modules",
            PhaseName::Packages => "Packages",
            PhaseName::System => "System",
            PhaseName::Files => "Files",
            PhaseName::Secrets => "Secrets",
            PhaseName::PostScripts => "Post-Scripts",
        }
    }

    /// The phase's section heading — the ONE spelling of the `Phase: <name>`
    /// title. Execution (`reconciler::apply`), preview (`reconciler::run`) and
    /// the drift surfaces (`cfgd diff`, `cfgd rollback`) all head their trees
    /// with it, so a reader matching a drift report against the plan that would
    /// fix it is matching identical strings.
    pub fn section_title(&self) -> String {
        format!("Phase: {}", self.display_name())
    }
}

impl FromStr for PhaseName {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pre-scripts" => Ok(PhaseName::PreScripts),
            // `env` is the phase's pre-merge spelling: it now names the
            // `cfgd:env` group of a wider phase, and a filter written against
            // it still selects the phase that holds that work.
            "prerequisites" | "env" => Ok(PhaseName::Prerequisites),
            "modules" => Ok(PhaseName::Modules),
            "system" => Ok(PhaseName::System),
            "packages" => Ok(PhaseName::Packages),
            "files" => Ok(PhaseName::Files),
            "secrets" => Ok(PhaseName::Secrets),
            "post-scripts" => Ok(PhaseName::PostScripts),
            _ => Err(format!("unknown phase: {}", s)),
        }
    }
}

/// Environment file action — write ~/.cfgd.env or inject source line into shell rc.
#[derive(Debug, Serialize)]
pub enum EnvAction {
    /// Write the generated env file (bash/zsh or fish).
    WriteEnvFile {
        path: std::path::PathBuf,
        content: String,
    },
    /// Inject a source line into a shell rc file (idempotent).
    InjectSourceLine {
        rc_path: std::path::PathBuf,
        line: String,
    },
    /// Refresh the current user's live session so already-running session
    /// managers spawn new processes with these vars, without a re-login
    /// (macOS `launchctl setenv`, Linux `systemctl --user set-environment`,
    /// Windows `setx`). Best-effort and idempotent.
    RefreshLiveSession { vars: Vec<(String, String)> },
}

/// Work on a package manager itself, rather than on a package: the
/// `cfgd:managers` owner group of the [`PhaseName::Prerequisites`] phase.
///
/// The group is a DAG, not a list. Each node carries the ids of the nodes it
/// must follow ([`ManagerAction::depends_on`]), so a scheduler reads the edges
/// off the plan instead of re-deriving them from provider probes that may
/// answer differently by the time it runs — and a failed node fails its
/// dependents transitively by the same edges.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ManagerAction {
    /// A manager already present on the host: refresh its package index.
    /// Always emitted for a present manager — the default system manager is
    /// almost always user-installed, and skipping its refresh fails most runs.
    RefreshIndex { manager: String },
    /// A manager this run will install, and the method its own cascade picks.
    /// A provisioned manager's index is fresh by construction, so it never also
    /// carries a [`ManagerAction::RefreshIndex`].
    Provision {
        manager: String,
        via: String,
        depends_on: Vec<String>,
    },
    /// A tool a manager's bootstrap cascade shells out to, missing from this
    /// host and installed from an available system manager.
    ///
    /// Never drawn from the user's declared package set, never recorded as a
    /// user-managed resource and never removed later: it is a tool cfgd needed.
    Prerequisite {
        tool: String,
        installer: String,
        /// The managers that named the tool, in sorted order — one node serves
        /// all of them, and the line says so.
        required_by: Vec<String>,
        depends_on: Vec<String>,
    },
    /// A manager this run needs and this host cannot provision, with the cause
    /// named. It runs nothing: the node exists so the refusal is visible in the
    /// phase the user is told to look at, instead of being a manager that
    /// quietly never appears.
    Refuse { manager: String, reason: String },
}

/// The `resource_type` half of every [`ManagerAction`]'s persisted identity.
/// Also the one value `record_managed_resources` refuses to write a row for:
/// cfgd's own scaffolding is journalled, never managed.
pub(super) const MANAGER_RESOURCE_TYPE: &str = "manager";

fn refresh_id(manager: &str) -> String {
    format!("refresh:{manager}")
}

fn provision_id(manager: &str) -> String {
    format!("provision:{manager}")
}

fn prereq_id(tool: &str) -> String {
    format!("prereq:{tool}")
}

fn refuse_id(manager: &str) -> String {
    format!("refuse:{manager}")
}

fn node_of(resource_id: &str) -> String {
    format!("{MANAGER_RESOURCE_TYPE}:{resource_id}")
}

impl ManagerAction {
    /// The node's persisted id, without its `manager:` type prefix —
    /// `refresh:<manager>`, `provision:<manager>`, `prereq:<tool>`.
    ///
    /// The ONE derivation: the journal `resource_id`, the `managed_resources`
    /// id, the description [`crate::reconciler::format_action_description`]
    /// returns and the DAG edges below are all this string, so an edge can
    /// never name a node no record was written under.
    pub fn resource_id(&self) -> String {
        match self {
            ManagerAction::RefreshIndex { manager } => refresh_id(manager),
            ManagerAction::Provision { manager, .. } => provision_id(manager),
            ManagerAction::Prerequisite { tool, .. } => prereq_id(tool),
            ManagerAction::Refuse { manager, .. } => refuse_id(manager),
        }
    }

    /// The node's id in the phase's DAG — the full `manager:<resource_id>`
    /// string, identical to this action's `format_action_description`.
    pub fn node_id(&self) -> String {
        node_of(&self.resource_id())
    }

    /// The DAG id of a refresh on `manager`, for the planner wiring an edge to
    /// a node it does not hold.
    pub fn refresh_node(manager: &str) -> String {
        node_of(&refresh_id(manager))
    }

    /// The DAG id of a provision of `manager`.
    pub fn provision_node(manager: &str) -> String {
        node_of(&provision_id(manager))
    }

    /// The DAG id of the prerequisite installing `tool`.
    pub fn prereq_node(tool: &str) -> String {
        node_of(&prereq_id(tool))
    }

    /// The DAG id of the refusal to provision `manager`. Nothing depends on a
    /// refusal — a node that would have is refused itself — so this exists for
    /// the planner's own bookkeeping and for the journal row.
    pub fn refuse_node(manager: &str) -> String {
        node_of(&refuse_id(manager))
    }

    /// The nodes this one must follow, as [`ManagerAction::node_id`] values.
    /// Empty for a refresh, which is always a root.
    pub fn depends_on(&self) -> &[String] {
        match self {
            ManagerAction::RefreshIndex { .. } | ManagerAction::Refuse { .. } => &[],
            ManagerAction::Provision { depends_on, .. }
            | ManagerAction::Prerequisite { depends_on, .. } => depends_on,
        }
    }

    /// The REGISTERED manager whose command this node runs — the manager itself
    /// for a refresh or a provision, and the installing system manager for a
    /// prerequisite, since `apt install curl` is apt's command.
    pub fn manager(&self) -> &str {
        match self {
            ManagerAction::RefreshIndex { manager }
            | ManagerAction::Provision { manager, .. }
            | ManagerAction::Refuse { manager, .. } => manager,
            ManagerAction::Prerequisite { installer, .. } => installer,
        }
    }
}

/// A unified action across all resource types.
#[derive(Debug, Serialize)]
pub enum Action {
    File(FileAction),
    Package(PackageAction),
    Secret(SecretAction),
    System(SystemAction),
    Script(ScriptAction),
    Module(ModuleAction),
    Env(EnvAction),
    Manager(ManagerAction),
}

/// Module-level action — first-class phase, not flattened into packages/files.
#[derive(Debug, Serialize)]
pub struct ModuleAction {
    pub module_name: String,
    pub kind: ModuleActionKind,
    /// Provenance of the module body: `None` = consumer-local module;
    /// `Some(source_name)` = body delivered by the named ConfigSource. Mirrors
    /// `ResolvedModule::origin` and drives the ` <- <source>` plan suffix and the
    /// structured `origin` field, exactly as file/package actions surface theirs.
    pub origin: Option<String>,
}

impl ModuleAction {
    /// Build a module action for a consumer-local module (no source provenance).
    pub fn local(module_name: impl Into<String>, kind: ModuleActionKind) -> Self {
        ModuleAction {
            module_name: module_name.into(),
            kind,
            origin: None,
        }
    }

    /// Build a module action, carrying the originating module's source
    /// provenance (`ResolvedModule::origin`) so the plan and structured output
    /// can attribute the module to the ConfigSource that delivered it.
    pub fn with_origin(
        module_name: impl Into<String>,
        kind: ModuleActionKind,
        origin: Option<String>,
    ) -> Self {
        ModuleAction {
            module_name: module_name.into(),
            kind,
            origin,
        }
    }
}

/// What kind of module action to take.
#[derive(Debug, Serialize)]
pub enum ModuleActionKind {
    /// Install/update packages resolved from a module.
    InstallPackages {
        resolved: Vec<crate::modules::ResolvedPackage>,
    },
    /// Deploy files from a module.
    DeployFiles {
        files: Vec<crate::modules::ResolvedFile>,
    },
    /// Run a module lifecycle script.
    RunScript {
        script: ScriptEntry,
        phase: ScriptPhase,
    },
    /// Skip a module (dependency not met, user declined, etc.).
    Skip { reason: String },
}

/// System configuration action.
#[derive(Debug, Serialize)]
pub enum SystemAction {
    SetValue {
        configurator: String,
        key: String,
        desired: String,
        current: String,
        origin: String,
    },
    Skip {
        configurator: String,
        reason: String,
        origin: String,
        /// `true` when no configurator is registered for this key (a likely
        /// typo, surfaced as a warning); `false` when the configurator exists
        /// but is unavailable on this host (expected, surfaced neutrally).
        unknown: bool,
    },
}

/// Script execution action.
#[derive(Debug, Serialize)]
pub enum ScriptAction {
    Run {
        entry: ScriptEntry,
        phase: ScriptPhase,
        origin: String,
    },
}

/// When a script runs relative to reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ScriptPhase {
    PreApply,
    PostApply,
    PreReconcile,
    PostReconcile,
    OnDrift,
    OnChange,
    /// A `patch.script` filter rewriting a managed file's content.
    Patch,
    /// A `spec.backups[].preBackup` hook, run before the snapshot is taken.
    PreBackup,
    /// A `spec.backups[].postBackup` hook, run after the copy step.
    PostBackup,
}

impl ScriptPhase {
    pub fn display_name(&self) -> &'static str {
        match self {
            ScriptPhase::PreApply => "preApply",
            ScriptPhase::PostApply => "postApply",
            ScriptPhase::PreReconcile => "preReconcile",
            ScriptPhase::PostReconcile => "postReconcile",
            ScriptPhase::OnDrift => "onDrift",
            ScriptPhase::OnChange => "onChange",
            ScriptPhase::Patch => "patch",
            ScriptPhase::PreBackup => "preBackup",
            ScriptPhase::PostBackup => "postBackup",
        }
    }
}

/// What a `--phase` value selects. `modules` is an owner filter, not a phase
/// filter, because module work lands in the phase whose kind it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseFilter {
    Phase(PhaseName),
    ModuleOwners,
}

/// Who declared the work: the complete, closed vocabulary.
///
/// A ConfigSource is deliberately not a kind. A source delivers a module's or
/// a file's body; the module still owns the action, and making the source an
/// owner would give an action two parents. Source attribution rides on the
/// action instead, as the ` <- name` provenance suffix and the `origin` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OwnerKind {
    Profile,
    Cfgd,
    Module,
    Backup,
    Source,
}

impl OwnerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OwnerKind::Profile => "profile",
            OwnerKind::Cfgd => "cfgd",
            OwnerKind::Module => "module",
            OwnerKind::Backup => "backup",
            OwnerKind::Source => "source",
        }
    }

    /// Parse an owner token's kind word. The inverse of [`OwnerKind::as_str`];
    /// the pair is round-trip tested so a sixth kind cannot be added to one
    /// side only.
    pub fn from_token(token: &str) -> Option<OwnerKind> {
        match token {
            "profile" => Some(OwnerKind::Profile),
            "cfgd" => Some(OwnerKind::Cfgd),
            "module" => Some(OwnerKind::Module),
            "backup" => Some(OwnerKind::Backup),
            "source" => Some(OwnerKind::Source),
            _ => None,
        }
    }

    fn rank(&self) -> u8 {
        match self {
            OwnerKind::Profile => 0,
            OwnerKind::Cfgd => 1,
            OwnerKind::Module => 2,
            OwnerKind::Backup => 3,
            OwnerKind::Source => 4,
        }
    }
}

/// The order cfgd's own groups run and render in, which is causal rather than
/// alphabetical: `managers` is the only group that changes what binaries exist,
/// `env` publishes where they live, `session` broadcasts that to the running
/// login session. Producer before consumer.
///
/// Only cfgd's names are ordered this way, and only because cfgd mints all of
/// them — a profile, module, backup or source name is a user string with no
/// meaning to order by, so those still sort by name.
const CFGD_GROUP_ORDER: &[&str] = &[MANAGERS_GROUP, "env", "session"];

/// The cfgd-owned group every [`ManagerAction`] belongs to. Named once: a
/// filter that keeps this group and a planner that mints into it must agree on
/// the spelling, and a mismatch drops the whole phase silently.
pub const MANAGERS_GROUP: &str = "managers";

/// Where a cfgd-owned group sits in [`CFGD_GROUP_ORDER`]; `0` for every other
/// owner kind, and last for a cfgd group the list does not name.
fn cfgd_group_rank(kind: &OwnerKind, name: &str) -> u8 {
    if *kind != OwnerKind::Cfgd {
        return 0;
    }
    CFGD_GROUP_ORDER
        .iter()
        .position(|group| *group == name)
        .unwrap_or(CFGD_GROUP_ORDER.len()) as u8
}

/// Who declared an action: a kind plus the name of the thing that declared it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Owner {
    pub kind: OwnerKind,
    pub name: String,
}

impl Owner {
    pub fn profile(name: impl Into<String>) -> Self {
        Owner {
            kind: OwnerKind::Profile,
            name: name.into(),
        }
    }

    pub fn cfgd(name: impl Into<String>) -> Self {
        Owner {
            kind: OwnerKind::Cfgd,
            name: name.into(),
        }
    }

    /// Whether this is the group every [`ManagerAction`] belongs to. A filter
    /// narrowing a plan to one owner's work asks this rather than matching the
    /// kind and the name itself, so no caller can keep `cfgd:env` by accident
    /// while meaning the managers.
    pub fn is_managers(&self) -> bool {
        self.kind == OwnerKind::Cfgd && self.name == MANAGERS_GROUP
    }

    pub fn module(name: impl Into<String>) -> Self {
        Owner {
            kind: OwnerKind::Module,
            name: name.into(),
        }
    }

    pub fn backup(name: impl Into<String>) -> Self {
        Owner {
            kind: OwnerKind::Backup,
            name: name.into(),
        }
    }

    pub fn source(name: impl Into<String>) -> Self {
        Owner {
            kind: OwnerKind::Source,
            name: name.into(),
        }
    }

    /// The `kind:name` string — the ONE constructor of it. Plain text; styling
    /// belongs to the renderer, never to a caller.
    pub fn token(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.name)
    }

    /// The ONE owner comparator: `Profile`(0) `Cfgd`(1) `Module`(2)
    /// `Backup`(3) `Source`(4), then — for cfgd's own closed vocabulary —
    /// [`CFGD_GROUP_ORDER`], then name. It orders **every** phase — there is
    /// no phase-scoped override and none may be added. Rule P's
    /// module-before-profile execution barrier is a scheduling rule
    /// ([`Phase::dispatch_order`]) and never touches this ordering.
    pub fn sort_key(&self) -> (u8, u8, &str) {
        (
            self.kind.rank(),
            cfgd_group_rank(&self.kind, &self.name),
            self.name.as_str(),
        )
    }

    /// Put a loose owner list in display order and drop repeats.
    ///
    /// The only way to order owners outside [`Phase::from_actions`]: display
    /// surfaces that name owners without holding a phase (the bootstrap
    /// attribution) read the same sequence as the tree they sit next to,
    /// because they read it from the same comparator.
    pub fn order(owners: &mut Vec<Owner>) {
        owners.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        owners.dedup();
    }
}

/// One owner's slice of a phase. Never empty — an owner with no actions in a
/// phase produces no group.
#[derive(Debug, Serialize)]
pub struct OwnerGroup {
    pub owner: Owner,
    pub actions: Vec<Action>,
}

/// Which owner an action belongs to, under the profile that planned it.
///
/// The single owner-assignment rule: every group in every phase is built
/// through it, so no surface can attribute the same action to two owners.
pub fn owner_of(action: &Action, profile: &Owner) -> Owner {
    match action {
        Action::Module(ma) => Owner::module(ma.module_name.clone()),
        // Env surfaces aggregate declarations from the profile *and* every
        // module, so no single user document owns them — cfgd authored the file
        // and cfgd owns it.
        Action::Env(EnvAction::RefreshLiveSession { .. }) => Owner::cfgd("session"),
        Action::Env(_) => Owner::cfgd("env"),
        // A manager is a prerequisite every owner may be waiting on; cfgd
        // provisions it, and no user document declares it.
        Action::Manager(_) => Owner::cfgd(MANAGERS_GROUP),
        _ => profile.clone(),
    }
}

/// Whether a batching action survives its batch being filtered: dropped only
/// when the filter is what emptied it. An action that arrived empty is left
/// exactly as any other filter found it, so
/// [`Phase::retain_actions`] — which retains every batch entry — stays a pure
/// action-level filter.
fn batch_survives(batched: usize, kept: usize) -> bool {
    batched == 0 || kept > 0
}

/// A phase in the reconciliation plan, as owner groups in display order.
///
/// `groups` is private and [`Phase::from_actions`] is the only constructor, so
/// a phase whose owners are out of [`Owner::sort_key`] order is unrepresentable
/// rather than merely discouraged: no caller can write a struct literal, insert
/// a group, or re-sort the vec. The mutators below only ever shrink an existing
/// ordering ([`Phase::retain_groups`], [`Phase::retain_actions`],
/// [`Phase::retain_actions_and_batches`]) or hand out an owner's action list
/// ([`Phase::groups_mut`]).
#[derive(Debug, Serialize)]
pub struct Phase {
    pub name: PhaseName,
    groups: Vec<OwnerGroup>,
}

impl Phase {
    /// Build a phase from a flat action list, grouping by each action's owner.
    ///
    /// A stable group-by: first-appearance order within an owner, owners in
    /// [`Owner::sort_key`] order — the one comparator, so a phase's tree reads
    /// profile first everywhere.
    pub fn from_actions(name: PhaseName, profile: &Owner, actions: Vec<Action>) -> Self {
        let mut groups: Vec<OwnerGroup> = Vec::new();
        for action in actions {
            let owner = owner_of(&action, profile);
            match groups.iter_mut().find(|g| g.owner == owner) {
                Some(group) => group.actions.push(action),
                None => groups.push(OwnerGroup {
                    owner,
                    actions: vec![action],
                }),
            }
        }
        groups.sort_by(|a, b| a.owner.sort_key().cmp(&b.owner.sort_key()));
        Self { name, groups }
    }

    /// The phase's owner groups, in display order.
    pub fn groups(&self) -> &[OwnerGroup] {
        &self.groups
    }

    /// Each group's owner paired with a mutable handle on its actions — the
    /// only mutable view. It cannot add, drop or reorder a group, so the
    /// [`Owner::sort_key`] order set by [`Phase::from_actions`] survives any
    /// edit. A caller that empties a group calls [`Phase::prune_empty_groups`]
    /// afterwards.
    pub fn groups_mut(&mut self) -> impl Iterator<Item = (&Owner, &mut Vec<Action>)> {
        self.groups.iter_mut().map(|g| (&g.owner, &mut g.actions))
    }

    /// Keep only the groups whose owner passes `keep`. Retaining a subset of an
    /// ordered vec preserves the order.
    pub fn retain_groups(&mut self, mut keep: impl FnMut(&Owner) -> bool) {
        self.groups.retain(|g| keep(&g.owner));
    }

    /// Keep only the actions passing `keep`, dropping any group left empty —
    /// the filter path (`--skip` / `--only` / `--no-scripts`).
    pub fn retain_actions(&mut self, keep: impl FnMut(&Action) -> bool) {
        self.retain_actions_and_batches(keep, |_, _| true, |_| true);
    }

    /// [`Phase::retain_actions`] plus per-entry retention inside the three
    /// actions that BATCH their work: `PackageAction::Install`/`Uninstall` and
    /// `ModuleActionKind::InstallPackages` (asked through `keep_package`, with
    /// `(manager, package)`), and `ModuleActionKind::DeployFiles` (asked through
    /// `keep_file`, with the deployed target). An action whose batch empties is
    /// dropped like any other filtered action.
    ///
    /// The narrow mutable capability the daemon's pending-decision prune needs:
    /// one undecided package — or one undecided file a module happens to
    /// deploy — must leave a batch its siblings still travel in. Shrinking a
    /// batch is the ONLY in-place edit it can make — no `&mut Action` is handed
    /// out, so the fields the persisted derivations read
    /// (`format_action_description`, journal `resource_id`) stay unreachable
    /// from a filter, and the [`Owner::sort_key`] group order still cannot move.
    ///
    /// A shrunk batch does change what those derivations RENDER for the action
    /// that survives: `cargo:bat,ripgrep` becomes `cargo:ripgrep`, and
    /// `module:m:files:2` becomes `module:m:files:1`. The id is a function of
    /// the batch, so a caller whose filter varies between runs (the daemon's,
    /// as decisions are made) records drift rows and journal entries under ids
    /// that move with it.
    pub fn retain_actions_and_batches(
        &mut self,
        mut keep_action: impl FnMut(&Action) -> bool,
        mut keep_package: impl FnMut(&str, &str) -> bool,
        mut keep_file: impl FnMut(&std::path::Path) -> bool,
    ) {
        for group in &mut self.groups {
            group.actions.retain_mut(|action| {
                if !keep_action(action) {
                    return false;
                }
                match action {
                    Action::Package(
                        PackageAction::Install {
                            manager, packages, ..
                        }
                        | PackageAction::Uninstall {
                            manager, packages, ..
                        },
                    ) => {
                        let batched = packages.len();
                        packages.retain(|package| keep_package(manager, package));
                        batch_survives(batched, packages.len())
                    }
                    Action::Module(ModuleAction {
                        kind: ModuleActionKind::InstallPackages { resolved },
                        ..
                    }) => {
                        let batched = resolved.len();
                        resolved.retain(|pkg| keep_package(&pkg.manager, &pkg.resolved_name));
                        batch_survives(batched, resolved.len())
                    }
                    Action::Module(ModuleAction {
                        kind: ModuleActionKind::DeployFiles { files },
                        ..
                    }) => {
                        let batched = files.len();
                        files.retain(|file| keep_file(&file.target));
                        batch_survives(batched, files.len())
                    }
                    _ => true,
                }
            });
        }
        self.prune_empty_groups();
    }

    /// Drop groups an in-place edit emptied. An `OwnerGroup` is never empty at
    /// construction, so an empty one is always the residue of a filter and must
    /// not reach display or `-o json`.
    pub fn prune_empty_groups(&mut self) {
        self.groups.retain(|g| !g.actions.is_empty());
    }

    /// Every action in the phase, in the plan's own (display) order. What
    /// filters, counts and payloads see.
    ///
    /// Deliberately not the dispatch order — see [`Phase::dispatch_order`].
    pub fn actions(&self) -> impl Iterator<Item = &Action> {
        self.groups.iter().flat_map(|g| g.actions.iter())
    }

    /// Every action paired with its group's owner, in the plan's own order.
    pub fn owned_actions(&self) -> impl Iterator<Item = (&Owner, &Action)> {
        self.groups
            .iter()
            .flat_map(|g| g.actions.iter().map(move |a| (&g.owner, a)))
    }

    /// Dispatch order — the ONE thing `Reconciler::apply` walks. Identical to
    /// [`Phase::owned_actions`] in every phase except `Packages`, where it is
    /// Rule P's two-tier partition: module-owned groups (tier 0), then the
    /// rest (tier 1). Separate from `actions()` because `actions()` is the
    /// plan's own order and is what filters, counts and payloads must keep
    /// seeing.
    pub fn dispatch_order(&self) -> impl Iterator<Item = (&Owner, &Action)> {
        let mut ordered: Vec<(&Owner, &Action)> = Vec::with_capacity(self.action_count());
        if self.name == PhaseName::Packages {
            for tier in Tier::ALL {
                ordered.extend(self.owned_actions().filter(|(o, _)| Tier::of(o) == tier));
            }
        } else {
            ordered.extend(self.owned_actions());
        }
        ordered.into_iter()
    }

    pub fn action_count(&self) -> usize {
        self.groups.iter().map(|g| g.actions.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|g| g.actions.is_empty())
    }
}

/// The `Packages` phase's two dispatch tiers, in the order they are released.
///
/// Two surfaces read this and they must not disagree: the dispatch order
/// ([`Phase::dispatch_order`]) partitions the phase by it, and the dispatcher
/// releases a tier only once the tier above it has *completed*. Manager
/// provisioning is a `Prerequisites`-phase [`ManagerAction`] node now, ahead
/// of the whole `Packages` phase, so nothing in this phase blocks on a
/// same-phase bootstrap any more — module work still runs first because a
/// profile install may consume a package a module just installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Module-owned package work.
    Modules,
    /// Everything else — in practice the profile's own package work.
    Rest,
}

impl Tier {
    /// The tiers in release order.
    pub const ALL: [Tier; 2] = [Tier::Modules, Tier::Rest];

    /// The ONE tier derivation.
    pub fn of(owner: &Owner) -> Self {
        if owner.kind == OwnerKind::Module {
            Tier::Modules
        } else {
            Tier::Rest
        }
    }

    /// What a group blocked behind this tier says it is waiting on.
    ///
    /// `Rest` is last, so nothing is ever blocked behind it and it can never
    /// name itself.
    pub fn wait_word(self) -> Option<&'static str> {
        match self {
            Tier::Modules => Some("modules"),
            Tier::Rest => None,
        }
    }
}

/// A complete reconciliation plan.
#[derive(Debug, Serialize)]
pub struct Plan {
    pub phases: Vec<Phase>,
    /// Run-level warnings the header renders and the `-o json` payload
    /// carries: shell rc conflicts (env/alias defined before the cfgd source
    /// line) and source batches withheld without a row
    /// (`UndecidableBatch::warning`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn total_actions(&self) -> usize {
        self.phases.iter().map(|p| p.action_count()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.phases.iter().all(|p| p.is_empty())
    }

    /// Serialize the plan to a stable string for hashing.
    ///
    /// Stable across group/phase restructuring: actions are serialized, then
    /// SORTED, so the hash identifies the SET of planned actions rather than
    /// the order a particular version happened to walk them in. Uses serde_json
    /// serialization instead of Debug formatting for stability across compiler
    /// versions.
    pub fn to_hash_string(&self) -> String {
        let mut parts: Vec<String> = self
            .phases
            .iter()
            .flat_map(|p| p.actions())
            .filter_map(|a| serde_json::to_string(a).ok())
            .collect();
        parts.sort_unstable();
        parts.join("|")
    }
}

/// Result of applying a single action.
#[derive(Debug, Serialize)]
pub struct ActionResult {
    pub phase: String,
    pub description: String,
    pub success: bool,
    pub error: Option<String>,
    pub changed: bool,
}

/// Result of an entire apply operation.
#[derive(Debug, Serialize)]
pub struct ApplyResult {
    pub action_results: Vec<ActionResult>,
    pub status: ApplyStatus,
    /// The apply_id in the state store — used for rollback.
    pub apply_id: i64,
    /// The intended process exit code when the apply was cooperatively aborted
    /// by a signal (`130` SIGINT / `143` SIGTERM), else `None`. Drives the
    /// CLI's signal-conventional exit; `status == Aborted` whenever this is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aborted: Option<u8>,
    /// Number of actions this run intended to execute under the active phase
    /// filter (`--phase`/`--skip`/`--only`/`--skip-scripts`). Equals the global
    /// plan size when unfiltered. Lets an aborted run honestly report
    /// "{applied} of {planned_total}" rather than counting phases that were
    /// never in scope.
    pub planned_total: usize,
}

/// Result of a rollback operation.
#[derive(Debug, Serialize)]
pub struct RollbackResult {
    pub files_restored: usize,
    pub files_removed: usize,
    /// Non-file actions that were not rolled back (require manual review),
    /// as (action_type, resource_id) pairs. `resource_id` for a "script"
    /// entry is the raw journal-recorded run_str body — kept alongside its
    /// type so a display site (`cli/rollback.rs`) can condense it without
    /// mistaking an unrelated resource_id (e.g. a package name) for one.
    pub non_file_actions: Vec<(String, String)>,
}

impl ApplyResult {
    pub fn succeeded(&self) -> usize {
        self.action_results.iter().filter(|r| r.success).count()
    }

    pub fn failed(&self) -> usize {
        self.action_results.iter().filter(|r| !r.success).count()
    }
}

/// The `(resource_type, resource_id)` pair a planned action is recorded under.
///
/// The ONE derivation of a persisted action identity: drift rows, journal
/// entries and the pending-decision match in
/// [`DecisionExclusions`](super::DecisionExclusions) all read it, so a resource
/// cannot be recorded under one id and matched under another. It is a
/// state-matching key, never a display string — nothing here is condensed or
/// re-shaped for a terminal.
pub(crate) fn action_resource_info(action: &Action) -> (String, String) {
    match action {
        Action::File(fa) => match fa {
            FileAction::Create { target, .. } => ("file".to_string(), to_posix_string(target)),
            FileAction::Update { target, .. } => ("file".to_string(), to_posix_string(target)),
            FileAction::Delete { target, .. } => ("file".to_string(), to_posix_string(target)),
            FileAction::SetPermissions { target, .. } => {
                ("file".to_string(), to_posix_string(target))
            }
            FileAction::Skip { target, .. } => ("file".to_string(), to_posix_string(target)),
        },
        Action::Package(pa) => match pa {
            PackageAction::Install {
                manager, packages, ..
            } => (
                "package".to_string(),
                format!("{}:{}", manager, packages.join(",")),
            ),
            PackageAction::Uninstall {
                manager, packages, ..
            } => (
                "package".to_string(),
                format!("{}:{}", manager, packages.join(",")),
            ),
            PackageAction::Skip { manager, .. } => ("package".to_string(), manager.clone()),
        },
        Action::Secret(sa) => match sa {
            SecretAction::Decrypt { target, .. } => ("secret".to_string(), to_posix_string(target)),
            SecretAction::Resolve { reference, .. } => ("secret".to_string(), reference.clone()),
            SecretAction::ResolveEnv { envs, .. } => {
                ("secret".to_string(), format!("env:[{}]", envs.join(",")))
            }
            SecretAction::Skip { source, .. } => ("secret".to_string(), source.clone()),
        },
        Action::System(sa) => match sa {
            SystemAction::SetValue {
                configurator, key, ..
            } => ("system".to_string(), format!("{}:{}", configurator, key)),
            SystemAction::Skip { configurator, .. } => ("system".to_string(), configurator.clone()),
        },
        Action::Script(sa) => {
            match sa {
                // Resource-id / state-matching key, NOT a display string:
                // stored as `resource_id` in `drift_events` and matched by
                // exact string on every tick (`UPDATE ... WHERE
                // resource_id = ?`). Condensing `run_str()` here would
                // reshape the id and re-open every already-recorded drift row
                // for a module with a multi-line inline script. Display-side
                // condensing for "script" rows happens where a status
                // subject or table cell is actually built (`cli/status.rs`).
                ScriptAction::Run { entry, .. } => {
                    ("script".to_string(), entry.run_str().to_string())
                }
            }
        }
        Action::Module(ma) => ("module".to_string(), ma.module_name.clone()),
        Action::Env(ea) => {
            use crate::reconciler::EnvAction;
            match ea {
                EnvAction::WriteEnvFile { path, .. } => ("env".to_string(), to_posix_string(path)),
                EnvAction::InjectSourceLine { rc_path, .. } => {
                    ("env-rc".to_string(), to_posix_string(rc_path))
                }
                EnvAction::RefreshLiveSession { .. } => {
                    ("env-session".to_string(), "live-session".to_string())
                }
            }
        }
        Action::Manager(ma) => (MANAGER_RESOURCE_TYPE.to_string(), ma.resource_id()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_name_from_str_round_trips() {
        assert_eq!(
            "env".parse::<PhaseName>().unwrap(),
            PhaseName::Prerequisites
        );
        assert_eq!("files".parse::<PhaseName>().unwrap(), PhaseName::Files);
        assert_eq!(
            "packages".parse::<PhaseName>().unwrap(),
            PhaseName::Packages
        );
        assert_eq!("system".parse::<PhaseName>().unwrap(), PhaseName::System);
        assert_eq!("secrets".parse::<PhaseName>().unwrap(), PhaseName::Secrets);
        assert_eq!(
            "pre-scripts".parse::<PhaseName>().unwrap(),
            PhaseName::PreScripts
        );
        assert_eq!(
            "post-scripts".parse::<PhaseName>().unwrap(),
            PhaseName::PostScripts
        );
        assert_eq!("modules".parse::<PhaseName>().unwrap(), PhaseName::Modules);
        assert!("bogus".parse::<PhaseName>().is_err());
    }

    #[test]
    fn script_phase_display_names() {
        assert_eq!(ScriptPhase::PreApply.display_name(), "preApply");
        assert_eq!(ScriptPhase::PostApply.display_name(), "postApply");
        assert_eq!(ScriptPhase::OnDrift.display_name(), "onDrift");
        assert_eq!(ScriptPhase::OnChange.display_name(), "onChange");
    }

    #[test]
    fn owner_token_covers_every_kind() {
        assert_eq!(Owner::profile("work").token(), "profile:work");
        assert_eq!(Owner::cfgd(MANAGERS_GROUP).token(), "cfgd:managers");
        assert_eq!(Owner::module("nvim").token(), "module:nvim");
        assert_eq!(Owner::backup("dotfiles").token(), "backup:dotfiles");
        assert_eq!(Owner::source("team").token(), "source:team");
    }

    #[test]
    fn owner_kind_as_str_from_token_round_trips() {
        for kind in [
            OwnerKind::Profile,
            OwnerKind::Cfgd,
            OwnerKind::Module,
            OwnerKind::Backup,
            OwnerKind::Source,
        ] {
            assert_eq!(OwnerKind::from_token(kind.as_str()), Some(kind.clone()));
        }
        assert_eq!(OwnerKind::from_token("packages"), None);
        assert_eq!(OwnerKind::from_token("files"), None);
    }

    #[test]
    fn owner_kind_serializes_as_its_token() {
        // The `-o json` payload carries `{"kind": "module", …}` beside the
        // rendered `token`, so the serde form and `as_str` must never diverge.
        for kind in [
            OwnerKind::Profile,
            OwnerKind::Cfgd,
            OwnerKind::Module,
            OwnerKind::Backup,
            OwnerKind::Source,
        ] {
            assert_eq!(
                serde_json::to_string(&kind).expect("OwnerKind serializes"),
                format!("\"{}\"", kind.as_str())
            );
        }
    }

    #[test]
    fn owner_sort_key_ranks_profile_first_then_cfgd_module_backup_source() {
        let mut owners = [
            Owner::source("team"),
            Owner::module("nvim"),
            Owner::backup("dotfiles"),
            Owner::cfgd(MANAGERS_GROUP),
            Owner::profile("work"),
        ];
        owners.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        let tokens: Vec<String> = owners.iter().map(Owner::token).collect();
        assert_eq!(
            tokens,
            vec![
                "profile:work",
                "cfgd:managers",
                "module:nvim",
                "backup:dotfiles",
                "source:team",
            ]
        );
    }

    #[test]
    fn owner_sort_key_breaks_rank_ties_by_name() {
        assert!(Owner::module("apt").sort_key() < Owner::module("brew").sort_key());
        assert!(Owner::cfgd("env").sort_key() < Owner::cfgd("session").sort_key());
    }
}

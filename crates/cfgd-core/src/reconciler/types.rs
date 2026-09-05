use std::str::FromStr;

use serde::Serialize;

use crate::config::ScriptEntry;
use crate::providers::{ActionNote, FileAction, PackageAction, SecretAction};
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
    ///
    /// A label rather than a string, because the heading is two theme slots:
    /// handing back the plain title let every call site open it as an ordinary
    /// section, and an ordinary section paints its whole header in one slot.
    pub fn section_label(&self) -> crate::output::PhaseLabel {
        crate::output::PhaseLabel::new(self.display_name())
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
        /// How many variables and aliases `content` renders, for the action
        /// line's own detail — a write that names a path says nothing about
        /// what landed in it. Counted from THIS file's own rendering, never
        /// from the run's merged totals: an `environment.d` or launchd
        /// surface holds no aliases at all, and a run's alias count quoted on
        /// its write line would describe a different file.
        ///
        /// Display-only, and `#[serde(skip)]` for that reason: the plan hash is
        /// a serialization of the actions, so a counted field that reached it
        /// would rewrite every stored `plan_hash` for a value nothing matches
        /// on.
        #[serde(skip)]
        vars: usize,
        #[serde(skip)]
        aliases: usize,
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
        /// The module's OWN declared route to this tool, when a resolved
        /// `spec.packages` entry names the same canonical tool as the manager
        /// being provisioned: the manager that entry's `prefer` chain picked
        /// and the name its `aliases` gave it there.
        ///
        /// `Some` means the provision installs THAT, not the manager's default
        /// cascade. cfgd provisioning `pipx` by its own route (apt) while a
        /// module declares `pipx` with `prefer: [brew, apt]` puts two pipx on
        /// the machine and leaves `PATH` order to decide which one every later
        /// command means; `package_survives_elision` cannot catch it, being
        /// asked against ONE manager's listing, and brew's listing does not
        /// know about apt's pipx. The module's statement is the more specific
        /// one, so it decides — and, unlike a cascade, it also carries the
        /// entry's `minVersion` floor, which `modules::resolve_package`
        /// already refused every candidate below.
        ///
        /// A declared route is never batched: the batch is one mediator
        /// command over `mediated_packages`, and those are the manager's own
        /// names, not the alias the module wrote.
        #[serde(skip_serializing_if = "Option::is_none")]
        declared: Option<DeclaredProvision>,
        /// The other managers this node's ONE `via` command provisions
        /// alongside `manager`, in provision order and never naming `manager`
        /// itself.
        ///
        /// Non-empty only when every member is delivered by an ordinary `via`
        /// package install (`PackageManager::mediated_packages`), so npm and
        /// pipx both coming from apt are one line and one `apt-get install`
        /// rather than two of each. A batched member has no node of its own,
        /// so the planner never batches a manager some other node's edge
        /// names — `manager` alone keeps this node's identity, and the id,
        /// the DAG edges and the `--skip`/`--only`/`--phase` subject are
        /// exactly what they were before it acquired company.
        batched: Vec<String>,
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

/// The route a module declared to a tool cfgd also needs as a MANAGER.
///
/// Built by the planner from the module's already-resolved `spec.packages`
/// entry, so the `prefer` chain and the `aliases` map are read exactly once,
/// by the code that owns them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclaredProvision {
    /// The registered manager the entry's `prefer` chain resolved to.
    pub installer: String,
    /// The package name that manager installs it under — the entry's alias for
    /// this manager, which is why `cargo` under apt reads `rustc`.
    pub package: String,
}

/// The `resource_type` half of a scaffolding [`ManagerAction`]'s persisted
/// identity (an index refresh, a prerequisite install). Also the one value
/// `record_managed_resources` refuses to write a row for: cfgd's own
/// scaffolding is journalled, never managed. A provision/refusal drift row is
/// typed `package` instead — see [`action_resource_info`]'s Manager arm.
pub(super) const MANAGER_RESOURCE_TYPE: &str = "manager";

/// The `resource_type` every surface cfgd authors on its OWN behalf is
/// recorded under — the generated env file, the rc source line, the
/// live-session publish. Named beside its sibling above because two crates
/// match on it: the apply that resolves an env row's per-item drift, and
/// `cfgd status`'s Managed Resources table, which reads it to give those rows
/// the `cfgd:` owner the plan and apply trees head them with.
pub const ENV_RESOURCE_TYPE: &str = "env";

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

    /// The persisted `resource_id` a provision of `manager` carries.
    ///
    /// For a caller holding one member of a BATCH rather than the node that
    /// speaks for it: a drift row names the manager that is missing, and the
    /// batch's own [`ManagerAction::resource_id`] names only its leader. The
    /// string is the same one the member's solo node would have minted, so a
    /// journal row written under a batch and a drift row read back out still
    /// meet.
    pub fn provision_resource_id(manager: &str) -> String {
        provision_id(manager)
    }

    /// The recorded id of the refusal to provision `manager` — the twin of
    /// [`ManagerAction::provision_resource_id`], for a caller settling the
    /// `("package", "refuse:<manager>")` drift row without holding a
    /// [`ManagerAction::Refuse`] to ask.
    pub fn refuse_resource_id(manager: &str) -> String {
        refuse_id(manager)
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

    /// What a `--phase`/`--skip`/`--only` selector names this node by — the
    /// manager for every variant but a prerequisite, which is keyed on its
    /// TOOL. Deliberately distinct from [`ManagerAction::manager`]: that
    /// accessor answers "which command runs this", the question a lane/prune
    /// needs, while this one answers "what does the user see in the tree"
    /// (`manager:prereq:curl` — the subject is the tool, not brew's name
    /// merely because brew happens to be the installer). Both this crate's
    /// `action_matches_phase_filter` (`--phase prerequisites.curl`) and the
    /// `cfgd` binary's `action_path` (`--skip prerequisites.curl`) key on this
    /// so the two matchers can never disagree about which node a selector
    /// reaches.
    pub fn filter_subject(&self) -> &str {
        match self {
            ManagerAction::RefreshIndex { manager }
            | ManagerAction::Provision { manager, .. }
            | ManagerAction::Refuse { manager, .. } => manager,
            ManagerAction::Prerequisite { tool, .. } => tool,
        }
    }

    /// Every manager this node provisions, in display order: `manager` first,
    /// then its batch. Empty for every variant that provisions nothing.
    ///
    /// The ONE enumeration of a batch's membership — the line, the executor's
    /// install, the `--skip`/`--only` split and the stranded-install check all
    /// read it, so none of them can disagree about who a node speaks for.
    pub fn provisioned_managers(&self) -> Vec<&str> {
        match self {
            ManagerAction::Provision {
                manager, batched, ..
            } => std::iter::once(manager.as_str())
                .chain(batched.iter().map(String::as_str))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Every manager this node FAILING leaves unusable for the rest of the run.
    ///
    /// A provision speaks for its whole batch, and a refusal speaks for the one
    /// manager it refuses. A prerequisite install and an index refresh name
    /// nothing: `apt install curl` failing says nothing about apt, and a stale
    /// index is not a missing binary.
    pub fn managers_left_unavailable(&self) -> Vec<&str> {
        match self {
            ManagerAction::Provision { .. } => self.provisioned_managers(),
            ManagerAction::Refuse { manager, .. } => vec![manager.as_str()],
            ManagerAction::Prerequisite { .. } | ManagerAction::RefreshIndex { .. } => Vec::new(),
        }
    }

    /// Whether a `--skip`/`--only`/`--phase` selector naming `subject` reaches
    /// this node — its own [`ManagerAction::filter_subject`], or any manager
    /// batched onto it.
    pub fn selector_names(&self, subject: &str) -> bool {
        if self.filter_subject() == subject {
            return true;
        }
        matches!(self, ManagerAction::Provision { batched, .. } if batched.iter().any(|m| m == subject))
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

impl Action {
    /// Why this action cannot run on THIS host, answered from the machine while
    /// the plan is still being read.
    ///
    /// A plan listing an action it is certain to skip must say so where it lists
    /// it, and must not price it into `N actions planned` — the apply reports
    /// the same skip, and a plan promising one more action than the apply
    /// performs is a shortfall the reader has no way to explain. The ONE seam:
    /// a second gate of this shape extends the match rather than filtering at
    /// its own call site, so the count and the render cannot disagree.
    pub fn pre_skip_reason(&self) -> Option<&'static str> {
        match self {
            Action::Env(EnvAction::RefreshLiveSession { .. })
                if !crate::session_manager_available() =>
            {
                Some(super::format::debug_checked_pre_skip_reason(
                    self,
                    crate::NO_SESSION_MANAGER,
                ))
            }
            _ => None,
        }
    }
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
        /// The files this run will actually write — the declared set minus the
        /// entries whose deployed target already matches.
        files: Vec<crate::modules::ResolvedFile>,
        /// How many files the module DECLARES, converged entries included.
        /// The pair is what lets a row's detail say `5 already deployed`
        /// and the persisted `module:<name>:files:<n>` id keep naming the
        /// declared set whatever subset survived elision.
        declared_total: usize,
    },
    /// Run a module lifecycle script.
    RunScript {
        script: ScriptEntry,
        phase: ScriptPhase,
    },
    /// The HOST declined this module whole — a platform gate answered before
    /// any of its work was priced. Nothing under it was probed, so it is
    /// counted nowhere: the header's `Modules` row states the skip and its
    /// reason once, neither tree draws a row, and no apply dispatches it.
    Skip { reason: String },
    /// This module's FILE work is refused — an encryption demand its strategy
    /// cannot honour, a source that is not encrypted, an encryption check that
    /// could not run. The module itself is fine and its other phases proceed;
    /// only the deploy is withheld.
    ///
    /// Unlike [`ModuleActionKind::Skip`] this is a finding the reader must act
    /// on, so it is an ACTION ROW everywhere: the header counts it, both trees
    /// draw it at [`crate::output::Role::Warn`], and the apply settles it as a
    /// skip carrying `reason` as its detail. It manages no resource and mints
    /// no drift row — cfgd refused to touch the files rather than finding them
    /// diverged.
    FilesRefused { reason: String },
}

/// The facet a refused file deploy's tracking id carries
/// ([`ModuleActionKind::FilesRefused`]), the sibling of `skip`'s.
///
/// Both name a module row that stands for NO resource on the machine, which is
/// why `record_managed_resources` writes neither and no live check re-mints
/// one.
pub const MODULE_FACET_FILES_REFUSED: &str = "files-refused";

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
    /// `<phase>.<selector>` — one cfgd-owned group (`managers`/`env`/`session`)
    /// or one manager, scoped to `PhaseName` (`prerequisites.managers`,
    /// `prerequisites.brew`). Resolved by [`crate::reconciler::action_matches_phase_filter`];
    /// `ModuleOwners` never carries a selector because it already spans every
    /// phase module work can land in, so nothing single-phase to scope it to.
    Selector(PhaseName, String),
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
    ///
    /// The generated env file's PATH line carries a `# manager:brew,cargo`
    /// comment, which uses a wider vocabulary than this enum on purpose:
    /// the name half is a comma list no `Owner` name may hold, so
    /// `from_token("manager")` stays `None` rather than minting an owner
    /// that names two things at once. See `EnvOrigins` in `env_engine.rs`.
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
///
/// `pub`, not `pub(super)`: the CLI's `--phase`/`--skip`/`--only` dotted
/// grammar (`prerequisites.managers`/`.env`/`.session`) and its selector
/// validation both read this list rather than minting their own copy of it —
/// two copies is how the group vocabulary drifted between `--phase` (via
/// `reconciler::action_matches_phase_filter`) and `--skip`/`--only` (via
/// `cfgd::cli::plan_ops::pattern_matches_action`) before it was unified here.
pub const CFGD_GROUP_ORDER: &[&str] = &[MANAGERS_GROUP, ENV_GROUP, SESSION_GROUP];

/// The cfgd-owned group every [`ManagerAction`] belongs to. Named once: a
/// filter that keeps this group and a planner that mints into it must agree on
/// the spelling, and a mismatch drops the whole phase silently.
pub const MANAGERS_GROUP: &str = "managers";

/// The cfgd-owned group every [`EnvAction`] but the live-session broadcast
/// belongs to. Named once for the same reason as [`MANAGERS_GROUP`]: the
/// assignment rule below and [`CFGD_GROUP_ORDER`] above spelled it twice, and
/// two spellings of a group name is how a filter and a planner stop agreeing.
pub const ENV_GROUP: &str = "env";

/// The cfgd-owned group the live-session broadcast belongs to; the sibling of
/// [`ENV_GROUP`], named for the same reason.
pub const SESSION_GROUP: &str = "session";

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

    /// This owner as the renderer's own tri-colour token — the ONE composition
    /// of one, so a heading, a table cell and a serialized token cannot come
    /// out as three spellings of the same owner.
    pub fn label(&self) -> crate::output::OwnerLabel {
        crate::output::OwnerLabel::new(self.kind.as_str(), self.name.as_str())
    }

    /// The `kind:name` string — the ONE constructor of it, and the uncoloured
    /// half of [`Owner::label`]. Plain text; styling belongs to the renderer,
    /// never to a caller.
    pub fn token(&self) -> String {
        self.label().plain()
    }

    /// What joins several owner tokens into one string, wherever a slot holds a
    /// list of them: the recorded scope of a `--module` run, which `-o json`
    /// carries and `cli::status::derivable_profile` reads back. Beside
    /// [`Owner::token`] because the writer and the reader of that column both
    /// have to agree with it — spelled by hand at either end, a one-byte change
    /// stops the reader parsing what the writer wrote.
    pub const TOKEN_SEPARATOR: &'static str = ", ";

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

    /// Whether this owner's group renders above `other`'s.
    ///
    /// The comparison itself, so an invariant about display order is ASKED of
    /// the comparator rather than re-derived from its key: a phase whose lane
    /// half is written as a tree while its serial half streams below reads in
    /// group order only while every lane owner is above every serial one, and
    /// a second `sort_key()` comparison spelled out at that check is how two
    /// orderings begin.
    pub fn renders_above(&self, other: &Owner) -> bool {
        self.sort_key() < other.sort_key()
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
        Action::Env(EnvAction::RefreshLiveSession { .. }) => Owner::cfgd(SESSION_GROUP),
        Action::Env(_) => Owner::cfgd(ENV_GROUP),
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
                        kind:
                            ModuleActionKind::DeployFiles {
                                files,
                                declared_total,
                            },
                        ..
                    }) => {
                        let batched = files.len();
                        files.retain(|file| keep_file(&file.target));
                        // A withheld entry leaves the declared set with it, or
                        // the survivor count renders as `k of N files` — a
                        // shape that reads as the other N−k having CONVERGED
                        // when they were pruned by a pending decision.
                        // Saturating: a `declared_total` that somehow undercounts
                        // its own batch must clamp at zero, not unwind mid-filter.
                        *declared_total = declared_total.saturating_sub(batched - files.len());
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

    /// The actions this phase intends to EXECUTE — a pre-skipped action is
    /// listed by the tree but never counted, so the plan's promise and the
    /// apply's tally are one number.
    pub fn action_count(&self) -> usize {
        attempted_count(self.groups.iter().flat_map(|g| g.actions.iter()))
    }

    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|g| g.actions.is_empty())
    }
}

/// How many of `actions` an apply will ATTEMPT.
///
/// Two shapes are excluded. An action [`Action::pre_skip_reason`] answers for is
/// still LISTED — its row says why it cannot run here — and is never counted. A
/// module skipped whole ([`module_skipped_whole`]) is not listed either: the header's
/// `Modules` row states the skip and its reason once, both trees omit the phase
/// holding it, and the executor never dispatches it, so counting it would
/// promise a row no reader can see.
///
/// This is the ONE spelling of that filter: [`Phase::action_count`] (and through
/// it [`Plan::total_actions`]) counts a whole plan with it, and a caller holding
/// a scoped subtree counts the same subset the same way, so a plan's footer, its
/// `-o json` total and the apply's tally cannot price one plan three ways.
pub fn attempted_count<'a>(actions: impl IntoIterator<Item = &'a Action>) -> usize {
    actions
        .into_iter()
        .filter(|a| a.pre_skip_reason().is_none() && !module_skipped_whole(a))
        .count()
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
    /// The actions this run promises to attempt, and the number its apply
    /// tallies against.
    ///
    /// Neither a pre-skipped action nor a module skipped whole is one, for the
    /// two different reasons [`attempted_count`] states. The `-o json` payload
    /// still carries every action under its phase, kind and reason included:
    /// only the COUNTS drop them.
    pub fn total_actions(&self) -> usize {
        self.phases.iter().map(|p| p.action_count()).sum()
    }

    /// Every action the plan's tree prints a ROW for: every phase but
    /// [`PhaseName::Modules`], pre-skipped actions included.
    ///
    /// The coverage is [`super::PhaseCoverage::Rendered`]'s — a module skipped
    /// whole is annotated by the header's `Modules` row rather than drawn as a
    /// block, so counting it here would name a row no reader can see.
    /// [`total_actions`](Self::total_actions) does not answer this either: it
    /// drops what the host already declined, and the tree prints it. A sentence
    /// closing a run whose BODY is that tree counts against this.
    pub fn listed_action_count(&self) -> usize {
        self.phases
            .iter()
            .filter(|p| p.name != PhaseName::Modules)
            .flat_map(|p| p.actions())
            .count()
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
    /// The action reached its own line and that line settled `Role::Skipped` —
    /// a declared skip, a converged no-op, an environment that had nowhere to
    /// publish to. Carried on the record rather than re-derived, so the closing
    /// tally, the stored summary and the glyph the reader saw are one verdict:
    /// counting a skip as a success is what let `13 actions succeeded` stand
    /// over a tree showing twelve ✓ and one —.
    pub skipped: bool,
    /// Why this action was never going to run here — the answer
    /// [`Action::pre_skip_reason`] gave while the plan was read — so the tally
    /// prices it the way the header did. The row still renders with the same
    /// reason; the counted rollup names it only in its parenthetical. `None`
    /// for every action the run attempted, whatever became of it.
    pub not_attempted: Option<String>,
    /// How many of the entries this action NAMED it actually put on the
    /// machine, when the executed set was narrower than the planned one.
    /// `description` keeps the planned set — it is the wire contract and the
    /// `managed_resources` id — so a consumer differencing the two reads
    /// exactly what the row's `N of M packages` detail states. `None` whenever
    /// the action installed everything it named, or installs nothing at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<usize>,
    /// The version each manager a provision LANDED reports, keyed by
    /// manager — what the row's detail states (`— 4.6.3`). Empty, and
    /// absent from the wire, for every other action.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub versions: std::collections::BTreeMap<String, String>,
    /// The `drift_events` keys a successful settle of this action HEALS —
    /// [`action_drift_rows`]' own output, carried off the action rather than
    /// re-derived from `description`, so the row a daemon tick recorded and the
    /// row this apply resolves are one set by construction. Empty for the four
    /// `Skip` variants ([`apply_heals_action_rows`]) and for an action the plan
    /// withheld.
    ///
    /// Not serialized: a heal key is bookkeeping, never part of the apply's
    /// `-o json` shape.
    #[serde(skip)]
    pub drift_rows: Vec<(String, String)>,
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
    /// Provider narration collected during the run, grouped by the owner
    /// (`kind:name`) that produced it — a package-manager caveat, a
    /// system-configurator warning. Rendered once as the run's closing
    /// `Caveats` section (see `render_caveats`) instead of inline under each
    /// action. Never serialized: a caveat is a display artifact, not part of
    /// the apply's persisted or `-o json` shape.
    #[serde(skip)]
    pub caveats: Vec<(Owner, Vec<ActionNote>)>,
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
    /// Actions that ran and did something. A skipped action is NOT one of
    /// these — it settled a skip dash on screen, and a count claiming it as a
    /// success contradicts the line the reader kept.
    pub fn succeeded(&self) -> usize {
        self.action_results
            .iter()
            .filter(|r| r.success && !r.skipped && r.not_attempted.is_none())
            .count()
    }

    /// Actions that ran and settled a skip. A pre-skipped action is NOT one of
    /// these either: it never reached its line, and the header priced it out
    /// through the same predicate that names it in [`Self::not_attempted`].
    pub fn skipped(&self) -> usize {
        self.action_results
            .iter()
            .filter(|r| r.success && r.skipped && r.not_attempted.is_none())
            .count()
    }

    /// The reason of every action [`Action::pre_skip_reason`] withheld, one
    /// entry per action, in result order.
    pub fn not_attempted(&self) -> Vec<String> {
        self.action_results
            .iter()
            .filter_map(|r| r.not_attempted.clone())
            .collect()
    }

    pub fn failed(&self) -> usize {
        self.action_results.iter().filter(|r| !r.success).count()
    }
}

/// The ONE grammar for a `"package"` drift row's `resource_id`, workspace-wide:
/// `<manager>:<package>`, always exactly one package. `:` rather than the
/// tracking table's `/` because a scoped npm name (`@org/name`) legitimately
/// carries a `/`, which `:` cannot.
///
/// The slice signature is what the one-package rule is stated in: every caller
/// passes a single-element slice, and the assertion below refuses a second
/// entry. A batch id (`<mgr>:<a>,<b>`) named a unit no per-package re-check
/// could ever match, so a row keyed on one could only be resolved by another
/// batch of exactly the same members — it outlived its own membership and no
/// CLI verb could settle it. [`action_drift_rows`] is now the only producer on
/// the action side and mints one row per package; the CLI live checks, the
/// floor pass and the apply's healed keys all mint here too, so a healed key
/// and the row it heals cannot spell the same package two ways.
///
/// The package half is the manager's own
/// [`package_identity`](crate::providers::PackageManager::package_identity) of
/// the declared entry — see [`package_entry_drift_id`], which every producer
/// holding a manager reaches instead of this.
pub fn package_drift_resource_id(manager: &str, packages: &[String]) -> String {
    // The keep-set split between the live grammar and the daemon's bare
    // `PackageAction::Skip` spelling rests on every minted id carrying its `:`
    // with a real manager in front — a bare id would read as that Skip
    // spelling and stand forever instead of healing.
    debug_assert!(
        !manager.is_empty() && packages.iter().all(|p| !p.is_empty()),
        "a package drift id needs a manager and real package names: {manager:?} / {packages:?}"
    );
    debug_assert!(
        packages.len() == 1,
        "a package drift row names exactly one package: {manager:?} / {packages:?}"
    );
    format!("{}:{}", manager, packages.join(","))
}

/// The `package` drift row id for ONE declared entry, folded through the
/// manager's own
/// [`package_identity`](crate::providers::PackageManager::package_identity).
///
/// The entry a module declares and the name its manager lists are not always
/// the same string — `go` installs `rsc.io/2fa` and lists `2fa`, `choco`
/// lowercases, FreeBSD `pkg` strips a trailing `-1.2.0,1` — and the identity is
/// what every presence check compares against and what the tracking row is
/// keyed on. Minting the row from the raw entry instead left a finding recorded
/// under one string and answered under another, and the FreeBSD form smuggled a
/// comma into an id the keep predicates read as a legacy batch.
///
/// `pm` is `None` only where no manager is registered under that name; the raw
/// entry is then the best identity available and the row is as answerable as
/// the manager is.
#[must_use]
pub fn package_entry_drift_id(
    manager: &str,
    entry: &str,
    pm: Option<&dyn crate::providers::PackageManager>,
) -> String {
    let identity = pm.map_or_else(|| entry.to_string(), |m| m.package_identity(entry));
    package_drift_resource_id(manager, std::slice::from_ref(&identity))
}

/// The `<manager>:<a>,<b>` identity of a BATCHING package action.
///
/// Never a drift row and never recorded as one — [`action_drift_rows`] mints
/// one row per package, because a batch key names a unit no per-package
/// re-check can match, and a row under one outlived its own membership. This is
/// the ACTION's identity, for a caller comparing one plan against another.
// composed-id-ok: the batch spelling is this function's whole subject, and it
// is deliberately not `package_drift_resource_id`, which mints drift rows.
fn package_batch_action_id(manager: &str, packages: &[String]) -> String {
    format!("{}:{}", manager, packages.join(","))
}

/// The inverse of [`package_drift_resource_id`], kept beside its producer so
/// the two spellings cannot drift: `<manager>:<a>,<b>` back into the manager
/// and its packages. `None` for a string carrying no `:` — the daemon's `Skip`
/// spelling, which names no manager. A reader RENDERING a package row's parts
/// is its whole audience; nothing re-derives a stored id from what this
/// returns.
#[must_use]
pub fn split_package_drift_resource_id(id: &str) -> Option<(&str, Vec<&str>)> {
    let (manager, packages) = id.split_once(':')?;
    if manager.is_empty() || packages.is_empty() {
        return None;
    }
    Some((manager, packages.split(',').collect()))
}

/// The `(resource_type, resource_id)` pair that identifies a planned action.
///
/// The ONE derivation of an action's identity, and — for every kind that stands
/// for exactly one resource — the row [`action_drift_rows`] records it as.
/// A batching kind is where the two part ways: a package batch and a module's
/// file deployment each name a UNIT no per-resource check can re-find, so
/// `action_drift_rows` breaks them into one row per package and per file while
/// this pair keeps naming the action. The pending-decision match in
/// [`DecisionExclusions`](super::DecisionExclusions) reads it for the file id.
///
/// It is a state-matching key, never a display string — nothing here is
/// condensed or re-shaped for a terminal.
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
            // composed-id-ok: the ACTION's identity, not a drift row.
            } => ("package".to_string(), package_batch_action_id(manager, packages)),
            PackageAction::Uninstall {
                manager, packages, ..
            // composed-id-ok: the ACTION's identity, not a drift row.
            } => ("package".to_string(), package_batch_action_id(manager, packages)),
            // A Skip row names the bare manager whose whole block was withheld,
            // so a live check can never resolve it as healed.
            // composed-id-ok: deliberately not a package id.
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
            } => (
                "system".to_string(),
                super::format::system_resource_key(configurator, key),
            ),
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
        // composed-id-ok: the ACTION's identity, not a drift row.
        Action::Module(ma) => ("module".to_string(), ma.module_name.clone()),
        Action::Env(ea) => {
            use crate::reconciler::EnvAction;
            match ea {
                EnvAction::WriteEnvFile { path, .. } => ("env".to_string(), to_posix_string(path)),
                EnvAction::InjectSourceLine { rc_path, .. } => {
                    ("env-rc".to_string(), to_posix_string(rc_path))
                }
                // The ONE spelling of the live-session surface, shared with
                // the tracking row the apply upserts: three spellings of one
                // fact left the tick recording a row no verb could settle.
                EnvAction::RefreshLiveSession { .. } => (
                    "env-session".to_string(),
                    crate::state::ENV_SESSION_RESOURCE_ID.to_string(),
                ),
            }
        }
        // A provision (and its refusal) is a PACKAGE fact — this manager's
        // tooling is missing from the machine — and the CLI's live check
        // records the same finding as ("package", "provision:<manager>") /
        // ("package", "refuse:<manager>"). One stored identity is what lets
        // either producer's next check heal the other's row; a refresh or a
        // prerequisite is cfgd's own scaffolding and keeps the "manager"
        // type, which `record_managed_resources` refuses to manage.
        Action::Manager(ma) => match ma {
            ManagerAction::Provision { .. } | ManagerAction::Refuse { .. } => {
                ("package".to_string(), ma.resource_id())
            }
            ManagerAction::RefreshIndex { .. } | ManagerAction::Prerequisite { .. } => {
                (MANAGER_RESOURCE_TYPE.to_string(), ma.resource_id())
            }
        },
    }
}

/// One `drift_events` row a planned action stands for: the identity every
/// producer records it under, and the operands a reader renders when the action
/// itself knows them.
///
/// `expected` / `actual` are `None` for an action whose divergence has no two
/// sides to state — a file whose content changed, a script that must run. The
/// store's `record_drift` COALESCEs a `None` over whatever is already stored, so
/// a tick re-affirming a row another producer worded does not blank its
/// operands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftRow {
    pub resource_type: String,
    pub resource_id: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

impl DriftRow {
    /// A row whose divergence states no operands.
    fn plain((resource_type, resource_id): (String, String)) -> Self {
        DriftRow {
            resource_type,
            resource_id,
            expected: None,
            actual: None,
        }
    }

    /// The `(resource_type, resource_id)` key a resolver matches on.
    #[must_use]
    pub fn key(&self) -> (String, String) {
        (self.resource_type.clone(), self.resource_id.clone())
    }
}

/// The `drift_events` rows a planned action stands for — the ONE producer, read
/// by the daemon tick that RECORDS them and by the apply that HEALS them.
///
/// A resource recorded under one grammar and resolved under another is a
/// finding no verb can settle: it stands until some other producer happens to
/// re-mint it, and `cfgd status` goes on advising the very command that just
/// ran. Deriving both sides here is what makes the two sets one set.
///
/// Per action kind:
///
/// * `Module(DeployFiles)` — one row per file the action will write, under the
///   live check's own `<module>/<target>` spelling
///   ([`module_file_spec_resource_id`](super::module_file_spec_resource_id)).
///   The aggregate `module:<name>:files:<n>` stays the `managed_resources`
///   tracking id and is deliberately NOT a drift row: it names a unit no
///   per-file check can match.
/// * `Module(InstallPackages)` — one `package` row per resolved package, from
///   the same composer the `Package` arm uses: a module's packages are checked
///   by the same live pass every other package is, under the same
///   `<manager>:<identity>` id. A `script`-installed entry mints none — a
///   custom install script has no queryable installed state, so a row for one
///   names something no check can re-find.
/// * Every other `Module` kind — the id its own executed description parses to
///   (`<name>:script`, `<name>:skip`), so the apply settles exactly what the
///   tick recorded.
/// * `Package(Install | Uninstall)` — one row PER PACKAGE, keyed on the
///   manager's [`package_entry_drift_id`], carrying the presence words every
///   live check words a package finding with.
/// * `Env(RefreshLiveSession)` — `("env-session", ENV_SESSION_RESOURCE_ID)`,
///   the one spelling of that surface.
/// * Everything else — `action_resource_info`'s pair, unchanged.
///
/// An action [`Action::pre_skip_reason`] answers for yields NO rows: the plan
/// already priced it out of every total, so a row recorded for it is a finding
/// the machine never had and no apply will ever settle.
///
/// The four `Skip` variants (`PackageAction::Skip`, `SystemAction::Skip`,
/// `SecretAction::Skip`, `FileAction::Skip`) do get rows — a withheld manager,
/// an unavailable configurator and a skipped file are real findings — but an
/// apply that skips them again has settled nothing, so
/// [`apply_heals_action_rows`] holds them back from the heal. They are cleared
/// by the tick's own complement, when a later plan stops carrying the skip.
///
/// The two module kinds that mint NO row are `ModuleActionKind::Skip` and
/// `ModuleActionKind::FilesRefused`. The other four Skips name a resource cfgd
/// probed and could not converge; a module the host declined whole was never
/// probed at all, and a refused file deploy is cfgd declining to touch the
/// files rather than finding them diverged — in neither case is there anything
/// under it to report as divergence. That is why the tick keeps the rows
/// standing under such a module rather than re-finding them
/// (`daemon::reconcile`'s `tick_cannot_refind`), and why no CLI check can
/// re-mint a `<name>:skip` row: a gate is information, not divergence. Both
/// kinds are still LISTED — the refusal as a counted action row, the host
/// decline as the header's `Modules` clause.
pub fn action_drift_rows(
    action: &Action,
    registry: &crate::providers::ProviderRegistry,
) -> Vec<DriftRow> {
    if action.pre_skip_reason().is_some() || module_skipped_whole(action) {
        return Vec::new();
    }
    match action {
        Action::Module(ma) => match &ma.kind {
            // `Skip` is settled by the guard above; both arms are what keep
            // the match exhaustive over `ModuleActionKind`.
            ModuleActionKind::Skip { .. } | ModuleActionKind::FilesRefused { .. } => Vec::new(),
            ModuleActionKind::DeployFiles { files, .. } => files
                .iter()
                .map(|f| {
                    DriftRow::plain((
                        "module".to_string(),
                        super::format::module_file_spec_resource_id(&ma.module_name, f),
                    ))
                })
                .collect(),
            // A module's packages are the machine's packages: the live scan
            // prices them into the same plan every other declared package
            // reaches it through, so the row it re-checks is a per-package one
            // under the manager's own identity. An aggregate over the whole
            // list named a unit no such check could match, and a scan that
            // could not match it resolved it blind.
            ModuleActionKind::InstallPackages { resolved } => {
                let mut by_manager: Vec<(&str, Vec<&str>)> = Vec::new();
                for p in resolved
                    .iter()
                    // A `prefer: [script]` entry is invisible to drift
                    // detection by design — a custom install script can put
                    // anything anywhere, so no installed-state read answers
                    // for it and no live pass ever re-mints its row.
                    .filter(|p| p.manager != crate::SCRIPT_SENTINEL)
                {
                    match by_manager.iter_mut().find(|(m, _)| *m == p.manager) {
                        Some((_, names)) => names.push(&p.resolved_name),
                        None => by_manager.push((&p.manager, vec![&p.resolved_name])),
                    }
                }
                by_manager
                    .into_iter()
                    .flat_map(|(manager, names)| {
                        presence_drift_rows(
                            manager,
                            names.into_iter(),
                            crate::PACKAGE_WANT_INSTALLED,
                            crate::Absence::NotInstalled.as_str(),
                            registry,
                        )
                    })
                    .collect()
            }
            // Parsed back out of the action's own description rather than
            // re-spelled: the apply upserts and settles under exactly this
            // string, and two hand-written composers of one id is how the
            // whole-module row came to be healed by nothing.
            _ => vec![DriftRow::plain(
                super::format::parse_resource_from_description(
                    &super::format::format_action_description(action),
                ),
            )],
        },
        // A Skip names the bare manager whose whole block was withheld — a
        // finding about the TOOLING, not about any package in it.
        Action::Package(PackageAction::Skip { .. }) => {
            vec![DriftRow::plain(action_resource_info(action))]
        }
        Action::Package(pa) => package_action_drift_rows(pa, registry),
        _ => vec![DriftRow::plain(action_resource_info(action))],
    }
}

/// The `actual` verb an uninstall's drift row carries — what the machine still
/// holds, worded to read in a status line ("to remove").
const PACKAGE_TO_REMOVE: &str = "to remove";

/// The per-PACKAGE drift rows a batching [`PackageAction`] stands for — ONE row
/// per package, in the manager's own identity grammar, carrying the presence
/// words every live check words a package finding with.
///
/// The narrower view of [`action_drift_rows`] for the CLI's live scan, which
/// holds a `PackageAction` rather than an `Action` and prices a plan it never
/// runs. Empty for `Skip`: the desired and installed sets agree, and the bare
/// manager row an `Action`-side producer mints for it is a finding about the
/// tooling that no live package check re-examines.
pub fn package_action_drift_rows(
    action: &PackageAction,
    registry: &crate::providers::ProviderRegistry,
) -> Vec<DriftRow> {
    let (manager, packages, expected, actual) = match action {
        PackageAction::Skip { .. } => return Vec::new(),
        PackageAction::Install {
            manager, packages, ..
        } => (
            manager,
            packages,
            crate::PACKAGE_WANT_INSTALLED,
            crate::Absence::NotInstalled.as_str(),
        ),
        PackageAction::Uninstall {
            manager, packages, ..
        } => (
            manager,
            packages,
            crate::PACKAGE_WANT_ABSENT,
            PACKAGE_TO_REMOVE,
        ),
    };
    presence_drift_rows(
        manager,
        packages.iter().map(String::as_str),
        expected,
        actual,
        registry,
    )
}

/// One presence drift row per package, in ONE manager's identity grammar.
///
/// The composer both package-bearing action kinds mint through, so a module's
/// packages and a profile's spell one package one way. The manager is looked
/// up once for the whole set: [`package_entry_drift_id`] asks it per package,
/// and a registry scan inside that map is quadratic in a module declaring a
/// few hundred.
fn presence_drift_rows<'a>(
    manager: &str,
    packages: impl Iterator<Item = &'a str>,
    expected: &str,
    actual: &str,
    registry: &crate::providers::ProviderRegistry,
) -> Vec<DriftRow> {
    let pm = registry
        .package_managers()
        .iter()
        .find(|m| m.name() == manager)
        .map(std::convert::AsRef::as_ref);
    packages
        .map(|p| DriftRow {
            resource_type: "package".to_string(),
            resource_id: package_entry_drift_id(manager, p, pm),
            expected: Some(expected.to_string()),
            actual: Some(actual.to_string()),
        })
        .collect()
}

/// Whether a successful settle of this action HEALS the rows
/// [`action_drift_rows`] mints for it.
///
/// False for the five `Skip` variants: each records that cfgd could not act on
/// a resource — an unavailable manager, an unregistered configurator, a file a
/// strategy declined — and an apply that reaches the same skip has changed
/// nothing about it. Resolving those rows on a skip would report a machine
/// converged by the very run that declined to touch it.
///
/// The two module kinds are held back as a belt: [`action_drift_rows`] mints no
/// row for either, so the heal each would perform is over an empty set.
#[must_use]
pub fn apply_heals_action_rows(action: &Action) -> bool {
    !matches!(
        action,
        Action::Package(PackageAction::Skip { .. })
            | Action::System(SystemAction::Skip { .. })
            | Action::Secret(SecretAction::Skip { .. })
            | Action::File(FileAction::Skip { .. })
            | Action::Module(ModuleAction {
                kind: ModuleActionKind::FilesRefused { .. },
                ..
            })
    ) && !module_skipped_whole(action)
}

/// Whether the HOST declined this module whole — [`ModuleActionKind::Skip`] and
/// nothing else.
///
/// The one spelling every reader of that fact shares: the count that prices a
/// tick's drift, the heal predicate, `action_drift_rows`' own arm and the
/// apply's dispatch guard. A refused file deploy
/// ([`ModuleActionKind::FilesRefused`]) is deliberately NOT this: it is work
/// the reader must see, counted and drawn like any other action, and folding it
/// in here is what made it vanish from the apply.
#[must_use]
pub fn module_skipped_whole(action: &Action) -> bool {
    matches!(
        action,
        Action::Module(ModuleAction {
            kind: ModuleActionKind::Skip { .. },
            ..
        })
    )
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
        // The generated env file's PATH line carries `# manager:brew,cargo`,
        // which is a COMMENT vocabulary rather than an owner: the name half
        // is a comma list no `Owner` name may hold, and reading it back as an
        // owner would mint one that names two things at once.
        assert_eq!(OwnerKind::from_token("manager"), None);
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

    #[test]
    fn renders_above_answers_from_the_comparator_it_is_asked_of() {
        // The prerequisites shape the apply path depends on: the lane group is
        // written as a tree, and the serial groups stream below it.
        assert!(Owner::cfgd("managers").renders_above(&Owner::cfgd("env")));
        assert!(Owner::cfgd("env").renders_above(&Owner::cfgd("session")));
        assert!(!Owner::cfgd("session").renders_above(&Owner::cfgd("managers")));
        assert!(
            !Owner::cfgd("env").renders_above(&Owner::cfgd("env")),
            "an owner does not render above itself"
        );
    }
}

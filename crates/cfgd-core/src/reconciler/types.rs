use std::str::FromStr;

use serde::Serialize;

use crate::config::ScriptEntry;
use crate::providers::{FileAction, PackageAction, SecretAction};
use crate::state::ApplyStatus;

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
    Env,
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
            PhaseName::Env => "env",
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
            PhaseName::Env => "Environment",
            PhaseName::Modules => "Modules",
            PhaseName::Packages => "Packages",
            PhaseName::System => "System",
            PhaseName::Files => "Files",
            PhaseName::Secrets => "Secrets",
            PhaseName::PostScripts => "Post-Scripts",
        }
    }
}

impl FromStr for PhaseName {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pre-scripts" => Ok(PhaseName::PreScripts),
            "env" => Ok(PhaseName::Env),
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
    /// `Backup`(3) `Source`(4), then name. It orders **every** phase — there is
    /// no phase-scoped override and none may be added. Rule P's
    /// module-before-profile execution barrier is a scheduling rule
    /// ([`Phase::dispatch_order`]) and never touches this ordering.
    pub fn sort_key(&self) -> (u8, &str) {
        (self.kind.rank(), self.name.as_str())
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
        // A `Bootstrap` installs a package *manager*, a prerequisite any owner
        // may be waiting on, so it belongs to cfgd rather than to the profile
        // whose planner happened to emit it.
        Action::Package(PackageAction::Bootstrap { .. }) => Owner::cfgd("managers"),
        // Env surfaces aggregate declarations from the profile *and* every
        // module, so no single user document owns them — cfgd authored the file
        // and cfgd owns it.
        Action::Env(EnvAction::RefreshLiveSession { .. }) => Owner::cfgd("session"),
        Action::Env(_) => Owner::cfgd("env"),
        _ => profile.clone(),
    }
}

/// A phase in the reconciliation plan, as owner groups in display order.
#[derive(Debug, Serialize)]
pub struct Phase {
    pub name: PhaseName,
    pub groups: Vec<OwnerGroup>,
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

    /// Every action in the phase, in the plan's own (display) order. What
    /// filters, counts and payloads see.
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
    /// Rule P's three-tier partition: module-owned groups (tier 0), then
    /// planned bootstraps (tier B), then the rest (tier 1). Separate from
    /// `actions()` because `actions()` is the plan's own order and is what
    /// filters, counts and payloads must keep seeing.
    pub fn dispatch_order(&self) -> impl Iterator<Item = (&Owner, &Action)> {
        let mut ordered: Vec<(&Owner, &Action)> = Vec::with_capacity(self.action_count());
        if self.name == PhaseName::Packages {
            // The tier-B predicate is written as "not module-owned AND a
            // bootstrap" rather than just "a bootstrap" so the three passes are
            // a partition by construction. Every `Bootstrap` belongs to the
            // `cfgd:managers` group, so under that membership rule the two
            // spellings select the same actions — but only this one keeps an
            // action from being dispatched twice if that rule ever slips.
            let is_module = |o: &Owner| o.kind == OwnerKind::Module;
            let is_bootstrap = |a: &Action| {
                matches!(
                    a,
                    Action::Package(crate::providers::PackageAction::Bootstrap { .. })
                )
            };
            ordered.extend(self.owned_actions().filter(|(o, _)| is_module(o)));
            ordered.extend(
                self.owned_actions()
                    .filter(|(o, a)| !is_module(o) && is_bootstrap(a)),
            );
            ordered.extend(
                self.owned_actions()
                    .filter(|(o, a)| !is_module(o) && !is_bootstrap(a)),
            );
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

/// A complete reconciliation plan.
#[derive(Debug, Serialize)]
pub struct Plan {
    pub phases: Vec<Phase>,
    /// Warnings about shell rc conflicts (env/alias defined before cfgd source line).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_name_from_str_round_trips() {
        assert_eq!("env".parse::<PhaseName>().unwrap(), PhaseName::Env);
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
        assert_eq!(Owner::cfgd("managers").token(), "cfgd:managers");
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
            Owner::cfgd("managers"),
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

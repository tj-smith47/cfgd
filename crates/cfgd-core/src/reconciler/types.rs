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
        }
    }
}

/// A phase in the reconciliation plan.
#[derive(Debug, Serialize)]
pub struct Phase {
    pub name: PhaseName,
    pub actions: Vec<Action>,
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
        self.phases.iter().map(|p| p.actions.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.phases.iter().all(|p| p.actions.is_empty())
    }

    /// Serialize the plan to a stable string for hashing.
    /// Uses serde_json serialization instead of Debug formatting for stability
    /// across compiler versions.
    pub fn to_hash_string(&self) -> String {
        let mut parts = Vec::new();
        for phase in &self.phases {
            for action in &phase.actions {
                if let Ok(json) = serde_json::to_string(action) {
                    parts.push(json);
                }
            }
        }
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
    /// Non-file actions that were not rolled back (require manual review).
    pub non_file_actions: Vec<String>,
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
}

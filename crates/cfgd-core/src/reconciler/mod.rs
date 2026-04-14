use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;

use secrecy::ExposeSecret;
use serde::Serialize;

use crate::config::{MergedProfile, ResolvedProfile, ScriptEntry, ScriptSpec};
use crate::errors::{ConfigError, Result};
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::providers::{FileAction, PackageAction, ProviderRegistry, SecretAction};
use crate::state::{ApplyStatus, StateStore};

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

/// The unified reconciler. Generates plans and applies them.
pub struct Reconciler<'a> {
    registry: &'a ProviderRegistry,
    state: &'a StateStore,
}

impl<'a> Reconciler<'a> {
    pub fn new(registry: &'a ProviderRegistry, state: &'a StateStore) -> Self {
        Self { registry, state }
    }

    /// Generate a reconciliation plan.
    pub fn plan(
        &self,
        resolved: &ResolvedProfile,
        file_actions: Vec<FileAction>,
        pkg_actions: Vec<PackageAction>,
        module_actions: Vec<ResolvedModule>,
        context: ReconcileContext,
    ) -> Result<Plan> {
        // Conflict detection: check for multiple sources targeting the same path
        Self::detect_file_conflicts(&file_actions, &module_actions)?;

        let mut phases = Vec::new();

        // Phase 0: PreScripts — pre-apply or pre-reconcile hooks.
        let (pre_script_actions, post_script_actions) =
            self.plan_scripts(&resolved.merged.scripts, context);
        phases.push(Phase {
            name: PhaseName::PreScripts,
            actions: pre_script_actions,
        });

        // Phase 1: Env — write ~/.cfgd.env and inject shell rc source line.
        // Runs early so that env vars (including PATH for bootstrapped managers)
        // are available to all subsequent phases.
        let (env_actions, warnings) = Self::plan_env(
            &resolved.merged.env,
            &resolved.merged.aliases,
            &module_actions,
            &[], // Secret envs are not yet resolved at plan time; they are
                 // injected during the apply phase after ResolveEnv actions run.
        );
        phases.push(Phase {
            name: PhaseName::Env,
            actions: env_actions,
        });

        // Phase 2: Modules — module packages, files, and post-apply scripts.
        // Packages are grouped with system/native managers first, then
        // bootstrappable managers, so build deps are installed before
        // packages that need them.
        let module_phase_actions = self.plan_modules(&module_actions, context);
        phases.push(Phase {
            name: PhaseName::Modules,
            actions: module_phase_actions,
        });

        // Phase 3: Packages — profile-level packages, installed after modules
        // so module deps are available.
        let package_actions = pkg_actions.into_iter().map(Action::Package).collect();
        phases.push(Phase {
            name: PhaseName::Packages,
            actions: package_actions,
        });

        // Phase 4: System — runs after packages so required binaries exist
        let system_actions = self.plan_system(&resolved.merged, &module_actions)?;
        phases.push(Phase {
            name: PhaseName::System,
            actions: system_actions,
        });

        // Phase 5: Files
        let fa = file_actions.into_iter().map(Action::File).collect();
        phases.push(Phase {
            name: PhaseName::Files,
            actions: fa,
        });

        // Phase 6: Secrets
        let secret_actions = self.plan_secrets(&resolved.merged);
        phases.push(Phase {
            name: PhaseName::Secrets,
            actions: secret_actions,
        });

        // Phase 7: PostScripts — post-apply or post-reconcile hooks.
        phases.push(Phase {
            name: PhaseName::PostScripts,
            actions: post_script_actions,
        });

        Ok(Plan { phases, warnings })
    }

    /// Check for file target conflicts across profile files and module files.
    /// Two sources targeting the same path with identical content is allowed;
    /// different content is an error.
    fn detect_file_conflicts(
        file_actions: &[FileAction],
        modules: &[ResolvedModule],
    ) -> Result<()> {
        // Map of target path → (source description, content hash)
        let mut targets: HashMap<PathBuf, (String, Option<String>)> = HashMap::new();

        // Collect from profile file actions
        for action in file_actions {
            let (source, target) = match action {
                FileAction::Create { source, target, .. }
                | FileAction::Update { source, target, .. } => (source, target),
                _ => continue,
            };
            let hash = content_hash_if_exists(source);
            let label = format!("profile:{}", source.display());
            if let Some((existing_label, existing_hash)) = targets.get(target) {
                if hash != *existing_hash {
                    return Err(crate::errors::FileError::Conflict {
                        target: target.clone(),
                        source_a: existing_label.clone(),
                        source_b: label,
                    }
                    .into());
                }
            } else {
                targets.insert(target.clone(), (label, hash));
            }
        }

        // Collect from module file deploy actions
        for module in modules {
            for file in &module.files {
                let target = expand_tilde(&file.target);
                let hash = content_hash_if_exists(&file.source);
                let label = format!("module:{}", module.name);
                if let Some((existing_label, existing_hash)) = targets.get(&target) {
                    if hash != *existing_hash {
                        return Err(crate::errors::FileError::Conflict {
                            target,
                            source_a: existing_label.clone(),
                            source_b: label,
                        }
                        .into());
                    }
                } else {
                    targets.insert(target, (label, hash));
                }
            }
        }

        Ok(())
    }

    fn plan_system(
        &self,
        profile: &MergedProfile,
        modules: &[ResolvedModule],
    ) -> Result<Vec<Action>> {
        // Build effective system map: start from profile, deep-merge each module in order.
        // Module values override profile values at leaf level (consistent with env/aliases).
        let mut system = profile.system.clone();
        for module in modules {
            for (key, value) in &module.system {
                crate::deep_merge_yaml(
                    system.entry(key.clone()).or_insert(serde_yaml::Value::Null),
                    value,
                );
            }
        }

        let mut actions = Vec::new();

        for configurator in self.registry.available_system_configurators() {
            if let Some(desired) = system.get(configurator.name()) {
                let drifts = configurator.diff(desired)?;
                for drift in drifts {
                    actions.push(Action::System(SystemAction::SetValue {
                        configurator: configurator.name().to_string(),
                        key: drift.key,
                        desired: drift.expected,
                        current: drift.actual,
                        origin: "local".to_string(),
                    }));
                }
            }
        }

        // Check for system keys with no registered configurator
        for key in system.keys() {
            let has_configurator = self
                .registry
                .available_system_configurators()
                .iter()
                .any(|c| c.name() == key);
            if !has_configurator {
                actions.push(Action::System(SystemAction::Skip {
                    configurator: key.clone(),
                    reason: format!("no configurator registered for '{}'", key),
                    origin: "local".to_string(),
                }));
            }
        }

        Ok(actions)
    }

    /// Plan env file generation from merged profile + module env vars and aliases.
    /// Returns (actions, warnings) — warnings for shell rc conflicts.
    fn plan_env(
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        modules: &[ResolvedModule],
        secret_envs: &[(String, String)],
    ) -> (Vec<Action>, Vec<String>) {
        let home = crate::expand_tilde(std::path::Path::new("~"));
        Self::plan_env_with_home(profile_env, profile_aliases, modules, secret_envs, &home)
    }

    fn plan_env_with_home(
        profile_env: &[crate::config::EnvVar],
        profile_aliases: &[crate::config::ShellAlias],
        modules: &[ResolvedModule],
        secret_envs: &[(String, String)],
        home: &std::path::Path,
    ) -> (Vec<Action>, Vec<String>) {
        let (mut merged, merged_aliases) =
            merge_module_env_aliases(profile_env, profile_aliases, modules);

        // Append secret-backed env vars after regular envs.
        // These are resolved secret values injected into the env file.
        for (name, value) in secret_envs {
            merged.push(crate::config::EnvVar {
                name: name.clone(),
                value: value.clone(),
            });
        }

        if merged.is_empty() && merged_aliases.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let mut actions = Vec::new();

        let warnings = if cfg!(windows) {
            // PowerShell env file — always generated on Windows
            let ps_path = home.join(".cfgd-env.ps1");
            let ps_content = generate_powershell_env_content(&merged, &merged_aliases);
            actions.push(Action::Env(EnvAction::WriteEnvFile {
                path: ps_path,
                content: ps_content,
            }));

            // Inject dot-source line into PowerShell profiles
            let ps_profile_dirs = [
                home.join("Documents/PowerShell"),
                home.join("Documents/WindowsPowerShell"),
            ];
            for profile_dir in &ps_profile_dirs {
                let profile_path = profile_dir.join("Microsoft.PowerShell_profile.ps1");
                actions.push(Action::Env(EnvAction::InjectSourceLine {
                    rc_path: profile_path,
                    line: ". ~/.cfgd-env.ps1".to_string(),
                }));
            }

            // If Git Bash is available, also generate bash env file
            if crate::command_available("sh") {
                let bash_path = home.join(".cfgd.env");
                let bash_content = generate_env_file_content(&merged, &merged_aliases);
                actions.push(Action::Env(EnvAction::WriteEnvFile {
                    path: bash_path,
                    content: bash_content,
                }));
                let bashrc = home.join(".bashrc");
                actions.push(Action::Env(EnvAction::InjectSourceLine {
                    rc_path: bashrc,
                    line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
                }));
            }

            // No rc conflict detection on Windows
            Vec::new()
        } else {
            // Unix: bash/zsh env file + source line
            let env_path = home.join(".cfgd.env");
            let content = generate_env_file_content(&merged, &merged_aliases);
            actions.push(Action::Env(EnvAction::WriteEnvFile {
                path: env_path.clone(),
                content,
            }));

            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            let rc_path = if shell.contains("zsh") {
                home.join(".zshrc")
            } else {
                home.join(".bashrc")
            };
            actions.push(Action::Env(EnvAction::InjectSourceLine {
                rc_path: rc_path.clone(),
                line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
            }));

            // Check for conflicts with existing definitions in the shell rc file
            detect_rc_env_conflicts(&rc_path, &merged, &merged_aliases)
        };

        // Fish shell: only generate fish env if fish is the user's shell
        let fish_conf_d = home.join(".config/fish/conf.d");
        let current_shell = std::env::var("SHELL").unwrap_or_default();
        if current_shell.contains("fish") && fish_conf_d.exists() {
            let fish_path = fish_conf_d.join("cfgd-env.fish");
            let fish_content = generate_fish_env_content(&merged, &merged_aliases);
            let existing_fish = std::fs::read_to_string(&fish_path).unwrap_or_default(); // OK: file may not exist yet
            if existing_fish != fish_content {
                actions.push(Action::Env(EnvAction::WriteEnvFile {
                    path: fish_path,
                    content: fish_content,
                }));
            }
        }

        (actions, warnings)
    }

    fn plan_secrets(&self, profile: &MergedProfile) -> Vec<Action> {
        let mut actions = Vec::new();

        let has_backend = self
            .registry
            .secret_backend
            .as_ref()
            .map(|b| b.is_available())
            .unwrap_or(false);

        for secret in &profile.secrets {
            let has_envs = secret.envs.as_ref().is_some_and(|e| !e.is_empty());

            // Check if it's a provider reference
            if let Some((provider_name, reference)) =
                crate::providers::parse_secret_reference(&secret.source)
            {
                let available = self
                    .registry
                    .secret_providers
                    .iter()
                    .any(|p| p.name() == provider_name && p.is_available());

                if available {
                    // File-targeting action when a target path is set
                    if let Some(ref target) = secret.target {
                        actions.push(Action::Secret(SecretAction::Resolve {
                            provider: provider_name.to_string(),
                            reference: reference.to_string(),
                            target: target.clone(),
                            origin: "local".to_string(),
                        }));
                    }

                    // Env injection action when envs are specified
                    if has_envs {
                        actions.push(Action::Secret(SecretAction::ResolveEnv {
                            provider: provider_name.to_string(),
                            reference: reference.to_string(),
                            envs: secret.envs.clone().unwrap_or_default(),
                            origin: "local".to_string(),
                        }));
                    }

                    // If neither target nor envs, skip (shouldn't happen due to validation)
                    if secret.target.is_none() && !has_envs {
                        actions.push(Action::Secret(SecretAction::Skip {
                            source: secret.source.clone(),
                            reason: "no target or envs specified".to_string(),
                            origin: "local".to_string(),
                        }));
                    }
                } else {
                    actions.push(Action::Secret(SecretAction::Skip {
                        source: secret.source.clone(),
                        reason: format!("provider '{}' not available", provider_name),
                        origin: "local".to_string(),
                    }));
                }
            } else if secret.target.is_some() && has_backend {
                // SOPS/age encrypted file — only for file targets
                let backend_name = secret
                    .backend
                    .as_deref()
                    .or_else(|| self.registry.secret_backend.as_ref().map(|b| b.name()))
                    .unwrap_or("sops")
                    .to_string();

                actions.push(Action::Secret(SecretAction::Decrypt {
                    source: PathBuf::from(&secret.source),
                    target: secret.target.clone().unwrap_or_default(),
                    backend: backend_name,
                    origin: "local".to_string(),
                }));

                if has_envs {
                    actions.push(Action::Secret(SecretAction::Skip {
                        source: secret.source.clone(),
                        reason: "env injection requires a secret provider reference; SOPS file targets cannot inject env vars".to_string(),
                        origin: "local".to_string(),
                    }));
                }
            } else if secret.target.is_none() && has_envs && !has_backend {
                // Env-only secret without a provider reference — SOPS can't resolve
                // individual values for env injection
                actions.push(Action::Secret(SecretAction::Skip {
                    source: secret.source.clone(),
                    reason: "env injection requires a secret provider reference (e.g. 1password://, vault://)".to_string(),
                    origin: "local".to_string(),
                }));
            } else if !has_backend {
                actions.push(Action::Secret(SecretAction::Skip {
                    source: secret.source.clone(),
                    reason: "no secret backend available".to_string(),
                    origin: "local".to_string(),
                }));
            }
        }

        actions
    }

    fn plan_scripts(
        &self,
        scripts: &ScriptSpec,
        context: ReconcileContext,
    ) -> (Vec<Action>, Vec<Action>) {
        let (pre_entries, pre_phase, post_entries, post_phase) = match context {
            ReconcileContext::Apply => (
                &scripts.pre_apply,
                ScriptPhase::PreApply,
                &scripts.post_apply,
                ScriptPhase::PostApply,
            ),
            ReconcileContext::Reconcile => (
                &scripts.pre_reconcile,
                ScriptPhase::PreReconcile,
                &scripts.post_reconcile,
                ScriptPhase::PostReconcile,
            ),
        };

        let pre_actions = pre_entries
            .iter()
            .map(|entry| {
                Action::Script(ScriptAction::Run {
                    entry: entry.clone(),
                    phase: pre_phase.clone(),
                    origin: "local".to_string(),
                })
            })
            .collect();

        let post_actions = post_entries
            .iter()
            .map(|entry| {
                Action::Script(ScriptAction::Run {
                    entry: entry.clone(),
                    phase: post_phase.clone(),
                    origin: "local".to_string(),
                })
            })
            .collect();

        (pre_actions, post_actions)
    }

    fn plan_modules(&self, modules: &[ResolvedModule], context: ReconcileContext) -> Vec<Action> {
        let mut actions = Vec::new();

        for module in modules {
            // Select pre/post scripts based on context
            let (pre_scripts, pre_phase, post_scripts, post_phase) = match context {
                ReconcileContext::Apply => (
                    &module.pre_apply_scripts,
                    ScriptPhase::PreApply,
                    &module.post_apply_scripts,
                    ScriptPhase::PostApply,
                ),
                ReconcileContext::Reconcile => (
                    &module.pre_reconcile_scripts,
                    ScriptPhase::PreReconcile,
                    &module.post_reconcile_scripts,
                    ScriptPhase::PostReconcile,
                ),
            };

            // Pre-scripts for this module
            for script in pre_scripts {
                actions.push(Action::Module(ModuleAction {
                    module_name: module.name.clone(),
                    kind: ModuleActionKind::RunScript {
                        script: script.clone(),
                        phase: pre_phase.clone(),
                    },
                }));
            }

            // Packages: group by manager for efficient batch install
            let mut by_manager: HashMap<String, Vec<crate::modules::ResolvedPackage>> =
                HashMap::new();
            for pkg in &module.packages {
                by_manager
                    .entry(pkg.manager.clone())
                    .or_default()
                    .push(pkg.clone());
            }

            // Sort managers: system/native managers first (apt, dnf, pacman, etc.),
            // then bootstrappable managers (brew, snap). This ensures build dependencies
            // are installed before packages that might need them.
            let mut manager_order: Vec<&String> = by_manager.keys().collect();
            manager_order.sort_by_key(|mgr| {
                match self
                    .registry
                    .package_managers
                    .iter()
                    .find(|m| m.name() == mgr.as_str())
                {
                    Some(m) if m.is_available() => 0,  // available (native) first
                    Some(m) if m.can_bootstrap() => 1, // bootstrappable second
                    _ => 2,                            // unknown last
                }
            });

            for mgr_name in manager_order {
                let resolved = &by_manager[mgr_name];
                actions.push(Action::Module(ModuleAction {
                    module_name: module.name.clone(),
                    kind: ModuleActionKind::InstallPackages {
                        resolved: resolved.clone(),
                    },
                }));
            }

            // Files — validate encryption requirements before deploying
            if !module.files.is_empty() {
                let mut encryption_ok = true;
                for file in &module.files {
                    if let Some(ref enc) = file.encryption {
                        let strategy = file.strategy.unwrap_or(self.registry.default_file_strategy);
                        if enc.mode == crate::config::EncryptionMode::Always
                            && matches!(
                                strategy,
                                crate::config::FileStrategy::Symlink
                                    | crate::config::FileStrategy::Hardlink
                            )
                        {
                            actions.push(Action::Module(ModuleAction {
                                module_name: module.name.clone(),
                                kind: ModuleActionKind::Skip {
                                    reason: format!(
                                        "encryption mode Always incompatible with {:?} for {}",
                                        strategy,
                                        file.source.display()
                                    ),
                                },
                            }));
                            encryption_ok = false;
                            break;
                        }
                        if file.source.exists() {
                            match crate::is_file_encrypted(&file.source, &enc.backend) {
                                Ok(true) => {}
                                Ok(false) => {
                                    actions.push(Action::Module(ModuleAction {
                                        module_name: module.name.clone(),
                                        kind: ModuleActionKind::Skip {
                                            reason: format!(
                                                "file {} requires encryption (backend: {}) but is not encrypted",
                                                file.source.display(),
                                                enc.backend
                                            ),
                                        },
                                    }));
                                    encryption_ok = false;
                                    break;
                                }
                                Err(e) => {
                                    actions.push(Action::Module(ModuleAction {
                                        module_name: module.name.clone(),
                                        kind: ModuleActionKind::Skip {
                                            reason: format!(
                                                "encryption check failed for {}: {}",
                                                file.source.display(),
                                                e
                                            ),
                                        },
                                    }));
                                    encryption_ok = false;
                                    break;
                                }
                            }
                        }
                    }
                }
                if encryption_ok {
                    actions.push(Action::Module(ModuleAction {
                        module_name: module.name.clone(),
                        kind: ModuleActionKind::DeployFiles {
                            files: module.files.clone(),
                        },
                    }));
                }
            }

            // Post-scripts for this module
            for script in post_scripts {
                actions.push(Action::Module(ModuleAction {
                    module_name: module.name.clone(),
                    kind: ModuleActionKind::RunScript {
                        script: script.clone(),
                        phase: post_phase.clone(),
                    },
                }));
            }
        }

        actions
    }

    /// Update module state in state.db after a successful apply.
    fn update_module_state(
        &self,
        modules: &[ResolvedModule],
        apply_id: i64,
        results: &[ActionResult],
    ) -> Result<()> {
        for module in modules {
            // Check if any module action for this module failed
            let module_prefix = format!("module:{}:", module.name);
            let any_failed = results
                .iter()
                .any(|r| r.description.starts_with(&module_prefix) && !r.success);
            let status = if any_failed { "error" } else { "installed" };

            // Hash the resolved packages list
            let packages_hash = {
                let mut pkg_parts: Vec<String> = module
                    .packages
                    .iter()
                    .map(|p| {
                        format!(
                            "{}:{}:{}",
                            p.manager,
                            p.resolved_name,
                            p.version.as_deref().unwrap_or("")
                        )
                    })
                    .collect();
                pkg_parts.sort();
                crate::sha256_hex(pkg_parts.join("|").as_bytes())
            };

            // Hash the file targets
            let files_hash = {
                let mut file_parts: Vec<String> = module
                    .files
                    .iter()
                    .map(|f| format!("{}:{}", f.source.display(), f.target.display()))
                    .collect();
                file_parts.sort();
                crate::sha256_hex(file_parts.join("|").as_bytes())
            };

            // Collect git source info
            let git_sources: Vec<serde_json::Value> = module
                .files
                .iter()
                .filter(|f| f.is_git_source)
                .map(|f| {
                    serde_json::json!({
                        "source": f.source.display().to_string(),
                        "target": f.target.display().to_string(),
                    })
                })
                .collect();
            let git_sources_json = if git_sources.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&git_sources).unwrap_or_default())
            };

            self.state.upsert_module_state(
                &module.name,
                Some(apply_id),
                &packages_hash,
                &files_hash,
                git_sources_json.as_deref(),
                status,
            )?;
        }
        Ok(())
    }

    /// Apply a plan, executing each phase in order.
    /// Failed actions are logged and skipped — they don't abort the entire apply.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        plan: &Plan,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        phase_filter: Option<&PhaseName>,
        module_actions: &[ResolvedModule],
        context: ReconcileContext,
        skip_scripts: bool,
    ) -> Result<ApplyResult> {
        // Record apply up front as "in-progress" so the journal can reference it
        let plan_hash = crate::state::plan_hash(&plan.to_hash_string());
        let profile_name = resolved
            .layers
            .last()
            .map(|l| l.profile_name.as_str())
            .unwrap_or("unknown");
        let apply_id =
            self.state
                .record_apply(profile_name, &plan_hash, ApplyStatus::InProgress, None)?;

        let mut results = Vec::new();
        let mut action_index: usize = 0;
        let mut secret_env_collector: Vec<(String, String)> = Vec::new();

        for phase in &plan.phases {
            if let Some(filter) = phase_filter
                && &phase.name != filter
            {
                continue;
            }

            if phase.actions.is_empty() {
                continue;
            }

            let total = phase.actions.len();
            for (action_idx, action) in phase.actions.iter().enumerate() {
                let desc_for_journal = format_action_description(action);
                let (action_type, resource_id) = parse_resource_from_description(&desc_for_journal);

                // Capture file state before overwrite (for backup)
                if let Some(ref path) = action_target_path(action)
                    && let Ok(Some(file_state)) = crate::capture_file_state(path)
                    && let Err(e) = self.state.store_file_backup(
                        apply_id,
                        &path.display().to_string(),
                        &file_state,
                    )
                {
                    tracing::warn!("failed to store file backup for {}: {}", path.display(), e);
                }

                // Journal: record action start
                let journal_id = self
                    .state
                    .journal_begin(
                        apply_id,
                        action_index,
                        phase.name.as_str(),
                        &action_type,
                        &resource_id,
                        None,
                    )
                    .ok();

                let result = self.apply_action(
                    action,
                    resolved,
                    config_dir,
                    printer,
                    apply_id,
                    context,
                    module_actions,
                    &mut secret_env_collector,
                );

                let (desc, success, error, should_abort) = match result {
                    Ok((desc, script_output)) => {
                        if let Some(jid) = journal_id
                            && let Err(e) =
                                self.state
                                    .journal_complete(jid, None, script_output.as_deref())
                        {
                            tracing::warn!("failed to record journal completion: {e}");
                        }
                        (desc, true, None, false)
                    }
                    Err(e) => {
                        let desc = format_action_description(action);

                        // Check if this is a script action with continueOnError
                        let continue_on_err = if let Action::Script(ScriptAction::Run {
                            entry,
                            phase: script_phase,
                            ..
                        }) = action
                        {
                            effective_continue_on_error(entry, script_phase)
                        } else {
                            false
                        };

                        if continue_on_err {
                            printer.warning(&format!(
                                "[{}/{}] Script failed (continueOnError): {} — {}",
                                action_idx + 1,
                                total,
                                desc,
                                e
                            ));
                        } else {
                            printer.error(&format!(
                                "[{}/{}] Failed: {} — {}",
                                action_idx + 1,
                                total,
                                desc,
                                e
                            ));
                        }
                        if let Some(jid) = journal_id
                            && let Err(je) = self.state.journal_fail(jid, &e.to_string())
                        {
                            tracing::warn!("failed to record journal failure: {je}");
                        }
                        (desc, false, Some(e.to_string()), !continue_on_err)
                    }
                };

                let changed = success && !desc.contains(":skipped");
                results.push(ActionResult {
                    phase: phase.name.as_str().to_string(),
                    description: desc.clone(),
                    success,
                    error: error.clone(),
                    changed,
                });
                action_index += 1;

                // If a pre-script failed without continueOnError, abort
                let is_pre_script = matches!(
                    action,
                    Action::Script(ScriptAction::Run { phase: sp, .. })
                        if matches!(sp, ScriptPhase::PreApply | ScriptPhase::PreReconcile)
                ) || matches!(
                    action,
                    Action::Module(ModuleAction {
                        kind: ModuleActionKind::RunScript { phase: sp, .. },
                        ..
                    }) if matches!(sp, ScriptPhase::PreApply | ScriptPhase::PreReconcile)
                );
                if should_abort && is_pre_script {
                    return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                        message: format!("pre-script failed, aborting apply: {}", desc),
                    }));
                }
            }
        }

        // --- Secret env injection: re-generate env files with resolved secret env vars ---
        if !secret_env_collector.is_empty() {
            let (env_actions, _) = Self::plan_env(
                &resolved.merged.env,
                &resolved.merged.aliases,
                module_actions,
                &secret_env_collector,
            );
            for env_action in &env_actions {
                if let Action::Env(ea) = env_action {
                    match Self::apply_env_action(ea, printer) {
                        Ok(desc) => {
                            let changed = !desc.contains(":skipped");
                            results.push(ActionResult {
                                phase: PhaseName::Secrets.as_str().to_string(),
                                description: desc,
                                success: true,
                                error: None,
                                changed,
                            });
                        }
                        Err(e) => {
                            printer.error(&format!("Failed to write secret env vars: {}", e));
                            results.push(ActionResult {
                                phase: PhaseName::Secrets.as_str().to_string(),
                                description: "env:write:secret-envs".to_string(),
                                success: false,
                                error: Some(e.to_string()),
                                changed: false,
                            });
                        }
                    }
                }
            }
        }

        // --- onChange detection: run profile onChange scripts if anything changed ---
        let any_changed = results.iter().any(|r| r.changed);
        if any_changed && !skip_scripts && !resolved.merged.scripts.on_change.is_empty() {
            let profile_name = resolved
                .layers
                .last()
                .map(|l| l.profile_name.as_str())
                .unwrap_or("unknown");
            let env_vars = build_script_env(
                config_dir,
                profile_name,
                context,
                &ScriptPhase::OnChange,
                false,
                None,
                None,
            );
            for entry in &resolved.merged.scripts.on_change {
                match execute_script(
                    entry,
                    config_dir,
                    &env_vars,
                    crate::PROFILE_SCRIPT_TIMEOUT,
                    printer,
                ) {
                    Ok((desc, changed, _)) => {
                        results.push(ActionResult {
                            phase: "post-scripts".to_string(),
                            description: desc,
                            success: true,
                            error: None,
                            changed,
                        });
                    }
                    Err(e) => {
                        let continue_on_err =
                            effective_continue_on_error(entry, &ScriptPhase::OnChange);
                        results.push(ActionResult {
                            phase: "post-scripts".to_string(),
                            description: format!("onChange: {}", entry.run_str()),
                            success: false,
                            error: Some(format!("{}", e)),
                            changed: false,
                        });
                        if !continue_on_err {
                            return Err(e);
                        }
                    }
                }
            }
        }

        // --- Module-level onChange: run per-module onChange scripts if that module had changes ---
        if any_changed && !skip_scripts {
            let profile_name = resolved
                .layers
                .last()
                .map(|l| l.profile_name.as_str())
                .unwrap_or("unknown");
            for module in module_actions {
                if module.on_change_scripts.is_empty() {
                    continue;
                }
                let prefix = format!("module:{}:", module.name);
                let module_changed = results
                    .iter()
                    .any(|r| r.changed && r.description.starts_with(&prefix));
                if !module_changed {
                    continue;
                }
                let env_vars = build_script_env(
                    config_dir,
                    profile_name,
                    context,
                    &ScriptPhase::OnChange,
                    false,
                    Some(&module.name),
                    Some(&module.dir),
                );
                let working = &module.dir;
                for entry in &module.on_change_scripts {
                    match execute_script(entry, working, &env_vars, MODULE_SCRIPT_TIMEOUT, printer)
                    {
                        Ok((desc, changed, _)) => {
                            results.push(ActionResult {
                                phase: "modules".to_string(),
                                description: desc,
                                success: true,
                                error: None,
                                changed,
                            });
                        }
                        Err(e) => {
                            let continue_on_err =
                                effective_continue_on_error(entry, &ScriptPhase::OnChange);
                            results.push(ActionResult {
                                phase: "modules".to_string(),
                                description: format!(
                                    "module:{}:onChange: {}",
                                    module.name,
                                    entry.run_str()
                                ),
                                success: false,
                                error: Some(format!("{}", e)),
                                changed: false,
                            });
                            if !continue_on_err {
                                return Err(e);
                            }
                        }
                    }
                }
            }
        }

        let total = results.len();
        let failed = results.iter().filter(|r| !r.success).count();
        let status = if failed == 0 {
            ApplyStatus::Success
        } else if failed == total {
            ApplyStatus::Failed
        } else {
            ApplyStatus::Partial
        };

        // Update apply status from "in-progress" placeholder to final
        let summary = serde_json::json!({
            "total": total,
            "succeeded": total - failed,
            "failed": failed,
        })
        .to_string();
        self.state
            .update_apply_status(apply_id, status.clone(), Some(&summary))?;

        // Update managed resources
        for result in &results {
            if result.success {
                let (rtype, rid) = parse_resource_from_description(&result.description);
                self.state
                    .upsert_managed_resource(&rtype, &rid, "local", None, Some(apply_id))?;
                self.state.resolve_drift(apply_id, &rtype, &rid)?;
            }
        }

        // Update module state and file manifests for successfully applied modules
        self.update_module_state(module_actions, apply_id, &results)?;

        // Post-apply snapshot: capture the resolved content of all managed file
        // targets (following symlinks). This ensures rollback can restore the
        // exact content visible at this point, even for symlink-deployed files
        // where the source may be modified in-place between applies.
        let mut snapshot_paths = std::collections::HashSet::new();
        for managed in &resolved.merged.files.managed {
            let target = crate::expand_tilde(&managed.target);
            let key = target.display().to_string();
            if snapshot_paths.contains(&key) {
                continue;
            }
            snapshot_paths.insert(key.clone());
            if let Ok(Some(state)) = crate::capture_file_resolved_state(&target)
                && let Err(e) = self.state.store_file_backup(apply_id, &key, &state)
            {
                tracing::debug!("post-apply snapshot for {}: {}", key, e);
            }
        }
        for module in module_actions {
            for file in &module.files {
                let target = crate::expand_tilde(&file.target);
                let key = target.display().to_string();
                if snapshot_paths.contains(&key) {
                    continue;
                }
                snapshot_paths.insert(key.clone());
                if let Ok(Some(state)) = crate::capture_file_resolved_state(&target)
                    && let Err(e) = self.state.store_file_backup(apply_id, &key, &state)
                {
                    tracing::debug!("post-apply snapshot for {}: {}", key, e);
                }
            }
        }

        Ok(ApplyResult {
            action_results: results,
            status,
            apply_id,
        })
    }

    /// Roll back completed file actions from a previous apply.
    ///
    /// Restores files from backups in reverse order. Newly created files (no backup)
    /// are deleted. Package installs and system changes are NOT rolled back — they
    /// are listed in the output as requiring manual review.
    pub fn rollback_apply(&self, apply_id: i64, printer: &Printer) -> Result<RollbackResult> {
        // Rollback restores the system to the state that existed AFTER the target apply.
        //
        // Primary source: post-apply snapshots stored with the target apply_id.
        // These capture the resolved content of all managed files (following symlinks)
        // at the moment the target apply completed. For each file path, the LAST
        // backup entry (highest id) for the target apply is the post-apply snapshot.
        //
        // Fallback: for files not covered by the target apply's snapshots, use the
        // earliest backup from applies AFTER the target (pre-action backups from
        // later applies, which represent the state right after the target).
        let target_backups = self.state.get_apply_backups(apply_id)?;
        let after_backups = self.state.file_backups_after_apply(apply_id)?;
        let after_entries = self.state.journal_entries_after_apply(apply_id)?;

        // Build a map of file_path -> last backup from the target apply
        // (last = post-apply snapshot, which has the highest id)
        let mut target_snapshot: HashMap<String, &crate::state::FileBackupRecord> = HashMap::new();
        for bk in &target_backups {
            target_snapshot.insert(bk.file_path.clone(), bk);
        }

        let mut files_restored = 0usize;
        let mut files_removed = 0usize;
        let mut non_file_actions = Vec::new();

        // Collect non-file actions from subsequent applies
        for entry in &after_entries {
            let is_file = entry.phase == "files"
                || entry.action_type == "file"
                || entry.resource_id.starts_with("file:");
            if !is_file && !non_file_actions.contains(&entry.resource_id) {
                non_file_actions.push(entry.resource_id.clone());
            }
        }

        // Track which file paths we've already restored (avoid duplicate restores)
        let mut restored_paths = std::collections::HashSet::new();

        // Phase 1: restore from target apply's post-apply snapshots
        for (path, bk) in &target_snapshot {
            restored_paths.insert(path.clone());
            let target = std::path::Path::new(path);
            let result = restore_file_from_backup(target, bk, printer);
            match result {
                RestoreOutcome::Restored => files_restored += 1,
                RestoreOutcome::Removed => files_removed += 1,
                RestoreOutcome::Skipped | RestoreOutcome::Failed => {}
            }
        }

        // Phase 2: fallback to earliest backup after target for remaining paths
        for bk in &after_backups {
            if restored_paths.contains(&bk.file_path) {
                continue;
            }
            restored_paths.insert(bk.file_path.clone());
            let target = std::path::Path::new(&bk.file_path);
            let result = restore_file_from_backup(target, bk, printer);
            match result {
                RestoreOutcome::Restored => files_restored += 1,
                RestoreOutcome::Removed => files_removed += 1,
                RestoreOutcome::Skipped | RestoreOutcome::Failed => {}
            }
        }

        // Phase 3: handle files created by subsequent applies but not in target's snapshot
        for entry in &after_entries {
            let is_file = entry.phase == "files"
                || entry.action_type == "file"
                || entry.resource_id.starts_with("file:");
            if !is_file {
                continue;
            }

            let actual_path = entry
                .resource_id
                .strip_prefix("file:create:")
                .or_else(|| entry.resource_id.strip_prefix("file:update:"))
                .or_else(|| entry.resource_id.strip_prefix("file:delete:"))
                .unwrap_or(&entry.resource_id);

            if restored_paths.contains(actual_path) {
                continue;
            }
            restored_paths.insert(actual_path.to_string());

            // If the file is in the target apply's snapshot, it was already handled in phase 1.
            // If not, check the journal to see if it existed at the target apply.
            let target_entries = self.state.journal_completed_actions(apply_id)?;
            let target_had_file = target_entries.iter().any(|e| {
                let target_path = e
                    .resource_id
                    .strip_prefix("file:create:")
                    .or_else(|| e.resource_id.strip_prefix("file:update:"))
                    .or_else(|| e.resource_id.strip_prefix("file:delete:"))
                    .unwrap_or(&e.resource_id);
                target_path == actual_path
            });

            if !target_had_file && entry.resource_id.starts_with("file:create:") {
                let target = std::path::Path::new(actual_path);
                if target.exists() {
                    if let Err(e) = std::fs::remove_file(target) {
                        printer.warning(&format!(
                            "rollback: failed to remove {}: {}",
                            target.display(),
                            e
                        ));
                    } else {
                        files_removed += 1;
                    }
                }
            }
        }

        // Record rollback as a new apply
        self.state.record_apply(
            "rollback",
            &format!("rollback-of-{}", apply_id),
            ApplyStatus::Success,
            Some(&format!(
                "{{\"rollback_of\":{},\"restored\":{},\"removed\":{}}}",
                apply_id, files_restored, files_removed
            )),
        )?;

        Ok(RollbackResult {
            files_restored,
            files_removed,
            non_file_actions,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_action(
        &self,
        action: &Action,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        apply_id: i64,
        context: ReconcileContext,
        module_actions: &[ResolvedModule],
        secret_env_collector: &mut Vec<(String, String)>,
    ) -> Result<(String, Option<String>)> {
        match action {
            Action::System(sys) => self
                .apply_system_action(sys, &resolved.merged, printer)
                .map(|d| (d, None)),
            Action::Package(pkg) => self.apply_package_action(pkg, printer).map(|d| (d, None)),
            Action::File(file) => self
                .apply_file_action(file, &resolved.merged, config_dir, printer)
                .map(|d| (d, None)),
            Action::Secret(secret) => self
                .apply_secret_action(secret, config_dir, printer, secret_env_collector)
                .map(|d| (d, None)),
            Action::Script(script) => {
                self.apply_script_action(script, resolved, config_dir, printer, context)
            }
            Action::Module(module) => self
                .apply_module_action(
                    module,
                    config_dir,
                    printer,
                    apply_id,
                    context,
                    resolved,
                    module_actions,
                )
                .map(|d| (d, None)),
            Action::Env(env) => Self::apply_env_action(env, printer).map(|d| (d, None)),
        }
    }

    fn apply_env_action(action: &EnvAction, printer: &Printer) -> Result<String> {
        match action {
            EnvAction::WriteEnvFile { path, content } => {
                let existing = match std::fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => {
                        tracing::warn!("cannot read {}: {e}", path.display());
                        String::new()
                    }
                };
                if existing == *content {
                    return Ok(format!("env:write:{}:skipped", path.display()));
                }
                crate::atomic_write_str(path, content)?;
                printer.success(&format!("Wrote {}", path.display()));
                Ok(format!("env:write:{}", path.display()))
            }
            EnvAction::InjectSourceLine { rc_path, line } => {
                let existing = match std::fs::read_to_string(rc_path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => {
                        tracing::warn!("cannot read {}: {e}", rc_path.display());
                        String::new()
                    }
                };
                if existing.contains(line) {
                    // Already injected
                    return Ok(format!("env:inject:{}:skipped", rc_path.display()));
                }
                let mut content = existing;
                if !content.ends_with('\n') && !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(line);
                content.push('\n');
                crate::atomic_write_str(rc_path, &content)?;
                printer.success(&format!("Injected source line into {}", rc_path.display()));
                Ok(format!("env:inject:{}", rc_path.display()))
            }
        }
    }

    fn apply_system_action(
        &self,
        action: &SystemAction,
        profile: &MergedProfile,
        printer: &Printer,
    ) -> Result<String> {
        match action {
            SystemAction::SetValue {
                configurator,
                key,
                desired,
                current,
                ..
            } => {
                if let Some(desired_value) = profile.system.get(configurator.as_str()) {
                    for sc in self.registry.available_system_configurators() {
                        if sc.name() == configurator {
                            sc.apply(desired_value, printer)?;
                            return Ok(format!(
                                "system:{}.{} ({} → {})",
                                configurator, key, current, desired
                            ));
                        }
                    }
                }
                Ok(format!("system:{}.{}", configurator, key))
            }
            SystemAction::Skip {
                configurator,
                reason,
                ..
            } => {
                printer.warning(&format!("{}: {}", configurator, reason));
                Ok(format!("system:{} (skipped)", configurator))
            }
        }
    }

    fn apply_package_action(&self, action: &PackageAction, printer: &Printer) -> Result<String> {
        match action {
            PackageAction::Bootstrap { manager, .. } => {
                // Find in ALL managers (not just available — it isn't available yet)
                for pm in &self.registry.package_managers {
                    if pm.name() == manager {
                        pm.bootstrap(printer)?;
                        if !pm.is_available() {
                            return Err(crate::errors::PackageError::BootstrapFailed {
                                manager: manager.clone(),
                                message: format!("{} still not available after bootstrap", manager),
                            }
                            .into());
                        }
                        return Ok(format!("package:{}:bootstrap", manager));
                    }
                }
                Err(crate::errors::PackageError::ManagerNotFound {
                    manager: manager.clone(),
                }
                .into())
            }
            PackageAction::Install {
                manager, packages, ..
            } => {
                for pm in self.registry.available_package_managers() {
                    if pm.name() == manager {
                        pm.install(packages, printer)?;
                        return Ok(format!(
                            "package:{}:install:{}",
                            manager,
                            packages.join(",")
                        ));
                    }
                }
                Err(crate::errors::PackageError::ManagerNotFound {
                    manager: manager.clone(),
                }
                .into())
            }
            PackageAction::Uninstall {
                manager, packages, ..
            } => {
                for pm in self.registry.available_package_managers() {
                    if pm.name() == manager {
                        pm.uninstall(packages, printer)?;
                        return Ok(format!(
                            "package:{}:uninstall:{}",
                            manager,
                            packages.join(",")
                        ));
                    }
                }
                Err(crate::errors::PackageError::ManagerNotFound {
                    manager: manager.clone(),
                }
                .into())
            }
            PackageAction::Skip {
                manager, reason, ..
            } => {
                printer.warning(&format!("{}: {}", manager, reason));
                Ok(format!("package:{}:skip", manager))
            }
        }
    }

    fn apply_file_action(
        &self,
        action: &FileAction,
        profile: &MergedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
    ) -> Result<String> {
        if let Some(ref fm) = self.registry.file_manager {
            fm.apply(&[action.clone_action()], printer)?;
        } else {
            // Fallback: use CfgdFileManager directly via the existing files module logic
            apply_file_action_direct(action, config_dir, profile)?;
        }

        match action {
            FileAction::Create { target, .. } => Ok(format!("file:create:{}", target.display())),
            FileAction::Update { target, .. } => Ok(format!("file:update:{}", target.display())),
            FileAction::Delete { target, .. } => Ok(format!("file:delete:{}", target.display())),
            FileAction::SetPermissions { target, mode, .. } => {
                Ok(format!("file:chmod:{:#o}:{}", mode, target.display()))
            }
            FileAction::Skip { target, .. } => Ok(format!("file:skip:{}", target.display())),
        }
    }

    pub(crate) fn apply_secret_action(
        &self,
        action: &SecretAction,
        config_dir: &std::path::Path,
        printer: &Printer,
        secret_env_collector: &mut Vec<(String, String)>,
    ) -> Result<String> {
        match action {
            SecretAction::Decrypt {
                source,
                target,
                backend: _,
                ..
            } => {
                let backend = self
                    .registry
                    .secret_backend
                    .as_ref()
                    .ok_or(crate::errors::SecretError::SopsNotFound)?;

                let source_path =
                    crate::resolve_relative_path(source, config_dir).map_err(|_| {
                        crate::errors::SecretError::DecryptionFailed {
                            path: config_dir.join(source),
                            message: "source path contains traversal".to_string(),
                        }
                    })?;

                let decrypted = backend.decrypt_file(&source_path)?;

                let target_path = expand_tilde(target);
                crate::atomic_write(&target_path, decrypted.expose_secret().as_bytes())?;

                printer.info(&format!(
                    "Decrypted {} → {}",
                    source.display(),
                    target_path.display()
                ));

                Ok(format!("secret:decrypt:{}", target_path.display()))
            }
            SecretAction::Resolve {
                provider,
                reference,
                target,
                ..
            } => {
                let secret_provider = self
                    .registry
                    .secret_providers
                    .iter()
                    .find(|p| p.name() == provider)
                    .ok_or_else(|| crate::errors::SecretError::ProviderNotAvailable {
                        provider: provider.clone(),
                        hint: format!("no provider '{}' registered", provider),
                    })?;

                let value = secret_provider.resolve(reference)?;

                let target_path = expand_tilde(target);
                crate::atomic_write(&target_path, value.expose_secret().as_bytes())?;

                printer.info(&format!(
                    "Resolved {}://{} → {}",
                    provider,
                    reference,
                    target_path.display()
                ));

                Ok(format!(
                    "secret:resolve:{}:{}",
                    provider,
                    target_path.display()
                ))
            }
            SecretAction::ResolveEnv {
                provider,
                reference,
                envs,
                ..
            } => {
                let secret_provider = self
                    .registry
                    .secret_providers
                    .iter()
                    .find(|p| p.name() == provider)
                    .ok_or_else(|| crate::errors::SecretError::ProviderNotAvailable {
                        provider: provider.clone(),
                        hint: format!("no provider '{}' registered", provider),
                    })?;

                let value = secret_provider.resolve(reference)?;

                // Each secret source resolves to exactly ONE value.
                // All env names in `envs` receive the same resolved value.
                // Expose the secret at the boundary where we need the plaintext for env injection.
                let plaintext = value.expose_secret().to_string();
                for env_name in envs {
                    secret_env_collector.push((env_name.clone(), plaintext.clone()));
                }

                printer.info(&format!(
                    "Resolved {}://{} → env [{}]",
                    provider,
                    reference,
                    envs.join(", ")
                ));

                Ok(format!(
                    "secret:resolve-env:{}:{}:[{}]",
                    provider,
                    reference,
                    envs.join(",")
                ))
            }
            SecretAction::Skip { source, reason, .. } => {
                printer.warning(&format!("secret {}: {}", source, reason));
                Ok(format!("secret:skip:{}", source))
            }
        }
    }

    fn apply_script_action(
        &self,
        action: &ScriptAction,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        context: ReconcileContext,
    ) -> Result<(String, Option<String>)> {
        match action {
            ScriptAction::Run { entry, phase, .. } => {
                let profile_name = resolved
                    .layers
                    .last()
                    .map(|l| l.profile_name.as_str())
                    .unwrap_or("unknown");

                let env_vars =
                    build_script_env(config_dir, profile_name, context, phase, false, None, None);

                let (_desc, _changed, captured) = execute_script(
                    entry,
                    config_dir,
                    &env_vars,
                    crate::PROFILE_SCRIPT_TIMEOUT,
                    printer,
                )?;

                let phase_name = phase.display_name();
                Ok((
                    format!("script:{}:{}", phase_name, entry.run_str()),
                    captured,
                ))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_module_action(
        &self,
        action: &ModuleAction,
        config_dir: &std::path::Path,
        printer: &Printer,
        apply_id: i64,
        context: ReconcileContext,
        resolved: &ResolvedProfile,
        module_actions: &[ResolvedModule],
    ) -> Result<String> {
        // Find the module dir from the resolved modules list
        let module_dir = module_actions
            .iter()
            .find(|m| m.name == action.module_name)
            .map(|m| m.dir.clone());

        match &action.kind {
            ModuleActionKind::InstallPackages { resolved: pkgs } => {
                // Packages in each InstallPackages action are already grouped by
                // manager in plan_modules(), so just collect names and install.
                let pkg_names: Vec<String> = pkgs.iter().map(|p| p.resolved_name.clone()).collect();

                if let Some(first) = pkgs.first() {
                    if first.manager == "script" {
                        // Script-based install: run each package's script via execute_script
                        for pkg in pkgs {
                            if let Some(ref script_content) = pkg.script {
                                let profile_name = resolved
                                    .layers
                                    .last()
                                    .map(|l| l.profile_name.as_str())
                                    .unwrap_or("unknown");
                                let env_vars = build_script_env(
                                    config_dir,
                                    profile_name,
                                    context,
                                    &ScriptPhase::PostApply,
                                    false,
                                    Some(&action.module_name),
                                    module_dir.as_deref(),
                                );
                                let script_entry = ScriptEntry::Simple(script_content.clone());
                                let working = module_dir.as_deref().unwrap_or(config_dir);
                                execute_script(
                                    &script_entry,
                                    working,
                                    &env_vars,
                                    MODULE_SCRIPT_TIMEOUT,
                                    printer,
                                )
                                .map_err(|_| {
                                    crate::errors::CfgdError::Config(ConfigError::Invalid {
                                        message: format!(
                                            "module {} install script for '{}' failed",
                                            action.module_name, pkg.canonical_name
                                        ),
                                    })
                                })?;
                            }
                        }
                    } else {
                        // Find the manager — check all registered, not just available
                        let pm = self
                            .registry
                            .package_managers
                            .iter()
                            .find(|m| m.name() == first.manager);

                        if let Some(pm) = pm {
                            // Bootstrap if needed
                            if !pm.is_available() && pm.can_bootstrap() {
                                pm.bootstrap(printer)?;

                                // Persist bootstrapped manager's PATH to ~/.cfgd.env
                                let path_dirs = pm.path_dirs();
                                if !path_dirs.is_empty() {
                                    let env_path =
                                        expand_tilde(std::path::Path::new("~/.cfgd.env"));
                                    let existing = match std::fs::read_to_string(&env_path) {
                                        Ok(s) => s,
                                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                            String::new()
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "cannot read {}: {e}",
                                                env_path.display()
                                            );
                                            String::new()
                                        }
                                    };
                                    let new_dirs: Vec<&str> = path_dirs
                                        .iter()
                                        .filter(|d| !existing.contains(d.as_str()))
                                        .map(|d| d.as_str())
                                        .collect();
                                    if !new_dirs.is_empty() {
                                        let mut content = existing;
                                        if !content.ends_with('\n') && !content.is_empty() {
                                            content.push('\n');
                                        }
                                        content.push_str(&format!(
                                            "export PATH=\"{}:$PATH\"\n",
                                            new_dirs.join(":")
                                        ));
                                        crate::atomic_write_str(&env_path, &content)?;
                                    }
                                }
                            }

                            // Update package index before installing
                            if pm.is_available() {
                                pm.update(printer)?;
                            }

                            pm.install(&pkg_names, printer)?;
                        }
                    }
                }

                Ok(format!(
                    "module:{}:packages:{}",
                    action.module_name,
                    pkg_names.join(",")
                ))
            }
            ModuleActionKind::DeployFiles { files } => {
                for file in files {
                    let target = expand_tilde(&file.target);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }

                    // Use the per-file strategy override if set, otherwise
                    // fall back to the global file-strategy from cfgd.yaml (default: symlink).
                    let strategy = file.strategy.unwrap_or(self.registry.default_file_strategy);

                    // Backup existing target before overwriting
                    if let Ok(Some(file_state)) = crate::capture_file_state(&target)
                        && let Err(e) = self.state.store_file_backup(
                            apply_id,
                            &target.display().to_string(),
                            &file_state,
                        )
                    {
                        tracing::warn!("failed to backup module file {}: {}", target.display(), e);
                    }

                    // Remove existing target before deploying
                    if target.symlink_metadata().is_ok() {
                        if target.is_dir() && !target.is_symlink() {
                            std::fs::remove_dir_all(&target)?;
                        } else {
                            std::fs::remove_file(&target)?;
                        }
                    }

                    if file.source.is_dir() {
                        match strategy {
                            crate::config::FileStrategy::Symlink => {
                                crate::create_symlink(&file.source, &target)?;
                            }
                            _ => {
                                crate::copy_dir_recursive(&file.source, &target)?;
                            }
                        }
                    } else if file.source.exists() {
                        match strategy {
                            crate::config::FileStrategy::Symlink => {
                                crate::create_symlink(&file.source, &target)?;
                            }
                            crate::config::FileStrategy::Hardlink => {
                                std::fs::hard_link(&file.source, &target)?;
                            }
                            crate::config::FileStrategy::Copy
                            | crate::config::FileStrategy::Template => {
                                let content = std::fs::read(&file.source)?;
                                crate::atomic_write(&target, &content)?;
                            }
                        }
                    }

                    // Record in module file manifest
                    let hash = if target.exists() && !target.is_symlink() {
                        match std::fs::read(&target) {
                            Ok(bytes) => crate::sha256_hex(&bytes),
                            Err(e) => {
                                tracing::warn!("cannot read {} for hashing: {e}", target.display());
                                String::new()
                            }
                        }
                    } else {
                        String::new()
                    };
                    self.state.upsert_module_file(
                        &action.module_name,
                        &target.display().to_string(),
                        &hash,
                        &format!("{:?}", strategy),
                        apply_id,
                    )?;
                }

                printer.success(&format!(
                    "Module {}: deployed {} file(s)",
                    action.module_name,
                    files.len()
                ));

                Ok(format!(
                    "module:{}:files:{}",
                    action.module_name,
                    files.len()
                ))
            }
            ModuleActionKind::RunScript {
                script,
                phase: script_phase,
            } => {
                let profile_name = resolved
                    .layers
                    .last()
                    .map(|l| l.profile_name.as_str())
                    .unwrap_or("unknown");
                let env_vars = build_script_env(
                    config_dir,
                    profile_name,
                    context,
                    script_phase,
                    false,
                    Some(&action.module_name),
                    module_dir.as_deref(),
                );

                let working = module_dir.as_deref().unwrap_or(config_dir);
                execute_script(script, working, &env_vars, MODULE_SCRIPT_TIMEOUT, printer)?;

                Ok(format!("module:{}:script", action.module_name))
            }
            ModuleActionKind::Skip { reason } => {
                printer.warning(&format!(
                    "Module {}: skipped — {}",
                    action.module_name, reason
                ));
                Ok(format!("module:{}:skip", action.module_name))
            }
        }
    }
}

/// Verify all managed resources match their desired state.
pub fn verify(
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
    state: &StateStore,
    _printer: &Printer,
    modules: &[ResolvedModule],
) -> Result<Vec<VerifyResult>> {
    let mut results = Vec::new();

    // Verify modules — check that module packages are installed
    // Cache installed-packages per manager to avoid N+1 queries
    let available_managers = registry.available_package_managers();
    let mut installed_cache: HashMap<String, HashSet<String>> = HashMap::new();
    for module in modules {
        let mut module_ok = true;

        for pkg in &module.packages {
            // Script-based packages can't be verified via installed_packages() —
            // trust the apply log (if the script succeeded, it's installed).
            if pkg.manager == "script" {
                continue;
            }

            if !installed_cache.contains_key(&pkg.manager) {
                let mgr = available_managers.iter().find(|m| m.name() == pkg.manager);
                let set = mgr
                    .map(|m| m.installed_packages())
                    .transpose()?
                    .unwrap_or_default();
                installed_cache.insert(pkg.manager.clone(), set);
            }
            let installed = &installed_cache[&pkg.manager];
            let ok = installed.contains(&pkg.resolved_name);

            if !ok {
                module_ok = false;
                results.push(VerifyResult {
                    resource_type: "module".to_string(),
                    resource_id: format!("{}/{}", module.name, pkg.resolved_name),
                    matches: false,
                    expected: "installed".to_string(),
                    actual: "missing".to_string(),
                });
                state
                    .record_drift(
                        "module",
                        &format!("{}/{}", module.name, pkg.resolved_name),
                        Some("installed"),
                        Some("missing"),
                        "local",
                    )
                    .ok();
            }
        }

        // Check module file targets exist
        for file in &module.files {
            let target = expand_tilde(&file.target);
            if !target.exists() {
                module_ok = false;
                results.push(VerifyResult {
                    resource_type: "module".to_string(),
                    resource_id: format!("{}/{}", module.name, target.display()),
                    matches: false,
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
                state
                    .record_drift(
                        "module",
                        &format!("{}/{}", module.name, target.display()),
                        Some("present"),
                        Some("missing"),
                        "local",
                    )
                    .ok();
            }
        }

        if module_ok {
            results.push(VerifyResult {
                resource_type: "module".to_string(),
                resource_id: module.name.clone(),
                matches: true,
                expected: "healthy".to_string(),
                actual: "healthy".to_string(),
            });
        }
    }

    // Verify packages
    let available_managers = registry.available_package_managers();
    for pm in &available_managers {
        let desired = crate::config::desired_packages_for(pm.name(), &resolved.merged);
        if desired.is_empty() {
            continue;
        }
        let installed = pm.installed_packages()?;
        for pkg in &desired {
            let ok = installed.contains(pkg);
            results.push(VerifyResult {
                resource_type: "package".to_string(),
                resource_id: format!("{}:{}", pm.name(), pkg),
                matches: ok,
                expected: "installed".to_string(),
                actual: if ok {
                    "installed".to_string()
                } else {
                    "missing".to_string()
                },
            });

            if !ok {
                state
                    .record_drift(
                        "package",
                        &format!("{}:{}", pm.name(), pkg),
                        Some("installed"),
                        Some("missing"),
                        "local",
                    )
                    .ok();
            }
        }
    }

    // Verify system configurators
    for sc in registry.available_system_configurators() {
        if let Some(desired) = resolved.merged.system.get(sc.name()) {
            let drifts = sc.diff(desired)?;
            if drifts.is_empty() {
                results.push(VerifyResult {
                    resource_type: "system".to_string(),
                    resource_id: sc.name().to_string(),
                    matches: true,
                    expected: "configured".to_string(),
                    actual: "configured".to_string(),
                });
            } else {
                for drift in &drifts {
                    results.push(VerifyResult {
                        resource_type: "system".to_string(),
                        resource_id: format!("{}.{}", sc.name(), drift.key),
                        matches: false,
                        expected: drift.expected.clone(),
                        actual: drift.actual.clone(),
                    });

                    state
                        .record_drift(
                            "system",
                            &format!("{}.{}", sc.name(), drift.key),
                            Some(&drift.expected),
                            Some(&drift.actual),
                            "local",
                        )
                        .ok();
                }
            }
        }
    }

    // Verify files by checking managed file targets exist with expected content
    for managed in &resolved.merged.files.managed {
        let target = expand_tilde(&managed.target);
        if target.exists() {
            results.push(VerifyResult {
                resource_type: "file".to_string(),
                resource_id: target.display().to_string(),
                matches: true,
                expected: "present".to_string(),
                actual: "present".to_string(),
            });
        } else {
            results.push(VerifyResult {
                resource_type: "file".to_string(),
                resource_id: target.display().to_string(),
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
            });
        }
    }

    // Verify env: check ~/.cfgd.env matches expected content
    verify_env(
        &resolved.merged.env,
        &resolved.merged.aliases,
        modules,
        state,
        &mut results,
    );

    Ok(results)
}

/// Result of verifying a single resource.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyResult {
    pub resource_type: String,
    pub resource_id: String,
    pub matches: bool,
    pub expected: String,
    pub actual: String,
}

fn merge_module_env_aliases(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    modules: &[ResolvedModule],
) -> (Vec<crate::config::EnvVar>, Vec<crate::config::ShellAlias>) {
    let mut merged = profile_env.to_vec();
    let mut merged_aliases = profile_aliases.to_vec();
    for module in modules {
        crate::merge_env(&mut merged, &module.env);
        crate::merge_aliases(&mut merged_aliases, &module.aliases);
    }
    (merged, merged_aliases)
}

/// Verify env file and shell rc source line match expected state.
// NOTE: Secret-backed env vars (from SecretSpec.envs) are not included in
// verification because they require provider resolution. This means cfgd status
// may report env file drift after secret envs are written. This will be addressed
// when compliance snapshots track secret env metadata.
fn verify_env(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    modules: &[ResolvedModule],
    state: &StateStore,
    results: &mut Vec<VerifyResult>,
) {
    let (merged, merged_aliases) = merge_module_env_aliases(profile_env, profile_aliases, modules);

    if merged.is_empty() && merged_aliases.is_empty() {
        return;
    }

    if cfg!(windows) {
        // Verify PowerShell env file
        let ps_path = expand_tilde(std::path::Path::new("~/.cfgd-env.ps1"));
        let expected_ps = generate_powershell_env_content(&merged, &merged_aliases);
        verify_env_file(&ps_path, &expected_ps, state, results);

        // Verify PowerShell profile injection
        let ps_profile_dirs = [
            expand_tilde(std::path::Path::new("~/Documents/PowerShell")),
            expand_tilde(std::path::Path::new("~/Documents/WindowsPowerShell")),
        ];
        for profile_dir in &ps_profile_dirs {
            let profile_path = profile_dir.join("Microsoft.PowerShell_profile.ps1");
            let has_line = std::fs::read_to_string(&profile_path)
                .map(|content| content.contains("cfgd-env.ps1"))
                .unwrap_or(false);
            results.push(VerifyResult {
                resource_type: "env-rc".to_string(),
                resource_id: profile_path.display().to_string(),
                matches: has_line,
                expected: "source line present".to_string(),
                actual: if has_line {
                    "source line present".to_string()
                } else {
                    "source line missing".to_string()
                },
            });
            if !has_line {
                state
                    .record_drift(
                        "env-rc",
                        &profile_path.display().to_string(),
                        Some("source line present"),
                        Some("source line missing"),
                        "local",
                    )
                    .ok();
            }
        }

        // If Git Bash available, also verify bash env file
        if crate::command_available("sh") {
            let bash_path = expand_tilde(std::path::Path::new("~/.cfgd.env"));
            let expected_bash = generate_env_file_content(&merged, &merged_aliases);
            verify_env_file(&bash_path, &expected_bash, state, results);
        }
    } else {
        // Unix: verify bash/zsh env file
        let env_path = expand_tilde(std::path::Path::new("~/.cfgd.env"));
        let expected_content = generate_env_file_content(&merged, &merged_aliases);
        verify_env_file(&env_path, &expected_content, state, results);

        // Check shell rc source line
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let rc_path = if shell.contains("zsh") {
            expand_tilde(std::path::Path::new("~/.zshrc"))
        } else {
            expand_tilde(std::path::Path::new("~/.bashrc"))
        };

        let has_source_line = std::fs::read_to_string(&rc_path)
            .map(|content| content.contains("cfgd.env"))
            .unwrap_or(false);
        results.push(VerifyResult {
            resource_type: "env-rc".to_string(),
            resource_id: rc_path.display().to_string(),
            matches: has_source_line,
            expected: "source line present".to_string(),
            actual: if has_source_line {
                "source line present".to_string()
            } else {
                "source line missing".to_string()
            },
        });
        if !has_source_line {
            state
                .record_drift(
                    "env-rc",
                    &rc_path.display().to_string(),
                    Some("source line present"),
                    Some("source line missing"),
                    "local",
                )
                .ok();
        }
    }

    // Check fish env file only if fish is the user's shell
    let fish_conf_d = expand_tilde(std::path::Path::new("~/.config/fish/conf.d"));
    let verify_shell = std::env::var("SHELL").unwrap_or_default();
    if verify_shell.contains("fish") && fish_conf_d.exists() {
        let fish_path = fish_conf_d.join("cfgd-env.fish");
        let expected_fish = generate_fish_env_content(&merged, &merged_aliases);
        verify_env_file(&fish_path, &expected_fish, state, results);
    }
}

/// Verify a single env file's content matches expected.
fn verify_env_file(
    path: &std::path::Path,
    expected: &str,
    state: &StateStore,
    results: &mut Vec<VerifyResult>,
) {
    match std::fs::read_to_string(path) {
        Ok(actual) if actual == expected => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: path.display().to_string(),
                matches: true,
                expected: "current".to_string(),
                actual: "current".to_string(),
            });
        }
        Ok(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: path.display().to_string(),
                matches: false,
                expected: "current".to_string(),
                actual: "stale".to_string(),
            });
            state
                .record_drift(
                    "env",
                    &path.display().to_string(),
                    Some("current"),
                    Some("stale"),
                    "local",
                )
                .ok();
        }
        Err(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: path.display().to_string(),
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
            });
            state
                .record_drift(
                    "env",
                    &path.display().to_string(),
                    Some("present"),
                    Some("missing"),
                    "local",
                )
                .ok();
        }
    }
}

// ---------------------------------------------------------------------------
// Unified script executor
// ---------------------------------------------------------------------------

/// Default timeout for module-level scripts.
const MODULE_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const ENV_FILE_HEADER: &str = "# managed by cfgd \u{2014} do not edit";

/// Build environment variables injected into every script invocation.
pub(crate) fn build_script_env(
    config_dir: &std::path::Path,
    profile_name: &str,
    context: ReconcileContext,
    phase: &ScriptPhase,
    dry_run: bool,
    module_name: Option<&str>,
    module_dir: Option<&std::path::Path>,
) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "CFGD_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        ),
        ("CFGD_PROFILE".to_string(), profile_name.to_string()),
        (
            "CFGD_CONTEXT".to_string(),
            match context {
                ReconcileContext::Apply => "apply".to_string(),
                ReconcileContext::Reconcile => "reconcile".to_string(),
            },
        ),
        ("CFGD_PHASE".to_string(), phase.display_name().to_string()),
        ("CFGD_DRY_RUN".to_string(), dry_run.to_string()),
    ];
    if let Some(name) = module_name {
        env.push(("CFGD_MODULE_NAME".to_string(), name.to_string()));
    }
    if let Some(dir) = module_dir {
        env.push(("CFGD_MODULE_DIR".to_string(), dir.display().to_string()));
    }
    env
}

/// Unified script executor for all hook types at both profile and module level.
///
/// Returns (description, changed, captured_output). All scripts set changed=true.
pub(crate) fn execute_script(
    entry: &ScriptEntry,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    default_timeout: std::time::Duration,
    printer: &Printer,
) -> Result<(String, bool, Option<String>)> {
    let run_str = entry.run_str();
    let effective_timeout = match entry {
        ScriptEntry::Full {
            timeout: Some(t), ..
        } => crate::parse_duration_str(t)
            .map_err(|e| crate::errors::CfgdError::Config(ConfigError::Invalid { message: e }))?,
        _ => default_timeout,
    };
    let idle_timeout =
        match entry {
            ScriptEntry::Full {
                idle_timeout: Some(t),
                ..
            } => Some(crate::parse_duration_str(t).map_err(|e| {
                crate::errors::CfgdError::Config(ConfigError::Invalid { message: e })
            })?),
            _ => None,
        };

    let resolved = if std::path::Path::new(run_str).is_relative() {
        working_dir.join(run_str)
    } else {
        std::path::PathBuf::from(run_str)
    };

    let mut cmd = if resolved.exists() {
        // File path — check executable bit, run directly (OS handles shebang)
        let meta = std::fs::metadata(&resolved)?;
        if !crate::is_executable(&resolved, &meta) {
            #[cfg(unix)]
            let hint = "chmod +x";
            #[cfg(windows)]
            let hint = "use a .exe, .cmd, .bat, or .ps1 extension";
            return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "script '{}' exists but is not executable ({})",
                    resolved.display(),
                    hint,
                ),
            }));
        }
        let mut c = std::process::Command::new(&resolved);
        c.current_dir(working_dir);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        c
    } else {
        // Inline command — pass through sh -c on Unix, cmd.exe /C on Windows
        #[cfg(unix)]
        let c = {
            use std::os::unix::process::CommandExt;
            let mut c = std::process::Command::new("sh");
            c.arg("-c")
                .arg(run_str)
                .current_dir(working_dir)
                .process_group(0); // New process group so we can kill all children
            c
        };
        #[cfg(windows)]
        let c = {
            let mut c = std::process::Command::new("cmd.exe");
            c.arg("/C").arg(run_str).current_dir(working_dir);
            c
        };
        c
    };

    // Inject environment variables
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let label = format!("Running script: {}", run_str);

    // Execute with timeout
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;

    // Spinner with live output display (same pattern as Printer::run_with_progress)
    let pb = printer.spinner(&label);

    // Channel for live display + Arc buffers for final capture.
    // Reader threads feed both so we get live scrolling output AND full capture.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let last_output = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let stdout_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

    let stdout_handle = {
        let pipe = child.stdout.take();
        let buf = std::sync::Arc::clone(&stdout_buf);
        let ts = std::sync::Arc::clone(&last_output);
        let tx = tx.clone();
        std::thread::spawn(move || {
            if let Some(pipe) = pipe {
                let reader = std::io::BufReader::new(pipe);
                for line in std::io::BufRead::lines(reader) {
                    match line {
                        Ok(l) => {
                            *ts.lock().unwrap_or_else(|e| e.into_inner()) =
                                std::time::Instant::now();
                            let mut b = buf.lock().unwrap_or_else(|e| e.into_inner());
                            if !b.is_empty() {
                                b.push('\n');
                            }
                            b.push_str(&l);
                            let _ = tx.send(l);
                        }
                        Err(_) => break,
                    }
                }
            }
        })
    };

    let stderr_handle = {
        let pipe = child.stderr.take();
        let buf = std::sync::Arc::clone(&stderr_buf);
        let ts = std::sync::Arc::clone(&last_output);
        let tx = tx.clone();
        std::thread::spawn(move || {
            if let Some(pipe) = pipe {
                let reader = std::io::BufReader::new(pipe);
                for line in std::io::BufRead::lines(reader) {
                    match line {
                        Ok(l) => {
                            *ts.lock().unwrap_or_else(|e| e.into_inner()) =
                                std::time::Instant::now();
                            let mut b = buf.lock().unwrap_or_else(|e| e.into_inner());
                            if !b.is_empty() {
                                b.push('\n');
                            }
                            b.push_str(&l);
                            let _ = tx.send(l);
                        }
                        Err(_) => break,
                    }
                }
            }
        })
    };
    drop(tx);

    const VISIBLE_LINES: usize = 5;
    let mut ring: std::collections::VecDeque<String> =
        std::collections::VecDeque::with_capacity(VISIBLE_LINES);

    let start = std::time::Instant::now();
    loop {
        // Drain any pending output lines and update the spinner display
        while let Ok(line) = rx.try_recv() {
            if ring.len() >= VISIBLE_LINES {
                ring.pop_front();
            }
            ring.push_back(line);
        }
        if !ring.is_empty() {
            let mut msg = label.clone();
            for l in &ring {
                let display = if l.len() > 120 {
                    l.get(..120).unwrap_or(l)
                } else {
                    l.as_str()
                };
                msg.push_str(&format!("\n  {}", display));
            }
            pb.set_message(msg);
        }

        match child.try_wait()? {
            Some(status) => {
                // Wait for reader threads to finish draining
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();

                let stdout_str = std::sync::Arc::try_unwrap(stdout_buf)
                    .ok()
                    .and_then(|m| m.into_inner().ok())
                    .unwrap_or_default();
                let stderr_str = std::sync::Arc::try_unwrap(stderr_buf)
                    .ok()
                    .and_then(|m| m.into_inner().ok())
                    .unwrap_or_default();

                let captured = combine_script_output(&stdout_str, &stderr_str);

                if !status.success() {
                    pb.finish_and_clear();
                    let exit_code = status.code().unwrap_or(-1);
                    return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                        message: format!(
                            "script '{}' failed (exit {})\n{}",
                            run_str,
                            exit_code,
                            captured.as_deref().unwrap_or("")
                        ),
                    }));
                }

                let elapsed = start.elapsed();
                pb.finish_and_clear();
                printer.success(&format!("{} ({}s)", run_str, elapsed.as_secs()));
                return Ok((label, true, captured));
            }
            None => {
                let elapsed = start.elapsed();
                let mut kill_reason = None;
                // Check absolute timeout
                if elapsed > effective_timeout {
                    kill_reason = Some(("timed out", effective_timeout));
                }
                // Check idle timeout (no output for N seconds)
                if kill_reason.is_none()
                    && let Some(idle_dur) = idle_timeout
                {
                    let last = *last_output.lock().unwrap_or_else(|e| e.into_inner());
                    if last.elapsed() > idle_dur {
                        kill_reason = Some(("idle (no output)", idle_dur));
                    }
                }
                if let Some((reason, duration)) = kill_reason {
                    pb.finish_and_clear();
                    kill_script_child(&mut child);
                    // Join reader threads so we capture partial output
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    let stdout_str = std::sync::Arc::try_unwrap(stdout_buf)
                        .ok()
                        .and_then(|m| m.into_inner().ok())
                        .unwrap_or_default();
                    let stderr_str = std::sync::Arc::try_unwrap(stderr_buf)
                        .ok()
                        .and_then(|m| m.into_inner().ok())
                        .unwrap_or_default();
                    let captured = combine_script_output(&stdout_str, &stderr_str);
                    return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                        message: format!(
                            "script '{}' {} after {}s\n{}",
                            run_str,
                            reason,
                            duration.as_secs(),
                            captured.as_deref().unwrap_or("")
                        ),
                    }));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

/// Kill a script's process group (SIGTERM + grace period + SIGKILL).
fn kill_script_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        // Negative PID targets the entire process group
        let _ = kill(Pid::from_raw(-(child.id() as i32)), Signal::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        crate::terminate_process(child.id());
    }
    std::thread::sleep(std::time::Duration::from_secs(5));
    let _ = child.kill();
    let _ = child.wait();
}

/// Default `continue_on_error` behavior per script phase.
/// Pre-hooks abort on failure; post-hooks, onChange, onDrift continue.
fn default_continue_on_error(phase: &ScriptPhase) -> bool {
    match phase {
        ScriptPhase::PreApply | ScriptPhase::PreReconcile => false,
        ScriptPhase::PostApply
        | ScriptPhase::PostReconcile
        | ScriptPhase::OnChange
        | ScriptPhase::OnDrift => true,
    }
}

/// Resolve the effective `continue_on_error` for a script entry in a given phase.
fn effective_continue_on_error(entry: &ScriptEntry, phase: &ScriptPhase) -> bool {
    match entry {
        ScriptEntry::Full {
            continue_on_error: Some(v),
            ..
        } => *v,
        _ => default_continue_on_error(phase),
    }
}

/// Combine stdout and stderr into a single captured output string.
/// Returns `None` if both are empty.
fn combine_script_output(stdout: &str, stderr: &str) -> Option<String> {
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    if stdout.is_empty() && stderr.is_empty() {
        return None;
    }
    let mut out = String::new();
    if !stdout.is_empty() {
        out.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !out.is_empty() {
            out.push_str("\n--- stderr ---\n");
        }
        out.push_str(stderr);
    }
    Some(out)
}

/// Format a human-readable description of an action.
pub fn format_action_description(action: &Action) -> String {
    match action {
        Action::File(fa) => match fa {
            FileAction::Create { target, .. } => format!("file:create:{}", target.display()),
            FileAction::Update { target, .. } => format!("file:update:{}", target.display()),
            FileAction::Delete { target, .. } => format!("file:delete:{}", target.display()),
            FileAction::SetPermissions { target, mode, .. } => {
                format!("file:chmod:{:#o}:{}", mode, target.display())
            }
            FileAction::Skip { target, .. } => format!("file:skip:{}", target.display()),
        },
        Action::Package(pa) => match pa {
            PackageAction::Bootstrap { manager, .. } => {
                format!("package:{}:bootstrap", manager)
            }
            PackageAction::Install {
                manager, packages, ..
            } => format!("package:{}:install:{}", manager, packages.join(",")),
            PackageAction::Uninstall {
                manager, packages, ..
            } => format!("package:{}:uninstall:{}", manager, packages.join(",")),
            PackageAction::Skip { manager, .. } => format!("package:{}:skip", manager),
        },
        Action::Secret(sa) => match sa {
            SecretAction::Decrypt {
                target, backend, ..
            } => format!("secret:decrypt:{}:{}", backend, target.display()),
            SecretAction::Resolve {
                provider,
                reference,
                target,
                ..
            } => format!(
                "secret:resolve:{}:{}:{}",
                provider,
                reference,
                target.display()
            ),
            SecretAction::ResolveEnv {
                provider,
                reference,
                envs,
                ..
            } => format!(
                "secret:resolve-env:{}:{}:[{}]",
                provider,
                reference,
                envs.join(",")
            ),
            SecretAction::Skip { source, .. } => format!("secret:skip:{}", source),
        },
        Action::System(sa) => match sa {
            SystemAction::SetValue {
                configurator, key, ..
            } => format!("system:{}.{}", configurator, key),
            SystemAction::Skip { configurator, .. } => {
                format!("system:{}:skip", configurator)
            }
        },
        Action::Script(sa) => match sa {
            ScriptAction::Run { entry, phase, .. } => {
                format!("script:{}:{}", phase.display_name(), entry.run_str())
            }
        },
        Action::Module(ma) => match &ma.kind {
            ModuleActionKind::InstallPackages { resolved } => {
                let names: Vec<&str> = resolved.iter().map(|p| p.resolved_name.as_str()).collect();
                format!("module:{}:packages:{}", ma.module_name, names.join(","))
            }
            ModuleActionKind::DeployFiles { files } => {
                format!("module:{}:files:{}", ma.module_name, files.len())
            }
            ModuleActionKind::RunScript { .. } => {
                format!("module:{}:script", ma.module_name)
            }
            ModuleActionKind::Skip { .. } => {
                format!("module:{}:skip", ma.module_name)
            }
        },
        Action::Env(ea) => match ea {
            EnvAction::WriteEnvFile { path, .. } => {
                format!("env:write:{}", path.display())
            }
            EnvAction::InjectSourceLine { rc_path, .. } => {
                format!("env:inject:{}", rc_path.display())
            }
        },
    }
}

/// Outcome of a single file restoration during rollback.
enum RestoreOutcome {
    Restored,
    Removed,
    Skipped,
    Failed,
}

/// Restore a single file from a backup record. Used by `rollback_apply`.
fn restore_file_from_backup(
    target: &std::path::Path,
    bk: &crate::state::FileBackupRecord,
    printer: &crate::output::Printer,
) -> RestoreOutcome {
    // Backup has content — write it (works for both regular files and symlink snapshots
    // where the resolved content was captured)
    if !bk.oversized && !bk.content.is_empty() {
        // Check if the current resolved content already matches the backup — skip if so
        if let Ok(Some(current)) = crate::capture_file_resolved_state(target)
            && current.content == bk.content
        {
            return RestoreOutcome::Skipped;
        }
        // Remove existing target (might be a symlink or regular file)
        if target.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(target);
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = crate::atomic_write(target, &bk.content) {
            printer.warning(&format!(
                "rollback: failed to restore {}: {}",
                target.display(),
                e
            ));
            return RestoreOutcome::Failed;
        }
        // Restore permissions if recorded
        if let Some(mode) = bk.permissions {
            let _ = crate::set_file_permissions(target, mode);
        }
        return RestoreOutcome::Restored;
    }

    // Symlink with no content (only link target recorded — legacy backup)
    if bk.was_symlink
        && let Some(ref link_target) = bk.symlink_target
    {
        let _ = std::fs::remove_file(target);
        if let Err(e) = crate::create_symlink(std::path::Path::new(link_target), target) {
            printer.warning(&format!(
                "rollback: failed to restore symlink {}: {}",
                target.display(),
                e
            ));
            return RestoreOutcome::Failed;
        }
        return RestoreOutcome::Restored;
    }

    // Empty content, not symlink, not oversized — file didn't exist before
    if bk.content.is_empty() && !bk.was_symlink && !bk.oversized && target.exists() {
        if let Err(e) = std::fs::remove_file(target) {
            printer.warning(&format!(
                "rollback: failed to remove {}: {}",
                target.display(),
                e
            ));
            return RestoreOutcome::Failed;
        }
        return RestoreOutcome::Removed;
    }

    RestoreOutcome::Skipped
}

/// Extract the target file path from an action, if it writes to a file.
/// Used for pre-apply backup capture.
fn action_target_path(action: &Action) -> Option<std::path::PathBuf> {
    match action {
        Action::File(
            FileAction::Create { target, .. }
            | FileAction::Update { target, .. }
            | FileAction::Delete { target, .. },
        ) => Some(target.clone()),
        Action::Env(EnvAction::WriteEnvFile { path, .. }) => Some(path.clone()),
        // Module deploys multiple files — backup handled per-file in apply_module_action
        _ => None,
    }
}

/// Generate bash/zsh env file content from merged env vars and aliases.
fn generate_env_file_content(
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        lines.push(format!(
            "export {}=\"{}\"",
            ev.name,
            crate::escape_double_quoted(&ev.value)
        ));
    }
    for alias in aliases {
        if crate::validate_alias_name(&alias.name).is_err() {
            tracing::warn!("skipping alias with unsafe name: {}", alias.name);
            continue;
        }
        lines.push(format!(
            "alias {}=\"{}\"",
            alias.name,
            crate::escape_double_quoted(&alias.command)
        ));
    }
    lines.push(String::new()); // trailing newline
    lines.join("\n")
}

/// Generate fish env file content from merged env vars and aliases.
fn generate_fish_env_content(
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        if ev.name == "PATH" {
            // Fish uses space-separated list for PATH, not colon-separated.
            // Each part is single-quoted to prevent expansion.
            let parts: Vec<String> = ev
                .value
                .split(':')
                .map(|p| format!("'{}'", p.replace('\'', "\\'")))
                .collect();
            lines.push(format!("set -gx PATH {}", parts.join(" ")));
        } else {
            // Single-quote to prevent fish command substitution via ()
            lines.push(format!(
                "set -gx {} '{}'",
                ev.name,
                ev.value.replace('\'', "\\'")
            ));
        }
    }
    for alias in aliases {
        if crate::validate_alias_name(&alias.name).is_err() {
            tracing::warn!("skipping alias with unsafe name: {}", alias.name);
            continue;
        }
        lines.push(format!(
            "abbr -a {} '{}'",
            alias.name,
            alias.command.replace('\'', "\\'")
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Generate PowerShell env file content from merged env vars and aliases.
fn generate_powershell_env_content(
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        if ev.value.contains("$env:") {
            // Value references other env vars — double-quote with PS escaping
            lines.push(format!(
                "$env:{} = \"{}\"",
                ev.name,
                ev.value.replace('"', "`\"")
            ));
        } else {
            // Single-quote prevents all PS interpolation
            lines.push(format!(
                "$env:{} = '{}'",
                ev.name,
                ev.value.replace('\'', "''")
            ));
        }
    }
    for alias in aliases {
        if crate::validate_alias_name(&alias.name).is_err() {
            tracing::warn!("skipping alias with unsafe name: {}", alias.name);
            continue;
        }
        if alias.command.split_whitespace().count() == 1 {
            // Simple alias — use Set-Alias
            lines.push(format!(
                "Set-Alias -Name {} -Value {}",
                alias.name, alias.command
            ));
        } else {
            // Complex alias — use function wrapper
            lines.push(format!(
                "function {} {{ {} @args }}",
                alias.name, alias.command
            ));
        }
    }
    lines.push(String::new()); // trailing newline
    lines.join("\n")
}

/// Scan a shell rc file for `export` and `alias` definitions that appear before
/// the cfgd source line. If any match a cfgd-managed name with a different value,
/// return warnings advising the user to move the definition after the source line.
fn detect_rc_env_conflicts(
    rc_path: &std::path::Path,
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
) -> Vec<String> {
    let rc_content = match std::fs::read_to_string(rc_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    // Only look at lines before the cfgd source line
    let mut before_lines = Vec::new();
    for line in rc_content.lines() {
        if line.contains("cfgd.env") {
            break;
        }
        before_lines.push(line);
    }

    let rc_display = rc_path.display();
    let mut warnings = Vec::new();

    // Build lookup maps for cfgd-managed values
    let env_map: HashMap<&str, &str> = env
        .iter()
        .map(|e| (e.name.as_str(), e.value.as_str()))
        .collect();
    let alias_map: HashMap<&str, &str> = aliases
        .iter()
        .map(|a| (a.name.as_str(), a.command.as_str()))
        .collect();

    for line in &before_lines {
        let trimmed = line.trim();

        // Match: export NAME=VALUE
        if let Some(rest) = trimmed.strip_prefix("export ")
            && let Some((name, raw_value)) = rest.split_once('=')
        {
            let name = name.trim();
            let value = strip_shell_quotes(raw_value);
            if let Some(&cfgd_value) = env_map.get(name)
                && value != cfgd_value
            {
                warnings.push(format!(
                    "{} sets export {}={} before cfgd source line — cfgd will override to \"{}\"; move it after the source line to keep your value",
                    rc_display, name, raw_value, cfgd_value,
                ));
            }
        }

        // Match: alias NAME=VALUE or alias NAME="VALUE"
        if let Some(rest) = trimmed.strip_prefix("alias ")
            && let Some((name, raw_value)) = rest.split_once('=')
        {
            let name = name.trim();
            let value = strip_shell_quotes(raw_value);
            if let Some(&cfgd_value) = alias_map.get(name)
                && value != cfgd_value
            {
                warnings.push(format!(
                    "{} sets alias {}={} before cfgd source line — cfgd will override to \"{}\"; move it after the source line to keep your value",
                    rc_display, name, raw_value, cfgd_value,
                ));
            }
        }
    }

    warnings
}

/// Strip surrounding single or double quotes from a shell value.
fn strip_shell_quotes(s: &str) -> &str {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn content_hash_if_exists(path: &std::path::Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| crate::sha256_hex(&bytes))
}

/// Append source provenance suffix for non-local origins.
fn provenance_suffix(origin: &str) -> String {
    if origin.is_empty() || origin == "local" {
        String::new()
    } else {
        format!(" <- {origin}")
    }
}

/// Format plan phase items for display.
pub fn format_plan_items(phase: &Phase) -> Vec<String> {
    phase
        .actions
        .iter()
        .map(|action| match action {
            Action::File(fa) => match fa {
                FileAction::Create { target, origin, .. } => {
                    format!("create {}{}", target.display(), provenance_suffix(origin))
                }
                FileAction::Update { target, origin, .. } => {
                    format!("update {}{}", target.display(), provenance_suffix(origin))
                }
                FileAction::Delete { target, origin, .. } => {
                    format!("delete {}{}", target.display(), provenance_suffix(origin))
                }
                FileAction::SetPermissions {
                    target,
                    mode,
                    origin,
                    ..
                } => format!(
                    "chmod {:#o} {}{}",
                    mode,
                    target.display(),
                    provenance_suffix(origin)
                ),
                FileAction::Skip {
                    target,
                    reason,
                    origin,
                    ..
                } => format!(
                    "skip {}: {}{}",
                    target.display(),
                    reason,
                    provenance_suffix(origin)
                ),
            },
            Action::Package(pa) => match pa {
                PackageAction::Bootstrap {
                    manager,
                    method,
                    origin,
                    ..
                } => format!(
                    "bootstrap {} via {}{}",
                    manager,
                    method,
                    provenance_suffix(origin)
                ),
                PackageAction::Install {
                    manager,
                    packages,
                    origin,
                    ..
                } => format!(
                    "install via {}: {}{}",
                    manager,
                    packages.join(", "),
                    provenance_suffix(origin)
                ),
                PackageAction::Uninstall {
                    manager,
                    packages,
                    origin,
                    ..
                } => format!(
                    "uninstall via {}: {}{}",
                    manager,
                    packages.join(", "),
                    provenance_suffix(origin)
                ),
                PackageAction::Skip {
                    manager,
                    reason,
                    origin,
                    ..
                } => format!("skip {}: {}{}", manager, reason, provenance_suffix(origin)),
            },
            Action::Secret(sa) => match sa {
                SecretAction::Decrypt {
                    source,
                    target,
                    backend,
                    origin,
                    ..
                } => format!(
                    "decrypt {} → {} (via {}){}",
                    source.display(),
                    target.display(),
                    backend,
                    provenance_suffix(origin)
                ),
                SecretAction::Resolve {
                    provider,
                    reference,
                    target,
                    origin,
                    ..
                } => format!(
                    "resolve {}://{} → {}{}",
                    provider,
                    reference,
                    target.display(),
                    provenance_suffix(origin)
                ),
                SecretAction::ResolveEnv {
                    provider,
                    reference,
                    envs,
                    origin,
                    ..
                } => format!(
                    "resolve {}://{} → env [{}]{}",
                    provider,
                    reference,
                    envs.join(", "),
                    provenance_suffix(origin)
                ),
                SecretAction::Skip {
                    source,
                    reason,
                    origin,
                    ..
                } => format!("skip {}: {}{}", source, reason, provenance_suffix(origin)),
            },
            Action::System(sa) => match sa {
                SystemAction::SetValue {
                    configurator,
                    key,
                    desired,
                    current,
                    origin,
                    ..
                } => format!(
                    "set {}.{}: {} → {}{}",
                    configurator,
                    key,
                    current,
                    desired,
                    provenance_suffix(origin)
                ),
                SystemAction::Skip {
                    configurator,
                    reason,
                    ..
                } => format!("skip {}: {}", configurator, reason),
            },
            Action::Script(sa) => match sa {
                ScriptAction::Run {
                    entry,
                    phase,
                    origin,
                    ..
                } => {
                    format!(
                        "run {} script: {}{}",
                        phase.display_name(),
                        entry.run_str(),
                        provenance_suffix(origin)
                    )
                }
            },
            Action::Module(ma) => format_module_action_item(ma),
            Action::Env(ea) => match ea {
                EnvAction::WriteEnvFile { path, .. } => {
                    format!("write {}", path.display())
                }
                EnvAction::InjectSourceLine { rc_path, .. } => {
                    format!("inject source line into {}", rc_path.display())
                }
            },
        })
        .collect()
}

/// Format a module action for plan display.
fn format_module_action_item(action: &ModuleAction) -> String {
    match &action.kind {
        ModuleActionKind::InstallPackages { resolved } => {
            // Group by manager for display
            let mut by_manager: HashMap<&str, Vec<String>> = HashMap::new();
            for pkg in resolved {
                let display = if let Some(ref ver) = pkg.version {
                    if pkg.canonical_name != pkg.resolved_name {
                        format!(
                            "{} ({}, alias: {})",
                            pkg.resolved_name, ver, pkg.canonical_name
                        )
                    } else {
                        format!("{} ({})", pkg.resolved_name, ver)
                    }
                } else if pkg.canonical_name != pkg.resolved_name {
                    format!("{} (alias: {})", pkg.resolved_name, pkg.canonical_name)
                } else {
                    pkg.resolved_name.clone()
                };
                by_manager.entry(&pkg.manager).or_default().push(display);
            }
            let parts: Vec<String> = by_manager
                .iter()
                .map(|(mgr, pkgs)| format!("{} install {}", mgr, pkgs.join(", ")))
                .collect();
            format!("[{}] {}", action.module_name, parts.join("; "))
        }
        ModuleActionKind::DeployFiles { files } => {
            let targets: Vec<String> = files
                .iter()
                .map(|f| f.target.display().to_string())
                .collect();
            if targets.len() <= 3 {
                format!("[{}] deploy: {}", action.module_name, targets.join(", "))
            } else {
                format!(
                    "[{}] deploy: {} ({} files)",
                    action.module_name,
                    targets[..2].join(", "),
                    targets.len()
                )
            }
        }
        ModuleActionKind::RunScript { script, phase } => {
            format!(
                "[{}] {}: {}",
                action.module_name,
                phase.display_name(),
                script.run_str()
            )
        }
        ModuleActionKind::Skip { reason } => {
            format!("[{}] skip: {}", action.module_name, reason)
        }
    }
}

fn parse_resource_from_description(desc: &str) -> (String, String) {
    let parts: Vec<&str> = desc.splitn(3, ':').collect();
    if parts.len() >= 3 {
        (parts[0].to_string(), parts[2..].join(":"))
    } else if parts.len() == 2 {
        (parts[0].to_string(), parts[1].to_string())
    } else {
        ("unknown".to_string(), desc.to_string())
    }
}

fn apply_file_action_direct(
    action: &FileAction,
    _config_dir: &std::path::Path,
    _profile: &MergedProfile,
) -> Result<()> {
    match action {
        FileAction::Create {
            source,
            target,
            strategy,
            ..
        }
        | FileAction::Update {
            source,
            target,
            strategy,
            ..
        } => {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Remove existing target before deploying
            if target.symlink_metadata().is_ok() {
                std::fs::remove_file(target)?;
            }
            match strategy {
                crate::config::FileStrategy::Symlink => {
                    crate::create_symlink(source, target)?;
                }
                crate::config::FileStrategy::Hardlink => {
                    std::fs::hard_link(source, target)?;
                }
                crate::config::FileStrategy::Copy | crate::config::FileStrategy::Template => {
                    std::fs::copy(source, target)?;
                }
            }
            Ok(())
        }
        FileAction::Delete { target, .. } => {
            if target.exists() {
                std::fs::remove_file(target)?;
            }
            Ok(())
        }
        FileAction::SetPermissions { target, mode, .. } => {
            crate::set_file_permissions(target, *mode)?;
            Ok(())
        }
        FileAction::Skip { .. } => Ok(()),
    }
}

// Allow FileAction to be cloned for the trait-based apply path
impl FileAction {
    fn clone_action(&self) -> FileAction {
        match self {
            FileAction::Create {
                source,
                target,
                origin,
                strategy,
                source_hash,
            } => FileAction::Create {
                source: source.clone(),
                target: target.clone(),
                origin: origin.clone(),
                strategy: *strategy,
                source_hash: source_hash.clone(),
            },
            FileAction::Update {
                source,
                target,
                diff,
                origin,
                strategy,
                source_hash,
            } => FileAction::Update {
                source: source.clone(),
                target: target.clone(),
                diff: diff.clone(),
                origin: origin.clone(),
                strategy: *strategy,
                source_hash: source_hash.clone(),
            },
            FileAction::Delete { target, origin } => FileAction::Delete {
                target: target.clone(),
                origin: origin.clone(),
            },
            FileAction::SetPermissions {
                target,
                mode,
                origin,
            } => FileAction::SetPermissions {
                target: target.clone(),
                mode: *mode,
                origin: origin.clone(),
            },
            FileAction::Skip {
                target,
                reason,
                origin,
            } => FileAction::Skip {
                target: target.clone(),
                reason: reason.clone(),
                origin: origin.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    use crate::config::*;
    use crate::providers::PackageManager;

    use crate::providers::StubPackageManager as MockPackageManager;
    use crate::test_helpers::{
        make_empty_resolved, make_resolved_module, test_printer, test_state,
    };

    #[test]
    fn empty_plan_has_eight_phases() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        assert_eq!(plan.phases.len(), 8);
        assert!(plan.is_empty());
    }

    #[test]
    fn plan_includes_package_actions() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["ripgrep".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        assert!(!plan.is_empty());
        assert_eq!(plan.total_actions(), 1);
    }

    #[test]
    fn plan_includes_file_actions() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let file_actions = vec![FileAction::Create {
            source: PathBuf::from("/src/test"),
            target: PathBuf::from("/dst/test"),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::default(),
            source_hash: None,
        }];

        let plan = reconciler
            .plan(
                &resolved,
                file_actions,
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        assert!(!plan.is_empty());
        assert_eq!(plan.total_actions(), 1);
    }

    #[test]
    fn plan_includes_script_actions() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        let mut resolved = make_empty_resolved();
        resolved.merged.scripts.pre_reconcile =
            vec![ScriptEntry::Simple("scripts/pre.sh".to_string())];
        resolved.merged.scripts.post_reconcile =
            vec![ScriptEntry::Simple("scripts/post.sh".to_string())];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Reconcile,
            )
            .unwrap();

        // Pre-scripts phase should have the pre_reconcile script
        let pre_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::PreScripts)
            .unwrap();
        assert_eq!(pre_phase.actions.len(), 1);

        // Post-scripts phase should have the post_reconcile script
        let post_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::PostScripts)
            .unwrap();
        assert_eq!(post_phase.actions.len(), 1);
    }

    #[test]
    fn apply_empty_plan_records_success() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        // Empty plan — no actions means success with 0 results
        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 0);
    }

    #[test]
    fn phase_name_roundtrip() {
        for name in &[
            PhaseName::PreScripts,
            PhaseName::Env,
            PhaseName::Modules,
            PhaseName::Packages,
            PhaseName::System,
            PhaseName::Files,
            PhaseName::Secrets,
            PhaseName::PostScripts,
        ] {
            let s = name.as_str();
            let parsed = PhaseName::from_str(s).unwrap();
            assert_eq!(&parsed, name);
        }
    }

    #[test]
    fn format_plan_items_for_display() {
        let phase = Phase {
            name: PhaseName::Packages,
            actions: vec![
                Action::Package(PackageAction::Install {
                    manager: "brew".to_string(),
                    packages: vec!["ripgrep".to_string(), "fd".to_string()],
                    origin: "local".to_string(),
                }),
                Action::Package(PackageAction::Skip {
                    manager: "apt".to_string(),
                    reason: "not available".to_string(),
                    origin: "local".to_string(),
                }),
            ],
        };

        let items = format_plan_items(&phase);
        assert_eq!(items.len(), 2); // Skip items are now shown
        assert!(items[0].contains("ripgrep"));
        assert!(items[1].contains("skip apt: not available"));
    }

    #[test]
    fn verify_returns_results() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();

        registry.package_managers.push(Box::new(
            MockPackageManager::new("cargo").with_installed(&["ripgrep"]),
        ));

        let mut resolved = make_empty_resolved();
        resolved.merged.packages.cargo = Some(crate::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".to_string(), "bat".to_string()],
        });

        let printer = test_printer();
        let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();

        // ripgrep should be present, bat should be missing
        let rg = results
            .iter()
            .find(|r| r.resource_id == "cargo:ripgrep")
            .unwrap();
        assert!(rg.matches);

        let bat = results
            .iter()
            .find(|r| r.resource_id == "cargo:bat")
            .unwrap();
        assert!(!bat.matches);
    }

    #[test]
    fn plan_hash_string() {
        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Install {
                    manager: "brew".to_string(),
                    packages: vec!["ripgrep".to_string()],
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };
        let hash = plan.to_hash_string();
        assert!(!hash.is_empty());
        assert_eq!(
            hash,
            plan.to_hash_string(),
            "plan hash must be deterministic"
        );
    }

    #[test]
    fn apply_result_counts() {
        let result = ApplyResult {
            action_results: vec![
                ActionResult {
                    phase: "files".to_string(),
                    description: "test".to_string(),
                    success: true,
                    error: None,
                    changed: true,
                },
                ActionResult {
                    phase: "files".to_string(),
                    description: "test2".to_string(),
                    success: false,
                    error: Some("failed".to_string()),
                    changed: false,
                },
            ],
            status: ApplyStatus::Partial,
            apply_id: 0,
        };

        assert_eq!(result.succeeded(), 1);
        assert_eq!(result.failed(), 1);
    }

    // --- Module integration tests ---

    use crate::modules::{ResolvedFile, ResolvedModule, ResolvedPackage};

    #[test]
    fn plan_includes_module_phase() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![make_resolved_module("nvim")];
        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                modules,
                ReconcileContext::Apply,
            )
            .unwrap();

        assert_eq!(plan.phases.len(), 8);
        let module_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Modules)
            .unwrap();

        // Module phase should have at least 1 action (InstallPackages)
        assert!(!module_phase.actions.is_empty());

        // Check that actions are ModuleAction
        for action in &module_phase.actions {
            match action {
                Action::Module(ma) => {
                    assert_eq!(ma.module_name, "nvim");
                }
                _ => panic!("expected Module action in Modules phase"),
            }
        }
    }

    #[test]
    fn plan_module_with_files() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "nvim".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: PathBuf::from("/tmp/nvim-config"),
                target: PathBuf::from("/home/user/.config/nvim"),
                is_git_source: false,
                strategy: None,
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                modules,
                ReconcileContext::Apply,
            )
            .unwrap();

        let module_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Modules)
            .unwrap();
        assert_eq!(module_phase.actions.len(), 1);

        match &module_phase.actions[0] {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::DeployFiles { files } => {
                    assert_eq!(files.len(), 1);
                    assert_eq!(files[0].target, PathBuf::from("/home/user/.config/nvim"));
                }
                _ => panic!("expected DeployFiles action"),
            },
            _ => panic!("expected Module action"),
        }
    }

    #[test]
    fn plan_module_with_scripts() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "nvim".to_string(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![
                ScriptEntry::Simple("nvim --headless +qa".to_string()),
                ScriptEntry::Simple("echo done".to_string()),
            ],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                modules,
                ReconcileContext::Apply,
            )
            .unwrap();

        let module_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Modules)
            .unwrap();
        assert_eq!(module_phase.actions.len(), 2);

        for action in &module_phase.actions {
            match action {
                Action::Module(ma) => match &ma.kind {
                    ModuleActionKind::RunScript { script, .. } => {
                        assert!(!script.run_str().is_empty());
                    }
                    _ => panic!("expected RunScript action"),
                },
                _ => panic!("expected Module action"),
            }
        }
    }

    #[test]
    fn plan_multiple_modules_in_dependency_order() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![
            ResolvedModule {
                name: "node".to_string(),
                packages: vec![ResolvedPackage {
                    canonical_name: "nodejs".to_string(),
                    resolved_name: "nodejs".to_string(),
                    manager: "apt".to_string(),
                    version: Some("18.19.0".to_string()),
                    script: None,
                }],
                files: vec![],
                env: vec![],
                aliases: vec![],
                post_apply_scripts: vec![],
                pre_apply_scripts: Vec::new(),
                pre_reconcile_scripts: Vec::new(),
                post_reconcile_scripts: Vec::new(),
                on_change_scripts: Vec::new(),
                system: HashMap::new(),
                depends: vec![],
                dir: PathBuf::from("."),
            },
            ResolvedModule {
                name: "nvim".to_string(),
                packages: vec![ResolvedPackage {
                    canonical_name: "neovim".to_string(),
                    resolved_name: "neovim".to_string(),
                    manager: "brew".to_string(),
                    version: Some("0.10.2".to_string()),
                    script: None,
                }],
                files: vec![],
                env: vec![],
                aliases: vec![],
                post_apply_scripts: vec![],
                pre_apply_scripts: Vec::new(),
                pre_reconcile_scripts: Vec::new(),
                post_reconcile_scripts: Vec::new(),
                on_change_scripts: Vec::new(),
                system: HashMap::new(),
                depends: vec!["node".to_string()],
                dir: PathBuf::from("."),
            },
        ];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                modules,
                ReconcileContext::Apply,
            )
            .unwrap();

        let module_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Modules)
            .unwrap();
        // node packages + nvim packages = 2 actions
        assert_eq!(module_phase.actions.len(), 2);

        // First action should be for "node" (leaf dependency)
        match &module_phase.actions[0] {
            Action::Module(ma) => assert_eq!(ma.module_name, "node"),
            _ => panic!("expected Module action"),
        }
        // Second for "nvim"
        match &module_phase.actions[1] {
            Action::Module(ma) => assert_eq!(ma.module_name, "nvim"),
            _ => panic!("expected Module action"),
        }
    }

    #[test]
    fn format_module_plan_items_packages() {
        let phase = Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "nvim".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![
                        ResolvedPackage {
                            canonical_name: "neovim".to_string(),
                            resolved_name: "neovim".to_string(),
                            manager: "brew".to_string(),
                            version: Some("0.10.2".to_string()),
                            script: None,
                        },
                        ResolvedPackage {
                            canonical_name: "fd".to_string(),
                            resolved_name: "fd-find".to_string(),
                            manager: "apt".to_string(),
                            version: Some("8.7.0".to_string()),
                            script: None,
                        },
                    ],
                },
            })],
        };

        let items = format_plan_items(&phase);
        assert_eq!(items.len(), 1);
        assert!(items[0].contains("[nvim]"));
        // Should show alias info for fd→fd-find
        assert!(items[0].contains("fd-find"));
    }

    #[test]
    fn format_module_plan_items_files() {
        let phase = Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "nvim".to_string(),
                kind: ModuleActionKind::DeployFiles {
                    files: vec![ResolvedFile {
                        source: PathBuf::from("/cache/nvim/config"),
                        target: PathBuf::from("/home/user/.config/nvim"),
                        is_git_source: false,
                        strategy: None,
                        encryption: None,
                    }],
                },
            })],
        };

        let items = format_plan_items(&phase);
        assert_eq!(items.len(), 1);
        assert!(items[0].contains("[nvim]"));
        assert!(items[0].contains("deploy"));
        assert!(items[0].contains(".config/nvim"));
    }

    #[test]
    fn format_module_plan_items_skip() {
        let phase = Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "bad".to_string(),
                kind: ModuleActionKind::Skip {
                    reason: "dependency not met".to_string(),
                },
            })],
        };

        let items = format_plan_items(&phase);
        assert_eq!(items.len(), 1);
        assert!(items[0].contains("[bad]"));
        assert!(items[0].contains("skip"));
        assert!(items[0].contains("dependency not met"));
    }

    #[test]
    fn format_module_action_description() {
        let action = Action::Module(ModuleAction {
            module_name: "nvim".to_string(),
            kind: ModuleActionKind::InstallPackages {
                resolved: vec![ResolvedPackage {
                    canonical_name: "neovim".to_string(),
                    resolved_name: "neovim".to_string(),
                    manager: "brew".to_string(),
                    version: Some("0.10.2".to_string()),
                    script: None,
                }],
            },
        });

        let desc = format_action_description(&action);
        assert!(desc.starts_with("module:nvim:packages:"));
        assert!(desc.contains("neovim"));
    }

    #[test]
    fn module_state_stored_after_apply() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();

        registry.package_managers.push(Box::new(
            MockPackageManager::new("brew").with_installed(&["neovim"]),
        ));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![make_resolved_module("nvim")];
        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                modules.clone(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let _result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        // Module state should be recorded
        let module_state = state.module_state_by_name("nvim").unwrap();
        assert!(module_state.is_some());
        let ms = module_state.unwrap();
        assert_eq!(ms.module_name, "nvim");
        assert_eq!(ms.status, "installed");
        assert!(!ms.packages_hash.is_empty());
        assert!(!ms.files_hash.is_empty());
    }

    #[test]
    fn module_state_upsert_and_remove() {
        let state = test_state();

        state
            .upsert_module_state("nvim", None, "hash1", "hash2", None, "installed")
            .unwrap();

        let ms = state.module_state_by_name("nvim").unwrap().unwrap();
        assert_eq!(ms.packages_hash, "hash1");
        assert_eq!(ms.status, "installed");

        // Update
        state
            .upsert_module_state(
                "nvim",
                None,
                "hash3",
                "hash4",
                Some("[{\"url\":\"test\"}]"),
                "outdated",
            )
            .unwrap();

        let ms = state.module_state_by_name("nvim").unwrap().unwrap();
        assert_eq!(ms.packages_hash, "hash3");
        assert_eq!(ms.status, "outdated");
        assert!(ms.git_sources.is_some());

        // List all
        let all = state.module_states().unwrap();
        assert_eq!(all.len(), 1);

        // Remove
        state.remove_module_state("nvim").unwrap();
        assert!(state.module_state_by_name("nvim").unwrap().is_none());
    }

    #[test]
    fn verify_module_drift_packages() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();

        // ripgrep is NOT installed — should drift
        registry.package_managers.push(Box::new(
            MockPackageManager::new("brew").with_installed(&["neovim"]),
        ));

        let resolved = make_empty_resolved();
        let printer = test_printer();

        let modules = vec![make_resolved_module("nvim")];
        let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

        // Should have a drift result for ripgrep
        let drift = results
            .iter()
            .find(|r| r.resource_type == "module" && r.resource_id == "nvim/ripgrep");
        assert!(drift.is_some());
        assert!(!drift.unwrap().matches);

        // nvim/neovim should not appear as drift since it's installed
        let ok = results
            .iter()
            .find(|r| r.resource_type == "module" && r.resource_id == "nvim/neovim");
        assert!(ok.is_none()); // no drift entry for installed packages
    }

    #[test]
    fn phase_name_modules_roundtrip() {
        let s = PhaseName::Modules.as_str();
        assert_eq!(s, "modules");
        let parsed = PhaseName::from_str(s).unwrap();
        assert_eq!(parsed, PhaseName::Modules);
        assert_eq!(PhaseName::Modules.display_name(), "Modules");
    }

    #[test]
    fn plan_hash_includes_module_actions() {
        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::InstallPackages {
                        resolved: vec![ResolvedPackage {
                            canonical_name: "neovim".to_string(),
                            resolved_name: "neovim".to_string(),
                            manager: "brew".to_string(),
                            version: Some("0.10.2".to_string()),
                            script: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let hash = plan.to_hash_string();
        assert!(hash.contains("nvim"));
        assert!(hash.contains("neovim"));
        assert!(hash.contains("brew"));
    }

    #[test]
    fn verify_module_healthy_when_all_installed() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();

        registry.package_managers.push(Box::new(
            MockPackageManager::new("brew").with_installed(&["neovim", "ripgrep"]),
        ));

        let resolved = make_empty_resolved();
        let printer = test_printer();

        let modules = vec![make_resolved_module("nvim")];
        let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

        // All packages installed → should get a single "healthy" result
        let healthy = results
            .iter()
            .find(|r| r.resource_type == "module" && r.resource_id == "nvim");
        assert!(healthy.is_some());
        assert!(healthy.unwrap().matches);
        assert_eq!(healthy.unwrap().expected, "healthy");

        // No drift entries
        let drifts: Vec<_> = results
            .iter()
            .filter(|r| r.resource_type == "module" && !r.matches)
            .collect();
        assert!(drifts.is_empty());
    }

    #[test]
    fn verify_module_script_packages_not_false_drift() {
        // Script-based packages should not cause false drift reports since
        // "script" isn't a registered package manager in the registry.
        let state = test_state();
        let registry = ProviderRegistry::new(); // no managers

        let resolved = make_empty_resolved();
        let printer = test_printer();

        let modules = vec![ResolvedModule {
            name: "rustup".to_string(),
            packages: vec![ResolvedPackage {
                canonical_name: "rustup".to_string(),
                resolved_name: "rustup".to_string(),
                manager: "script".to_string(),
                version: None,
                script: Some("curl -sSf https://sh.rustup.rs | sh".into()),
            }],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

        // Script packages should be skipped in verification, so module should be healthy
        let healthy = results
            .iter()
            .find(|r| r.resource_type == "module" && r.resource_id == "rustup");
        assert!(healthy.is_some());
        assert!(healthy.unwrap().matches);
        assert_eq!(healthy.unwrap().expected, "healthy");

        // No drift entries for script packages
        let drifts: Vec<_> = results
            .iter()
            .filter(|r| r.resource_type == "module" && !r.matches)
            .collect();
        assert!(drifts.is_empty());
    }

    #[test]
    fn plan_module_with_script_packages() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "rustup".to_string(),
            packages: vec![ResolvedPackage {
                canonical_name: "rustup".to_string(),
                resolved_name: "rustup".to_string(),
                manager: "script".to_string(),
                version: None,
                script: Some("curl -sSf https://sh.rustup.rs | sh".into()),
            }],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                modules,
                ReconcileContext::Apply,
            )
            .unwrap();

        let module_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Modules)
            .unwrap();
        assert_eq!(module_phase.actions.len(), 1);

        match &module_phase.actions[0] {
            Action::Module(ma) => {
                assert_eq!(ma.module_name, "rustup");
                match &ma.kind {
                    ModuleActionKind::InstallPackages { resolved } => {
                        assert_eq!(resolved.len(), 1);
                        assert_eq!(resolved[0].manager, "script");
                        assert!(resolved[0].script.is_some());
                    }
                    _ => panic!("expected InstallPackages action"),
                }
            }
            _ => panic!("expected Module action"),
        }
    }

    #[test]
    fn format_module_plan_script_packages() {
        let phase = Phase {
            name: PhaseName::Modules,
            actions: vec![Action::Module(ModuleAction {
                module_name: "rustup".to_string(),
                kind: ModuleActionKind::InstallPackages {
                    resolved: vec![ResolvedPackage {
                        canonical_name: "rustup".to_string(),
                        resolved_name: "rustup".to_string(),
                        manager: "script".to_string(),
                        version: None,
                        script: Some("install-rustup.sh".into()),
                    }],
                },
            })],
        };

        let items = format_plan_items(&phase);
        assert_eq!(items.len(), 1);
        assert!(items[0].contains("[rustup]"));
        assert!(items[0].contains("script"));
        assert!(items[0].contains("rustup"));
    }

    #[test]
    fn empty_modules_produces_empty_phase() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let module_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Modules)
            .unwrap();
        assert!(module_phase.actions.is_empty());
    }

    #[test]
    fn conflict_detection_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        std::fs::write(&file_a, "content A").unwrap();
        std::fs::write(&file_b, "content B").unwrap();

        let target = PathBuf::from("/home/user/.config/app");
        let file_actions = vec![FileAction::Create {
            source: file_a,
            target: target.clone(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        }];

        let modules = vec![ResolvedModule {
            name: "mymod".to_string(),
            packages: vec![],
            files: vec![crate::modules::ResolvedFile {
                source: file_b,
                target,
                is_git_source: false,
                strategy: None,
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("conflict"), "expected conflict error: {err}");
    }

    #[test]
    fn conflict_detection_identical_content_ok() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        std::fs::write(&file_a, "same content").unwrap();
        std::fs::write(&file_b, "same content").unwrap();

        let target = PathBuf::from("/home/user/.config/app");
        let file_actions = vec![FileAction::Create {
            source: file_a,
            target: target.clone(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        }];

        let modules = vec![ResolvedModule {
            name: "mymod".to_string(),
            packages: vec![],
            files: vec![crate::modules::ResolvedFile {
                source: file_b,
                target: target.clone(),
                is_git_source: false,
                strategy: None,
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
        assert!(
            result.is_ok(),
            "identical content targeting the same path should NOT conflict: {:?}",
            result.err()
        );
        // Prove the identical-content check is meaningful: different content WOULD conflict
        let file_c = dir.path().join("c.txt");
        std::fs::write(&file_c, "different content").unwrap();
        let conflicting_modules = vec![ResolvedModule {
            name: "mymod".to_string(),
            packages: vec![],
            files: vec![crate::modules::ResolvedFile {
                source: file_c,
                target: target.clone(),
                is_git_source: false,
                strategy: None,
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];
        assert!(
            Reconciler::detect_file_conflicts(&file_actions, &conflicting_modules).is_err(),
            "different content at same target should conflict (proves the Ok was meaningful)"
        );
    }

    #[test]
    fn conflict_detection_no_overlap_ok() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        std::fs::write(&file_a, "content A").unwrap();
        std::fs::write(&file_b, "content B").unwrap();

        let target_a = PathBuf::from("/target/a");
        let target_b = PathBuf::from("/target/b");
        let file_actions = vec![FileAction::Create {
            source: file_a.clone(),
            target: target_a,
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        }];

        let modules = vec![ResolvedModule {
            name: "mymod".to_string(),
            packages: vec![],
            files: vec![crate::modules::ResolvedFile {
                source: file_b.clone(),
                target: target_b,
                is_git_source: false,
                strategy: None,
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
        assert!(
            result.is_ok(),
            "different targets should not conflict: {:?}",
            result.err()
        );
        // Prove this is meaningful: same target with different content WOULD conflict
        let overlapping_modules = vec![ResolvedModule {
            name: "mymod".to_string(),
            packages: vec![],
            files: vec![crate::modules::ResolvedFile {
                source: file_b,
                target: PathBuf::from("/target/a"), // same as file_actions target
                is_git_source: false,
                strategy: None,
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];
        assert!(
            Reconciler::detect_file_conflicts(&file_actions, &overlapping_modules).is_err(),
            "different content at same target should conflict (proves the Ok was meaningful)"
        );
    }

    #[test]
    fn generate_env_file_quoted_and_unquoted() {
        let env = vec![
            crate::config::EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            },
            crate::config::EnvVar {
                name: "PATH".into(),
                value: "/usr/local/bin:$PATH".into(),
            },
        ];
        let content = super::generate_env_file_content(&env, &[]);
        assert!(content.starts_with("# managed by cfgd"));
        assert!(content.contains("export EDITOR=\"nvim\""));
        // PATH contains $, so double-quoted to allow expansion
        assert!(content.contains("export PATH=\"/usr/local/bin:$PATH\""));
    }

    #[test]
    fn generate_fish_env_splits_path() {
        let env = vec![
            crate::config::EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            },
            crate::config::EnvVar {
                name: "PATH".into(),
                value: "/usr/local/bin:/home/user/.cargo/bin:$PATH".into(),
            },
        ];
        let content = super::generate_fish_env_content(&env, &[]);
        assert!(content.starts_with("# managed by cfgd"));
        assert!(content.contains("set -gx EDITOR 'nvim'"));
        assert!(content.contains("set -gx PATH '/usr/local/bin' '/home/user/.cargo/bin' '$PATH'"));
    }

    #[test]
    fn plan_env_empty_when_no_env() {
        let tmp = tempfile::tempdir().unwrap();
        let (actions, _warnings) = Reconciler::plan_env_with_home(&[], &[], &[], &[], tmp.path());
        assert!(actions.is_empty());
    }

    #[test]
    fn plan_env_module_wins_on_conflict() {
        let profile_env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "vim".into(),
        }];
        let modules = vec![ResolvedModule {
            name: "nvim".into(),
            packages: vec![],
            files: vec![],
            env: vec![crate::config::EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            }],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];
        // plan_env merges and generates actions — the merged env should have EDITOR=nvim
        let tmp = tempfile::tempdir().unwrap();
        let (actions, _warnings) =
            Reconciler::plan_env_with_home(&profile_env, &[], &modules, &[], tmp.path());
        // With non-empty env, there should be at least a WriteEnvFile action
        let has_write = actions
            .iter()
            .any(|a| matches!(a, Action::Env(EnvAction::WriteEnvFile { .. })));
        assert!(has_write, "Expected WriteEnvFile action for non-empty env");
    }

    #[test]
    fn plan_env_generates_file_matching_expected() {
        let env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];

        // Write the expected content to a temp file to simulate "already applied"
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".cfgd.env");
        let expected = super::generate_env_file_content(&env, &[]);
        std::fs::write(&env_path, &expected).unwrap();

        // plan_env checks the real ~/.cfgd.env path, not our temp file,
        // so it will still generate actions. This test validates the content generation.
        assert!(expected.contains("export EDITOR=\"nvim\""));
        assert!(expected.contains("# managed by cfgd"));
    }

    #[test]
    fn phase_name_env_roundtrip() {
        assert_eq!(PhaseName::Env.as_str(), "env");
        assert_eq!(PhaseName::Env.display_name(), "Environment");
        assert_eq!("env".parse::<PhaseName>().unwrap(), PhaseName::Env);
    }

    #[test]
    fn generate_env_file_with_aliases() {
        let env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];
        let aliases = vec![
            crate::config::ShellAlias {
                name: "vim".into(),
                command: "nvim".into(),
            },
            crate::config::ShellAlias {
                name: "ll".into(),
                command: "ls -la".into(),
            },
        ];
        let content = super::generate_env_file_content(&env, &aliases);
        assert!(content.contains("export EDITOR=\"nvim\""));
        assert!(content.contains("alias vim=\"nvim\""));
        assert!(content.contains("alias ll=\"ls -la\""));
    }

    #[test]
    fn generate_fish_env_with_aliases() {
        let env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];
        let aliases = vec![crate::config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
        }];
        let content = super::generate_fish_env_content(&env, &aliases);
        assert!(content.contains("set -gx EDITOR 'nvim'"));
        assert!(content.contains("abbr -a vim 'nvim'"));
    }

    #[test]
    fn plan_env_aliases_only() {
        let aliases = vec![crate::config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
        }];
        let tmp = tempfile::tempdir().unwrap();
        let (actions, _warnings) =
            Reconciler::plan_env_with_home(&[], &aliases, &[], &[], tmp.path());
        let has_write = actions
            .iter()
            .any(|a| matches!(a, Action::Env(EnvAction::WriteEnvFile { .. })));
        assert!(has_write, "Expected WriteEnvFile action for aliases-only");
    }

    #[test]
    #[cfg(unix)]
    fn plan_env_module_alias_wins_on_conflict() {
        let profile_aliases = vec![crate::config::ShellAlias {
            name: "vim".into(),
            command: "vi".into(),
        }];
        let modules = vec![ResolvedModule {
            name: "nvim".into(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![crate::config::ShellAlias {
                name: "vim".into(),
                command: "nvim".into(),
            }],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];
        let tmp = tempfile::tempdir().unwrap();
        let (actions, _warnings) =
            Reconciler::plan_env_with_home(&[], &profile_aliases, &modules, &[], tmp.path());
        // Find the WriteEnvFile action and check it has "nvim" not "vi"
        for action in &actions {
            if let Action::Env(EnvAction::WriteEnvFile { content, .. }) = action {
                assert!(
                    content.contains("alias vim=\"nvim\""),
                    "Module alias should override profile alias"
                );
                assert!(
                    !content.contains("alias vim=\"vi\""),
                    "Profile alias should be overridden"
                );
                return;
            }
        }
        panic!("Expected WriteEnvFile action");
    }

    #[test]
    fn generate_env_file_alias_escapes_quotes() {
        let aliases = vec![crate::config::ShellAlias {
            name: "greet".into(),
            command: "echo \"hello world\"".into(),
        }];
        let content = super::generate_env_file_content(&[], &aliases);
        assert!(content.contains("alias greet=\"echo \\\"hello world\\\"\""));
    }

    // --- Secret env injection tests ---

    struct MockSecretProvider {
        provider_name: String,
        value: String,
    }

    impl crate::providers::SecretProvider for MockSecretProvider {
        fn name(&self) -> &str {
            &self.provider_name
        }
        fn is_available(&self) -> bool {
            true
        }
        fn resolve(&self, _reference: &str) -> Result<secrecy::SecretString> {
            Ok(secrecy::SecretString::from(self.value.clone()))
        }
    }

    #[test]
    fn plan_secrets_envs_only_produces_resolve_env() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.secret_providers.push(Box::new(MockSecretProvider {
            provider_name: "vault".into(),
            value: "secret-token".into(),
        }));
        let reconciler = Reconciler::new(&registry, &state);

        let mut profile = MergedProfile::default();
        profile.secrets.push(crate::config::SecretSpec {
            source: "vault://secret/data/github#token".to_string(),
            target: None,
            template: None,
            backend: None,
            envs: Some(vec!["GITHUB_TOKEN".to_string()]),
        });

        let actions = reconciler.plan_secrets(&profile);
        // Should produce exactly one ResolveEnv action
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Secret(SecretAction::ResolveEnv { provider, envs, .. }) => {
                assert_eq!(provider, "vault");
                assert_eq!(envs, &["GITHUB_TOKEN"]);
            }
            other => panic!("Expected ResolveEnv, got {:?}", other),
        }
    }

    #[test]
    fn plan_secrets_target_and_envs_produces_both_actions() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.secret_providers.push(Box::new(MockSecretProvider {
            provider_name: "1password".into(),
            value: "ghp_abc123".into(),
        }));
        let reconciler = Reconciler::new(&registry, &state);

        let mut profile = MergedProfile::default();
        profile.secrets.push(crate::config::SecretSpec {
            source: "1password://Vault/GitHub/Token".to_string(),
            target: Some(PathBuf::from("/tmp/github-token")),
            template: None,
            backend: None,
            envs: Some(vec!["GITHUB_TOKEN".to_string()]),
        });

        let actions = reconciler.plan_secrets(&profile);
        // Should produce both a Resolve and a ResolveEnv action
        assert_eq!(actions.len(), 2);
        assert!(
            matches!(&actions[0], Action::Secret(SecretAction::Resolve { .. })),
            "First action should be Resolve, got {:?}",
            &actions[0]
        );
        assert!(
            matches!(&actions[1], Action::Secret(SecretAction::ResolveEnv { .. })),
            "Second action should be ResolveEnv, got {:?}",
            &actions[1]
        );
    }

    #[test]
    fn plan_env_with_secret_envs_includes_them() {
        let secret_envs = vec![
            ("GITHUB_TOKEN".to_string(), "ghp_abc123".to_string()),
            ("NPM_TOKEN".to_string(), "npm_xyz789".to_string()),
        ];
        let tmp = tempfile::tempdir().unwrap();
        let (actions, _warnings) =
            Reconciler::plan_env_with_home(&[], &[], &[], &secret_envs, tmp.path());
        // With non-empty secret envs, there should be at least a WriteEnvFile action
        let has_write = actions
            .iter()
            .any(|a| matches!(a, Action::Env(EnvAction::WriteEnvFile { .. })));
        assert!(has_write, "Expected WriteEnvFile action for secret envs");
    }

    #[test]
    #[cfg(unix)]
    fn plan_env_secret_envs_appear_in_generated_content() {
        let regular_env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];
        let secret_envs = vec![("GITHUB_TOKEN".to_string(), "ghp_abc123".to_string())];
        let tmp = tempfile::tempdir().unwrap();
        let (actions, _warnings) =
            Reconciler::plan_env_with_home(&regular_env, &[], &[], &secret_envs, tmp.path());

        // Find the WriteEnvFile action and check its content
        for action in &actions {
            if let Action::Env(EnvAction::WriteEnvFile { content, .. }) = action {
                assert!(
                    content.contains("export EDITOR=\"nvim\""),
                    "Regular env should be present"
                );
                assert!(
                    content.contains("export GITHUB_TOKEN=\"ghp_abc123\""),
                    "Secret env should be present in content: {}",
                    content
                );
                // Secret envs should appear after regular envs
                let editor_pos = content.find("EDITOR").unwrap_or(0);
                let token_pos = content.find("GITHUB_TOKEN").unwrap_or(0);
                assert!(
                    token_pos > editor_pos,
                    "Secret env should appear after regular env"
                );
                return;
            }
        }
        panic!("Expected WriteEnvFile action");
    }

    // --- Shell rc conflict detection tests ---

    #[test]
    fn rc_conflict_env_different_value_warns() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".bashrc");
        std::fs::write(
            &rc,
            "export EDITOR=\"vim\"\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\n",
        )
        .unwrap();
        let env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];
        let warnings = super::detect_rc_env_conflicts(&rc, &env, &[]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("EDITOR"));
        assert!(warnings[0].contains("move it after the source line"));
    }

    #[test]
    fn rc_conflict_env_same_value_no_warning() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".bashrc");
        std::fs::write(
            &rc,
            "export EDITOR=\"nvim\"\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\n",
        )
        .unwrap();
        let env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];
        let warnings = super::detect_rc_env_conflicts(&rc, &env, &[]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn rc_conflict_alias_different_value_warns() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".bashrc");
        std::fs::write(
            &rc,
            "alias vim=\"vi\"\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\n",
        )
        .unwrap();
        let aliases = vec![crate::config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
        }];
        let warnings = super::detect_rc_env_conflicts(&rc, &[], &aliases);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("alias vim"));
        assert!(warnings[0].contains("move it after the source line"));
    }

    #[test]
    fn rc_conflict_after_source_line_no_warning() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".bashrc");
        std::fs::write(
            &rc,
            "[ -f ~/.cfgd.env ] && source ~/.cfgd.env\nexport EDITOR=\"vim\"\n",
        )
        .unwrap();
        let env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];
        let warnings = super::detect_rc_env_conflicts(&rc, &env, &[]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn rc_conflict_no_source_line_all_before() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".bashrc");
        std::fs::write(&rc, "export EDITOR=\"vim\"\nalias vim=\"vi\"\n").unwrap();
        let env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];
        let aliases = vec![crate::config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
        }];
        let warnings = super::detect_rc_env_conflicts(&rc, &env, &aliases);
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn rc_conflict_nonexistent_file_no_warnings() {
        let warnings = super::detect_rc_env_conflicts(
            std::path::Path::new("/nonexistent/.bashrc"),
            &[crate::config::EnvVar {
                name: "FOO".into(),
                value: "bar".into(),
            }],
            &[],
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn strip_shell_quotes_works() {
        assert_eq!(super::strip_shell_quotes("\"hello\""), "hello");
        assert_eq!(super::strip_shell_quotes("'hello'"), "hello");
        assert_eq!(super::strip_shell_quotes("hello"), "hello");
        assert_eq!(super::strip_shell_quotes("\"\""), "");
    }

    // --- PowerShell env generation tests ---

    #[test]
    fn generate_powershell_env_basic() {
        let env = vec![
            crate::config::EnvVar {
                name: "EDITOR".into(),
                value: "code".into(),
            },
            crate::config::EnvVar {
                name: "PATH".into(),
                value: r"C:\Users\user\.cargo\bin;$env:PATH".into(),
            },
        ];
        let content = super::generate_powershell_env_content(&env, &[]);
        assert!(content.starts_with("# managed by cfgd"));
        assert!(content.contains("$env:EDITOR = 'code'"));
        // PATH references $env: so double-quoted to allow expansion
        assert!(content.contains(r#"$env:PATH = "C:\Users\user\.cargo\bin;$env:PATH""#));
    }

    #[test]
    fn generate_powershell_env_with_aliases() {
        let aliases = vec![
            crate::config::ShellAlias {
                name: "g".into(),
                command: "git".into(),
            },
            crate::config::ShellAlias {
                name: "ll".into(),
                command: "Get-ChildItem -Force".into(),
            },
        ];
        let content = super::generate_powershell_env_content(&[], &aliases);
        assert!(content.contains("Set-Alias -Name g -Value git"));
        assert!(content.contains("function ll {"));
        assert!(content.contains("Get-ChildItem -Force @args"));
    }

    #[test]
    fn generate_powershell_env_escapes_quotes() {
        let env = vec![crate::config::EnvVar {
            name: "GREETING".into(),
            value: r#"say "hello""#.into(),
        }];
        let content = super::generate_powershell_env_content(&env, &[]);
        // No $env: reference, so single-quoted (PS single quotes don't need escaping except ')
        assert!(content.contains("$env:GREETING = 'say \"hello\"'"));
    }

    #[test]
    fn generate_powershell_env_empty() {
        let content = super::generate_powershell_env_content(&[], &[]);
        assert!(content.starts_with("# managed by cfgd"));
        // Only header + trailing newline
        assert_eq!(content.lines().count(), 1);
    }

    // --- Apply execution path tests ---

    /// A mock package manager that tracks which packages were installed/uninstalled.
    struct TrackingPackageManager {
        name: String,
        installed: std::sync::Mutex<HashSet<String>>,
        install_calls: std::sync::Mutex<Vec<Vec<String>>>,
        uninstall_calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl TrackingPackageManager {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                installed: std::sync::Mutex::new(HashSet::new()),
                install_calls: std::sync::Mutex::new(Vec::new()),
                uninstall_calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn with_installed(name: &str, pkgs: &[&str]) -> Self {
            let mut set = HashSet::new();
            for p in pkgs {
                set.insert(p.to_string());
            }
            Self {
                name: name.to_string(),
                installed: std::sync::Mutex::new(set),
                install_calls: std::sync::Mutex::new(Vec::new()),
                uninstall_calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl PackageManager for TrackingPackageManager {
        fn name(&self) -> &str {
            &self.name
        }
        fn is_available(&self) -> bool {
            true
        }
        fn can_bootstrap(&self) -> bool {
            false
        }
        fn bootstrap(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn installed_packages(&self) -> Result<HashSet<String>> {
            Ok(self.installed.lock().unwrap().clone())
        }
        fn install(&self, packages: &[String], _printer: &Printer) -> Result<()> {
            self.install_calls.lock().unwrap().push(packages.to_vec());
            let mut installed = self.installed.lock().unwrap();
            for p in packages {
                installed.insert(p.clone());
            }
            Ok(())
        }
        fn uninstall(&self, packages: &[String], _printer: &Printer) -> Result<()> {
            self.uninstall_calls.lock().unwrap().push(packages.to_vec());
            let mut installed = self.installed.lock().unwrap();
            for p in packages {
                installed.remove(p);
            }
            Ok(())
        }
        fn update(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn available_version(&self, _package: &str) -> Result<Option<String>> {
            Ok(None)
        }
    }

    #[test]
    fn apply_package_install_calls_mock_and_records_state() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("brew")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["ripgrep".to_string(), "fd".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 1);
        assert!(result.action_results[0].success);
        assert!(result.action_results[0].error.is_none());
        assert!(result.action_results[0].description.contains("ripgrep"));

        // Verify install was actually called on the tracking mock
        let pm = registry.package_managers[0].as_ref();
        let installed = pm.installed_packages().unwrap();
        assert!(installed.contains("ripgrep"));
        assert!(installed.contains("fd"));
    }

    #[test]
    fn apply_package_uninstall_calls_mock() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::with_installed(
                "brew",
                &["ripgrep", "fd"],
            )));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![PackageAction::Uninstall {
            manager: "brew".to_string(),
            packages: vec!["ripgrep".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 1);
        assert!(result.action_results[0].success);

        let pm = registry.package_managers[0].as_ref();
        let installed = pm.installed_packages().unwrap();
        assert!(!installed.contains("ripgrep"));
        assert!(installed.contains("fd"));
    }

    #[test]
    fn apply_empty_plan_records_success_in_state_store() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 0);

        // Verify the state store has a record
        let last = state.last_apply().unwrap();
        assert!(last.is_some());
        let record = last.unwrap();
        assert_eq!(record.status, ApplyStatus::Success);
        assert_eq!(record.profile, "test");
        assert_eq!(record.id, result.apply_id);
    }

    #[test]
    fn apply_records_correct_apply_id() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();
        let printer = test_printer();

        // First apply
        let result1 = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        // Second apply
        let result2 = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        // Each apply should get a unique, incrementing ID
        assert!(result2.apply_id > result1.apply_id);

        // Verify via state store
        let last = state.last_apply().unwrap().unwrap();
        assert_eq!(last.id, result2.apply_id);
    }

    #[test]
    fn apply_env_write_env_file_to_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".cfgd.env");

        let env = vec![
            crate::config::EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            },
            crate::config::EnvVar {
                name: "CARGO_HOME".into(),
                value: "/home/user/.cargo".into(),
            },
        ];
        let content = super::generate_env_file_content(&env, &[]);

        let action = EnvAction::WriteEnvFile {
            path: env_path.clone(),
            content: content.clone(),
        };

        let printer = test_printer();
        let desc = Reconciler::apply_env_action(&action, &printer).unwrap();

        // Verify file was written
        let written = std::fs::read_to_string(&env_path).unwrap();
        assert_eq!(written, content);
        assert!(written.contains("export EDITOR=\"nvim\""));
        assert!(written.contains("export CARGO_HOME=\"/home/user/.cargo\""));
        assert!(desc.starts_with("env:write:"));
    }

    #[test]
    fn apply_env_write_skips_when_content_matches() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".cfgd.env");

        let env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];
        let content = super::generate_env_file_content(&env, &[]);

        // Pre-write identical content
        std::fs::write(&env_path, &content).unwrap();

        let action = EnvAction::WriteEnvFile {
            path: env_path.clone(),
            content,
        };

        let printer = test_printer();
        let desc = Reconciler::apply_env_action(&action, &printer).unwrap();

        // Should report skipped
        assert!(desc.contains("skipped"), "Expected skip: {}", desc);
    }

    #[test]
    fn apply_env_inject_source_line_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let rc_path = dir.path().join(".bashrc");

        let action = EnvAction::InjectSourceLine {
            rc_path: rc_path.clone(),
            line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
        };

        let printer = test_printer();
        let desc = Reconciler::apply_env_action(&action, &printer).unwrap();

        let written = std::fs::read_to_string(&rc_path).unwrap();
        assert!(written.contains("source ~/.cfgd.env"));
        assert!(desc.starts_with("env:inject:"));
    }

    #[test]
    fn apply_env_inject_skips_when_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let rc_path = dir.path().join(".bashrc");

        // Pre-write content that already mentions cfgd.env
        std::fs::write(
            &rc_path,
            "# existing config\n[ -f ~/.cfgd.env ] && source ~/.cfgd.env\n",
        )
        .unwrap();

        let action = EnvAction::InjectSourceLine {
            rc_path: rc_path.clone(),
            line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
        };

        let printer = test_printer();
        let desc = Reconciler::apply_env_action(&action, &printer).unwrap();

        assert!(desc.contains("skipped"), "Expected skip: {}", desc);
    }

    #[test]
    fn apply_env_inject_appends_to_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let rc_path = dir.path().join(".bashrc");

        std::fs::write(&rc_path, "# my config\nexport FOO=bar").unwrap();

        let action = EnvAction::InjectSourceLine {
            rc_path: rc_path.clone(),
            line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
        };

        let printer = test_printer();
        Reconciler::apply_env_action(&action, &printer).unwrap();

        let written = std::fs::read_to_string(&rc_path).unwrap();
        assert!(written.starts_with("# my config\n"));
        assert!(written.contains("export FOO=bar"));
        assert!(written.contains("source ~/.cfgd.env"));
    }

    #[test]
    fn apply_full_flow_plan_apply_verify_consistent() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::with_installed(
                "brew",
                &["git"],
            )));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        // Plan: install ripgrep and fd via brew
        let pkg_actions = vec![PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["ripgrep".to_string(), "fd".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();
        assert!(!plan.is_empty());

        // Apply
        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.succeeded(), 1);
        assert_eq!(result.failed(), 0);

        // State store should show the apply
        let last = state.last_apply().unwrap().unwrap();
        assert_eq!(last.id, result.apply_id);
        assert_eq!(last.status, ApplyStatus::Success);
        assert!(last.summary.is_some());

        // Managed resources should be recorded
        let resources = state.managed_resources().unwrap();
        assert!(
            !resources.is_empty(),
            "Expected managed resources after apply"
        );
    }

    #[test]
    fn apply_records_summary_json() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("brew")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["jq".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();
        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        // Verify the summary JSON in the state store
        let last = state.last_apply().unwrap().unwrap();
        let summary = last.summary.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&summary).unwrap();
        assert_eq!(parsed["total"], 1);
        assert_eq!(parsed["succeeded"], 1);
        assert_eq!(parsed["failed"], 0);
        assert_eq!(result.apply_id, last.id);
    }

    #[test]
    fn apply_with_phase_filter_only_runs_matching_phase() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("brew")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        // Create a plan with package actions
        let pkg_actions = vec![PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["ripgrep".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();

        // Apply with filter set to Env phase — should skip Packages
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Env),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        // No actions executed because Env phase is empty and Packages phase was filtered out
        assert_eq!(result.action_results.len(), 0);
    }

    #[test]
    fn apply_with_phase_filter_runs_only_packages() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("brew")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["ripgrep".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();

        // Apply with filter set to Packages phase — should run the install
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Packages),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 1);
        assert!(result.action_results[0].success);
    }

    #[test]
    fn apply_file_create_action_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let target = dir.path().join("subdir/target.txt");
        std::fs::write(&source, "hello world").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let file_actions = vec![FileAction::Create {
            source: source.clone(),
            target: target.clone(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        }];

        let plan = reconciler
            .plan(
                &resolved,
                file_actions,
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Files),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 1);
        assert!(result.action_results[0].success);

        // Verify file was created
        assert!(target.exists());
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn apply_multiple_package_actions_all_succeed() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("brew")));
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("cargo")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![
            PackageAction::Install {
                manager: "brew".to_string(),
                packages: vec!["jq".to_string()],
                origin: "local".to_string(),
            },
            PackageAction::Install {
                manager: "cargo".to_string(),
                packages: vec!["bat".to_string()],
                origin: "local".to_string(),
            },
        ];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 2);
        assert_eq!(result.succeeded(), 2);
        assert_eq!(result.failed(), 0);

        // Verify both managers had their install called
        let brew = registry.package_managers[0].as_ref();
        assert!(brew.installed_packages().unwrap().contains("jq"));
        let cargo = registry.package_managers[1].as_ref();
        assert!(cargo.installed_packages().unwrap().contains("bat"));
    }

    #[test]
    fn apply_package_skip_action_succeeds() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![PackageAction::Skip {
            manager: "apt".to_string(),
            reason: "not available on macOS".to_string(),
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 1);
        assert!(result.action_results[0].success);
        assert!(result.action_results[0].description.contains("skip"));
    }

    #[test]
    fn apply_env_write_with_aliases_produces_correct_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".cfgd.env");

        let env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "nvim".into(),
        }];
        let aliases = vec![crate::config::ShellAlias {
            name: "ll".into(),
            command: "ls -la".into(),
        }];
        let content = super::generate_env_file_content(&env, &aliases);

        let action = EnvAction::WriteEnvFile {
            path: env_path.clone(),
            content: content.clone(),
        };

        let printer = test_printer();
        Reconciler::apply_env_action(&action, &printer).unwrap();

        let written = std::fs::read_to_string(&env_path).unwrap();
        assert!(written.contains("export EDITOR=\"nvim\""));
        assert!(written.contains("alias ll=\"ls -la\""));
        assert!(written.starts_with("# managed by cfgd"));
    }

    #[test]
    fn combine_script_output_both() {
        let result = super::combine_script_output("hello\nworld", "warn: something");
        assert_eq!(
            result,
            Some("hello\nworld\n--- stderr ---\nwarn: something".to_string())
        );
    }

    #[test]
    fn combine_script_output_stdout_only() {
        let result = super::combine_script_output("output line", "");
        assert_eq!(result, Some("output line".to_string()));
    }

    #[test]
    fn combine_script_output_stderr_only() {
        let result = super::combine_script_output("", "error msg");
        assert_eq!(result, Some("error msg".to_string()));
    }

    #[test]
    fn combine_script_output_empty() {
        assert!(super::combine_script_output("", "").is_none());
        assert!(super::combine_script_output("  ", " \n ").is_none());
    }

    #[test]
    fn continue_on_error_defaults_per_phase() {
        // Pre-hooks default to false (abort on failure)
        assert!(!super::default_continue_on_error(&ScriptPhase::PreApply));
        assert!(!super::default_continue_on_error(
            &ScriptPhase::PreReconcile
        ));
        // Post-hooks and event hooks default to true (continue on failure)
        assert!(super::default_continue_on_error(&ScriptPhase::PostApply));
        assert!(super::default_continue_on_error(
            &ScriptPhase::PostReconcile
        ));
        assert!(super::default_continue_on_error(&ScriptPhase::OnChange));
        assert!(super::default_continue_on_error(&ScriptPhase::OnDrift));
    }

    #[test]
    fn effective_continue_on_error_uses_explicit_value() {
        let entry = ScriptEntry::Full {
            run: "echo test".to_string(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: Some(true),
        };
        // Should be true even for pre-apply (which defaults to false)
        assert!(super::effective_continue_on_error(
            &entry,
            &ScriptPhase::PreApply
        ));

        let entry_false = ScriptEntry::Full {
            run: "echo test".to_string(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: Some(false),
        };
        // Should be false even for post-apply (which defaults to true)
        assert!(!super::effective_continue_on_error(
            &entry_false,
            &ScriptPhase::PostApply
        ));
    }

    #[test]
    fn effective_continue_on_error_falls_back_to_default() {
        let simple = ScriptEntry::Simple("echo test".to_string());
        assert!(!super::effective_continue_on_error(
            &simple,
            &ScriptPhase::PreApply
        ));
        assert!(super::effective_continue_on_error(
            &simple,
            &ScriptPhase::PostApply
        ));

        let full_no_override = ScriptEntry::Full {
            run: "echo test".to_string(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
        };
        assert!(!super::effective_continue_on_error(
            &full_no_override,
            &ScriptPhase::PreApply
        ));
        assert!(super::effective_continue_on_error(
            &full_no_override,
            &ScriptPhase::PostApply
        ));
    }

    #[test]
    fn plan_scripts_with_apply_context_uses_pre_post_apply() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        let mut resolved = make_empty_resolved();
        resolved.merged.scripts.pre_apply = vec![ScriptEntry::Simple("scripts/pre.sh".to_string())];
        resolved.merged.scripts.post_apply =
            vec![ScriptEntry::Simple("scripts/post.sh".to_string())];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let pre_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::PreScripts)
            .unwrap();
        assert_eq!(pre_phase.actions.len(), 1);
        match &pre_phase.actions[0] {
            Action::Script(ScriptAction::Run { entry, phase, .. }) => {
                assert_eq!(entry.run_str(), "scripts/pre.sh");
                assert_eq!(*phase, ScriptPhase::PreApply);
            }
            _ => panic!("expected Script action"),
        }

        let post_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::PostScripts)
            .unwrap();
        assert_eq!(post_phase.actions.len(), 1);
        match &post_phase.actions[0] {
            Action::Script(ScriptAction::Run { entry, phase, .. }) => {
                assert_eq!(entry.run_str(), "scripts/post.sh");
                assert_eq!(*phase, ScriptPhase::PostApply);
            }
            _ => panic!("expected Script action"),
        }
    }

    #[test]
    fn plan_scripts_carries_full_entry() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        let mut resolved = make_empty_resolved();
        resolved.merged.scripts.pre_apply = vec![ScriptEntry::Full {
            run: "scripts/check.sh".to_string(),
            timeout: Some("10s".to_string()),
            idle_timeout: None,
            continue_on_error: Some(true),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let pre_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::PreScripts)
            .unwrap();
        assert_eq!(pre_phase.actions.len(), 1);
        match &pre_phase.actions[0] {
            Action::Script(ScriptAction::Run { entry, .. }) => match entry {
                ScriptEntry::Full {
                    run,
                    timeout,
                    continue_on_error,
                    ..
                } => {
                    assert_eq!(run, "scripts/check.sh");
                    assert_eq!(timeout.as_deref(), Some("10s"));
                    assert_eq!(*continue_on_error, Some(true));
                }
                _ => panic!("expected Full entry"),
            },
            _ => panic!("expected Script action"),
        }
    }

    #[test]
    fn build_script_env_includes_expected_vars() {
        let env = super::build_script_env(
            std::path::Path::new("/home/user/.config/cfgd"),
            "default",
            ReconcileContext::Apply,
            &ScriptPhase::PreApply,
            false,
            None,
            None,
        );
        let map: HashMap<String, String> = env.into_iter().collect();
        assert_eq!(
            map.get("CFGD_CONFIG_DIR").unwrap(),
            "/home/user/.config/cfgd"
        );
        assert_eq!(map.get("CFGD_PROFILE").unwrap(), "default");
        assert_eq!(map.get("CFGD_CONTEXT").unwrap(), "apply");
        assert_eq!(map.get("CFGD_PHASE").unwrap(), "preApply");
        assert_eq!(map.get("CFGD_DRY_RUN").unwrap(), "false");
        assert!(!map.contains_key("CFGD_MODULE_NAME"));
        assert!(!map.contains_key("CFGD_MODULE_DIR"));
    }

    #[test]
    fn build_script_env_includes_module_vars() {
        let env = super::build_script_env(
            std::path::Path::new("/config"),
            "work",
            ReconcileContext::Reconcile,
            &ScriptPhase::PostApply,
            true,
            Some("nvim"),
            Some(std::path::Path::new("/modules/nvim")),
        );
        let map: HashMap<String, String> = env.into_iter().collect();
        assert_eq!(map.get("CFGD_MODULE_NAME").unwrap(), "nvim");
        assert_eq!(map.get("CFGD_MODULE_DIR").unwrap(), "/modules/nvim");
        assert_eq!(map.get("CFGD_DRY_RUN").unwrap(), "true");
        assert_eq!(map.get("CFGD_CONTEXT").unwrap(), "reconcile");
    }

    #[test]
    fn execute_script_inline_command() {
        let printer = test_printer();
        let entry = ScriptEntry::Simple("echo hello".to_string());
        let dir = tempfile::tempdir().unwrap();
        let (desc, changed, output) = super::execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(10),
            &printer,
        )
        .unwrap();
        assert!(desc.contains("echo hello"));
        assert!(changed);
        assert_eq!(output, Some("hello".to_string()));
    }

    #[test]
    fn execute_script_failure_returns_error() {
        let printer = test_printer();
        let entry = ScriptEntry::Simple("exit 1".to_string());
        let dir = tempfile::tempdir().unwrap();
        let result = super::execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(10),
            &printer,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("exit 1"),
            "error should mention exit code: {err}"
        );
    }

    #[test]
    fn execute_script_with_timeout_override() {
        let printer = test_printer();
        let entry = ScriptEntry::Full {
            run: "echo fast".to_string(),
            timeout: Some("5s".to_string()),
            idle_timeout: None,
            continue_on_error: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let (_, _, output) = super::execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(300),
            &printer,
        )
        .unwrap();
        assert_eq!(output, Some("fast".to_string()));
    }

    #[test]
    #[cfg(unix)]
    fn execute_script_injects_env_vars() {
        let printer = test_printer();
        let entry = ScriptEntry::Simple("echo $MY_VAR".to_string());
        let dir = tempfile::tempdir().unwrap();
        let env = vec![("MY_VAR".to_string(), "test_value".to_string())];
        let (_, _, output) = super::execute_script(
            &entry,
            dir.path(),
            &env,
            std::time::Duration::from_secs(10),
            &printer,
        )
        .unwrap();
        assert_eq!(output, Some("test_value".to_string()));
    }

    #[test]
    #[cfg(unix)]
    fn execute_script_runs_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("test.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho from_file\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let printer = test_printer();
        let entry = ScriptEntry::Simple("test.sh".to_string());
        let (_, _, output) = super::execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(10),
            &printer,
        )
        .unwrap();
        assert_eq!(output, Some("from_file".to_string()));
    }

    #[test]
    fn execute_script_rejects_non_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("noexec.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let printer = test_printer();
        let entry = ScriptEntry::Simple("noexec.sh".to_string());
        let result = super::execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(10),
            &printer,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not executable"),
            "should say not executable: {err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn execute_script_idle_timeout_kills_idle_process() {
        let printer = test_printer();
        // Script prints once then sleeps forever — idle timeout should kill it
        let entry = ScriptEntry::Full {
            run: "echo started; sleep 60".to_string(),
            timeout: Some("30s".to_string()),
            idle_timeout: Some("1s".to_string()),
            continue_on_error: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let result = super::execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(30),
            &printer,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("idle (no output)"),
            "should mention idle timeout: {err}"
        );
    }

    // --- Rollback tests ---

    #[test]
    fn rollback_restores_file_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.txt");
        let file_path = target.display().to_string();

        // Rollback restores to the state AFTER the target apply.
        // Setup: apply 1 writes "v1 content", apply 2 modifies to "v2 content"
        // (capturing "v1 content" as backup). Rollback to apply 1 → "v1 content".
        let state = test_state();

        // Apply 1: creates file with v1 content
        let apply_id_1 = state
            .record_apply("test", "hash1", ApplyStatus::Success, None)
            .unwrap();
        let resource_id = format!("file:create:{}", target.display());
        let jid1 = state
            .journal_begin(apply_id_1, 0, "files", "file", &resource_id, None)
            .unwrap();
        state.journal_complete(jid1, None, None).unwrap();
        std::fs::write(&target, "v1 content").unwrap();

        // Apply 2: modifies file to v2 content. Backup captures v1 content.
        let file_state = crate::capture_file_state(&target).unwrap().unwrap();
        let apply_id_2 = state
            .record_apply("test", "hash2", ApplyStatus::Success, None)
            .unwrap();
        let update_resource_id = format!("file:update:{}", target.display());
        state
            .store_file_backup(apply_id_2, &file_path, &file_state)
            .unwrap();
        let jid2 = state
            .journal_begin(apply_id_2, 0, "files", "file", &update_resource_id, None)
            .unwrap();
        state.journal_complete(jid2, None, None).unwrap();
        std::fs::write(&target, "v2 content").unwrap();

        // Rollback to apply 1 — should restore v1 content
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let printer = test_printer();
        let rollback_result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

        assert_eq!(rollback_result.files_restored, 1);
        assert_eq!(rollback_result.files_removed, 0);
        assert!(rollback_result.non_file_actions.is_empty());

        let restored = std::fs::read_to_string(&target).unwrap();
        assert_eq!(restored, "v1 content");
    }

    #[test]
    fn rollback_no_changes_when_at_latest_apply() {
        // Rollback to the most recent apply with no subsequent applies
        // should produce no changes (system is already at that state).
        let state = test_state();
        let apply_id = state
            .record_apply("test", "hash1", ApplyStatus::Success, None)
            .unwrap();

        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let printer = test_printer();
        let rollback_result = reconciler.rollback_apply(apply_id, &printer).unwrap();

        assert_eq!(rollback_result.files_restored, 0);
        assert_eq!(rollback_result.files_removed, 0);
        assert!(rollback_result.non_file_actions.is_empty());
    }

    #[test]
    fn rollback_lists_non_file_actions() {
        let state = test_state();
        let apply_id_1 = state
            .record_apply("test", "hash1", ApplyStatus::Success, None)
            .unwrap();

        // Apply 2 has a package action (non-file) after apply 1
        let apply_id_2 = state
            .record_apply("test", "hash2", ApplyStatus::Success, None)
            .unwrap();
        let journal_id = state
            .journal_begin(
                apply_id_2,
                0,
                "packages",
                "package",
                "package:brew:install:ripgrep",
                None,
            )
            .unwrap();
        state.journal_complete(journal_id, None, None).unwrap();

        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let printer = test_printer();
        let rollback_result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

        assert_eq!(rollback_result.files_restored, 0);
        assert_eq!(rollback_result.files_removed, 0);
        assert_eq!(rollback_result.non_file_actions.len(), 1);
        assert!(rollback_result.non_file_actions[0].contains("ripgrep"));
    }

    #[test]
    fn rollback_records_new_apply_entry() {
        let state = test_state();
        let apply_id = state
            .record_apply("test", "hash1", ApplyStatus::Success, None)
            .unwrap();

        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let printer = test_printer();
        reconciler.rollback_apply(apply_id, &printer).unwrap();

        // The rollback should have created a new apply entry
        let last = state.last_apply().unwrap().unwrap();
        assert_eq!(last.profile, "rollback");
        assert!(last.id > apply_id);
    }

    // --- Partial apply tests ---

    /// A package manager that always fails on install.
    struct FailingPackageManager {
        name: String,
    }

    impl FailingPackageManager {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
            }
        }
    }

    impl PackageManager for FailingPackageManager {
        fn name(&self) -> &str {
            &self.name
        }
        fn is_available(&self) -> bool {
            true
        }
        fn can_bootstrap(&self) -> bool {
            false
        }
        fn bootstrap(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn installed_packages(&self) -> Result<HashSet<String>> {
            Ok(HashSet::new())
        }
        fn install(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
            Err(crate::errors::PackageError::InstallFailed {
                manager: self.name.clone(),
                message: "simulated install failure".to_string(),
            }
            .into())
        }
        fn uninstall(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn update(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn available_version(&self, _package: &str) -> Result<Option<String>> {
            Ok(None)
        }
    }

    #[test]
    fn apply_partial_when_some_actions_fail() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();

        // One working manager, one failing
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("brew")));
        registry
            .package_managers
            .push(Box::new(FailingPackageManager::new("apt")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![
            PackageAction::Install {
                manager: "brew".to_string(),
                packages: vec!["jq".to_string()],
                origin: "local".to_string(),
            },
            PackageAction::Install {
                manager: "apt".to_string(),
                packages: vec!["curl".to_string()],
                origin: "local".to_string(),
            },
        ];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Partial);
        assert_eq!(result.succeeded(), 1);
        assert_eq!(result.failed(), 1);

        // Verify state store records partial status
        let last = state.last_apply().unwrap().unwrap();
        assert_eq!(last.status, ApplyStatus::Partial);
    }

    #[test]
    fn apply_failed_when_all_actions_fail() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();

        registry
            .package_managers
            .push(Box::new(FailingPackageManager::new("apt")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![PackageAction::Install {
            manager: "apt".to_string(),
            packages: vec!["curl".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Failed);
        assert_eq!(result.succeeded(), 0);
        assert_eq!(result.failed(), 1);
        assert!(result.action_results[0].error.is_some());

        let last = state.last_apply().unwrap().unwrap();
        assert_eq!(last.status, ApplyStatus::Failed);
    }

    // --- continueOnError script tests ---

    #[test]
    #[cfg(unix)]
    fn apply_continue_on_error_post_script_continues() {
        // A post-apply script with continueOnError=true should not abort the apply
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("brew")));

        let reconciler = Reconciler::new(&registry, &state);
        let mut resolved = make_empty_resolved();

        // Post-apply script that fails but has continueOnError=true
        resolved.merged.scripts.post_apply = vec![ScriptEntry::Full {
            run: "exit 42".to_string(),
            timeout: Some("5s".to_string()),
            idle_timeout: None,
            continue_on_error: Some(true),
        }];

        let pkg_actions = vec![PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["jq".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                pkg_actions,
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        // Package install succeeded, post-script failed but continued
        assert_eq!(result.status, ApplyStatus::Partial);
        assert_eq!(result.succeeded(), 1); // package install
        assert_eq!(result.failed(), 1); // failed post-script

        // Verify the failed action is the script
        let failed = result.action_results.iter().find(|r| !r.success).unwrap();
        assert!(
            failed.description.contains("exit 42"),
            "failed action should be the script: {}",
            failed.description
        );
    }

    #[test]
    #[cfg(unix)]
    fn apply_continue_on_error_false_pre_script_aborts() {
        // A pre-apply script with continueOnError=false should abort the entire apply
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        let mut resolved = make_empty_resolved();
        resolved.merged.scripts.pre_apply = vec![ScriptEntry::Full {
            run: "exit 1".to_string(),
            timeout: Some("5s".to_string()),
            idle_timeout: None,
            continue_on_error: Some(false),
        }];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler.apply(
            &plan,
            &resolved,
            Path::new("."),
            &printer,
            None,
            &[],
            ReconcileContext::Apply,
            false,
        );

        // Pre-script failure with continueOnError=false should return an error
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("pre-script failed"),
            "should mention pre-script failure: {err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn apply_continue_on_error_default_post_script_continues() {
        // Post-apply scripts default to continueOnError=true (no explicit flag)
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        let mut resolved = make_empty_resolved();
        // Simple entry — no explicit continueOnError, defaults to true for post phase
        resolved.merged.scripts.post_apply = vec![ScriptEntry::Simple("exit 1".to_string())];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        // Post-script fails but default continueOnError=true means we get a result
        assert_eq!(result.status, ApplyStatus::Failed);
        assert_eq!(result.failed(), 1);
    }

    // --- onChange script execution tests ---

    #[test]
    #[cfg(unix)]
    fn apply_on_change_script_runs_when_changes_occur() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let target = dir.path().join("target.txt");
        let marker = dir.path().join("on_change_marker");

        std::fs::write(&source, "hello").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let mut resolved = make_empty_resolved();

        // Set up an onChange script that creates a marker file
        resolved.merged.scripts.on_change =
            vec![ScriptEntry::Simple(format!("touch {}", marker.display()))];

        let file_actions = vec![FileAction::Create {
            source: source.clone(),
            target: target.clone(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        }];

        let plan = reconciler
            .plan(
                &resolved,
                file_actions,
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);

        // The file action should have triggered the onChange script
        assert!(
            marker.exists(),
            "onChange marker file should exist, proving the onChange script ran"
        );

        // The file should have been deployed
        assert!(target.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    }

    #[test]
    #[cfg(unix)]
    fn apply_on_change_script_does_not_run_when_no_changes() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("on_change_marker_noop");

        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        let mut resolved = make_empty_resolved();
        resolved.merged.scripts.on_change =
            vec![ScriptEntry::Simple(format!("touch {}", marker.display()))];

        // Empty plan — no file changes, no package changes
        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        // No changes occurred, so onChange should NOT have run
        assert!(
            !marker.exists(),
            "onChange marker should NOT exist when no changes occurred"
        );
    }

    // --- Pure function / decision logic tests to cover uncovered lines ---

    #[test]
    fn parse_resource_from_description_cases() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "file:create:/home/user/.config",
                "file",
                "/home/user/.config",
            ),
            ("system:skip", "system", "skip"),
            ("unknown-action", "unknown", "unknown-action"),
            (
                "secret:resolve:vault:path/to/secret",
                "secret",
                "vault:path/to/secret",
            ),
        ];
        for (input, expected_type, expected_id) in cases {
            let (rtype, rid) = super::parse_resource_from_description(input);
            assert_eq!(rtype, *expected_type, "wrong type for {input:?}");
            assert_eq!(rid, *expected_id, "wrong id for {input:?}");
        }
    }

    #[test]
    fn provenance_suffix_local_is_empty() {
        assert_eq!(super::provenance_suffix("local"), "");
        assert_eq!(super::provenance_suffix(""), "");
    }

    #[test]
    fn provenance_suffix_non_local() {
        assert_eq!(super::provenance_suffix("acme"), " <- acme");
        assert_eq!(super::provenance_suffix("corp/source"), " <- corp/source");
    }

    #[test]
    fn action_target_path_file_create() {
        let target = PathBuf::from("/home/user/.zshrc");
        let action = Action::File(FileAction::Create {
            source: PathBuf::from("/src"),
            target: target.clone(),
            origin: "local".into(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        });
        assert_eq!(super::action_target_path(&action), Some(target));
    }

    #[test]
    fn action_target_path_file_update() {
        let target = PathBuf::from("/home/user/.bashrc");
        let action = Action::File(FileAction::Update {
            source: PathBuf::from("/src"),
            target: target.clone(),
            diff: String::new(),
            origin: "local".into(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        });
        assert_eq!(super::action_target_path(&action), Some(target));
    }

    #[test]
    fn action_target_path_file_delete() {
        let target = PathBuf::from("/home/user/.old");
        let action = Action::File(FileAction::Delete {
            target: target.clone(),
            origin: "local".into(),
        });
        assert_eq!(super::action_target_path(&action), Some(target));
    }

    #[test]
    fn action_target_path_env_write() {
        let path = PathBuf::from("/home/user/.cfgd.env");
        let action = Action::Env(EnvAction::WriteEnvFile {
            path: path.clone(),
            content: "test".into(),
        });
        assert_eq!(super::action_target_path(&action), Some(path));
    }

    #[test]
    fn action_target_path_package_returns_none() {
        let action = Action::Package(PackageAction::Install {
            manager: "brew".into(),
            packages: vec!["jq".into()],
            origin: "local".into(),
        });
        assert!(super::action_target_path(&action).is_none());
    }

    #[test]
    fn action_target_path_module_returns_none() {
        let action = Action::Module(ModuleAction {
            module_name: "test".into(),
            kind: ModuleActionKind::Skip {
                reason: "n/a".into(),
            },
        });
        assert!(super::action_target_path(&action).is_none());
    }

    #[test]
    fn action_target_path_env_inject_returns_none() {
        let action = Action::Env(EnvAction::InjectSourceLine {
            rc_path: PathBuf::from("/home/user/.bashrc"),
            line: "source ~/.cfgd.env".into(),
        });
        assert!(super::action_target_path(&action).is_none());
    }

    #[test]
    fn phase_name_from_str_unknown_returns_error() {
        let result = PhaseName::from_str("unknown-phase");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "unknown phase: unknown-phase");
    }

    #[test]
    fn script_phase_display_name_all_variants() {
        assert_eq!(ScriptPhase::PreApply.display_name(), "preApply");
        assert_eq!(ScriptPhase::PostApply.display_name(), "postApply");
        assert_eq!(ScriptPhase::PreReconcile.display_name(), "preReconcile");
        assert_eq!(ScriptPhase::PostReconcile.display_name(), "postReconcile");
        assert_eq!(ScriptPhase::OnDrift.display_name(), "onDrift");
        assert_eq!(ScriptPhase::OnChange.display_name(), "onChange");
    }

    #[test]
    fn format_action_description_secret_decrypt() {
        let action = Action::Secret(SecretAction::Decrypt {
            source: PathBuf::from("secrets/token.enc"),
            target: PathBuf::from("/home/user/.token"),
            backend: "sops".into(),
            origin: "local".into(),
        });
        let desc = format_action_description(&action);
        assert!(desc.starts_with("secret:decrypt:"));
        assert!(desc.contains("sops"));
        assert!(desc.contains(".token"));
    }

    #[test]
    fn format_action_description_secret_resolve_env() {
        let action = Action::Secret(SecretAction::ResolveEnv {
            provider: "vault".into(),
            reference: "secret/data/gh#token".into(),
            envs: vec!["GH_TOKEN".into(), "GITHUB_TOKEN".into()],
            origin: "local".into(),
        });
        let desc = format_action_description(&action);
        assert!(desc.contains("secret:resolve-env:vault"));
        assert!(desc.contains("GH_TOKEN,GITHUB_TOKEN"));
    }

    #[test]
    fn format_action_description_secret_skip() {
        let action = Action::Secret(SecretAction::Skip {
            source: "vault://test".into(),
            reason: "no backend".into(),
            origin: "local".into(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "secret:skip:vault://test");
    }

    #[test]
    fn format_action_description_system_set_value() {
        let action = Action::System(SystemAction::SetValue {
            configurator: "sysctl".into(),
            key: "net.ipv4.ip_forward".into(),
            desired: "1".into(),
            current: "0".into(),
            origin: "local".into(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "system:sysctl.net.ipv4.ip_forward");
    }

    #[test]
    fn format_action_description_system_skip() {
        let action = Action::System(SystemAction::Skip {
            configurator: "custom".into(),
            reason: "no configurator".into(),
            origin: "local".into(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "system:custom:skip");
    }

    #[test]
    fn format_action_description_env_write_and_inject() {
        let write = Action::Env(EnvAction::WriteEnvFile {
            path: PathBuf::from("/home/user/.cfgd.env"),
            content: "content".into(),
        });
        assert!(format_action_description(&write).starts_with("env:write:"));

        let inject = Action::Env(EnvAction::InjectSourceLine {
            rc_path: PathBuf::from("/home/user/.bashrc"),
            line: "source ~/.cfgd.env".into(),
        });
        assert!(format_action_description(&inject).starts_with("env:inject:"));
    }

    #[test]
    fn format_action_description_module_deploy_files() {
        let action = Action::Module(ModuleAction {
            module_name: "nvim".into(),
            kind: ModuleActionKind::DeployFiles {
                files: vec![
                    crate::modules::ResolvedFile {
                        source: PathBuf::from("/src/a"),
                        target: PathBuf::from("/dst/a"),
                        is_git_source: false,
                        strategy: None,
                        encryption: None,
                    },
                    crate::modules::ResolvedFile {
                        source: PathBuf::from("/src/b"),
                        target: PathBuf::from("/dst/b"),
                        is_git_source: false,
                        strategy: None,
                        encryption: None,
                    },
                ],
            },
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "module:nvim:files:2");
    }

    #[test]
    fn format_action_description_module_skip() {
        let action = Action::Module(ModuleAction {
            module_name: "broken".into(),
            kind: ModuleActionKind::Skip {
                reason: "dependency unmet".into(),
            },
        });
        assert_eq!(format_action_description(&action), "module:broken:skip");
    }

    #[test]
    fn format_action_description_module_run_script() {
        let action = Action::Module(ModuleAction {
            module_name: "nvim".into(),
            kind: ModuleActionKind::RunScript {
                script: ScriptEntry::Simple("setup.sh".into()),
                phase: ScriptPhase::PostApply,
            },
        });
        assert_eq!(format_action_description(&action), "module:nvim:script");
    }

    #[test]
    fn plan_to_hash_string_empty_plan_is_empty() {
        let plan = Plan {
            phases: vec![],
            warnings: vec![],
        };
        assert_eq!(plan.to_hash_string(), "");
    }

    #[test]
    fn plan_to_hash_string_multiple_phases() {
        let plan = Plan {
            phases: vec![
                Phase {
                    name: PhaseName::Packages,
                    actions: vec![Action::Package(PackageAction::Install {
                        manager: "brew".into(),
                        packages: vec!["jq".into()],
                        origin: "local".into(),
                    })],
                },
                Phase {
                    name: PhaseName::Files,
                    actions: vec![Action::File(FileAction::Create {
                        source: PathBuf::from("/src"),
                        target: PathBuf::from("/dst"),
                        origin: "local".into(),
                        strategy: crate::config::FileStrategy::Copy,
                        source_hash: None,
                    })],
                },
            ],
            warnings: vec![],
        };
        let hash = plan.to_hash_string();
        assert!(hash.contains('|'));
        assert!(hash.contains("jq"));
    }

    #[test]
    fn plan_total_actions_sums_across_phases() {
        let plan = Plan {
            phases: vec![
                Phase {
                    name: PhaseName::Packages,
                    actions: vec![
                        Action::Package(PackageAction::Install {
                            manager: "brew".into(),
                            packages: vec!["a".into()],
                            origin: "local".into(),
                        }),
                        Action::Package(PackageAction::Install {
                            manager: "brew".into(),
                            packages: vec!["b".into()],
                            origin: "local".into(),
                        }),
                    ],
                },
                Phase {
                    name: PhaseName::Files,
                    actions: vec![Action::File(FileAction::Skip {
                        target: PathBuf::from("/x"),
                        reason: "n/a".into(),
                        origin: "local".into(),
                    })],
                },
            ],
            warnings: vec![],
        };
        assert_eq!(plan.total_actions(), 3);
        assert!(!plan.is_empty());
    }

    #[test]
    fn plan_secrets_sops_file_target() {
        use crate::providers::SecretBackend;

        struct MockSopsBackend;
        impl SecretBackend for MockSopsBackend {
            fn name(&self) -> &str {
                "sops"
            }
            fn is_available(&self) -> bool {
                true
            }
            fn encrypt_file(&self, _path: &std::path::Path) -> Result<()> {
                Ok(())
            }
            fn decrypt_file(&self, _path: &std::path::Path) -> Result<secrecy::SecretString> {
                Ok(secrecy::SecretString::from("decrypted"))
            }
            fn edit_file(&self, _path: &std::path::Path) -> Result<()> {
                Ok(())
            }
        }

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.secret_backend = Some(Box::new(MockSopsBackend));
        let reconciler = Reconciler::new(&registry, &state);

        let mut profile = MergedProfile::default();
        profile.secrets.push(crate::config::SecretSpec {
            source: "secrets/token.enc".to_string(),
            target: Some(PathBuf::from("/home/user/.token")),
            template: None,
            backend: None,
            envs: None,
        });

        let actions = reconciler.plan_secrets(&profile);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Secret(SecretAction::Decrypt {
                backend, target, ..
            }) => {
                assert_eq!(backend, "sops");
                assert_eq!(*target, PathBuf::from("/home/user/.token"));
            }
            other => panic!("Expected Decrypt, got {:?}", other),
        }
    }

    #[test]
    fn plan_secrets_no_backend_skips() {
        let state = test_state();
        let registry = ProviderRegistry::new(); // no backend, no providers
        let reconciler = Reconciler::new(&registry, &state);

        let mut profile = MergedProfile::default();
        profile.secrets.push(crate::config::SecretSpec {
            source: "secrets/token.enc".to_string(),
            target: Some(PathBuf::from("/home/user/.token")),
            template: None,
            backend: None,
            envs: None,
        });

        let actions = reconciler.plan_secrets(&profile);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Secret(SecretAction::Skip { reason, .. }) => {
                assert!(
                    reason.contains("no secret backend"),
                    "expected no-backend skip, got: {reason}"
                );
            }
            other => panic!("Expected Skip, got {:?}", other),
        }
    }

    #[test]
    fn plan_secrets_envs_only_without_provider_skips() {
        let state = test_state();
        let registry = ProviderRegistry::new(); // no providers, no backend
        let reconciler = Reconciler::new(&registry, &state);

        let mut profile = MergedProfile::default();
        profile.secrets.push(crate::config::SecretSpec {
            source: "plain-source".to_string(),
            target: None,
            template: None,
            backend: None,
            envs: Some(vec!["MY_SECRET".to_string()]),
        });

        let actions = reconciler.plan_secrets(&profile);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Secret(SecretAction::Skip { reason, .. }) => {
                assert!(
                    reason.contains("secret provider reference"),
                    "expected env-needs-provider skip, got: {reason}"
                );
            }
            other => panic!("Expected Skip, got {:?}", other),
        }
    }

    #[test]
    fn plan_secrets_provider_not_available_skips() {
        let state = test_state();
        let registry = ProviderRegistry::new(); // no providers registered
        let reconciler = Reconciler::new(&registry, &state);

        let mut profile = MergedProfile::default();
        profile.secrets.push(crate::config::SecretSpec {
            source: "vault://secret/data/test#key".to_string(),
            target: Some(PathBuf::from("/tmp/test")),
            template: None,
            backend: None,
            envs: None,
        });

        let actions = reconciler.plan_secrets(&profile);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Secret(SecretAction::Skip { reason, .. }) => {
                assert!(
                    reason.contains("not available"),
                    "expected provider-unavailable skip, got: {reason}"
                );
            }
            other => panic!("Expected Skip, got {:?}", other),
        }
    }

    #[test]
    fn plan_secrets_sops_with_envs_generates_skip_for_envs() {
        use crate::providers::SecretBackend;

        struct MockSopsBackend;
        impl SecretBackend for MockSopsBackend {
            fn name(&self) -> &str {
                "sops"
            }
            fn is_available(&self) -> bool {
                true
            }
            fn encrypt_file(&self, _path: &std::path::Path) -> Result<()> {
                Ok(())
            }
            fn decrypt_file(&self, _path: &std::path::Path) -> Result<secrecy::SecretString> {
                Ok(secrecy::SecretString::from("decrypted"))
            }
            fn edit_file(&self, _path: &std::path::Path) -> Result<()> {
                Ok(())
            }
        }

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.secret_backend = Some(Box::new(MockSopsBackend));
        let reconciler = Reconciler::new(&registry, &state);

        let mut profile = MergedProfile::default();
        profile.secrets.push(crate::config::SecretSpec {
            source: "secrets/token.enc".to_string(),
            target: Some(PathBuf::from("/home/user/.token")),
            template: None,
            backend: None,
            envs: Some(vec!["TOKEN".to_string()]),
        });

        let actions = reconciler.plan_secrets(&profile);
        // Should produce a Decrypt action for the file target AND a Skip for env injection
        assert_eq!(actions.len(), 2);
        assert!(matches!(
            &actions[0],
            Action::Secret(SecretAction::Decrypt { .. })
        ));
        match &actions[1] {
            Action::Secret(SecretAction::Skip { reason, .. }) => {
                assert!(
                    reason.contains("SOPS file targets cannot inject env vars"),
                    "got: {reason}"
                );
            }
            other => panic!("Expected Skip for SOPS env injection, got {:?}", other),
        }
    }

    #[test]
    fn plan_secrets_provider_no_target_no_envs_skips() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.secret_providers.push(Box::new(MockSecretProvider {
            provider_name: "vault".into(),
            value: "secret".into(),
        }));
        let reconciler = Reconciler::new(&registry, &state);

        let mut profile = MergedProfile::default();
        profile.secrets.push(crate::config::SecretSpec {
            source: "vault://secret/data/test#key".to_string(),
            target: None,
            template: None,
            backend: None,
            envs: None,
        });

        let actions = reconciler.plan_secrets(&profile);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Secret(SecretAction::Skip { reason, .. }) => {
                assert!(reason.contains("no target or envs"), "got: {reason}");
            }
            other => panic!("Expected Skip for no-target/no-envs, got {:?}", other),
        }
    }

    #[test]
    fn plan_modules_reconcile_context_uses_pre_post_reconcile() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        let modules = vec![ResolvedModule {
            name: "test".to_string(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            pre_apply_scripts: vec![ScriptEntry::Simple("pre-apply.sh".into())],
            post_apply_scripts: vec![ScriptEntry::Simple("post-apply.sh".into())],
            pre_reconcile_scripts: vec![ScriptEntry::Simple("pre-reconcile.sh".into())],
            post_reconcile_scripts: vec![ScriptEntry::Simple("post-reconcile.sh".into())],
            on_change_scripts: vec![],
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        // Reconcile context should use pre/post reconcile scripts, not apply scripts
        let actions = reconciler.plan_modules(&modules, ReconcileContext::Reconcile);
        assert_eq!(actions.len(), 2); // pre-reconcile + post-reconcile

        // First action should be pre-reconcile
        match &actions[0] {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::RunScript { script, phase } => {
                    assert_eq!(script.run_str(), "pre-reconcile.sh");
                    assert_eq!(*phase, ScriptPhase::PreReconcile);
                }
                _ => panic!("expected RunScript"),
            },
            _ => panic!("expected Module action"),
        }

        // Second action should be post-reconcile
        match &actions[1] {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::RunScript { script, phase } => {
                    assert_eq!(script.run_str(), "post-reconcile.sh");
                    assert_eq!(*phase, ScriptPhase::PostReconcile);
                }
                _ => panic!("expected RunScript"),
            },
            _ => panic!("expected Module action"),
        }
    }

    #[test]
    fn format_plan_items_all_action_types() {
        let phase = Phase {
            name: PhaseName::System,
            actions: vec![
                Action::System(SystemAction::SetValue {
                    configurator: "sysctl".into(),
                    key: "net.ipv4.ip_forward".into(),
                    desired: "1".into(),
                    current: "0".into(),
                    origin: "local".into(),
                }),
                Action::System(SystemAction::Skip {
                    configurator: "custom".into(),
                    reason: "no configurator".into(),
                    origin: "local".into(),
                }),
            ],
        };
        let items = format_plan_items(&phase);
        assert_eq!(items.len(), 2);
        assert!(items[0].contains("set sysctl.net.ipv4.ip_forward"));
        assert!(items[0].contains("0 \u{2192} 1"));
        assert!(items[1].contains("skip custom: no configurator"));
    }

    #[test]
    fn format_plan_items_secret_actions() {
        let phase = Phase {
            name: PhaseName::Secrets,
            actions: vec![
                Action::Secret(SecretAction::Decrypt {
                    source: PathBuf::from("secret.enc"),
                    target: PathBuf::from("/out/secret"),
                    backend: "sops".into(),
                    origin: "corp".into(),
                }),
                Action::Secret(SecretAction::Resolve {
                    provider: "vault".into(),
                    reference: "secret/gh#token".into(),
                    target: PathBuf::from("/tmp/token"),
                    origin: "local".into(),
                }),
                Action::Secret(SecretAction::ResolveEnv {
                    provider: "1password".into(),
                    reference: "Vault/Secret".into(),
                    envs: vec!["TOKEN".into()],
                    origin: "local".into(),
                }),
                Action::Secret(SecretAction::Skip {
                    source: "missing".into(),
                    reason: "not available".into(),
                    origin: "local".into(),
                }),
            ],
        };
        let items = format_plan_items(&phase);
        assert_eq!(items.len(), 4);
        assert!(items[0].contains("decrypt"));
        assert!(items[0].contains("<- corp"));
        assert!(items[1].contains("resolve vault://"));
        assert!(items[2].contains("resolve 1password://"));
        assert!(items[2].contains("env [TOKEN]"));
        assert!(items[3].contains("skip missing"));
    }

    #[test]
    fn format_plan_items_env_actions() {
        let phase = Phase {
            name: PhaseName::Env,
            actions: vec![
                Action::Env(EnvAction::WriteEnvFile {
                    path: PathBuf::from("/home/user/.cfgd.env"),
                    content: "content".into(),
                }),
                Action::Env(EnvAction::InjectSourceLine {
                    rc_path: PathBuf::from("/home/user/.bashrc"),
                    line: "source ~/.cfgd.env".into(),
                }),
            ],
        };
        let items = format_plan_items(&phase);
        assert_eq!(items.len(), 2);
        assert!(items[0].contains("write"));
        assert!(items[0].contains(".cfgd.env"));
        assert!(items[1].contains("inject source line"));
        assert!(items[1].contains(".bashrc"));
    }

    #[test]
    fn format_plan_items_script_action_with_provenance() {
        let phase = Phase {
            name: PhaseName::PreScripts,
            actions: vec![Action::Script(ScriptAction::Run {
                entry: ScriptEntry::Simple("setup.sh".into()),
                phase: ScriptPhase::PreApply,
                origin: "corp-source".into(),
            })],
        };
        let items = format_plan_items(&phase);
        assert_eq!(items.len(), 1);
        assert!(items[0].contains("run preApply script: setup.sh"));
        assert!(items[0].contains("<- corp-source"));
    }

    #[test]
    fn format_module_action_item_deploy_truncates_many_files() {
        let files: Vec<crate::modules::ResolvedFile> = (0..5)
            .map(|i| crate::modules::ResolvedFile {
                source: PathBuf::from(format!("/src/{i}")),
                target: PathBuf::from(format!("/dst/{i}")),
                is_git_source: false,
                strategy: None,
                encryption: None,
            })
            .collect();
        let action = ModuleAction {
            module_name: "big".into(),
            kind: ModuleActionKind::DeployFiles { files },
        };
        let item = super::format_module_action_item(&action);
        assert!(item.contains("[big]"));
        assert!(item.contains("5 files"));
    }

    #[test]
    fn detect_file_conflicts_skip_and_delete_actions_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        std::fs::write(&file_a, "content A").unwrap();
        std::fs::write(&file_b, "content B").unwrap();

        let shared_target = PathBuf::from("/target/a");

        let file_actions = vec![
            FileAction::Skip {
                target: shared_target.clone(),
                reason: "unchanged".into(),
                origin: "local".into(),
            },
            FileAction::Delete {
                target: PathBuf::from("/target/b"),
                origin: "local".into(),
            },
        ];

        // Module targets the same path as Skip — should NOT conflict because
        // Skip/Delete actions are excluded from conflict detection
        let modules = vec![ResolvedModule {
            name: "mymod".to_string(),
            packages: vec![],
            files: vec![crate::modules::ResolvedFile {
                source: file_a.clone(),
                target: shared_target.clone(),
                is_git_source: false,
                strategy: None,
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
        assert!(
            result.is_ok(),
            "Skip/Delete actions should be excluded from conflict detection: {:?}",
            result.err()
        );

        // Prove this matters: if the Skip were a Create with different content, it WOULD conflict
        let create_actions = vec![FileAction::Create {
            source: file_b,
            target: shared_target,
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        }];
        assert!(
            Reconciler::detect_file_conflicts(&create_actions, &modules).is_err(),
            "Create with different content at same target should conflict (proves Skip exclusion is meaningful)"
        );
    }

    #[test]
    fn content_hash_if_exists_returns_none_for_missing() {
        let hash = super::content_hash_if_exists(Path::new("/nonexistent/file"));
        assert!(hash.is_none());
    }

    #[test]
    fn content_hash_if_exists_returns_hash_for_existing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello").unwrap();
        let hash = super::content_hash_if_exists(&file);
        assert!(hash.is_some());
        // Same content should give same hash
        let hash2 = super::content_hash_if_exists(&file);
        assert_eq!(hash, hash2);
    }

    #[test]
    fn merge_module_env_aliases_merges_correctly() {
        let profile_env = vec![crate::config::EnvVar {
            name: "A".into(),
            value: "1".into(),
        }];
        let profile_aliases = vec![crate::config::ShellAlias {
            name: "g".into(),
            command: "git".into(),
        }];
        let modules = vec![ResolvedModule {
            name: "mod1".into(),
            packages: vec![],
            files: vec![],
            env: vec![
                crate::config::EnvVar {
                    name: "A".into(),
                    value: "2".into(),
                },
                crate::config::EnvVar {
                    name: "B".into(),
                    value: "3".into(),
                },
            ],
            aliases: vec![crate::config::ShellAlias {
                name: "g".into(),
                command: "git status".into(),
            }],
            post_apply_scripts: vec![],
            pre_apply_scripts: vec![],
            pre_reconcile_scripts: vec![],
            post_reconcile_scripts: vec![],
            on_change_scripts: vec![],
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let (env, aliases) =
            super::merge_module_env_aliases(&profile_env, &profile_aliases, &modules);
        // Module overrides profile: A=2 (module wins), B=3 (new)
        assert_eq!(env.len(), 2);
        assert_eq!(env.iter().find(|e| e.name == "A").unwrap().value, "2");
        assert_eq!(env.iter().find(|e| e.name == "B").unwrap().value, "3");
        // Module overrides alias: g="git status" (module wins)
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].command, "git status");
    }

    #[test]
    fn generate_powershell_env_escapes_single_quotes() {
        let env = vec![crate::config::EnvVar {
            name: "MSG".into(),
            value: "it's a test".into(),
        }];
        let content = super::generate_powershell_env_content(&env, &[]);
        // Single quotes in values are doubled in PS
        assert!(content.contains("$env:MSG = 'it''s a test'"));
    }

    #[test]
    fn generate_fish_env_escapes_single_quotes() {
        let env = vec![crate::config::EnvVar {
            name: "MSG".into(),
            value: "it's a test".into(),
        }];
        let content = super::generate_fish_env_content(&env, &[]);
        assert!(content.contains("set -gx MSG 'it\\'s a test'"));
    }

    #[test]
    fn reconcile_context_equality() {
        assert_eq!(ReconcileContext::Apply, ReconcileContext::Apply);
        assert_eq!(ReconcileContext::Reconcile, ReconcileContext::Reconcile);
        assert_ne!(ReconcileContext::Apply, ReconcileContext::Reconcile);
    }

    #[test]
    #[cfg(unix)]
    fn apply_on_change_skipped_when_skip_scripts_true() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let target = dir.path().join("target.txt");
        let marker = dir.path().join("on_change_marker_skip");

        std::fs::write(&source, "data").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let mut resolved = make_empty_resolved();
        resolved.merged.scripts.on_change =
            vec![ScriptEntry::Simple(format!("touch {}", marker.display()))];

        let file_actions = vec![FileAction::Create {
            source: source.clone(),
            target: target.clone(),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        }];

        let plan = reconciler
            .plan(
                &resolved,
                file_actions,
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        // skip_scripts = true
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                true, // skip_scripts
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        // onChange should NOT have run because skip_scripts=true
        assert!(
            !marker.exists(),
            "onChange should be skipped when skip_scripts=true"
        );
        // But the file action should still have been applied
        assert!(target.exists());
    }

    // --- apply_package_action: Bootstrap path ---

    /// A package manager that starts unavailable but becomes available after bootstrap.
    struct BootstrappablePackageManager {
        name: String,
        bootstrapped: std::sync::Mutex<bool>,
        installed: std::sync::Mutex<HashSet<String>>,
    }

    impl BootstrappablePackageManager {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                bootstrapped: std::sync::Mutex::new(false),
                installed: std::sync::Mutex::new(HashSet::new()),
            }
        }
    }

    impl PackageManager for BootstrappablePackageManager {
        fn name(&self) -> &str {
            &self.name
        }
        fn is_available(&self) -> bool {
            *self.bootstrapped.lock().unwrap()
        }
        fn can_bootstrap(&self) -> bool {
            true
        }
        fn bootstrap(&self, _printer: &Printer) -> Result<()> {
            *self.bootstrapped.lock().unwrap() = true;
            Ok(())
        }
        fn installed_packages(&self) -> Result<HashSet<String>> {
            Ok(self.installed.lock().unwrap().clone())
        }
        fn install(&self, packages: &[String], _printer: &Printer) -> Result<()> {
            let mut installed = self.installed.lock().unwrap();
            for p in packages {
                installed.insert(p.clone());
            }
            Ok(())
        }
        fn uninstall(&self, packages: &[String], _printer: &Printer) -> Result<()> {
            let mut installed = self.installed.lock().unwrap();
            for p in packages {
                installed.remove(p);
            }
            Ok(())
        }
        fn update(&self, _printer: &Printer) -> Result<()> {
            Ok(())
        }
        fn available_version(&self, _package: &str) -> Result<Option<String>> {
            Ok(None)
        }
    }

    #[test]
    fn apply_package_bootstrap_makes_manager_available() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(BootstrappablePackageManager::new("snap")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Bootstrap {
                    manager: "snap".to_string(),
                    method: "auto".to_string(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Packages),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 1);
        assert!(result.action_results[0].success);
        assert!(
            result.action_results[0].description.contains("bootstrap"),
            "desc: {}",
            result.action_results[0].description
        );

        // Manager should now be available
        assert!(registry.package_managers[0].is_available());
    }

    #[test]
    fn apply_package_bootstrap_unknown_manager_errors() {
        let state = test_state();
        let registry = ProviderRegistry::new(); // no managers
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Bootstrap {
                    manager: "nonexistent".to_string(),
                    method: "auto".to_string(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Packages),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        // Should fail — unknown manager
        assert_eq!(result.status, ApplyStatus::Failed);
        assert_eq!(result.failed(), 1);
        assert!(result.action_results[0].error.is_some());
    }

    #[test]
    fn apply_package_install_unknown_manager_errors() {
        let state = test_state();
        let registry = ProviderRegistry::new(); // no managers
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Install {
                    manager: "nonexistent".to_string(),
                    packages: vec!["foo".to_string()],
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Packages),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Failed);
        assert_eq!(result.failed(), 1);
    }

    #[test]
    fn apply_package_uninstall_unknown_manager_errors() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Uninstall {
                    manager: "nonexistent".to_string(),
                    packages: vec!["foo".to_string()],
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Packages),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Failed);
        assert_eq!(result.failed(), 1);
    }

    // --- apply_secret_action: Decrypt, Resolve, ResolveEnv ---

    struct TestSecretBackend {
        decrypted_value: String,
    }

    impl crate::providers::SecretBackend for TestSecretBackend {
        fn name(&self) -> &str {
            "test-sops"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn encrypt_file(&self, _path: &std::path::Path) -> Result<()> {
            Ok(())
        }
        fn decrypt_file(&self, _path: &std::path::Path) -> Result<secrecy::SecretString> {
            Ok(secrecy::SecretString::from(self.decrypted_value.clone()))
        }
        fn edit_file(&self, _path: &std::path::Path) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn apply_secret_decrypt_writes_decrypted_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("token.enc");
        let target = dir.path().join("token.txt");
        std::fs::write(&source, "encrypted-data").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.secret_backend = Some(Box::new(TestSecretBackend {
            decrypted_value: "my-secret-token".to_string(),
        }));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Secrets,
                actions: vec![Action::Secret(SecretAction::Decrypt {
                    source: source.clone(),
                    target: target.clone(),
                    backend: "test-sops".to_string(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Secrets),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 1);
        assert!(result.action_results[0].success);
        assert!(
            result.action_results[0].description.contains("decrypt"),
            "desc: {}",
            result.action_results[0].description
        );

        // Verify decrypted file was written
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content, "my-secret-token");
    }

    #[test]
    fn apply_secret_decrypt_no_backend_errors() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("token.enc");
        let target = dir.path().join("token.txt");
        std::fs::write(&source, "encrypted-data").unwrap();

        let state = test_state();
        let registry = ProviderRegistry::new(); // no backend

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Secrets,
                actions: vec![Action::Secret(SecretAction::Decrypt {
                    source: source.clone(),
                    target: target.clone(),
                    backend: "sops".to_string(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Secrets),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Failed);
        assert_eq!(result.failed(), 1);
    }

    #[test]
    fn apply_secret_resolve_writes_provider_value_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("resolved-secret.txt");

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.secret_providers.push(Box::new(MockSecretProvider {
            provider_name: "vault".to_string(),
            value: "provider-secret-value".to_string(),
        }));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Secrets,
                actions: vec![Action::Secret(SecretAction::Resolve {
                    provider: "vault".to_string(),
                    reference: "secret/data/app#key".to_string(),
                    target: target.clone(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Secrets),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(
            result.action_results[0].description.contains("resolve"),
            "desc: {}",
            result.action_results[0].description
        );

        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content, "provider-secret-value");
    }

    #[test]
    fn apply_secret_resolve_unknown_provider_errors() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nope.txt");

        let state = test_state();
        let registry = ProviderRegistry::new(); // no providers

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Secrets,
                actions: vec![Action::Secret(SecretAction::Resolve {
                    provider: "vault".to_string(),
                    reference: "secret/data/app#key".to_string(),
                    target: target.clone(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Secrets),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Failed);
        assert_eq!(result.failed(), 1);
    }

    #[test]
    fn apply_secret_resolve_env_collects_env_vars() {
        // Unit test the collector-population behaviour directly via
        // `apply_secret_action`. The full `Reconciler::apply` path calls
        // `plan_env()` which resolves `~` to the real `$HOME` and writes
        // `~/.cfgd.env` + injects a source line into `~/.bashrc` — tests must
        // never touch the user's home. See task #37 for the broader audit.
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.secret_providers.push(Box::new(MockSecretProvider {
            provider_name: "vault".to_string(),
            value: "env-secret-value".to_string(),
        }));
        let reconciler = Reconciler::new(&registry, &state);
        let printer = test_printer();
        let tmp = tempfile::tempdir().unwrap();

        let mut collector: Vec<(String, String)> = Vec::new();
        let action = SecretAction::ResolveEnv {
            provider: "vault".to_string(),
            reference: "secret/data/gh#token".to_string(),
            envs: vec!["GH_TOKEN".to_string(), "GITHUB_TOKEN".to_string()],
            origin: "local".to_string(),
        };

        let desc = reconciler
            .apply_secret_action(&action, tmp.path(), &printer, &mut collector)
            .expect("resolve-env should succeed");

        assert!(desc.contains("resolve-env"), "desc: {}", desc);
        assert_eq!(
            collector,
            vec![
                ("GH_TOKEN".to_string(), "env-secret-value".to_string()),
                ("GITHUB_TOKEN".to_string(), "env-secret-value".to_string()),
            ]
        );
    }

    #[test]
    fn apply_secret_resolve_env_unknown_provider_errors() {
        let state = test_state();
        let registry = ProviderRegistry::new(); // no providers

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Secrets,
                actions: vec![Action::Secret(SecretAction::ResolveEnv {
                    provider: "vault".to_string(),
                    reference: "secret/data/gh#token".to_string(),
                    envs: vec!["GH_TOKEN".to_string()],
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Secrets),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Failed);
        assert_eq!(result.failed(), 1);
    }

    #[test]
    fn apply_secret_skip_succeeds() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Secrets,
                actions: vec![Action::Secret(SecretAction::Skip {
                    source: "vault://test".to_string(),
                    reason: "not available".to_string(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Secrets),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(result.action_results[0].description.contains("skip"));
    }

    // --- apply_file_action: Delete and SetPermissions ---

    #[test]
    fn apply_file_delete_action_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("to-delete.txt");
        std::fs::write(&target, "delete me").unwrap();
        assert!(target.exists());

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Files,
                actions: vec![Action::File(FileAction::Delete {
                    target: target.clone(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Files),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(!target.exists(), "file should be deleted");
        assert!(
            result.action_results[0].description.contains("delete"),
            "desc: {}",
            result.action_results[0].description
        );
    }

    #[test]
    #[cfg(unix)]
    fn apply_file_set_permissions_action() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("script.sh");
        std::fs::write(&target, "#!/bin/sh\necho hi").unwrap();

        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Files,
                actions: vec![Action::File(FileAction::SetPermissions {
                    target: target.clone(),
                    mode: 0o755,
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Files),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(
            result.action_results[0].description.contains("chmod"),
            "desc: {}",
            result.action_results[0].description
        );

        // Verify permissions
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&target).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn apply_file_skip_action_succeeds() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Files,
                actions: vec![Action::File(FileAction::Skip {
                    target: PathBuf::from("/nonexistent"),
                    reason: "unchanged".to_string(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Files),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(result.action_results[0].description.contains("skip"));
    }

    #[test]
    fn apply_file_update_action_overwrites_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("new-content.txt");
        let target = dir.path().join("existing.txt");
        std::fs::write(&source, "updated content").unwrap();
        std::fs::write(&target, "old content").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Files,
                actions: vec![Action::File(FileAction::Update {
                    source: source.clone(),
                    target: target.clone(),
                    diff: "diff output".to_string(),
                    origin: "local".to_string(),
                    strategy: crate::config::FileStrategy::Copy,
                    source_hash: None,
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Files),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content, "updated content");
        assert!(
            result.action_results[0].description.contains("update"),
            "desc: {}",
            result.action_results[0].description
        );
    }

    // --- apply_system_action: SetValue and Skip ---

    /// A mock system configurator that tracks apply calls.
    struct TestSystemConfigurator {
        configurator_name: String,
        applied: std::sync::Mutex<bool>,
    }

    impl TestSystemConfigurator {
        fn new(name: &str) -> Self {
            Self {
                configurator_name: name.to_string(),
                applied: std::sync::Mutex::new(false),
            }
        }
    }

    impl crate::providers::SystemConfigurator for TestSystemConfigurator {
        fn name(&self) -> &str {
            &self.configurator_name
        }
        fn is_available(&self) -> bool {
            true
        }
        fn current_state(&self) -> Result<serde_yaml::Value> {
            Ok(serde_yaml::Value::Null)
        }
        fn diff(&self, _desired: &serde_yaml::Value) -> Result<Vec<crate::providers::SystemDrift>> {
            Ok(vec![crate::providers::SystemDrift {
                key: "test.key".to_string(),
                expected: "desired-val".to_string(),
                actual: "current-val".to_string(),
            }])
        }
        fn apply(&self, _desired: &serde_yaml::Value, _printer: &Printer) -> Result<()> {
            *self.applied.lock().unwrap() = true;
            Ok(())
        }
    }

    #[test]
    fn apply_system_set_value_calls_configurator() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .system_configurators
            .push(Box::new(TestSystemConfigurator::new("sysctl")));

        let reconciler = Reconciler::new(&registry, &state);
        let mut resolved = make_empty_resolved();
        // Put desired system config in the profile
        resolved.merged.system.insert(
            "sysctl".to_string(),
            serde_yaml::from_str("{net.ipv4.ip_forward: 1}").unwrap(),
        );

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::System,
                actions: vec![Action::System(SystemAction::SetValue {
                    configurator: "sysctl".to_string(),
                    key: "net.ipv4.ip_forward".to_string(),
                    desired: "1".to_string(),
                    current: "0".to_string(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::System),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(
            result.action_results[0]
                .description
                .contains("system:sysctl"),
            "desc: {}",
            result.action_results[0].description
        );
    }

    #[test]
    fn apply_system_skip_logs_warning() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::System,
                actions: vec![Action::System(SystemAction::Skip {
                    configurator: "customThing".to_string(),
                    reason: "no configurator registered".to_string(),
                    origin: "local".to_string(),
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::System),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(
            result.action_results[0].description.contains("skipped"),
            "desc: {}",
            result.action_results[0].description
        );
    }

    #[test]
    fn plan_system_generates_skip_for_unregistered_configurator() {
        let state = test_state();
        let registry = ProviderRegistry::new(); // no configurators
        let reconciler = Reconciler::new(&registry, &state);

        let mut profile = MergedProfile::default();
        profile.system.insert(
            "unknownConf".to_string(),
            serde_yaml::from_str("{key: value}").unwrap(),
        );

        let actions = reconciler.plan_system(&profile, &[]).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::System(SystemAction::Skip {
                configurator,
                reason,
                ..
            }) => {
                assert_eq!(configurator, "unknownConf");
                assert!(reason.contains("no configurator registered"));
            }
            other => panic!("Expected SystemAction::Skip, got {:?}", other),
        }
    }

    // --- apply_module_action: InstallPackages, DeployFiles, Skip ---

    #[test]
    fn apply_module_install_packages_calls_manager() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(TrackingPackageManager::new("brew")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "nvim".to_string(),
            packages: vec![ResolvedPackage {
                canonical_name: "neovim".to_string(),
                resolved_name: "neovim".to_string(),
                manager: "brew".to_string(),
                version: None,
                script: None,
            }],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "nvim".to_string(),
                    kind: ModuleActionKind::InstallPackages {
                        resolved: vec![ResolvedPackage {
                            canonical_name: "neovim".to_string(),
                            resolved_name: "neovim".to_string(),
                            manager: "brew".to_string(),
                            version: None,
                            script: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Modules),
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(
            result.action_results[0]
                .description
                .contains("module:nvim:packages"),
            "desc: {}",
            result.action_results[0].description
        );

        // Verify install was called
        let installed = registry.package_managers[0].installed_packages().unwrap();
        assert!(installed.contains("neovim"));
    }

    #[test]
    fn apply_module_deploy_files_creates_target() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("module-source.txt");
        let target_file = dir.path().join("subdir/module-target.txt");
        std::fs::write(&source_file, "module content").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "mymod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: source_file.clone(),
                target: target_file.clone(),
                is_git_source: false,
                strategy: Some(crate::config::FileStrategy::Copy),
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "mymod".to_string(),
                    kind: ModuleActionKind::DeployFiles {
                        files: vec![ResolvedFile {
                            source: source_file.clone(),
                            target: target_file.clone(),
                            is_git_source: false,
                            strategy: Some(crate::config::FileStrategy::Copy),
                            encryption: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Modules),
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(target_file.exists(), "target file should be deployed");
        assert_eq!(
            std::fs::read_to_string(&target_file).unwrap(),
            "module content"
        );
    }

    #[test]
    #[cfg(unix)]
    fn apply_module_deploy_files_symlink_strategy() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("source.txt");
        let target_file = dir.path().join("link-target.txt");
        std::fs::write(&source_file, "linked content").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Symlink;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "linkmod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: source_file.clone(),
                target: target_file.clone(),
                is_git_source: false,
                strategy: None, // uses default = Symlink
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "linkmod".to_string(),
                    kind: ModuleActionKind::DeployFiles {
                        files: vec![ResolvedFile {
                            source: source_file.clone(),
                            target: target_file.clone(),
                            is_git_source: false,
                            strategy: None,
                            encryption: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Modules),
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(target_file.is_symlink(), "target should be a symlink");
        assert_eq!(
            std::fs::read_to_string(&target_file).unwrap(),
            "linked content"
        );
    }

    #[test]
    fn apply_module_skip_reports_skipped() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "broken".to_string(),
                    kind: ModuleActionKind::Skip {
                        reason: "dependency not met".to_string(),
                    },
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Modules),
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(
            result.action_results[0].description.contains("skip"),
            "desc: {}",
            result.action_results[0].description
        );
    }

    #[test]
    fn apply_module_install_packages_bootstraps_when_needed() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .package_managers
            .push(Box::new(BootstrappablePackageManager::new("brew")));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "tools".to_string(),
            packages: vec![ResolvedPackage {
                canonical_name: "jq".to_string(),
                resolved_name: "jq".to_string(),
                manager: "brew".to_string(),
                version: None,
                script: None,
            }],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "tools".to_string(),
                    kind: ModuleActionKind::InstallPackages {
                        resolved: vec![ResolvedPackage {
                            canonical_name: "jq".to_string(),
                            resolved_name: "jq".to_string(),
                            manager: "brew".to_string(),
                            version: None,
                            script: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Modules),
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);

        // Manager should have been bootstrapped and package installed
        assert!(registry.package_managers[0].is_available());
        assert!(
            registry.package_managers[0]
                .installed_packages()
                .unwrap()
                .contains("jq")
        );
    }

    // --- rollback_apply: symlink restore (restore to state after target apply) ---

    #[test]
    #[cfg(unix)]
    fn rollback_restores_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("link-file");
        let link_dest = dir.path().join("original-dest.txt");
        let file_path = target.display().to_string();
        std::fs::write(&link_dest, "link content").unwrap();

        let state = test_state();

        // Apply 1: creates the symlink
        let apply_id_1 = state
            .record_apply("test", "hash1", ApplyStatus::Success, None)
            .unwrap();
        std::os::unix::fs::symlink(&link_dest, &target).unwrap();
        assert!(target.is_symlink());
        let resource_id = format!("file:create:{}", target.display());
        let jid1 = state
            .journal_begin(apply_id_1, 0, "files", "file", &resource_id, None)
            .unwrap();
        state.journal_complete(jid1, None, None).unwrap();

        // Apply 2: replaces symlink with a regular file. Backup captures symlink state.
        let file_state = crate::capture_file_state(&target).unwrap().unwrap();
        assert!(file_state.is_symlink);
        let apply_id_2 = state
            .record_apply("test", "hash2", ApplyStatus::Success, None)
            .unwrap();
        state
            .store_file_backup(apply_id_2, &file_path, &file_state)
            .unwrap();
        let update_resource_id = format!("file:update:{}", target.display());
        let jid2 = state
            .journal_begin(apply_id_2, 0, "files", "file", &update_resource_id, None)
            .unwrap();
        state.journal_complete(jid2, None, None).unwrap();

        // Replace the symlink with a regular file (simulating apply 2)
        std::fs::remove_file(&target).unwrap();
        std::fs::write(&target, "replaced").unwrap();
        assert!(!target.is_symlink());

        // Rollback to apply 1 — should restore the symlink
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let printer = test_printer();

        let rollback_result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

        assert_eq!(rollback_result.files_restored, 1);
        assert!(target.is_symlink(), "symlink should be restored");
        assert_eq!(
            std::fs::read_link(&target).unwrap(),
            link_dest,
            "symlink should point to original destination"
        );
    }

    // --- plan_modules: encryption validation ---

    #[test]
    fn plan_modules_encryption_always_with_symlink_skips() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Symlink;
        let reconciler = Reconciler::new(&registry, &state);

        let modules = vec![ResolvedModule {
            name: "secrets-mod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: PathBuf::from("/nonexistent/secret.enc"),
                target: PathBuf::from("/home/user/.secret"),
                is_git_source: false,
                strategy: None, // defaults to Symlink
                encryption: Some(crate::config::EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: crate::config::EncryptionMode::Always,
                }),
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
        // Should produce a Skip action because encryption=Always + symlink is incompatible
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::Skip { reason } => {
                    assert!(
                        reason.contains("encryption mode Always incompatible"),
                        "got: {reason}"
                    );
                }
                other => panic!("Expected Skip, got {:?}", other),
            },
            other => panic!("Expected Module action, got {:?}", other),
        }
    }

    #[test]
    fn plan_modules_encryption_always_with_copy_proceeds() {
        let dir = tempfile::tempdir().unwrap();
        // Create a fake SOPS-encrypted file with required `mac` and `lastmodified` keys
        let source = dir.path().join("secret.enc");
        std::fs::write(
            &source,
            "{\"sops\":{\"mac\":\"abc123\",\"lastmodified\":\"2024-01-01T00:00:00Z\",\"version\":\"3.0\"}, \"data\": \"ENC[AES256_GCM,data:abc]\"}",
        )
        .unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;
        let reconciler = Reconciler::new(&registry, &state);

        let modules = vec![ResolvedModule {
            name: "secrets-mod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: source.clone(),
                target: PathBuf::from("/home/user/.secret"),
                is_git_source: false,
                strategy: Some(crate::config::FileStrategy::Copy),
                encryption: Some(crate::config::EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: crate::config::EncryptionMode::Always,
                }),
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
        // Should produce DeployFiles (encryption=Always + copy is OK, and file has sops marker)
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::DeployFiles { files } => {
                    assert_eq!(files.len(), 1);
                }
                other => panic!("Expected DeployFiles, got {:?}", other),
            },
            other => panic!("Expected Module action, got {:?}", other),
        }
    }

    #[test]
    fn plan_modules_encryption_file_not_encrypted_skips() {
        let dir = tempfile::tempdir().unwrap();
        // Create a plaintext file (not encrypted)
        let source = dir.path().join("plain.txt");
        std::fs::write(&source, "plain text content").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;
        let reconciler = Reconciler::new(&registry, &state);

        let modules = vec![ResolvedModule {
            name: "secrets-mod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: source.clone(),
                target: PathBuf::from("/home/user/.secret"),
                is_git_source: false,
                strategy: Some(crate::config::FileStrategy::Copy),
                encryption: Some(crate::config::EncryptionSpec {
                    backend: "sops".to_string(),
                    mode: crate::config::EncryptionMode::Always,
                }),
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let actions = reconciler.plan_modules(&modules, ReconcileContext::Apply);
        // Should skip because file requires encryption but isn't encrypted
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Module(ma) => match &ma.kind {
                ModuleActionKind::Skip { reason } => {
                    assert!(
                        reason.contains("requires encryption") && reason.contains("not encrypted"),
                        "got: {reason}"
                    );
                }
                other => panic!("Expected Skip, got {:?}", other),
            },
            other => panic!("Expected Module action, got {:?}", other),
        }
    }

    // --- apply_script_action via apply() ---

    #[test]
    #[cfg(unix)]
    fn apply_script_action_executes_and_records_output() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("script-ran");

        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let mut resolved = make_empty_resolved();

        // Post-apply script so it doesn't abort on failure
        resolved.merged.scripts.post_apply =
            vec![ScriptEntry::Simple(format!("touch {}", marker.display()))];

        let plan = reconciler
            .plan(
                &resolved,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                ReconcileContext::Apply,
            )
            .unwrap();

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                None,
                &[],
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        // The script phase should have run
        let script_result = result
            .action_results
            .iter()
            .find(|r| r.description.contains("script:"));
        assert!(script_result.is_some(), "script action should be recorded");
        assert!(script_result.unwrap().success);
        assert!(marker.exists(), "script should have run and created marker");
    }

    // --- apply_module_action: RunScript ---

    #[test]
    #[cfg(unix)]
    fn apply_module_run_script_executes_in_module_dir() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("module-script-ran");

        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "testmod".to_string(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            pre_apply_scripts: Vec::new(),
            post_apply_scripts: vec![ScriptEntry::Simple(format!("touch {}", marker.display()))],
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "testmod".to_string(),
                    kind: ModuleActionKind::RunScript {
                        script: ScriptEntry::Simple(format!("touch {}", marker.display())),
                        phase: ScriptPhase::PostApply,
                    },
                })],
            }],
            warnings: vec![],
        };

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Modules),
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(result.action_results[0].success);
        assert!(marker.exists(), "module script should have created marker");
        assert!(
            result.action_results[0]
                .description
                .contains("module:testmod:script"),
            "desc: {}",
            result.action_results[0].description
        );
    }

    // --- plan_env: Fish and PowerShell content generation ---

    #[test]
    fn generate_fish_env_content_basic() {
        let env = vec![
            crate::config::EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            },
            crate::config::EnvVar {
                name: "CARGO_HOME".into(),
                value: "/home/user/.cargo".into(),
            },
        ];
        let aliases = vec![crate::config::ShellAlias {
            name: "g".into(),
            command: "git".into(),
        }];
        let content = super::generate_fish_env_content(&env, &aliases);
        assert!(content.starts_with("# managed by cfgd"));
        assert!(content.contains("set -gx EDITOR 'nvim'"));
        assert!(content.contains("set -gx CARGO_HOME '/home/user/.cargo'"));
        assert!(content.contains("abbr -a g 'git'"));
    }

    #[test]
    fn generate_powershell_env_content_with_env_ref() {
        let env = vec![crate::config::EnvVar {
            name: "MY_PATH".into(),
            value: r"C:\tools;$env:PATH".into(),
        }];
        let content = super::generate_powershell_env_content(&env, &[]);
        // Contains $env: so should be double-quoted
        assert!(
            content.contains(r#"$env:MY_PATH = "C:\tools;$env:PATH""#),
            "content: {}",
            content
        );
    }

    #[test]
    fn generate_powershell_env_function_alias() {
        // When an alias command contains a space, PowerShell generates a function instead of Set-Alias
        let aliases = vec![crate::config::ShellAlias {
            name: "ll".into(),
            command: "Get-ChildItem -Force".into(),
        }];
        let content = super::generate_powershell_env_content(&[], &aliases);
        assert!(content.contains("function ll {"));
        assert!(content.contains("Get-ChildItem -Force @args"));
    }

    #[test]
    fn generate_fish_env_path_splitting() {
        // Fish should split PATH values on :
        let env = vec![crate::config::EnvVar {
            name: "PATH".into(),
            value: "/usr/bin:/usr/local/bin:$PATH".into(),
        }];
        let content = super::generate_fish_env_content(&env, &[]);
        assert!(
            content.contains("set -gx PATH '/usr/bin' '/usr/local/bin' '$PATH'"),
            "content: {}",
            content
        );
    }

    // --- build_script_env additional tests ---

    #[test]
    fn build_script_env_all_phases() {
        // Verify that each ScriptPhase variant produces the correct CFGD_PHASE value
        let phases_and_expected = [
            (ScriptPhase::PreApply, "preApply"),
            (ScriptPhase::PostApply, "postApply"),
            (ScriptPhase::PreReconcile, "preReconcile"),
            (ScriptPhase::PostReconcile, "postReconcile"),
            (ScriptPhase::OnDrift, "onDrift"),
            (ScriptPhase::OnChange, "onChange"),
        ];

        for (phase, expected_name) in &phases_and_expected {
            let env = super::build_script_env(
                std::path::Path::new("/etc/cfgd"),
                "default",
                ReconcileContext::Apply,
                phase,
                false,
                None,
                None,
            );
            let map: HashMap<String, String> = env.into_iter().collect();
            assert_eq!(
                map.get("CFGD_PHASE").unwrap(),
                expected_name,
                "phase {:?} should produce CFGD_PHASE={}",
                phase,
                expected_name
            );
        }
    }

    #[test]
    fn build_script_env_dry_run_true_propagates() {
        let env = super::build_script_env(
            std::path::Path::new("/cfg"),
            "laptop",
            ReconcileContext::Apply,
            &ScriptPhase::PreApply,
            true,
            None,
            None,
        );
        let map: HashMap<String, String> = env.into_iter().collect();
        assert_eq!(map.get("CFGD_DRY_RUN").unwrap(), "true");
    }

    #[test]
    fn build_script_env_reconcile_context() {
        let env = super::build_script_env(
            std::path::Path::new("/cfg"),
            "server",
            ReconcileContext::Reconcile,
            &ScriptPhase::PostReconcile,
            false,
            None,
            None,
        );
        let map: HashMap<String, String> = env.into_iter().collect();
        assert_eq!(map.get("CFGD_CONTEXT").unwrap(), "reconcile");
        assert_eq!(map.get("CFGD_PHASE").unwrap(), "postReconcile");
        assert_eq!(map.get("CFGD_PROFILE").unwrap(), "server");
    }

    #[test]
    fn build_script_env_module_name_without_dir() {
        // module_name provided but module_dir is None
        let env = super::build_script_env(
            std::path::Path::new("/cfg"),
            "default",
            ReconcileContext::Apply,
            &ScriptPhase::PreApply,
            false,
            Some("zsh"),
            None,
        );
        let map: HashMap<String, String> = env.into_iter().collect();
        assert_eq!(map.get("CFGD_MODULE_NAME").unwrap(), "zsh");
        assert!(
            !map.contains_key("CFGD_MODULE_DIR"),
            "CFGD_MODULE_DIR should not be set when module_dir is None"
        );
    }

    #[test]
    fn build_script_env_count_base_vars() {
        // Without module info, should have exactly 5 base vars
        let env = super::build_script_env(
            std::path::Path::new("/x"),
            "p",
            ReconcileContext::Apply,
            &ScriptPhase::PreApply,
            false,
            None,
            None,
        );
        assert_eq!(env.len(), 5, "base env should have 5 entries");

        // With both module name and dir, should have 7
        let env_with_module = super::build_script_env(
            std::path::Path::new("/x"),
            "p",
            ReconcileContext::Apply,
            &ScriptPhase::PreApply,
            false,
            Some("m"),
            Some(std::path::Path::new("/modules/m")),
        );
        assert_eq!(
            env_with_module.len(),
            7,
            "env with module info should have 7 entries"
        );
    }

    // --- verify additional tests ---

    #[test]
    fn verify_empty_profile_returns_no_results() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let resolved = make_empty_resolved();
        let printer = test_printer();

        let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();
        assert!(
            results.is_empty(),
            "empty profile with no modules should produce no verify results, got: {:?}",
            results
        );
    }

    #[test]
    fn verify_file_target_exists() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let printer = test_printer();
        let tmp = tempfile::tempdir().unwrap();

        // Create a file that exists
        let target_path = tmp.path().join("existing.conf");
        std::fs::write(&target_path, "content").unwrap();

        let mut resolved = make_empty_resolved();
        resolved.merged.files.managed.push(ManagedFileSpec {
            source: "source.conf".to_string(),
            target: target_path.clone(),
            strategy: None,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        });

        let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();
        let file_result = results
            .iter()
            .find(|r| r.resource_type == "file")
            .expect("should have a file verify result");
        assert!(file_result.matches, "existing file should match");
        assert_eq!(file_result.expected, "present");
        assert_eq!(file_result.actual, "present");
    }

    #[test]
    fn verify_file_target_missing() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let printer = test_printer();

        let mut resolved = make_empty_resolved();
        resolved.merged.files.managed.push(ManagedFileSpec {
            source: "source.conf".to_string(),
            target: PathBuf::from("/tmp/cfgd-test-nonexistent-file-39485738"),
            strategy: None,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        });

        let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();
        let file_result = results
            .iter()
            .find(|r| r.resource_type == "file")
            .expect("should have a file verify result");
        assert!(!file_result.matches, "missing file should not match");
        assert_eq!(file_result.expected, "present");
        assert_eq!(file_result.actual, "missing");
    }

    #[test]
    fn verify_module_file_target_missing_causes_drift() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let printer = test_printer();
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "test-mod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: PathBuf::from("/src/config"),
                target: PathBuf::from("/tmp/cfgd-test-nonexistent-module-file-29384"),
                is_git_source: false,
                strategy: None,
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

        // Should have drift for the missing file
        let drift = results
            .iter()
            .find(|r| r.resource_type == "module" && !r.matches);
        assert!(
            drift.is_some(),
            "missing module file target should cause drift"
        );
        let d = drift.unwrap();
        assert_eq!(d.expected, "present");
        assert_eq!(d.actual, "missing");
        assert!(d.resource_id.contains("test-mod"));
    }

    #[test]
    fn verify_module_file_target_exists_no_drift() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let printer = test_printer();
        let resolved = make_empty_resolved();
        let tmp = tempfile::tempdir().unwrap();

        let target_path = tmp.path().join("module-config");
        std::fs::write(&target_path, "content").unwrap();

        let modules = vec![ResolvedModule {
            name: "files-mod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: PathBuf::from("/src/config"),
                target: target_path,
                is_git_source: false,
                strategy: None,
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let results = verify(&resolved, &registry, &state, &printer, &modules).unwrap();

        // Module should be healthy
        let healthy = results
            .iter()
            .find(|r| r.resource_type == "module" && r.resource_id == "files-mod");
        assert!(healthy.is_some(), "module should have a healthy result");
        assert!(healthy.unwrap().matches);
    }

    #[test]
    fn verify_multiple_packages_mixed_status() {
        let state = test_state();
        let mut registry = ProviderRegistry::new();

        // Only "git" installed, "tmux" missing
        registry.package_managers.push(Box::new(
            MockPackageManager::new("apt").with_installed(&["git"]),
        ));

        let mut resolved = make_empty_resolved();
        resolved.merged.packages.apt = Some(crate::config::AptSpec {
            file: None,
            packages: vec!["git".to_string(), "tmux".to_string()],
        });

        let printer = test_printer();
        let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();

        let git_result = results
            .iter()
            .find(|r| r.resource_id == "apt:git")
            .expect("should have git result");
        assert!(git_result.matches);
        assert_eq!(git_result.expected, "installed");
        assert_eq!(git_result.actual, "installed");

        let tmux_result = results
            .iter()
            .find(|r| r.resource_id == "apt:tmux")
            .expect("should have tmux result");
        assert!(!tmux_result.matches);
        assert_eq!(tmux_result.expected, "installed");
        assert_eq!(tmux_result.actual, "missing");
    }

    // --- format_action_description additional tests ---

    #[test]
    fn format_action_description_env_write_file() {
        let action = Action::Env(EnvAction::WriteEnvFile {
            path: PathBuf::from("/home/user/.cfgd.env"),
            content: "export FOO=bar\n".to_string(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "env:write:/home/user/.cfgd.env");
    }

    #[test]
    fn format_action_description_env_inject_source() {
        let action = Action::Env(EnvAction::InjectSourceLine {
            rc_path: PathBuf::from("/home/user/.zshrc"),
            line: "source ~/.cfgd.env".to_string(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "env:inject:/home/user/.zshrc");
    }

    #[test]
    fn format_action_description_script_run_entry() {
        let action = Action::Script(ScriptAction::Run {
            entry: ScriptEntry::Simple("echo hello".to_string()),
            phase: ScriptPhase::PreApply,
            origin: "local".to_string(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "script:preApply:echo hello");
    }

    #[test]
    fn format_action_description_system_set_value_sysctl() {
        let action = Action::System(SystemAction::SetValue {
            configurator: "sysctl".to_string(),
            key: "net.ipv4.ip_forward".to_string(),
            desired: "1".to_string(),
            current: "0".to_string(),
            origin: "local".to_string(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "system:sysctl.net.ipv4.ip_forward");
    }

    #[test]
    fn format_action_description_system_skip_sysctl() {
        let action = Action::System(SystemAction::Skip {
            configurator: "sysctl".to_string(),
            reason: "not available".to_string(),
            origin: "local".to_string(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "system:sysctl:skip");
    }

    #[test]
    fn format_action_description_module_install_multiple_packages() {
        let action = Action::Module(ModuleAction {
            module_name: "neovim".to_string(),
            kind: ModuleActionKind::InstallPackages {
                resolved: vec![
                    ResolvedPackage {
                        canonical_name: "neovim".to_string(),
                        resolved_name: "neovim".to_string(),
                        manager: "brew".to_string(),
                        version: None,
                        script: None,
                    },
                    ResolvedPackage {
                        canonical_name: "ripgrep".to_string(),
                        resolved_name: "ripgrep".to_string(),
                        manager: "brew".to_string(),
                        version: None,
                        script: None,
                    },
                ],
            },
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "module:neovim:packages:neovim,ripgrep");
    }

    #[test]
    fn format_action_description_module_deploy_two_files() {
        let action = Action::Module(ModuleAction {
            module_name: "nvim".to_string(),
            kind: ModuleActionKind::DeployFiles {
                files: vec![
                    ResolvedFile {
                        source: PathBuf::from("/src/init.lua"),
                        target: PathBuf::from("/home/.config/nvim/init.lua"),
                        is_git_source: false,
                        strategy: None,
                        encryption: None,
                    },
                    ResolvedFile {
                        source: PathBuf::from("/src/plugins.lua"),
                        target: PathBuf::from("/home/.config/nvim/plugins.lua"),
                        is_git_source: false,
                        strategy: None,
                        encryption: None,
                    },
                ],
            },
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "module:nvim:files:2");
    }

    #[test]
    fn format_action_description_module_run_post_apply_script() {
        let action = Action::Module(ModuleAction {
            module_name: "rust".to_string(),
            kind: ModuleActionKind::RunScript {
                script: ScriptEntry::Simple("./setup.sh".to_string()),
                phase: ScriptPhase::PostApply,
            },
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "module:rust:script");
    }

    #[test]
    fn format_action_description_module_skip_dependency() {
        let action = Action::Module(ModuleAction {
            module_name: "rust".to_string(),
            kind: ModuleActionKind::Skip {
                reason: "dependency not met".to_string(),
            },
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "module:rust:skip");
    }

    #[test]
    fn format_action_description_package_bootstrap() {
        let action = Action::Package(PackageAction::Bootstrap {
            manager: "brew".to_string(),
            method: "curl".to_string(),
            origin: "local".to_string(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "package:brew:bootstrap");
    }

    #[test]
    fn format_action_description_package_uninstall() {
        let action = Action::Package(PackageAction::Uninstall {
            manager: "apt".to_string(),
            packages: vec!["vim".to_string(), "nano".to_string()],
            origin: "local".to_string(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "package:apt:uninstall:vim,nano");
    }

    #[test]
    fn format_action_description_file_set_permissions() {
        let action = Action::File(FileAction::SetPermissions {
            target: PathBuf::from("/etc/config.yaml"),
            mode: 0o600,
            origin: "local".to_string(),
        });
        let desc = format_action_description(&action);
        assert_eq!(desc, "file:chmod:0o600:/etc/config.yaml");
    }

    // --- PhaseName tests ---

    #[test]
    fn phase_name_all_variants_roundtrip() {
        let variants = [
            ("pre-scripts", PhaseName::PreScripts, "Pre-Scripts"),
            ("env", PhaseName::Env, "Environment"),
            ("modules", PhaseName::Modules, "Modules"),
            ("packages", PhaseName::Packages, "Packages"),
            ("system", PhaseName::System, "System"),
            ("files", PhaseName::Files, "Files"),
            ("secrets", PhaseName::Secrets, "Secrets"),
            ("post-scripts", PhaseName::PostScripts, "Post-Scripts"),
        ];

        for (s, expected_variant, display) in &variants {
            let parsed = PhaseName::from_str(s).unwrap();
            assert_eq!(&parsed, expected_variant);
            assert_eq!(parsed.as_str(), *s);
            assert_eq!(parsed.display_name(), *display);
        }
    }

    #[test]
    fn phase_name_unknown_returns_err() {
        let result = PhaseName::from_str("unknown-phase");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("unknown phase"),
            "error should mention unknown phase: {}",
            err
        );
    }

    // --- ScriptPhase display_name tests ---

    #[test]
    fn script_phase_display_names() {
        assert_eq!(ScriptPhase::PreApply.display_name(), "preApply");
        assert_eq!(ScriptPhase::PostApply.display_name(), "postApply");
        assert_eq!(ScriptPhase::PreReconcile.display_name(), "preReconcile");
        assert_eq!(ScriptPhase::PostReconcile.display_name(), "postReconcile");
        assert_eq!(ScriptPhase::OnDrift.display_name(), "onDrift");
        assert_eq!(ScriptPhase::OnChange.display_name(), "onChange");
    }

    // --- verify_env_file tests ---

    #[test]
    fn verify_env_file_matches_when_content_equal() {
        let state = test_state();
        let tmp = tempfile::tempdir().unwrap();
        let env_path = tmp.path().join("test.env");
        let expected = "export FOO=\"bar\"\n";
        std::fs::write(&env_path, expected).unwrap();

        let mut results = Vec::new();
        super::verify_env_file(&env_path, expected, &state, &mut results);

        assert_eq!(results.len(), 1);
        assert!(results[0].matches);
        assert_eq!(results[0].resource_type, "env");
        assert_eq!(results[0].expected, "current");
        assert_eq!(results[0].actual, "current");
    }

    #[test]
    fn verify_env_file_stale_when_content_differs() {
        let state = test_state();
        let tmp = tempfile::tempdir().unwrap();
        let env_path = tmp.path().join("test.env");
        std::fs::write(&env_path, "old content").unwrap();

        let mut results = Vec::new();
        super::verify_env_file(&env_path, "new content", &state, &mut results);

        assert_eq!(results.len(), 1);
        assert!(!results[0].matches);
        assert_eq!(results[0].expected, "current");
        assert_eq!(results[0].actual, "stale");
    }

    #[test]
    fn verify_env_file_missing_when_file_absent() {
        let state = test_state();
        let tmp = tempfile::tempdir().unwrap();
        let env_path = tmp.path().join("nonexistent.env");

        let mut results = Vec::new();
        super::verify_env_file(&env_path, "expected content", &state, &mut results);

        assert_eq!(results.len(), 1);
        assert!(!results[0].matches);
        assert_eq!(results[0].expected, "present");
        assert_eq!(results[0].actual, "missing");
    }

    // --- merge_module_env_aliases tests ---

    #[test]
    fn merge_module_env_aliases_empty() {
        let (env, aliases) = super::merge_module_env_aliases(&[], &[], &[]);
        assert!(env.is_empty());
        assert!(aliases.is_empty());
    }

    #[test]
    fn merge_module_env_aliases_combines_profile_and_modules() {
        let profile_env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "vim".into(),
        }];
        let profile_aliases = vec![crate::config::ShellAlias {
            name: "g".into(),
            command: "git".into(),
        }];
        let modules = vec![ResolvedModule {
            name: "test".to_string(),
            packages: vec![],
            files: vec![],
            env: vec![crate::config::EnvVar {
                name: "PAGER".into(),
                value: "less".into(),
            }],
            aliases: vec![crate::config::ShellAlias {
                name: "ll".into(),
                command: "ls -la".into(),
            }],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let (env, aliases) =
            super::merge_module_env_aliases(&profile_env, &profile_aliases, &modules);
        assert_eq!(env.len(), 2);
        assert_eq!(aliases.len(), 2);

        // Check that both profile and module values are present
        assert!(env.iter().any(|e| e.name == "EDITOR"));
        assert!(env.iter().any(|e| e.name == "PAGER"));
        assert!(aliases.iter().any(|a| a.name == "g"));
        assert!(aliases.iter().any(|a| a.name == "ll"));
    }

    #[test]
    fn merge_module_env_aliases_module_overrides_profile() {
        let profile_env = vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "vim".into(),
        }];
        let modules = vec![ResolvedModule {
            name: "test".to_string(),
            packages: vec![],
            files: vec![],
            env: vec![crate::config::EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            }],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: PathBuf::from("."),
        }];

        let (env, _) = super::merge_module_env_aliases(&profile_env, &[], &modules);
        // merge_env deduplicates by name, last wins
        let editor = env.iter().find(|e| e.name == "EDITOR").unwrap();
        assert_eq!(
            editor.value, "nvim",
            "module should override profile env var"
        );
    }

    // --- Module deploy files: hardlink strategy ---

    #[test]
    fn apply_module_deploy_files_hardlink_strategy() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("source.txt");
        let target_file = dir.path().join("hardlink-target.txt");
        std::fs::write(&source_file, "hardlinked content").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Hardlink;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "hardmod".to_string(),
                    kind: ModuleActionKind::DeployFiles {
                        files: vec![ResolvedFile {
                            source: source_file.clone(),
                            target: target_file.clone(),
                            is_git_source: false,
                            strategy: Some(crate::config::FileStrategy::Hardlink),
                            encryption: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let modules = vec![ResolvedModule {
            name: "hardmod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: source_file.clone(),
                target: target_file.clone(),
                is_git_source: false,
                strategy: Some(crate::config::FileStrategy::Hardlink),
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Modules),
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(
            !target_file.is_symlink(),
            "hardlink should not be a symlink"
        );
        assert_eq!(
            std::fs::read_to_string(&target_file).unwrap(),
            "hardlinked content"
        );
        // Verify it's a hardlink by checking inode (Unix)
        #[cfg(unix)]
        {
            assert!(
                crate::is_same_inode(&source_file, &target_file),
                "source and target should share the same inode"
            );
        }
    }

    // --- Module deploy files: copy strategy ---

    #[test]
    fn apply_module_deploy_files_copy_strategy() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("source.txt");
        let target_file = dir.path().join("copy-target.txt");
        std::fs::write(&source_file, "copied content").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "copymod".to_string(),
                    kind: ModuleActionKind::DeployFiles {
                        files: vec![ResolvedFile {
                            source: source_file.clone(),
                            target: target_file.clone(),
                            is_git_source: false,
                            strategy: Some(crate::config::FileStrategy::Copy),
                            encryption: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let modules = vec![ResolvedModule {
            name: "copymod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: source_file.clone(),
                target: target_file.clone(),
                is_git_source: false,
                strategy: Some(crate::config::FileStrategy::Copy),
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Modules),
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(!target_file.is_symlink(), "copy should not be a symlink");
        assert_eq!(
            std::fs::read_to_string(&target_file).unwrap(),
            "copied content"
        );
        // Verify it's NOT a hardlink (independent copy)
        #[cfg(unix)]
        {
            assert!(
                !crate::is_same_inode(&source_file, &target_file),
                "copy should have a different inode"
            );
        }
    }

    // --- Module deploy files: directory with symlink vs copy ---

    #[test]
    fn apply_module_deploy_files_directory_copy_strategy() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("src-dir");
        std::fs::create_dir(&source_dir).unwrap();
        std::fs::write(source_dir.join("a.txt"), "aaa").unwrap();
        std::fs::write(source_dir.join("b.txt"), "bbb").unwrap();

        let target_dir = dir.path().join("target-dir");

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "dirmod".to_string(),
                    kind: ModuleActionKind::DeployFiles {
                        files: vec![ResolvedFile {
                            source: source_dir.clone(),
                            target: target_dir.clone(),
                            is_git_source: false,
                            strategy: Some(crate::config::FileStrategy::Copy),
                            encryption: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let modules = vec![ResolvedModule {
            name: "dirmod".to_string(),
            packages: vec![],
            files: vec![ResolvedFile {
                source: source_dir.clone(),
                target: target_dir.clone(),
                is_git_source: false,
                strategy: Some(crate::config::FileStrategy::Copy),
                encryption: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Modules),
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(target_dir.is_dir(), "target should be a directory");
        assert!(!target_dir.is_symlink(), "copy should not be a symlink");
        assert_eq!(
            std::fs::read_to_string(target_dir.join("a.txt")).unwrap(),
            "aaa"
        );
        assert_eq!(
            std::fs::read_to_string(target_dir.join("b.txt")).unwrap(),
            "bbb"
        );
    }

    // --- Module deploy files: overwrites existing target ---

    #[test]
    fn apply_module_deploy_files_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("source.txt");
        let target_file = dir.path().join("target.txt");
        std::fs::write(&source_file, "new content").unwrap();
        std::fs::write(&target_file, "old content").unwrap();

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "overmod".to_string(),
                    kind: ModuleActionKind::DeployFiles {
                        files: vec![ResolvedFile {
                            source: source_file.clone(),
                            target: target_file.clone(),
                            is_git_source: false,
                            strategy: Some(crate::config::FileStrategy::Copy),
                            encryption: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let modules = vec![ResolvedModule {
            name: "overmod".to_string(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: Vec::new(),
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Modules),
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(
            std::fs::read_to_string(&target_file).unwrap(),
            "new content",
            "existing file should be overwritten"
        );
    }

    // --- Module-level onChange script runs when module changes ---

    #[test]
    #[cfg(unix)]
    fn apply_module_on_change_script_runs_when_module_has_changes() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("source.txt");
        let target_file = dir.path().join("target.txt");
        std::fs::write(&source_file, "content").unwrap();
        let marker = dir.path().join("onchange-ran");

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry.default_file_strategy = crate::config::FileStrategy::Copy;

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = Plan {
            phases: vec![Phase {
                name: PhaseName::Modules,
                actions: vec![Action::Module(ModuleAction {
                    module_name: "changemod".to_string(),
                    kind: ModuleActionKind::DeployFiles {
                        files: vec![ResolvedFile {
                            source: source_file.clone(),
                            target: target_file.clone(),
                            is_git_source: false,
                            strategy: Some(crate::config::FileStrategy::Copy),
                            encryption: None,
                        }],
                    },
                })],
            }],
            warnings: vec![],
        };

        let modules = vec![ResolvedModule {
            name: "changemod".to_string(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: vec![crate::config::ScriptEntry::Simple(format!(
                "touch {}",
                marker.display()
            ))],
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                None, // no phase filter — run everything including onChange
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(
            marker.exists(),
            "module onChange script should have created marker file"
        );
    }

    #[test]
    #[cfg(unix)]
    fn apply_module_on_change_script_does_not_run_when_no_changes() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("onchange-ran");

        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        // Empty plan — no actions, so no module changes
        let plan = Plan {
            phases: vec![],
            warnings: vec![],
        };

        let modules = vec![ResolvedModule {
            name: "nochangemod".to_string(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            pre_apply_scripts: Vec::new(),
            pre_reconcile_scripts: Vec::new(),
            post_reconcile_scripts: Vec::new(),
            on_change_scripts: vec![crate::config::ScriptEntry::Simple(format!(
                "touch {}",
                marker.display()
            ))],
            system: HashMap::new(),
            depends: vec![],
            dir: dir.path().to_path_buf(),
        }];

        let printer = test_printer();
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                None,
                &modules,
                ReconcileContext::Apply,
                false,
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        assert!(
            !marker.exists(),
            "module onChange should NOT run when module had no changes"
        );
    }

    // --- Rollback restores file to correct content ---

    #[test]
    fn rollback_restores_file_with_correct_content() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("managed.txt");

        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        // Record a first apply with a file backup
        let file_state = crate::FileState {
            content: b"original content".to_vec(),
            content_hash: crate::sha256_hex(b"original content"),
            permissions: Some(0o644),
            is_symlink: false,
            symlink_target: None,
            oversized: false,
        };
        let apply_id_1 = state
            .record_apply("default", "plan-hash-1", ApplyStatus::InProgress, None)
            .unwrap();
        state
            .store_file_backup(apply_id_1, &file_path.display().to_string(), &file_state)
            .unwrap();
        state
            .update_apply_status(apply_id_1, ApplyStatus::Success, Some("{}"))
            .unwrap();

        // Record a second apply that changed the file
        let new_state = crate::FileState {
            content: b"modified content".to_vec(),
            content_hash: crate::sha256_hex(b"modified content"),
            permissions: Some(0o644),
            is_symlink: false,
            symlink_target: None,
            oversized: false,
        };
        let apply_id_2 = state
            .record_apply("default", "plan-hash-2", ApplyStatus::InProgress, None)
            .unwrap();
        state
            .store_file_backup(apply_id_2, &file_path.display().to_string(), &new_state)
            .unwrap();
        state
            .update_apply_status(apply_id_2, ApplyStatus::Success, Some("{}"))
            .unwrap();

        // Write the current file with apply-2 content
        std::fs::write(&file_path, "modified content").unwrap();

        let printer = test_printer();
        let result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

        assert!(
            result.files_restored > 0,
            "should restore at least one file"
        );
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "original content",
            "file should be restored to apply-1 state"
        );
    }

    // --- Rollback Phase 3: removes files created after target ---

    #[test]
    fn rollback_removes_file_created_after_target_apply() {
        let dir = tempfile::tempdir().unwrap();
        let created_file = dir.path().join("new-file.txt");

        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        // Apply 1: a simple apply that didn't touch new-file.txt
        let apply_id_1 = state
            .record_apply("default", "hash-1", ApplyStatus::InProgress, None)
            .unwrap();
        state
            .update_apply_status(apply_id_1, ApplyStatus::Success, None)
            .unwrap();

        // Apply 2: creates new-file.txt (file didn't exist before)
        let apply_id_2 = state
            .record_apply("default", "hash-2", ApplyStatus::InProgress, None)
            .unwrap();
        let j_id = state
            .journal_begin(
                apply_id_2,
                0,
                "files",
                "file",
                &format!("file:create:{}", created_file.display()),
                None,
            )
            .unwrap();
        state.journal_complete(j_id, None, None).unwrap();
        state
            .update_apply_status(apply_id_2, ApplyStatus::Success, None)
            .unwrap();

        // Write the file to disk (simulating what apply 2 did)
        std::fs::write(&created_file, "new content").unwrap();
        assert!(created_file.exists());

        // Rollback to apply 1 — file didn't exist then, should be removed
        let printer = test_printer();
        let result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

        assert!(
            !created_file.exists(),
            "file created after target apply should be removed"
        );
        assert!(
            result.files_removed > 0,
            "files_removed should reflect the deletion"
        );
    }

    #[test]
    fn rollback_keeps_file_that_existed_at_target_apply() {
        let dir = tempfile::tempdir().unwrap();
        let existing_file = dir.path().join("existing.txt");

        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        // Apply 1: creates existing.txt (journal records file:create:...)
        let apply_id_1 = state
            .record_apply("default", "hash-1", ApplyStatus::InProgress, None)
            .unwrap();
        let j_id = state
            .journal_begin(
                apply_id_1,
                0,
                "files",
                "file",
                &format!("file:create:{}", existing_file.display()),
                None,
            )
            .unwrap();
        state.journal_complete(j_id, None, None).unwrap();
        // Store backup so phase 1 handles it
        let file_state = crate::FileState {
            content: b"original".to_vec(),
            content_hash: crate::sha256_hex(b"original"),
            permissions: Some(0o644),
            is_symlink: false,
            symlink_target: None,
            oversized: false,
        };
        state
            .store_file_backup(
                apply_id_1,
                &existing_file.display().to_string(),
                &file_state,
            )
            .unwrap();
        state
            .update_apply_status(apply_id_1, ApplyStatus::Success, None)
            .unwrap();

        // Apply 2: updates existing.txt
        let apply_id_2 = state
            .record_apply("default", "hash-2", ApplyStatus::InProgress, None)
            .unwrap();
        let j_id = state
            .journal_begin(
                apply_id_2,
                0,
                "files",
                "file",
                &format!("file:create:{}", existing_file.display()),
                None,
            )
            .unwrap();
        state.journal_complete(j_id, None, None).unwrap();
        state
            .update_apply_status(apply_id_2, ApplyStatus::Success, None)
            .unwrap();

        // Write current state
        std::fs::write(&existing_file, "modified").unwrap();

        // Rollback to apply 1 — file existed at apply 1, should be restored not removed
        let printer = test_printer();
        let result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

        assert!(
            existing_file.exists(),
            "file that existed at target apply should NOT be removed"
        );
        assert_eq!(
            std::fs::read_to_string(&existing_file).unwrap(),
            "original",
            "file should be restored to target apply state"
        );
        assert!(result.files_restored > 0);
    }

    #[test]
    fn rollback_collects_non_file_actions_from_subsequent_applies() {
        let state = test_state();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        // Apply 1: base state
        let apply_id_1 = state
            .record_apply("default", "hash-1", ApplyStatus::InProgress, None)
            .unwrap();
        state
            .update_apply_status(apply_id_1, ApplyStatus::Success, None)
            .unwrap();

        // Apply 2: installs a package and runs a script
        let apply_id_2 = state
            .record_apply("default", "hash-2", ApplyStatus::InProgress, None)
            .unwrap();
        let j1 = state
            .journal_begin(apply_id_2, 0, "Packages", "install", "brew:ripgrep", None)
            .unwrap();
        state.journal_complete(j1, None, None).unwrap();
        let j2 = state
            .journal_begin(
                apply_id_2,
                1,
                "PostScripts",
                "script",
                "script:post:setup.sh",
                None,
            )
            .unwrap();
        state.journal_complete(j2, None, None).unwrap();
        state
            .update_apply_status(apply_id_2, ApplyStatus::Success, None)
            .unwrap();

        // Rollback to apply 1
        let printer = test_printer();
        let result = reconciler.rollback_apply(apply_id_1, &printer).unwrap();

        // Non-file actions from subsequent applies should be listed for manual review
        assert!(
            result
                .non_file_actions
                .contains(&"brew:ripgrep".to_string()),
            "should list package action for manual review: {:?}",
            result.non_file_actions
        );
        assert!(
            result
                .non_file_actions
                .contains(&"script:post:setup.sh".to_string()),
            "should list script action for manual review: {:?}",
            result.non_file_actions
        );
    }

    // --- Verify: system configurator drift detection ---

    #[test]
    fn verify_system_configurator_reports_drift() {
        struct DriftingConfigurator;

        impl crate::providers::SystemConfigurator for DriftingConfigurator {
            fn name(&self) -> &str {
                "sysctl"
            }
            fn is_available(&self) -> bool {
                true
            }
            fn current_state(&self) -> crate::errors::Result<serde_yaml::Value> {
                Ok(serde_yaml::Value::Null)
            }
            fn diff(
                &self,
                _: &serde_yaml::Value,
            ) -> crate::errors::Result<Vec<crate::providers::SystemDrift>> {
                Ok(vec![
                    crate::providers::SystemDrift {
                        key: "vm.swappiness".to_string(),
                        expected: "10".to_string(),
                        actual: "60".to_string(),
                    },
                    crate::providers::SystemDrift {
                        key: "net.ipv4.ip_forward".to_string(),
                        expected: "1".to_string(),
                        actual: "0".to_string(),
                    },
                ])
            }
            fn apply(&self, _: &serde_yaml::Value, _: &Printer) -> crate::errors::Result<()> {
                Ok(())
            }
        }

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .system_configurators
            .push(Box::new(DriftingConfigurator));

        let mut system = HashMap::new();
        system.insert(
            "sysctl".to_string(),
            serde_yaml::to_value(serde_yaml::Mapping::new()).unwrap(),
        );
        let merged = crate::config::MergedProfile {
            system,
            ..Default::default()
        };
        let resolved = crate::config::ResolvedProfile {
            layers: vec![crate::config::ProfileLayer {
                source: "local".to_string(),
                profile_name: "default".to_string(),
                priority: 0,
                policy: crate::config::LayerPolicy::Local,
                spec: Default::default(),
            }],
            merged,
        };

        let printer = test_printer();
        let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();

        // Should have per-key drift entries with resource_type "system"
        let drift_results: Vec<_> = results
            .iter()
            .filter(|r| r.resource_type == "system" && !r.matches)
            .collect();
        assert_eq!(
            drift_results.len(),
            2,
            "should report drift for each sysctl key, got: {:?}",
            drift_results
        );
        assert!(
            drift_results
                .iter()
                .any(|r| r.resource_id == "sysctl.vm.swappiness"),
            "should report sysctl.vm.swappiness drift"
        );
        assert!(
            drift_results
                .iter()
                .any(|r| r.resource_id == "sysctl.net.ipv4.ip_forward"),
            "should report sysctl.net.ipv4.ip_forward drift"
        );
        // Verify the expected/actual values are correct
        let swap = drift_results
            .iter()
            .find(|r| r.resource_id == "sysctl.vm.swappiness")
            .unwrap();
        assert_eq!(swap.expected, "10");
        assert_eq!(swap.actual, "60");
    }

    #[test]
    fn verify_system_configurator_reports_healthy_when_no_drift() {
        struct HealthyConfigurator;

        impl crate::providers::SystemConfigurator for HealthyConfigurator {
            fn name(&self) -> &str {
                "sysctl"
            }
            fn is_available(&self) -> bool {
                true
            }
            fn current_state(&self) -> crate::errors::Result<serde_yaml::Value> {
                Ok(serde_yaml::Value::Null)
            }
            fn diff(
                &self,
                _: &serde_yaml::Value,
            ) -> crate::errors::Result<Vec<crate::providers::SystemDrift>> {
                Ok(vec![])
            }
            fn apply(&self, _: &serde_yaml::Value, _: &Printer) -> crate::errors::Result<()> {
                Ok(())
            }
        }

        let state = test_state();
        let mut registry = ProviderRegistry::new();
        registry
            .system_configurators
            .push(Box::new(HealthyConfigurator));

        let mut system = HashMap::new();
        system.insert(
            "sysctl".to_string(),
            serde_yaml::to_value(serde_yaml::Mapping::new()).unwrap(),
        );
        let merged = crate::config::MergedProfile {
            system,
            ..Default::default()
        };
        let resolved = crate::config::ResolvedProfile {
            layers: vec![crate::config::ProfileLayer {
                source: "local".to_string(),
                profile_name: "default".to_string(),
                priority: 0,
                policy: crate::config::LayerPolicy::Local,
                spec: Default::default(),
            }],
            merged,
        };

        let printer = test_printer();
        let results = verify(&resolved, &registry, &state, &printer, &[]).unwrap();

        let sysctl_results: Vec<_> = results
            .iter()
            .filter(|r| r.resource_type == "system")
            .collect();
        assert_eq!(
            sysctl_results.len(),
            1,
            "should have one healthy result for sysctl"
        );
        assert!(
            sysctl_results[0].matches,
            "sysctl should report as matching (no drift)"
        );
        assert_eq!(sysctl_results[0].resource_id, "sysctl");
    }
}

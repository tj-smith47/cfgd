use std::collections::HashMap;
use std::path::PathBuf;

use secrecy::ExposeSecret;

use crate::config::{MergedProfile, ResolvedProfile, ScriptEntry, ScriptSpec};
use crate::errors::{ConfigError, Result};
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::providers::{FileAction, PackageAction, ProviderRegistry, SecretAction};
use crate::state::{ApplyStatus, StateStore};

mod env_files;
mod file_action;
mod format;
mod restore;
mod scripts;
mod types;
mod verify;

#[cfg(test)]
mod tests;

pub use format::{format_action_description, format_plan_items};
pub use types::{
    Action, ActionResult, ApplyResult, EnvAction, ModuleAction, ModuleActionKind, Phase, PhaseName,
    Plan, ReconcileContext, RollbackResult, ScriptAction, ScriptPhase, SystemAction,
};
pub use verify::{VerifyResult, verify};

pub(crate) use scripts::{build_script_env, execute_script};

// Glob-bring-in for the Reconciler<'a> impl below and for the externalized
// `tests` child module (which references helpers via `super::*`). Plain `use`
// keeps items at module-private scope while still making them reachable
// through the parent's namespace from child modules.
use env_files::*;
use file_action::*;
use format::*;
use restore::*;
use scripts::*;
use verify::*;

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
                    build_script_env(config_dir, profile_name, context, phase, None, None);

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

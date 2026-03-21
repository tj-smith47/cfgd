use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;

use serde::Serialize;

use crate::config::{MergedProfile, ResolvedProfile, ScriptSpec};
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::providers::{FileAction, PackageAction, ProviderRegistry, SecretAction};
use crate::state::{ApplyStatus, StateStore};

/// Ordered reconciliation phases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PhaseName {
    Modules,
    System,
    Packages,
    Files,
    Env,
    Secrets,
    Scripts,
}

impl PhaseName {
    pub fn as_str(&self) -> &str {
        match self {
            PhaseName::Modules => "modules",
            PhaseName::System => "system",
            PhaseName::Packages => "packages",
            PhaseName::Files => "files",
            PhaseName::Env => "env",
            PhaseName::Secrets => "secrets",
            PhaseName::Scripts => "scripts",
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            PhaseName::Modules => "Modules",
            PhaseName::System => "System",
            PhaseName::Packages => "Packages",
            PhaseName::Files => "Files",
            PhaseName::Env => "Environment",
            PhaseName::Secrets => "Secrets",
            PhaseName::Scripts => "Scripts",
        }
    }
}

impl FromStr for PhaseName {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "modules" => Ok(PhaseName::Modules),
            "system" => Ok(PhaseName::System),
            "packages" => Ok(PhaseName::Packages),
            "files" => Ok(PhaseName::Files),
            "env" => Ok(PhaseName::Env),
            "secrets" => Ok(PhaseName::Secrets),
            "scripts" => Ok(PhaseName::Scripts),
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
    RunScript { script: String },
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
        path: PathBuf,
        phase: ScriptPhase,
        origin: String,
    },
}

/// When a script runs relative to reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ScriptPhase {
    PreReconcile,
    PostReconcile,
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
    ) -> Result<Plan> {
        // Conflict detection: check for multiple sources targeting the same path
        Self::detect_file_conflicts(&file_actions, &module_actions)?;

        let mut phases = Vec::new();

        // Phase 0: Env — write ~/.cfgd.env and inject shell rc source line.
        // Runs first so that env vars (including PATH for bootstrapped managers)
        // are available to all subsequent phases, especially post-apply scripts.
        let (env_actions, warnings) = Self::plan_env(
            &resolved.merged.env,
            &resolved.merged.aliases,
            &module_actions,
        );
        phases.push(Phase {
            name: PhaseName::Env,
            actions: env_actions,
        });

        // Phase 1: Modules — module packages, files, and post-apply scripts.
        // Packages are grouped with system/native managers first, then
        // bootstrappable managers, so build deps are installed before
        // packages that need them.
        let module_phase_actions = self.plan_modules(&module_actions);
        phases.push(Phase {
            name: PhaseName::Modules,
            actions: module_phase_actions,
        });

        // Phase 2: Packages — profile-level packages, installed after modules
        // so module deps are available.
        let package_actions = pkg_actions.into_iter().map(Action::Package).collect();
        phases.push(Phase {
            name: PhaseName::Packages,
            actions: package_actions,
        });

        // Phase 3: System — runs after packages so required binaries exist
        let system_actions = self.plan_system(&resolved.merged)?;
        phases.push(Phase {
            name: PhaseName::System,
            actions: system_actions,
        });

        // Phase 4: Files
        let fa = file_actions.into_iter().map(Action::File).collect();
        phases.push(Phase {
            name: PhaseName::Files,
            actions: fa,
        });

        // Phase 5: Secrets
        let secret_actions = self.plan_secrets(&resolved.merged);
        phases.push(Phase {
            name: PhaseName::Secrets,
            actions: secret_actions,
        });

        // Phase 6: Scripts
        let script_actions = self.plan_scripts(&resolved.merged.scripts);
        phases.push(Phase {
            name: PhaseName::Scripts,
            actions: script_actions,
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

    fn plan_system(&self, profile: &MergedProfile) -> Result<Vec<Action>> {
        let mut actions = Vec::new();

        for configurator in self.registry.available_system_configurators() {
            if let Some(desired) = profile.system.get(configurator.name()) {
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
        for key in profile.system.keys() {
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
    ) -> (Vec<Action>, Vec<String>) {
        let (merged, merged_aliases) =
            merge_module_env_aliases(profile_env, profile_aliases, modules);

        if merged.is_empty() && merged_aliases.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let mut actions = Vec::new();

        // Always plan the env file write — apply phase skips if content matches
        let env_path = crate::expand_tilde(std::path::Path::new("~/.cfgd.env"));
        let content = generate_env_file_content(&merged, &merged_aliases);
        actions.push(Action::Env(EnvAction::WriteEnvFile {
            path: env_path.clone(),
            content,
        }));

        // Always plan shell rc injection — apply phase skips if already present
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let rc_path = if shell.contains("zsh") {
            crate::expand_tilde(std::path::Path::new("~/.zshrc"))
        } else {
            crate::expand_tilde(std::path::Path::new("~/.bashrc"))
        };
        actions.push(Action::Env(EnvAction::InjectSourceLine {
            rc_path: rc_path.clone(),
            line: "[ -f ~/.cfgd.env ] && source ~/.cfgd.env".to_string(),
        }));

        // Fish shell: generate separate file if fish config dir exists
        let fish_conf_d = crate::expand_tilde(std::path::Path::new("~/.config/fish/conf.d"));
        if fish_conf_d.exists() {
            let fish_path = fish_conf_d.join("cfgd-env.fish");
            let fish_content = generate_fish_env_content(&merged, &merged_aliases);
            let existing_fish = std::fs::read_to_string(&fish_path).unwrap_or_default();
            if existing_fish != fish_content {
                actions.push(Action::Env(EnvAction::WriteEnvFile {
                    path: fish_path,
                    content: fish_content,
                }));
            }
        }

        // Check for conflicts with existing definitions in the shell rc file
        let warnings = detect_rc_env_conflicts(&rc_path, &merged, &merged_aliases);

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
                    actions.push(Action::Secret(SecretAction::Resolve {
                        provider: provider_name.to_string(),
                        reference: reference.to_string(),
                        target: secret.target.clone(),
                        origin: "local".to_string(),
                    }));
                } else {
                    actions.push(Action::Secret(SecretAction::Skip {
                        source: secret.source.clone(),
                        reason: format!("provider '{}' not available", provider_name),
                        origin: "local".to_string(),
                    }));
                }
            } else if has_backend {
                // SOPS/age encrypted file
                let backend_name = secret
                    .backend
                    .as_deref()
                    .or_else(|| self.registry.secret_backend.as_ref().map(|b| b.name()))
                    .unwrap_or("sops")
                    .to_string();

                actions.push(Action::Secret(SecretAction::Decrypt {
                    source: PathBuf::from(&secret.source),
                    target: secret.target.clone(),
                    backend: backend_name,
                    origin: "local".to_string(),
                }));
            } else {
                actions.push(Action::Secret(SecretAction::Skip {
                    source: secret.source.clone(),
                    reason: "no secret backend available".to_string(),
                    origin: "local".to_string(),
                }));
            }
        }

        actions
    }

    fn plan_scripts(&self, scripts: &ScriptSpec) -> Vec<Action> {
        let mut actions = Vec::new();

        for path in &scripts.pre_reconcile {
            actions.push(Action::Script(ScriptAction::Run {
                path: path.clone(),
                phase: ScriptPhase::PreReconcile,
                origin: "local".to_string(),
            }));
        }

        for path in &scripts.post_reconcile {
            actions.push(Action::Script(ScriptAction::Run {
                path: path.clone(),
                phase: ScriptPhase::PostReconcile,
                origin: "local".to_string(),
            }));
        }

        actions
    }

    fn plan_modules(&self, modules: &[ResolvedModule]) -> Vec<Action> {
        let mut actions = Vec::new();

        for module in modules {
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

            // Files
            if !module.files.is_empty() {
                actions.push(Action::Module(ModuleAction {
                    module_name: module.name.clone(),
                    kind: ModuleActionKind::DeployFiles {
                        files: module.files.clone(),
                    },
                }));
            }

            // Post-apply scripts
            for script in &module.post_apply_scripts {
                actions.push(Action::Module(ModuleAction {
                    module_name: module.name.clone(),
                    kind: ModuleActionKind::RunScript {
                        script: script.clone(),
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
    pub fn apply(
        &self,
        plan: &Plan,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        phase_filter: Option<&PhaseName>,
        module_actions: &[ResolvedModule],
    ) -> Result<ApplyResult> {
        // Record apply up front as "in-progress" so the journal can reference it
        let plan_hash = crate::state::plan_hash(&plan.to_hash_string());
        let profile_name = resolved
            .layers
            .last()
            .map(|l| l.profile_name.as_str())
            .unwrap_or("unknown");
        let apply_id = self.state.record_apply(
            profile_name,
            &plan_hash,
            ApplyStatus::Success, // will be updated at the end
            None,
        )?;

        let mut results = Vec::new();
        let mut action_index: usize = 0;

        for phase in &plan.phases {
            if let Some(filter) = phase_filter
                && &phase.name != filter
            {
                continue;
            }

            if phase.actions.is_empty() {
                continue;
            }

            printer.subheader(&format!("Phase: {}", phase.name.display_name()));

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

                let result = self.apply_action(action, resolved, config_dir, printer, apply_id);

                let (desc, success, error) = match result {
                    Ok((desc, script_output)) => {
                        if let Some(jid) = journal_id {
                            let _ =
                                self.state
                                    .journal_complete(jid, None, script_output.as_deref());
                        }
                        (desc, true, None)
                    }
                    Err(e) => {
                        let desc = format_action_description(action);
                        printer.error(&format!(
                            "[{}/{}] Failed: {} — {}",
                            action_idx + 1,
                            total,
                            desc,
                            e
                        ));
                        if let Some(jid) = journal_id {
                            let _ = self.state.journal_fail(jid, &e.to_string());
                        }
                        (desc, false, Some(e.to_string()))
                    }
                };

                results.push(ActionResult {
                    phase: phase.name.as_str().to_string(),
                    description: desc,
                    success,
                    error,
                });
                action_index += 1;
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
        let entries = self.state.journal_completed_actions(apply_id)?;
        let mut files_restored = 0usize;
        let mut files_removed = 0usize;
        let mut non_file_actions = Vec::new();

        // Process in reverse order
        for entry in entries.iter().rev() {
            let is_file = entry.phase == "files"
                || entry.action_type == "file"
                || entry.resource_id.starts_with("file:");

            if !is_file {
                non_file_actions.push(entry.resource_id.clone());
                continue;
            }

            // Try to get backup
            let backup = self.state.get_file_backup(apply_id, &entry.resource_id)?;
            // Strip "file:create:" or "file:update:" prefix to get the actual path
            let actual_path = entry
                .resource_id
                .strip_prefix("file:create:")
                .or_else(|| entry.resource_id.strip_prefix("file:update:"))
                .or_else(|| entry.resource_id.strip_prefix("file:delete:"))
                .unwrap_or(&entry.resource_id);
            let target = std::path::Path::new(actual_path);

            if let Some(ref bk) = backup {
                if bk.was_symlink {
                    // Restore symlink
                    if let Some(ref link_target) = bk.symlink_target {
                        let _ = std::fs::remove_file(target);
                        if let Err(e) = std::os::unix::fs::symlink(link_target, target) {
                            printer.warning(&format!(
                                "rollback: failed to restore symlink {}: {}",
                                target.display(),
                                e
                            ));
                        } else {
                            files_restored += 1;
                        }
                    }
                } else if !bk.oversized && !bk.content.is_empty() {
                    // Restore file content
                    if let Err(e) = crate::atomic_write(target, &bk.content) {
                        printer.warning(&format!(
                            "rollback: failed to restore {}: {}",
                            target.display(),
                            e
                        ));
                    } else {
                        files_restored += 1;
                    }
                }
            } else {
                // No backup means file was newly created — remove it
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

    fn apply_action(
        &self,
        action: &Action,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        apply_id: i64,
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
                .apply_secret_action(secret, config_dir, printer)
                .map(|d| (d, None)),
            Action::Script(script) => self.apply_script_action(script, config_dir, printer),
            Action::Module(module) => self
                .apply_module_action(module, config_dir, printer, apply_id)
                .map(|d| (d, None)),
            Action::Env(env) => Self::apply_env_action(env, printer).map(|d| (d, None)),
        }
    }

    fn apply_env_action(action: &EnvAction, printer: &Printer) -> Result<String> {
        match action {
            EnvAction::WriteEnvFile { path, content } => {
                let existing = std::fs::read_to_string(path).unwrap_or_default();
                if existing == *content {
                    return Ok(format!("env:write:{}:skipped", path.display()));
                }
                crate::atomic_write_str(path, content)?;
                printer.success(&format!("Wrote {}", path.display()));
                Ok(format!("env:write:{}", path.display()))
            }
            EnvAction::InjectSourceLine { rc_path, line } => {
                let existing = std::fs::read_to_string(rc_path).unwrap_or_default();
                if existing.contains("cfgd.env") {
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

    fn apply_secret_action(
        &self,
        action: &SecretAction,
        config_dir: &std::path::Path,
        printer: &Printer,
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

                let source_path = if source.is_absolute() {
                    source.clone()
                } else {
                    config_dir.join(source)
                };

                let decrypted = backend.decrypt_file(&source_path)?;

                let target_path = expand_tilde(target);
                crate::atomic_write(&target_path, decrypted.as_bytes())?;

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
                crate::atomic_write(&target_path, value.as_bytes())?;

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
            SecretAction::Skip { source, reason, .. } => {
                printer.warning(&format!("secret {}: {}", source, reason));
                Ok(format!("secret:skip:{}", source))
            }
        }
    }

    fn apply_script_action(
        &self,
        action: &ScriptAction,
        config_dir: &std::path::Path,
        printer: &Printer,
    ) -> Result<(String, Option<String>)> {
        match action {
            ScriptAction::Run { path, phase, .. } => {
                let script_path = if path.is_absolute() {
                    path.clone()
                } else {
                    config_dir.join(path)
                };

                let phase_name = match phase {
                    ScriptPhase::PreReconcile => "preReconcile",
                    ScriptPhase::PostReconcile => "postReconcile",
                };

                let label = format!("Running {} script: {}", phase_name, path.display());

                let result = printer
                    .run_with_output(
                        std::process::Command::new("sh")
                            .arg("-c")
                            .arg(script_path.display().to_string())
                            .current_dir(config_dir),
                        &label,
                    )
                    .map_err(crate::errors::CfgdError::Io)?;

                if !result.status.success() {
                    return Err(crate::errors::CfgdError::Config(
                        crate::errors::ConfigError::Invalid {
                            message: format!(
                                "script {} failed (exit {})",
                                path.display(),
                                result.status.code().unwrap_or(-1)
                            ),
                        },
                    ));
                }

                let captured = combine_script_output(&result.stdout, &result.stderr);
                Ok((
                    format!("script:{}:{}", phase_name, path.display()),
                    captured,
                ))
            }
        }
    }

    fn apply_module_action(
        &self,
        action: &ModuleAction,
        config_dir: &std::path::Path,
        printer: &Printer,
        apply_id: i64,
    ) -> Result<String> {
        match &action.kind {
            ModuleActionKind::InstallPackages { resolved } => {
                // Packages in each InstallPackages action are already grouped by
                // manager in plan_modules(), so just collect names and install.
                let pkg_names: Vec<String> =
                    resolved.iter().map(|p| p.resolved_name.clone()).collect();

                if let Some(first) = resolved.first() {
                    if first.manager == "script" {
                        // Script-based install: run each package's script
                        for pkg in resolved {
                            if let Some(ref script_content) = pkg.script {
                                let label = format!(
                                    "Module {}: running install script for {}",
                                    action.module_name, pkg.canonical_name
                                );
                                let result = printer
                                    .run_with_output(
                                        std::process::Command::new("sh")
                                            .arg("-c")
                                            .arg(script_content)
                                            .current_dir(config_dir),
                                        &label,
                                    )
                                    .map_err(crate::errors::CfgdError::Io)?;

                                if !result.status.success() {
                                    return Err(crate::errors::CfgdError::Config(
                                        crate::errors::ConfigError::Invalid {
                                            message: format!(
                                                "module {} install script for '{}' failed (exit {})",
                                                action.module_name,
                                                pkg.canonical_name,
                                                result.status.code().unwrap_or(-1)
                                            ),
                                        },
                                    ));
                                }
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
                                    let existing =
                                        std::fs::read_to_string(&env_path).unwrap_or_default();
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
                                std::os::unix::fs::symlink(&file.source, &target)?;
                            }
                            _ => {
                                crate::copy_dir_recursive(&file.source, &target)?;
                            }
                        }
                    } else if file.source.exists() {
                        match strategy {
                            crate::config::FileStrategy::Symlink => {
                                std::os::unix::fs::symlink(&file.source, &target)?;
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
                        let bytes = std::fs::read(&target).unwrap_or_default();
                        crate::sha256_hex(&bytes)
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
            ModuleActionKind::RunScript { script, .. } => {
                let label = format!("Module {}: running post-apply script", action.module_name);

                let result = printer
                    .run_with_output(
                        std::process::Command::new("sh")
                            .arg("-c")
                            .arg(script)
                            .current_dir(config_dir),
                        &label,
                    )
                    .map_err(crate::errors::CfgdError::Io)?;

                if !result.status.success() {
                    return Err(crate::errors::CfgdError::Config(
                        crate::errors::ConfigError::Invalid {
                            message: format!(
                                "module {} post-apply script failed (exit {})",
                                action.module_name,
                                result.status.code().unwrap_or(-1)
                            ),
                        },
                    ));
                }

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
fn verify_env(
    profile_env: &[crate::config::EnvVar],
    profile_aliases: &[crate::config::ShellAlias],
    modules: &[ResolvedModule],
    state: &StateStore,
    results: &mut Vec<VerifyResult>,
) {
    let (merged, merged_aliases) =
        merge_module_env_aliases(profile_env, profile_aliases, modules);

    if merged.is_empty() && merged_aliases.is_empty() {
        return;
    }

    let expected_content = generate_env_file_content(&merged, &merged_aliases);
    let env_path = expand_tilde(std::path::Path::new("~/.cfgd.env"));

    // Check env file content
    match std::fs::read_to_string(&env_path) {
        Ok(actual) if actual == expected_content => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: env_path.display().to_string(),
                matches: true,
                expected: "current".to_string(),
                actual: "current".to_string(),
            });
        }
        Ok(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: env_path.display().to_string(),
                matches: false,
                expected: "current".to_string(),
                actual: "stale".to_string(),
            });
            state
                .record_drift(
                    "env",
                    &env_path.display().to_string(),
                    Some("current"),
                    Some("stale"),
                    "local",
                )
                .ok();
        }
        Err(_) => {
            results.push(VerifyResult {
                resource_type: "env".to_string(),
                resource_id: env_path.display().to_string(),
                matches: false,
                expected: "present".to_string(),
                actual: "missing".to_string(),
            });
            state
                .record_drift(
                    "env",
                    &env_path.display().to_string(),
                    Some("present"),
                    Some("missing"),
                    "local",
                )
                .ok();
        }
    }

    // Check shell rc source line
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let rc_path = if shell.contains("zsh") {
        expand_tilde(std::path::Path::new("~/.zshrc"))
    } else {
        expand_tilde(std::path::Path::new("~/.bashrc"))
    };

    if let Ok(rc_content) = std::fs::read_to_string(&rc_path) {
        if rc_content.contains("cfgd.env") {
            results.push(VerifyResult {
                resource_type: "env-rc".to_string(),
                resource_id: rc_path.display().to_string(),
                matches: true,
                expected: "source line present".to_string(),
                actual: "source line present".to_string(),
            });
        } else {
            results.push(VerifyResult {
                resource_type: "env-rc".to_string(),
                resource_id: rc_path.display().to_string(),
                matches: false,
                expected: "source line present".to_string(),
                actual: "source line missing".to_string(),
            });
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

    // Check fish env file if fish conf.d exists
    let fish_conf_d = expand_tilde(std::path::Path::new("~/.config/fish/conf.d"));
    if fish_conf_d.exists() {
        let fish_path = fish_conf_d.join("cfgd-env.fish");
        let expected_fish = generate_fish_env_content(&merged, &merged_aliases);
        match std::fs::read_to_string(&fish_path) {
            Ok(actual) if actual == expected_fish => {
                results.push(VerifyResult {
                    resource_type: "env".to_string(),
                    resource_id: fish_path.display().to_string(),
                    matches: true,
                    expected: "current".to_string(),
                    actual: "current".to_string(),
                });
            }
            Ok(_) => {
                results.push(VerifyResult {
                    resource_type: "env".to_string(),
                    resource_id: fish_path.display().to_string(),
                    matches: false,
                    expected: "current".to_string(),
                    actual: "stale".to_string(),
                });
            }
            Err(_) => {
                results.push(VerifyResult {
                    resource_type: "env".to_string(),
                    resource_id: fish_path.display().to_string(),
                    matches: false,
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }
        }
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
            ScriptAction::Run { path, phase, .. } => {
                let p = match phase {
                    ScriptPhase::PreReconcile => "preReconcile",
                    ScriptPhase::PostReconcile => "postReconcile",
                };
                format!("script:{}:{}", p, path.display())
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
    let mut lines = vec!["# managed by cfgd — do not edit".to_string()];
    for ev in env {
        if ev.value.contains('$') {
            // Unquoted so shell expansion works (e.g. $PATH)
            lines.push(format!("export {}={}", ev.name, ev.value));
        } else {
            lines.push(format!(
                "export {}=\"{}\"",
                ev.name,
                ev.value.replace('"', "\\\"")
            ));
        }
    }
    for alias in aliases {
        lines.push(format!(
            "alias {}=\"{}\"",
            alias.name,
            alias.command.replace('"', "\\\"")
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
    let mut lines = vec!["# managed by cfgd — do not edit".to_string()];
    for ev in env {
        if ev.name == "PATH" {
            // Fish uses space-separated list for PATH, not colon-separated
            let parts: Vec<&str> = ev.value.split(':').collect();
            lines.push(format!("set -gx PATH {}", parts.join(" ")));
        } else {
            lines.push(format!("set -gx {} {}", ev.name, ev.value));
        }
    }
    for alias in aliases {
        lines.push(format!("abbr -a {} {}", alias.name, alias.command));
    }
    lines.push(String::new());
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
                    path,
                    phase,
                    origin,
                    ..
                } => {
                    let p = match phase {
                        ScriptPhase::PreReconcile => "preReconcile",
                        ScriptPhase::PostReconcile => "postReconcile",
                    };
                    format!(
                        "run {} script: {}{}",
                        p,
                        path.display(),
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
        ModuleActionKind::RunScript { script, .. } => {
            format!("[{}] post-apply: {}", action.module_name, script)
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
                    std::os::unix::fs::symlink(source, target)?;
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
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(*mode);
            std::fs::set_permissions(target, perms)?;
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

    struct MockPackageManager {
        name: String,
        installed: HashSet<String>,
    }

    impl PackageManager for MockPackageManager {
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
            Ok(self.installed.clone())
        }
        fn install(&self, _packages: &[String], _printer: &Printer) -> Result<()> {
            Ok(())
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

    fn make_empty_resolved() -> ResolvedProfile {
        ResolvedProfile {
            layers: vec![ProfileLayer {
                source: "local".to_string(),
                profile_name: "test".to_string(),
                priority: 1000,
                policy: LayerPolicy::Local,
                spec: ProfileSpec::default(),
            }],
            merged: MergedProfile::default(),
        }
    }

    #[test]
    fn empty_plan_has_five_phases() {
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), Vec::new())
            .unwrap();

        assert_eq!(plan.phases.len(), 7);
        assert!(plan.is_empty());
    }

    #[test]
    fn plan_includes_package_actions() {
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![PackageAction::Install {
            manager: "brew".to_string(),
            packages: vec!["ripgrep".to_string()],
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(&resolved, Vec::new(), pkg_actions, Vec::new())
            .unwrap();

        assert!(!plan.is_empty());
        assert_eq!(plan.total_actions(), 1);
    }

    #[test]
    fn plan_includes_file_actions() {
        let state = StateStore::open_in_memory().unwrap();
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
            .plan(&resolved, file_actions, Vec::new(), Vec::new())
            .unwrap();

        assert!(!plan.is_empty());
        assert_eq!(plan.total_actions(), 1);
    }

    #[test]
    fn plan_includes_script_actions() {
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);

        let mut resolved = make_empty_resolved();
        resolved.merged.scripts.pre_reconcile = vec![PathBuf::from("scripts/pre.sh")];
        resolved.merged.scripts.post_reconcile = vec![PathBuf::from("scripts/post.sh")];

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), Vec::new())
            .unwrap();

        // Should have 2 script actions
        let script_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Scripts)
            .unwrap();
        assert_eq!(script_phase.actions.len(), 2);
    }

    #[test]
    fn apply_empty_plan_records_success() {
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), Vec::new())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
            .unwrap();

        // Empty plan — no actions means success with 0 results
        assert_eq!(result.status, ApplyStatus::Success);
        assert_eq!(result.action_results.len(), 0);
    }

    #[test]
    fn phase_name_roundtrip() {
        for name in &[
            PhaseName::System,
            PhaseName::Packages,
            PhaseName::Files,
            PhaseName::Secrets,
            PhaseName::Scripts,
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
        let state = StateStore::open_in_memory().unwrap();
        let mut registry = ProviderRegistry::new();

        let mut installed = HashSet::new();
        installed.insert("ripgrep".to_string());
        registry.package_managers.push(Box::new(MockPackageManager {
            name: "cargo".to_string(),
            installed,
        }));

        let mut resolved = make_empty_resolved();
        resolved.merged.packages.cargo = Some(crate::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".to_string(), "bat".to_string()],
        });

        let printer = Printer::new(crate::output::Verbosity::Quiet);
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
                },
                ActionResult {
                    phase: "files".to_string(),
                    description: "test2".to_string(),
                    success: false,
                    error: Some("failed".to_string()),
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

    fn make_resolved_module(name: &str) -> ResolvedModule {
        ResolvedModule {
            name: name.to_string(),
            packages: vec![
                ResolvedPackage {
                    canonical_name: "neovim".to_string(),
                    resolved_name: "neovim".to_string(),
                    manager: "brew".to_string(),
                    version: Some("0.10.2".to_string()),
                    script: None,
                },
                ResolvedPackage {
                    canonical_name: "ripgrep".to_string(),
                    resolved_name: "ripgrep".to_string(),
                    manager: "brew".to_string(),
                    version: Some("14.1.0".to_string()),
                    script: None,
                },
            ],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            depends: vec![],
        }
    }

    #[test]
    fn plan_includes_module_phase() {
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![make_resolved_module("nvim")];
        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules)
            .unwrap();

        // Should have 6 phases, first is Modules
        assert_eq!(plan.phases.len(), 7);
        let module_phase = plan
            .phases
            .iter()
            .find(|p| p.name == PhaseName::Modules)
            .unwrap();

        // Module phase should have at least 1 action (InstallPackages)
        let module_phase = module_phase;
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
        let state = StateStore::open_in_memory().unwrap();
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
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            depends: vec![],
        }];

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules)
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
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![ResolvedModule {
            name: "nvim".to_string(),
            packages: vec![],
            files: vec![],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec!["nvim --headless +qa".to_string(), "echo done".to_string()],
            depends: vec![],
        }];

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules)
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
                    ModuleActionKind::RunScript { script } => {
                        assert!(!script.is_empty());
                    }
                    _ => panic!("expected RunScript action"),
                },
                _ => panic!("expected Module action"),
            }
        }
    }

    #[test]
    fn plan_multiple_modules_in_dependency_order() {
        let state = StateStore::open_in_memory().unwrap();
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
                depends: vec![],
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
                depends: vec!["node".to_string()],
            },
        ];

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules)
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
        let state = StateStore::open_in_memory().unwrap();
        let mut registry = ProviderRegistry::new();

        let mut installed = HashSet::new();
        installed.insert("neovim".to_string());
        registry.package_managers.push(Box::new(MockPackageManager {
            name: "brew".to_string(),
            installed,
        }));

        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let modules = vec![make_resolved_module("nvim")];
        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules.clone())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let _result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &modules)
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
        let state = StateStore::open_in_memory().unwrap();

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
        let state = StateStore::open_in_memory().unwrap();
        let mut registry = ProviderRegistry::new();

        let mut installed = HashSet::new();
        installed.insert("neovim".to_string());
        // ripgrep is NOT installed — should drift
        registry.package_managers.push(Box::new(MockPackageManager {
            name: "brew".to_string(),
            installed,
        }));

        let resolved = make_empty_resolved();
        let printer = Printer::new(crate::output::Verbosity::Quiet);

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
        let state = StateStore::open_in_memory().unwrap();
        let mut registry = ProviderRegistry::new();

        let mut installed = HashSet::new();
        installed.insert("neovim".to_string());
        installed.insert("ripgrep".to_string());
        registry.package_managers.push(Box::new(MockPackageManager {
            name: "brew".to_string(),
            installed,
        }));

        let resolved = make_empty_resolved();
        let printer = Printer::new(crate::output::Verbosity::Quiet);

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
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new(); // no managers

        let resolved = make_empty_resolved();
        let printer = Printer::new(crate::output::Verbosity::Quiet);

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
            depends: vec![],
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
        let state = StateStore::open_in_memory().unwrap();
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
            depends: vec![],
        }];

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules)
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
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), Vec::new())
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
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            depends: vec![],
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
                target,
                is_git_source: false,
                strategy: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            depends: vec![],
        }];

        let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
        assert!(result.is_ok());
    }

    #[test]
    fn conflict_detection_no_overlap_ok() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        std::fs::write(&file_a, "content A").unwrap();
        std::fs::write(&file_b, "content B").unwrap();

        let file_actions = vec![FileAction::Create {
            source: file_a,
            target: PathBuf::from("/target/a"),
            origin: "local".to_string(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
        }];

        let modules = vec![ResolvedModule {
            name: "mymod".to_string(),
            packages: vec![],
            files: vec![crate::modules::ResolvedFile {
                source: file_b,
                target: PathBuf::from("/target/b"),
                is_git_source: false,
                strategy: None,
            }],
            env: vec![],
            aliases: vec![],
            post_apply_scripts: vec![],
            depends: vec![],
        }];

        let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
        assert!(result.is_ok());
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
        // PATH contains $, so unquoted
        assert!(content.contains("export PATH=/usr/local/bin:$PATH"));
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
        assert!(content.contains("set -gx EDITOR nvim"));
        assert!(content.contains("set -gx PATH /usr/local/bin /home/user/.cargo/bin $PATH"));
    }

    #[test]
    fn plan_env_empty_when_no_env() {
        let (actions, _warnings) = Reconciler::plan_env(&[], &[], &[]);
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
            depends: vec![],
        }];
        // plan_env merges and generates actions — the merged env should have EDITOR=nvim
        let (actions, _warnings) = Reconciler::plan_env(&profile_env, &[], &modules);
        // With non-empty env, there should be at least a WriteEnvFile action
        // (since ~/.cfgd.env won't exist in test env)
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
        assert!(content.contains("set -gx EDITOR nvim"));
        assert!(content.contains("abbr -a vim nvim"));
    }

    #[test]
    fn plan_env_aliases_only() {
        let aliases = vec![crate::config::ShellAlias {
            name: "vim".into(),
            command: "nvim".into(),
        }];
        let (actions, _warnings) = Reconciler::plan_env(&[], &aliases, &[]);
        let has_write = actions
            .iter()
            .any(|a| matches!(a, Action::Env(EnvAction::WriteEnvFile { .. })));
        assert!(has_write, "Expected WriteEnvFile action for aliases-only");
    }

    #[test]
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
            depends: vec![],
        }];
        let (actions, _warnings) = Reconciler::plan_env(&[], &profile_aliases, &modules);
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
        let state = StateStore::open_in_memory().unwrap();
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
            .plan(&resolved, Vec::new(), pkg_actions, Vec::new())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
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
        let state = StateStore::open_in_memory().unwrap();
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
            .plan(&resolved, Vec::new(), pkg_actions, Vec::new())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
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
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), Vec::new())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
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
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), Vec::new())
            .unwrap();
        let printer = Printer::new(crate::output::Verbosity::Quiet);

        // First apply
        let result1 = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
            .unwrap();

        // Second apply
        let result2 = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
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

        let printer = Printer::new(crate::output::Verbosity::Quiet);
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

        let printer = Printer::new(crate::output::Verbosity::Quiet);
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

        let printer = Printer::new(crate::output::Verbosity::Quiet);
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

        let printer = Printer::new(crate::output::Verbosity::Quiet);
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

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        Reconciler::apply_env_action(&action, &printer).unwrap();

        let written = std::fs::read_to_string(&rc_path).unwrap();
        assert!(written.starts_with("# my config\n"));
        assert!(written.contains("export FOO=bar"));
        assert!(written.contains("source ~/.cfgd.env"));
    }

    #[test]
    fn apply_full_flow_plan_apply_verify_consistent() {
        let state = StateStore::open_in_memory().unwrap();
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
            .plan(&resolved, Vec::new(), pkg_actions, Vec::new())
            .unwrap();
        assert!(!plan.is_empty());

        // Apply
        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
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
        let state = StateStore::open_in_memory().unwrap();
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
            .plan(&resolved, Vec::new(), pkg_actions, Vec::new())
            .unwrap();
        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
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
        let state = StateStore::open_in_memory().unwrap();
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
            .plan(&resolved, Vec::new(), pkg_actions, Vec::new())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);

        // Apply with filter set to Env phase — should skip Packages
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Env),
                &[],
            )
            .unwrap();

        assert_eq!(result.status, ApplyStatus::Success);
        // No actions executed because Env phase is empty and Packages phase was filtered out
        assert_eq!(result.action_results.len(), 0);
    }

    #[test]
    fn apply_with_phase_filter_runs_only_packages() {
        let state = StateStore::open_in_memory().unwrap();
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
            .plan(&resolved, Vec::new(), pkg_actions, Vec::new())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);

        // Apply with filter set to Packages phase — should run the install
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                Path::new("."),
                &printer,
                Some(&PhaseName::Packages),
                &[],
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

        let state = StateStore::open_in_memory().unwrap();
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
            .plan(&resolved, file_actions, Vec::new(), Vec::new())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(
                &plan,
                &resolved,
                dir.path(),
                &printer,
                Some(&PhaseName::Files),
                &[],
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
        let state = StateStore::open_in_memory().unwrap();
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
            .plan(&resolved, Vec::new(), pkg_actions, Vec::new())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
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
        let state = StateStore::open_in_memory().unwrap();
        let registry = ProviderRegistry::new();
        let reconciler = Reconciler::new(&registry, &state);
        let resolved = make_empty_resolved();

        let pkg_actions = vec![PackageAction::Skip {
            manager: "apt".to_string(),
            reason: "not available on macOS".to_string(),
            origin: "local".to_string(),
        }];

        let plan = reconciler
            .plan(&resolved, Vec::new(), pkg_actions, Vec::new())
            .unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None, &[])
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

        let printer = Printer::new(crate::output::Verbosity::Quiet);
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
}

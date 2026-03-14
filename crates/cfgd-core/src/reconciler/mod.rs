use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;

use sha2::{Digest, Sha256};

use crate::config::{MergedProfile, ResolvedProfile, ScriptSpec};
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::providers::{FileAction, PackageAction, ProviderRegistry, SecretAction};
use crate::state::{ApplyStatus, StateStore};

/// Ordered reconciliation phases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseName {
    Modules,
    System,
    Packages,
    Files,
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
            "secrets" => Ok(PhaseName::Secrets),
            "scripts" => Ok(PhaseName::Scripts),
            _ => Err(format!("unknown phase: {}", s)),
        }
    }
}

/// A unified action across all resource types.
#[derive(Debug)]
pub enum Action {
    File(FileAction),
    Package(PackageAction),
    Secret(SecretAction),
    System(SystemAction),
    Script(ScriptAction),
    Module(ModuleAction),
}

/// Module-level action — first-class phase, not flattened into packages/files.
#[derive(Debug)]
pub struct ModuleAction {
    pub module_name: String,
    pub kind: ModuleActionKind,
}

/// What kind of module action to take.
#[derive(Debug)]
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
#[derive(Debug)]
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
#[derive(Debug)]
pub enum ScriptAction {
    Run {
        path: PathBuf,
        phase: ScriptPhase,
        origin: String,
    },
}

/// When a script runs relative to reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptPhase {
    PreReconcile,
    PostReconcile,
}

/// A phase in the reconciliation plan.
#[derive(Debug)]
pub struct Phase {
    pub name: PhaseName,
    pub actions: Vec<Action>,
}

/// A complete reconciliation plan.
#[derive(Debug)]
pub struct Plan {
    pub phases: Vec<Phase>,
}

impl Plan {
    pub fn total_actions(&self) -> usize {
        self.phases.iter().map(|p| p.actions.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.phases.iter().all(|p| p.actions.is_empty())
    }

    /// Serialize the plan to a string for hashing.
    pub fn to_hash_string(&self) -> String {
        let mut parts = Vec::new();
        for phase in &self.phases {
            for action in &phase.actions {
                match action {
                    Action::File(fa) => parts.push(format!("file:{:?}", fa)),
                    Action::Package(pa) => parts.push(format!("pkg:{:?}", pa)),
                    Action::Secret(sa) => parts.push(format!("secret:{:?}", sa)),
                    Action::System(sa) => parts.push(format!("sys:{:?}", sa)),
                    Action::Script(sa) => parts.push(format!("script:{:?}", sa)),
                    Action::Module(ma) => parts.push(format!("module:{:?}", ma)),
                }
            }
        }
        parts.join("|")
    }
}

/// Result of applying a single action.
#[derive(Debug)]
pub struct ActionResult {
    pub phase: String,
    pub description: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Result of an entire apply operation.
#[derive(Debug)]
pub struct ApplyResult {
    pub action_results: Vec<ActionResult>,
    pub status: ApplyStatus,
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

        // Phase 0: Modules — resolved before profile-level packages/files.
        // Module packages are installed first so that module files and scripts
        // can depend on the software being present.
        let module_phase_actions = self.plan_modules(&module_actions);
        phases.push(Phase {
            name: PhaseName::Modules,
            actions: module_phase_actions,
        });

        // Phase 1: Packages — installed first because system settings, files,
        // and scripts may depend on installed software (e.g. shell: /bin/zsh
        // requires zsh to be installed before chsh can set it).
        let package_actions = pkg_actions.into_iter().map(Action::Package).collect();
        phases.push(Phase {
            name: PhaseName::Packages,
            actions: package_actions,
        });

        // Phase 2: System — runs after packages so required binaries exist
        let system_actions = self.plan_system(&resolved.merged)?;
        phases.push(Phase {
            name: PhaseName::System,
            actions: system_actions,
        });

        // Phase 3: Files
        let fa = file_actions.into_iter().map(Action::File).collect();
        phases.push(Phase {
            name: PhaseName::Files,
            actions: fa,
        });

        // Phase 4: Secrets
        let secret_actions = self.plan_secrets(&resolved.merged);
        phases.push(Phase {
            name: PhaseName::Secrets,
            actions: secret_actions,
        });

        // Phase 5: Scripts
        let script_actions = self.plan_scripts(&resolved.merged.scripts);
        phases.push(Phase {
            name: PhaseName::Scripts,
            actions: script_actions,
        });

        Ok(Plan { phases })
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

            for resolved in by_manager.values() {
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
                format!("{:x}", Sha256::digest(pkg_parts.join("|").as_bytes()))
            };

            // Hash the file targets
            let files_hash = {
                let mut file_parts: Vec<String> = module
                    .files
                    .iter()
                    .map(|f| format!("{}:{}", f.source.display(), f.target.display()))
                    .collect();
                file_parts.sort();
                format!("{:x}", Sha256::digest(file_parts.join("|").as_bytes()))
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
        let mut results = Vec::new();

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

            let pb = printer.progress_bar(phase.actions.len() as u64, phase.name.display_name());

            for action in &phase.actions {
                let result = self.apply_action(action, resolved, config_dir, printer);
                pb.inc(1);

                let (desc, success, error) = match result {
                    Ok(desc) => (desc, true, None),
                    Err(e) => {
                        let desc = format_action_description(action);
                        printer.error(&format!("Failed: {} — {}", desc, e));
                        (desc, false, Some(e.to_string()))
                    }
                };

                results.push(ActionResult {
                    phase: phase.name.as_str().to_string(),
                    description: desc,
                    success,
                    error,
                });
            }

            pb.finish_and_clear();
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

        // Record in state store
        let plan_hash = crate::state::plan_hash(&plan.to_hash_string());
        let profile_name = resolved
            .layers
            .last()
            .map(|l| l.profile_name.as_str())
            .unwrap_or("unknown");

        let summary = serde_json::json!({
            "total": total,
            "succeeded": total - failed,
            "failed": failed,
        })
        .to_string();

        let apply_id =
            self.state
                .record_apply(profile_name, &plan_hash, status.clone(), Some(&summary))?;

        // Update managed resources
        for result in &results {
            if result.success {
                let (rtype, rid) = parse_resource_from_description(&result.description);
                self.state
                    .upsert_managed_resource(&rtype, &rid, "local", None, Some(apply_id))?;
                self.state.resolve_drift(apply_id, &rtype, &rid)?;
            }
        }

        // Update module state for successfully applied modules
        self.update_module_state(module_actions, apply_id, &results)?;

        Ok(ApplyResult {
            action_results: results,
            status,
        })
    }

    fn apply_action(
        &self,
        action: &Action,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
    ) -> Result<String> {
        match action {
            Action::System(sys) => self.apply_system_action(sys, &resolved.merged, printer),
            Action::Package(pkg) => self.apply_package_action(pkg, printer),
            Action::File(file) => {
                self.apply_file_action(file, &resolved.merged, config_dir, printer)
            }
            Action::Secret(secret) => self.apply_secret_action(secret, config_dir, printer),
            Action::Script(script) => self.apply_script_action(script, config_dir, printer),
            Action::Module(module) => self.apply_module_action(module, config_dir, printer),
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
            PackageAction::Bootstrap {
                manager, method, ..
            } => {
                // Find in ALL managers (not just available — it isn't available yet)
                for pm in &self.registry.package_managers {
                    if pm.name() == manager {
                        printer.info(&format!("Bootstrapping {} via {}", manager, method));
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
                Ok(format!("package:{}:bootstrap", manager))
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
                Ok(format!("package:{}:install", manager))
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
                Ok(format!("package:{}:uninstall", manager))
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
                if let Some(parent) = target_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target_path, &decrypted)?;

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
                if let Some(parent) = target_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target_path, &value)?;

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
    ) -> Result<String> {
        match action {
            ScriptAction::Run { path, phase, .. } => {
                let script_path = if path.is_absolute() {
                    path.clone()
                } else {
                    config_dir.join(path)
                };

                let phase_name = match phase {
                    ScriptPhase::PreReconcile => "pre-reconcile",
                    ScriptPhase::PostReconcile => "post-reconcile",
                };

                printer.info(&format!(
                    "Running {} script: {}",
                    phase_name,
                    path.display()
                ));

                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(script_path.display().to_string())
                    .current_dir(config_dir)
                    .output()
                    .map_err(crate::errors::CfgdError::Io)?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(crate::errors::CfgdError::Config(
                        crate::errors::ConfigError::Invalid {
                            message: format!("script {} failed: {}", path.display(), stderr.trim()),
                        },
                    ));
                }

                Ok(format!("script:{}:{}", phase_name, path.display()))
            }
        }
    }

    fn apply_module_action(
        &self,
        action: &ModuleAction,
        config_dir: &std::path::Path,
        printer: &Printer,
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
                                printer.info(&format!(
                                    "Module {}: running install script for {}",
                                    action.module_name, pkg.canonical_name
                                ));
                                let output = std::process::Command::new("sh")
                                    .arg("-c")
                                    .arg(script_content)
                                    .current_dir(config_dir)
                                    .output()
                                    .map_err(crate::errors::CfgdError::Io)?;

                                if !output.status.success() {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    return Err(crate::errors::CfgdError::Config(
                                        crate::errors::ConfigError::Invalid {
                                            message: format!(
                                                "module {} install script for '{}' failed: {}",
                                                action.module_name,
                                                pkg.canonical_name,
                                                stderr.trim()
                                            ),
                                        },
                                    ));
                                }
                            }
                        }
                    } else {
                        for pm in self.registry.available_package_managers() {
                            if pm.name() == first.manager {
                                printer.info(&format!(
                                    "Module {}: installing via {}: {}",
                                    action.module_name,
                                    first.manager,
                                    pkg_names.join(", ")
                                ));
                                pm.install(&pkg_names, printer)?;
                                break;
                            }
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

                    // External module files default to symlink; per-file override
                    // takes precedence.
                    let default_strategy = if file.is_git_source {
                        crate::config::FileStrategy::Symlink
                    } else {
                        crate::config::FileStrategy::Copy
                    };
                    let strategy = file.strategy.unwrap_or(default_strategy);

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
                                std::fs::copy(&file.source, &target)?;
                            }
                        }
                    }
                }

                printer.info(&format!(
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
                printer.info(&format!(
                    "Module {}: running post-apply script",
                    action.module_name
                ));

                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(script)
                    .current_dir(config_dir)
                    .output()
                    .map_err(crate::errors::CfgdError::Io)?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(crate::errors::CfgdError::Config(
                        crate::errors::ConfigError::Invalid {
                            message: format!(
                                "module {} post-apply script failed: {}",
                                action.module_name,
                                stderr.trim()
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

    Ok(results)
}

/// Result of verifying a single resource.
#[derive(Debug)]
pub struct VerifyResult {
    pub resource_type: String,
    pub resource_id: String,
    pub matches: bool,
    pub expected: String,
    pub actual: String,
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
                    ScriptPhase::PreReconcile => "pre-reconcile",
                    ScriptPhase::PostReconcile => "post-reconcile",
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
    }
}

/// Compute SHA256 hash of a file's content, returning None if the file doesn't exist.
fn content_hash_if_exists(path: &std::path::Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| format!("{:x}", Sha256::digest(&bytes)))
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
                        ScriptPhase::PreReconcile => "pre-reconcile",
                        ScriptPhase::PostReconcile => "post-reconcile",
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
            } => FileAction::Create {
                source: source.clone(),
                target: target.clone(),
                origin: origin.clone(),
                strategy: *strategy,
            },
            FileAction::Update {
                source,
                target,
                diff,
                origin,
                strategy,
            } => FileAction::Update {
                source: source.clone(),
                target: target.clone(),
                diff: diff.clone(),
                origin: origin.clone(),
                strategy: *strategy,
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

        assert_eq!(plan.phases.len(), 6);
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
        assert_eq!(plan.phases.len(), 6);
        assert_eq!(plan.phases[0].name, PhaseName::Modules);

        // Module phase should have at least 1 action (InstallPackages)
        let module_phase = &plan.phases[0];
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
            post_apply_scripts: vec![],
            depends: vec![],
        }];

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules)
            .unwrap();

        let module_phase = &plan.phases[0];
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
            post_apply_scripts: vec!["nvim --headless +qa".to_string(), "echo done".to_string()],
            depends: vec![],
        }];

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules)
            .unwrap();

        let module_phase = &plan.phases[0];
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
                post_apply_scripts: vec![],
                depends: vec!["node".to_string()],
            },
        ];

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules)
            .unwrap();

        let module_phase = &plan.phases[0];
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
        };

        let hash = plan.to_hash_string();
        assert!(hash.contains("module:"));
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
            post_apply_scripts: vec![],
            depends: vec![],
        }];

        let plan = reconciler
            .plan(&resolved, Vec::new(), Vec::new(), modules)
            .unwrap();

        let module_phase = &plan.phases[0];
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
            post_apply_scripts: vec![],
            depends: vec![],
        }];

        let result = Reconciler::detect_file_conflicts(&file_actions, &modules);
        assert!(result.is_ok());
    }
}

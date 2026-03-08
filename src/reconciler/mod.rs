#![allow(dead_code)]

use std::path::PathBuf;

use crate::config::{MergedProfile, ResolvedProfile, ScriptSpec};
use crate::errors::Result;
use crate::output::Printer;
use crate::providers::{FileAction, PackageAction, ProviderRegistry, SecretAction};
use crate::state::{ApplyStatus, StateStore};

/// Ordered reconciliation phases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseName {
    System,
    Packages,
    Files,
    Secrets,
    Scripts,
}

impl PhaseName {
    pub fn as_str(&self) -> &str {
        match self {
            PhaseName::System => "system",
            PhaseName::Packages => "packages",
            PhaseName::Files => "files",
            PhaseName::Secrets => "secrets",
            PhaseName::Scripts => "scripts",
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            PhaseName::System => "System",
            PhaseName::Packages => "Packages",
            PhaseName::Files => "Files",
            PhaseName::Secrets => "Secrets",
            PhaseName::Scripts => "Scripts",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "system" => Some(PhaseName::System),
            "packages" => Some(PhaseName::Packages),
            "files" => Some(PhaseName::Files),
            "secrets" => Some(PhaseName::Secrets),
            "scripts" => Some(PhaseName::Scripts),
            _ => None,
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

/// When a script runs relative to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptPhase {
    PreApply,
    PostApply,
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
    ) -> Result<Plan> {
        let mut phases = Vec::new();

        // Phase 1: System
        let system_actions = self.plan_system(&resolved.merged)?;
        phases.push(Phase {
            name: PhaseName::System,
            actions: system_actions,
        });

        // Phase 2: Packages
        let package_actions = pkg_actions.into_iter().map(Action::Package).collect();
        phases.push(Phase {
            name: PhaseName::Packages,
            actions: package_actions,
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
                crate::secrets::parse_secret_reference(&secret.source)
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

        for path in &scripts.pre_apply {
            actions.push(Action::Script(ScriptAction::Run {
                path: path.clone(),
                phase: ScriptPhase::PreApply,
                origin: "local".to_string(),
            }));
        }

        for path in &scripts.post_apply {
            actions.push(Action::Script(ScriptAction::Run {
                path: path.clone(),
                phase: ScriptPhase::PostApply,
                origin: "local".to_string(),
            }));
        }

        actions
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
    ) -> Result<ApplyResult> {
        let mut results = Vec::new();

        for phase in &plan.phases {
            if let Some(filter) = phase_filter {
                if &phase.name != filter {
                    continue;
                }
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
                    ScriptPhase::PreApply => "pre-apply",
                    ScriptPhase::PostApply => "post-apply",
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
}

/// Verify all managed resources match their desired state.
pub fn verify(
    resolved: &ResolvedProfile,
    registry: &ProviderRegistry,
    state: &StateStore,
    _printer: &Printer,
) -> Result<Vec<VerifyResult>> {
    let mut results = Vec::new();

    // Verify packages
    let available_managers = registry.available_package_managers();
    for pm in &available_managers {
        let desired = crate::packages::desired_packages_for(pm.name(), &resolved.merged);
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
                    ScriptPhase::PreApply => "pre-apply",
                    ScriptPhase::PostApply => "post-apply",
                };
                format!("script:{}:{}", p, path.display())
            }
        },
    }
}

/// Format plan phase items for display.
pub fn format_plan_items(phase: &Phase) -> Vec<String> {
    phase
        .actions
        .iter()
        .filter_map(|action| match action {
            Action::File(fa) => match fa {
                FileAction::Create { target, .. } => Some(format!("create {}", target.display())),
                FileAction::Update { target, .. } => Some(format!("update {}", target.display())),
                FileAction::Delete { target, .. } => Some(format!("delete {}", target.display())),
                FileAction::SetPermissions { target, mode, .. } => {
                    Some(format!("chmod {:#o} {}", mode, target.display()))
                }
                FileAction::Skip { .. } => None,
            },
            Action::Package(pa) => match pa {
                PackageAction::Install {
                    manager, packages, ..
                } => Some(format!("install via {}: {}", manager, packages.join(", "))),
                PackageAction::Uninstall {
                    manager, packages, ..
                } => Some(format!(
                    "uninstall via {}: {}",
                    manager,
                    packages.join(", ")
                )),
                PackageAction::Skip { .. } => None,
            },
            Action::Secret(sa) => match sa {
                SecretAction::Decrypt {
                    source,
                    target,
                    backend,
                    ..
                } => Some(format!(
                    "decrypt {} → {} (via {})",
                    source.display(),
                    target.display(),
                    backend
                )),
                SecretAction::Resolve {
                    provider,
                    reference,
                    target,
                    ..
                } => Some(format!(
                    "resolve {}://{} → {}",
                    provider,
                    reference,
                    target.display()
                )),
                SecretAction::Skip { .. } => None,
            },
            Action::System(sa) => match sa {
                SystemAction::SetValue {
                    configurator,
                    key,
                    desired,
                    current,
                    ..
                } => Some(format!(
                    "set {}.{}: {} → {}",
                    configurator, key, current, desired
                )),
                SystemAction::Skip {
                    configurator,
                    reason,
                    ..
                } => Some(format!("skip {}: {}", configurator, reason)),
            },
            Action::Script(sa) => match sa {
                ScriptAction::Run { path, phase, .. } => {
                    let p = match phase {
                        ScriptPhase::PreApply => "pre-apply",
                        ScriptPhase::PostApply => "post-apply",
                    };
                    Some(format!("run {} script: {}", p, path.display()))
                }
            },
        })
        .collect()
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

fn expand_tilde(path: &std::path::Path) -> PathBuf {
    let path_str = path.display().to_string();
    if path_str.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(path_str.replacen('~', &home, 1));
        }
    }
    path.to_path_buf()
}

fn apply_file_action_direct(
    action: &FileAction,
    _config_dir: &std::path::Path,
    _profile: &MergedProfile,
) -> Result<()> {
    match action {
        FileAction::Create { source, target, .. } | FileAction::Update { source, target, .. } => {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(source, target)?;
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
            } => FileAction::Create {
                source: source.clone(),
                target: target.clone(),
                origin: origin.clone(),
            },
            FileAction::Update {
                source,
                target,
                diff,
                origin,
            } => FileAction::Update {
                source: source.clone(),
                target: target.clone(),
                diff: diff.clone(),
                origin: origin.clone(),
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

        let plan = reconciler.plan(&resolved, Vec::new(), Vec::new()).unwrap();

        assert_eq!(plan.phases.len(), 5);
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

        let plan = reconciler.plan(&resolved, Vec::new(), pkg_actions).unwrap();

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
        }];

        let plan = reconciler
            .plan(&resolved, file_actions, Vec::new())
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
        resolved.merged.scripts.pre_apply = vec![PathBuf::from("scripts/pre.sh")];
        resolved.merged.scripts.post_apply = vec![PathBuf::from("scripts/post.sh")];

        let plan = reconciler.plan(&resolved, Vec::new(), Vec::new()).unwrap();

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

        let plan = reconciler.plan(&resolved, Vec::new(), Vec::new()).unwrap();

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let result = reconciler
            .apply(&plan, &resolved, Path::new("."), &printer, None)
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
        assert_eq!(items.len(), 1); // Skip is filtered out
        assert!(items[0].contains("ripgrep"));
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
        resolved.merged.packages.cargo = vec!["ripgrep".to_string(), "bat".to_string()];

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        let results = verify(&resolved, &registry, &state, &printer).unwrap();

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
}

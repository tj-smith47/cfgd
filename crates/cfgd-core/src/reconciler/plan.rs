use std::collections::HashMap;
use std::path::PathBuf;

use crate::PathDisplayExt;
use crate::config::{MergedProfile, ResolvedProfile, ScriptSpec};
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::providers::{FileAction, PackageAction, SecretAction};

use super::restore::content_hash_if_exists;
use super::types::{
    Action, ModuleAction, ModuleActionKind, Phase, PhaseName, Plan, ReconcileContext, ScriptAction,
    ScriptPhase, SystemAction,
};

impl<'a> super::Reconciler<'a> {
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

        // PreScripts: pre-apply or pre-reconcile hooks.
        let (pre_script_actions, post_script_actions) =
            self.plan_scripts(&resolved.merged.scripts, context);
        phases.push(Phase {
            name: PhaseName::PreScripts,
            actions: pre_script_actions,
        });

        // Env: write ~/.cfgd.env and inject shell rc source line.
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

        // Modules: module packages, files, and post-apply scripts.
        // Packages are grouped with system/native managers first, then
        // bootstrappable managers, so build deps are installed before
        // packages that need them.
        let module_phase_actions = self.plan_modules(&module_actions, context);
        phases.push(Phase {
            name: PhaseName::Modules,
            actions: module_phase_actions,
        });

        // Packages: profile-level packages, installed after modules
        // so module deps are available.
        let package_actions = pkg_actions.into_iter().map(Action::Package).collect();
        phases.push(Phase {
            name: PhaseName::Packages,
            actions: package_actions,
        });

        // System: runs after packages so required binaries exist.
        let system_actions = self.plan_system(&resolved.merged, &module_actions)?;
        phases.push(Phase {
            name: PhaseName::System,
            actions: system_actions,
        });

        // Files.
        let fa = file_actions.into_iter().map(Action::File).collect();
        phases.push(Phase {
            name: PhaseName::Files,
            actions: fa,
        });

        // Secrets.
        let secret_actions = self.plan_secrets(&resolved.merged);
        phases.push(Phase {
            name: PhaseName::Secrets,
            actions: secret_actions,
        });

        // PostScripts: post-apply or post-reconcile hooks.
        phases.push(Phase {
            name: PhaseName::PostScripts,
            actions: post_script_actions,
        });

        Ok(Plan { phases, warnings })
    }

    /// Check for file target conflicts across profile files and module files.
    /// Two sources targeting the same path with identical content is allowed;
    /// different content is an error.
    pub(super) fn detect_file_conflicts(
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
            let label = format!("profile:{}", source.posix());
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

    pub(super) fn plan_system(
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

    pub(super) fn plan_secrets(&self, profile: &MergedProfile) -> Vec<Action> {
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

    pub(super) fn plan_modules(
        &self,
        modules: &[ResolvedModule],
        context: ReconcileContext,
    ) -> Vec<Action> {
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
                                        file.source.posix()
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
                                                file.source.posix(),
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
                                                file.source.posix(),
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
}

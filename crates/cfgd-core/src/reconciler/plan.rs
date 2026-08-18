use std::collections::HashMap;
use std::path::PathBuf;

use crate::PathDisplayExt;
use crate::config::{LOCAL_LAYER, MergedProfile, ResolvedProfile, ScriptSpec};
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::providers::{FileAction, PackageAction, PackageManagerExt, SecretAction};

use super::restore::content_hash_if_exists;
use super::types::{
    Action, ModuleAction, ModuleActionKind, Owner, Phase, PhaseName, Plan, ReconcileContext,
    ScriptAction, ScriptPhase, SystemAction,
};

/// The phase a module action belongs to.
///
/// Total: every variant returns a phase and nothing panics. `Skip` is the one
/// kind whose phase the *emit site* decides — `plan_modules` emits a
/// platform-gated skip and a file-encryption skip from different places and
/// only `plan_modules` can tell them apart — and its arm here answers with the
/// meta phase, the answer that is correct for the only `Skip` a caller could
/// route through this function by mistake.
fn phase_for_module_kind(kind: &ModuleActionKind) -> PhaseName {
    match kind {
        ModuleActionKind::RunScript { phase, .. } => match phase {
            ScriptPhase::PreApply | ScriptPhase::PreReconcile | ScriptPhase::PreBackup => {
                PhaseName::PreScripts
            }
            ScriptPhase::PostApply
            | ScriptPhase::PostReconcile
            | ScriptPhase::OnDrift
            | ScriptPhase::OnChange
            | ScriptPhase::PostBackup => PhaseName::PostScripts,
            // A `patch.script` filter runs inline through `PatchBinding`, never
            // as a planned lifecycle action — but the match must stay total.
            ScriptPhase::Patch => PhaseName::Files,
        },
        ModuleActionKind::InstallPackages { .. } => PhaseName::Packages,
        ModuleActionKind::DeployFiles { .. } => PhaseName::Files,
        ModuleActionKind::Skip { .. } => PhaseName::Modules,
    }
}

/// Build a module action tagged with the phase its kind routes to.
fn routed(module: &ResolvedModule, kind: ModuleActionKind) -> (PhaseName, Action) {
    routed_to(module, phase_for_module_kind(&kind), kind)
}

/// Build a module action tagged with an explicitly named phase — the emit-site
/// form, for the two `Skip` shapes whose phase only the emit site can tell
/// apart.
fn routed_to(
    module: &ResolvedModule,
    phase: PhaseName,
    kind: ModuleActionKind,
) -> (PhaseName, Action) {
    (
        phase,
        Action::Module(ModuleAction::with_origin(
            module.name.clone(),
            kind,
            module.origin.clone(),
        )),
    )
}

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

        // The profile is the document the user edited, so every action derived
        // from the merged profile is owned by its leaf profile name.
        let profile = Owner::profile(resolved.profile_name());

        let (pre_script_actions, post_script_actions) =
            self.plan_scripts(&resolved.merged.scripts, context);

        // Module work is attributed to the phase whose KIND it is, so a
        // module's packages sit beside the profile's in `Packages` rather than
        // in a bucket of their own. Packages are grouped with system/native
        // managers first, then bootstrappable managers, so build deps are
        // installed before packages that need them.
        let mut module_routed = self.plan_modules(&module_actions, context);

        // Cross-scope package dedup: a (manager, resolved_name) declared in both a
        // module and the profile (or in two modules) installs once. Module installs
        // win because module-owned package work is dispatched first (Rule P's
        // tier 0) and a module's postApply may need the package present; among
        // modules the earlier one wins (module-order walk).
        let claimed = Self::dedup_module_packages(&mut module_routed);

        // Profile-level packages are applied after module packages so module
        // deps are available — an execution-order property held by
        // `Phase::dispatch_order`, not by the phase list. Profile entries
        // already claimed by a module install are dropped here so the package
        // installs only once.
        let profile_packages = Self::filter_profile_packages(pkg_actions, &claimed);

        // Prerequisites: the managers that create binaries, then the env file
        // that publishes where they live, then the live-session broadcast. It
        // is planned from the package work that SURVIVED dedup and filtering,
        // because a manager whose every install was claimed elsewhere has no
        // consumer left in this run and must not mint a node — a converged
        // host plans nothing, which is what keeps a daemon tick from running
        // `apt update` on every interval.
        let manager_actions =
            super::managers::plan_managers(self.registry, &profile_packages, &module_routed);

        // The env file publishes where a manager's binaries live, so it has to
        // know about a manager this very run is about to provision, not only
        // the ones past runs already recorded — otherwise a script sourcing
        // the freshly-written env file right after `cfgd apply` still can't
        // find a binary the same run just installed.
        let path_dirs = super::managers::fold_provision_path_dirs(
            self.registry,
            &manager_actions,
            super::env::recorded_manager_path_dirs(self.state, &resolved.merged, &module_actions),
        );
        let (env_actions, warnings) = self.plan_env(
            &resolved.merged.env,
            &resolved.merged.aliases,
            resolved.merged.env_scope,
            &module_actions,
            &[], // Secret envs are not yet resolved at plan time; they are
            // injected during the apply phase after ResolveEnv actions run.
            &path_dirs,
            &super::env::recorded_managed_env_files(self.state),
        );

        let package_actions = profile_packages
            .into_iter()
            .map(Action::Package)
            .collect::<Vec<_>>();

        let system_actions = self.plan_system(&resolved.merged, &module_actions)?;
        let secret_actions = self.plan_secrets(&resolved.merged);

        let mut buckets: Vec<(PhaseName, Vec<Action>)> = vec![
            // `Modules` holds only platform-gated skips — the meta phase — and
            // is first so a "not for this host" answer precedes every step.
            (PhaseName::Modules, Vec::new()),
            (PhaseName::PreScripts, pre_script_actions),
            // One phase, three cfgd-owned groups in producer-before-consumer
            // order: `cfgd:managers` creates the binaries, `cfgd:env` publishes
            // where they live, `cfgd:session` broadcasts. `Owner::sort_key`
            // orders the groups; the concatenation order here is irrelevant.
            (
                PhaseName::Prerequisites,
                manager_actions.into_iter().chain(env_actions).collect(),
            ),
            (PhaseName::Packages, package_actions),
            // `Files` precedes `System` so a file is materialised before
            // anything that consumes it: a unit file deployed through `files:`
            // has to exist before `systemctl enable` names it.
            (
                PhaseName::Files,
                file_actions.into_iter().map(Action::File).collect(),
            ),
            (PhaseName::System, system_actions),
            (PhaseName::Secrets, secret_actions),
            (PhaseName::PostScripts, post_script_actions),
        ];

        for (phase_name, action) in module_routed {
            if let Some((_, bucket)) = buckets.iter_mut().find(|(n, _)| *n == phase_name) {
                bucket.push(action);
            }
        }

        let mut phases: Vec<Phase> = buckets
            .into_iter()
            .map(|(name, actions)| Phase::from_actions(name, &profile, actions))
            .collect();

        // A phase with no actions is not a step the run will take — it is an
        // artifact of every phase being constructed unconditionally. Applying a
        // module with no profile (`init --apply-module`) resolves an empty
        // profile, so files/packages/system materialized as empty phases that
        // rendered "(nothing to do)" beside phases doing real work. Drop them
        // here, at the source, so display, `-o json`, and apply all agree.
        phases.retain(|p| !p.is_empty());

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
        let system = crate::effective::effective_system_map(profile, modules);

        let mut actions = Vec::new();

        // One sweep for both loops below: the second asks which keys no
        // available configurator claims, which is the same population.
        let available = self.registry.available_system_configurators();

        for configurator in &available {
            if let Some(desired) = system.get(configurator.name()) {
                let drifts = configurator.diff(desired)?;
                for drift in drifts {
                    crate::reconciler::debug_assert_system_key_undoubled(
                        configurator.name(),
                        &drift.key,
                    );
                    actions.push(Action::System(SystemAction::SetValue {
                        configurator: configurator.name().to_string(),
                        key: drift.key,
                        desired: drift.expected,
                        current: drift.actual,
                        origin: LOCAL_LAYER.to_string(),
                    }));
                }
            }
        }

        // Surface system keys that won't be applied, distinguishing "this host
        // can't run it" from "cfgd has no such configurator" — a registered-but-
        // unavailable configurator (e.g. systemdUnits where systemctl is absent)
        // must not masquerade as "not registered".
        for key in system.keys() {
            if available.iter().any(|c| c.name() == key) {
                continue;
            }
            let registered = self
                .registry
                .system_configurators
                .iter()
                .any(|c| c.name() == key);
            let reason = if registered {
                format!("'{}' is not available on this host", key)
            } else {
                format!("no configurator registered for '{}'", key)
            };
            actions.push(Action::System(SystemAction::Skip {
                configurator: key.clone(),
                reason,
                origin: LOCAL_LAYER.to_string(),
                unknown: !registered,
            }));
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
                            target: crate::expand_tilde(target),
                            origin: LOCAL_LAYER.to_string(),
                        }));
                    }

                    // Env injection action when envs are specified
                    if has_envs {
                        actions.push(Action::Secret(SecretAction::ResolveEnv {
                            provider: provider_name.to_string(),
                            reference: reference.to_string(),
                            envs: secret.envs.clone().unwrap_or_default(),
                            origin: LOCAL_LAYER.to_string(),
                        }));
                    }

                    // If neither target nor envs, skip (shouldn't happen due to validation)
                    if secret.target.is_none() && !has_envs {
                        actions.push(Action::Secret(SecretAction::Skip {
                            source: secret.source.clone(),
                            reason: "no target or envs specified".to_string(),
                            origin: LOCAL_LAYER.to_string(),
                        }));
                    }
                } else {
                    actions.push(Action::Secret(SecretAction::Skip {
                        source: secret.source.clone(),
                        reason: format!("provider '{}' not available", provider_name),
                        origin: LOCAL_LAYER.to_string(),
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
                    target: secret
                        .target
                        .as_deref()
                        .map(crate::expand_tilde)
                        .unwrap_or_default(),
                    backend: backend_name,
                    origin: LOCAL_LAYER.to_string(),
                }));

                if has_envs {
                    actions.push(Action::Secret(SecretAction::Skip {
                        source: secret.source.clone(),
                        reason: "env injection requires a secret provider reference; SOPS file targets cannot inject env vars".to_string(),
                        origin: LOCAL_LAYER.to_string(),
                    }));
                }
            } else if secret.target.is_none() && has_envs && !has_backend {
                // Env-only secret without a provider reference — SOPS can't resolve
                // individual values for env injection
                actions.push(Action::Secret(SecretAction::Skip {
                    source: secret.source.clone(),
                    reason: "env injection requires a secret provider reference (e.g. 1password://, vault://)".to_string(),
                    origin: LOCAL_LAYER.to_string(),
                }));
            } else if !has_backend {
                actions.push(Action::Secret(SecretAction::Skip {
                    source: secret.source.clone(),
                    reason: "no secret backend available".to_string(),
                    origin: LOCAL_LAYER.to_string(),
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
                    origin: LOCAL_LAYER.to_string(),
                })
            })
            .collect();

        let post_actions = post_entries
            .iter()
            .map(|entry| {
                Action::Script(ScriptAction::Run {
                    entry: entry.clone(),
                    phase: post_phase.clone(),
                    origin: LOCAL_LAYER.to_string(),
                })
            })
            .collect();

        (pre_actions, post_actions)
    }

    pub(super) fn plan_modules(
        &self,
        modules: &[ResolvedModule],
        context: ReconcileContext,
    ) -> Vec<(PhaseName, Action)> {
        let mut actions = Vec::new();

        for module in modules {
            // Platform-gated module: surface a single visible Skip and emit no
            // other actions. A Skip action reports changed=false and does not
            // fire onChange, so no apply-side handling is needed. It is the
            // whole module that is gated off this host, so it names the meta
            // phase rather than the phase of any work it would have done.
            if let Some(reason) = &module.platform_skip_reason {
                actions.push(routed_to(
                    module,
                    PhaseName::Modules,
                    ModuleActionKind::Skip {
                        reason: reason.clone(),
                    },
                ));
                continue;
            }

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
                actions.push(routed(
                    module,
                    ModuleActionKind::RunScript {
                        script: script.clone(),
                        phase: pre_phase.clone(),
                    },
                ));
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
            // The sort is the offer order, not the guarantee: what actually keeps
            // a bootstrappable manager's action from overlapping an available
            // one's is the dispatcher's serial gate around any action whose
            // manager is not currently available.
            // The class is computed once PER MANAGER, ahead of the sort, rather
            // than inside the comparator: `is_available()` is a PATH probe, and
            // a comparator runs it O(n log n) times per module.
            let mut manager_order: Vec<(u8, &String)> = by_manager
                .keys()
                .map(|mgr| {
                    let class = match self
                        .registry
                        .package_managers
                        .iter()
                        .find(|m| m.name() == mgr.as_str())
                    {
                        Some(m) if m.is_available() => 0, // available (native) first
                        // Bootstrappable second — a manager with a plan to provision it.
                        Some(m) if m.can_bootstrap() => 1,
                        _ => 2, // unknown last
                    };
                    (class, mgr)
                })
                .collect();
            // `(class, name)`, not `class` alone: a key that ties on `class`
            // would otherwise fall back to `HashMap::keys()`'s arbitrary,
            // per-process order. Two managers in the same class (`cargo` and
            // `npm`, both `0`) would then emit their `InstallPackages` actions
            // in a different relative order on every run — the plan tree's
            // bullet order, the `-o json` payload order, the journal
            // `action_index`, and the phase's execution offer order all read
            // from this `Vec`.
            manager_order.sort_by(|a, b| (a.0, a.1.as_str()).cmp(&(b.0, b.1.as_str())));

            for (_, mgr_name) in manager_order {
                let resolved = &by_manager[mgr_name];
                actions.push(routed(
                    module,
                    ModuleActionKind::InstallPackages {
                        resolved: resolved.clone(),
                    },
                ));
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
                            // This module's FILE work cannot proceed — the skip
                            // stays attached to the phase that work belongs to.
                            actions.push(routed_to(
                                module,
                                PhaseName::Files,
                                ModuleActionKind::Skip {
                                    reason: format!(
                                        "encryption mode Always incompatible with {:?} for {}",
                                        strategy,
                                        file.source.posix()
                                    ),
                                },
                            ));
                            encryption_ok = false;
                            break;
                        }
                        if file.source.exists() {
                            match crate::is_file_encrypted(&file.source, &enc.backend) {
                                Ok(true) => {}
                                Ok(false) => {
                                    actions.push(routed_to(
                                        module,
                                        PhaseName::Files,
                                        ModuleActionKind::Skip {
                                            reason: format!(
                                                "file {} requires encryption (backend: {}) but is not encrypted",
                                                file.source.posix(),
                                                enc.backend
                                            ),
                                        },
                                    ));
                                    encryption_ok = false;
                                    break;
                                }
                                Err(e) => {
                                    actions.push(routed_to(
                                        module,
                                        PhaseName::Files,
                                        ModuleActionKind::Skip {
                                            reason: format!(
                                                "encryption check failed for {}: {}",
                                                file.source.posix(),
                                                e
                                            ),
                                        },
                                    ));
                                    encryption_ok = false;
                                    break;
                                }
                            }
                        }
                    }
                }
                if encryption_ok {
                    actions.push(routed(
                        module,
                        ModuleActionKind::DeployFiles {
                            files: module.files.clone(),
                        },
                    ));
                }
            }

            // Post-scripts for this module
            for script in post_scripts {
                actions.push(routed(
                    module,
                    ModuleActionKind::RunScript {
                        script: script.clone(),
                        phase: post_phase.clone(),
                    },
                ));
            }
        }

        actions
    }

    /// Dedupe module `InstallPackages` actions in place, keeping the first-seen
    /// `(manager, resolved_name)` across every module action, and return the
    /// [`crate::config::PackageClaim`] recording the keys that were kept.
    ///
    /// Emptied `InstallPackages` actions are dropped so no empty install is
    /// emitted. The claiming rules (earlier-module-wins, `script` exemption) live
    /// in [`crate::config::PackageClaim::claim_module`].
    pub(super) fn dedup_module_packages(
        module_actions: &mut Vec<(PhaseName, Action)>,
    ) -> crate::config::PackageClaim {
        let mut claim = crate::config::PackageClaim::new();
        module_actions.retain_mut(|(_, action)| {
            let Action::Module(ModuleAction {
                kind: ModuleActionKind::InstallPackages { resolved },
                ..
            }) = action
            else {
                return true;
            };
            resolved.retain(|rp| claim.claim_module(&rp.manager, &rp.resolved_name));
            !resolved.is_empty()
        });
        claim
    }

    /// Drop profile `Install` entries whose `(manager, name)` was already claimed
    /// by a module install, dropping the whole `Install` when it empties.
    ///
    /// Module installs win over profile duplicates: module-owned package work is
    /// dispatched ahead of profile-owned (Rule P's tier 0), and a module's own
    /// postApply scripts may depend on the package being present, so the
    /// module's install must be the one that runs.
    /// All non-`Install` variants pass through untouched.
    pub(super) fn filter_profile_packages(
        pkg_actions: Vec<PackageAction>,
        claim: &crate::config::PackageClaim,
    ) -> Vec<PackageAction> {
        pkg_actions
            .into_iter()
            .filter_map(|action| match action {
                PackageAction::Install {
                    manager,
                    packages,
                    origin,
                } => {
                    let kept: Vec<String> = packages
                        .into_iter()
                        .filter(|name| !claim.is_claimed(&manager, name))
                        .collect();
                    if kept.is_empty() {
                        None
                    } else {
                        Some(PackageAction::Install {
                            manager,
                            packages: kept,
                            origin,
                        })
                    }
                }
                other => Some(other),
            })
            .collect()
    }
}

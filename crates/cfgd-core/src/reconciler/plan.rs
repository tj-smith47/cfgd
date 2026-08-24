use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::PathDisplayExt;
use crate::config::{LOCAL_LAYER, MergedProfile, ResolvedProfile, ScriptSpec};
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::providers::{
    FileAction, PackageAction, PackageManager, PackageManagerExt, SecretAction,
};

use super::env::EnvPlanOutcome;
use super::restore::content_hash_if_exists;
use super::types::{
    Action, ModuleAction, ModuleActionKind, Owner, Phase, PhaseName, Plan, ReconcileContext,
    ScriptAction, ScriptPhase, SystemAction,
};

/// Actions tagged with the phase each is routed to.
pub(super) type RoutedActions = Vec<(PhaseName, Action)>;

/// The per-manager sort key `plan_modules` orders a module's package batches
/// by: (availability class, family, source-registering managers first within
/// the family, name), with the registry's resolved manager carried alongside
/// so the elision below never re-walks the registry.
type ManagerOrder<'a> = (
    u8,
    &'a str,
    bool,
    &'a String,
    Option<&'a dyn PackageManager>,
);

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
        self.plan_observed(
            resolved,
            file_actions,
            pkg_actions,
            module_actions,
            context,
            &mut |_| {},
        )
    }

    /// [`Reconciler::plan`], reporting each phase to `observe` as that phase's
    /// contents are computed.
    ///
    /// The seam exists so a command can narrate a plan while it is being built
    /// instead of standing silent until the whole tree is ready. `observe` fires
    /// at real computation boundaries, in COMPUTATION order rather than render
    /// order — the two differ because the phases are not independent: no bucket
    /// is final until module work has been routed into it, `Prerequisites` is
    /// planned from the package work that survived dedup, and the caller hands
    /// `file_actions` in already computed. Two phases therefore never fire.
    /// `Files` fires for the conflict sweep that hashes every declared source,
    /// which is the only file work this function does; `PostScripts` fires not
    /// at all, because profile post-scripts are computed in the same pass as
    /// pre-scripts and a module's own are computed under `Modules`.
    ///
    /// Observation changes nothing about the plan: the `Plan` this returns is
    /// the one [`Reconciler::plan`] returns for the same inputs.
    pub fn plan_observed(
        &self,
        resolved: &ResolvedProfile,
        file_actions: Vec<FileAction>,
        pkg_actions: Vec<PackageAction>,
        module_actions: Vec<ResolvedModule>,
        context: ReconcileContext,
        observe: &mut dyn FnMut(PhaseName),
    ) -> Result<Plan> {
        // Conflict detection: check for multiple sources targeting the same path
        observe(PhaseName::Files);
        Self::detect_file_conflicts(&file_actions, &module_actions)?;

        // The profile is the document the user edited, so every action derived
        // from the merged profile is owned by its leaf profile name.
        let profile = Owner::profile(resolved.profile_name());

        observe(PhaseName::PreScripts);
        let (pre_script_actions, post_script_actions) =
            self.plan_scripts(&resolved.merged.scripts, context);

        // Module work is attributed to the phase whose KIND it is, so a
        // module's packages sit beside the profile's in `Packages` rather than
        // in a bucket of their own. Packages are grouped with system/native
        // managers first, then bootstrappable managers, so build deps are
        // installed before packages that need them.
        observe(PhaseName::Modules);
        let (mut module_routed, env_gated_hooks) =
            self.plan_modules(&module_actions, resolved.profile_name(), context);

        // Cross-scope package dedup: a (manager, resolved_name) declared in both a
        // module and the profile (or in two modules) installs once. Module installs
        // win because module-owned package work is dispatched first (Rule P's
        // tier 0) and a module's postApply may need the package present; among
        // modules the earlier one wins (module-order walk).
        observe(PhaseName::Packages);
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
        observe(PhaseName::Prerequisites);
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
        let env_plan = self.plan_env(
            &resolved.merged.env,
            &resolved.merged.aliases,
            resolved.merged.env_scope,
            &module_actions,
            &[], // Secret envs are not yet resolved at plan time; they are
            // injected during the apply phase after ResolveEnv actions run.
            &path_dirs,
            &super::env::recorded_managed_env_files(self.state),
        );

        // The env-gated hooks deferred by `plan_modules`: a module's hooks
        // revive off the env surface only when its OWN env/alias entries
        // moved between the deployed primary env file and the content this
        // plan writes — another layer rewriting the shared surface is that
        // layer's work, not this module's, so its hooks stay dropped. No
        // planned primary rewrite means no declared entry moved (every entry
        // renders into that file), so nothing revives; an unanswerable
        // baseline fails OPEN and revives. Safe to fold in this late —
        // `dedup_module_packages` and `plan_managers` above read only
        // `InstallPackages` actions, and hooks are none.
        let EnvPlanOutcome {
            actions: env_actions,
            warnings,
            primary_write,
        } = env_plan;
        for (module_name, hooks) in env_gated_hooks {
            let revive = primary_write.as_ref().is_some_and(|w| {
                module_actions
                    .iter()
                    .find(|m| m.name == module_name)
                    .is_none_or(|m| w.module_env_moved(&m.env, &m.aliases))
            });
            if revive {
                module_routed.extend(hooks);
            }
        }

        let package_actions = profile_packages
            .into_iter()
            .map(Action::Package)
            .collect::<Vec<_>>();

        observe(PhaseName::System);
        let system_actions = self.plan_system(&resolved.merged, &module_actions)?;
        observe(PhaseName::Secrets);
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
                .system_configurators()
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
                            template: secret.template.clone(),
                            origin: LOCAL_LAYER.to_string(),
                        }));
                    }

                    // Env injection action when envs are specified
                    if has_envs {
                        actions.push(Action::Secret(SecretAction::ResolveEnv {
                            provider: provider_name.to_string(),
                            reference: reference.to_string(),
                            envs: secret.envs.clone().unwrap_or_default(),
                            template: secret.template.clone(),
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

    /// Returns the module-routed actions plus the lifecycle hooks whose "does
    /// this module have work" question only the env plan can answer: a module
    /// whose packages and files all converged but which declares env/aliases
    /// has work exactly when its OWN env/alias contribution moves, and that
    /// surface is planned later, as a unit, by `plan_env`. The deferred hooks
    /// are grouped under the declaring module's name so the caller can revive
    /// each module's hooks per [`super::env::PrimaryEnvWrite::module_env_moved`]
    /// instead of reviving all of them whenever any env action was planned.
    pub(super) fn plan_modules(
        &self,
        modules: &[ResolvedModule],
        profile_name: &str,
        context: ReconcileContext,
    ) -> (RoutedActions, Vec<(String, RoutedActions)>) {
        let mut actions = Vec::new();
        let mut env_gated_hooks = Vec::new();

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

            // The module's own body, buffered so the lifecycle hooks can be
            // gated on whether any of it survived elision: hooks exist to
            // bracket work, and a converged module performs none.
            let mut body: Vec<(PhaseName, Action)> = Vec::new();
            let mut work = 0usize;

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
            // The resolved manager travels WITH its class: the elision below
            // needs the very `&dyn PackageManager` this lookup already found,
            // and re-`find`ing it there walked the registry a second time per
            // manager per module.
            let mut manager_order: Vec<ManagerOrder<'_>> = by_manager
                .keys()
                .map(|mgr| {
                    let found = self
                        .registry
                        .package_managers()
                        .iter()
                        .find(|m| m.name() == mgr.as_str());
                    let class = match found {
                        Some(m) if m.is_available() => 0, // available (native) first
                        // Bootstrappable second — a manager with a plan to provision it.
                        Some(m) if m.can_bootstrap() => 1,
                        _ => 2, // unknown last
                    };
                    // A source-registering manager's installs go ahead of
                    // its OWN family: a formula may only exist in the tap
                    // being added by this very run. The family bound is
                    // the point — a tap has nothing to say about another
                    // family's packages, so it does not jump those.
                    let sources_last = !found.is_some_and(|m| m.registers_family_sources());
                    (
                        class,
                        crate::manager_family(mgr),
                        sources_last,
                        mgr,
                        found.map(|m| m.as_ref()),
                    )
                })
                .collect();
            // `(class, family, sources, name)`, not `class` alone: a key that
            // ties on `class` would otherwise fall back to
            // `HashMap::keys()`'s arbitrary, per-process order. Two managers
            // in the same class (`cargo` and `npm`, both `0`) would then emit
            // their `InstallPackages` actions in a different relative order on
            // every run — the plan tree's bullet order, the `-o json` payload
            // order, the journal `action_index`, and the phase's execution
            // offer order all read from this `Vec`.
            manager_order
                .sort_by(|a, b| (a.0, a.1, a.2, a.3.as_str()).cmp(&(b.0, b.1, b.2, b.3.as_str())));

            for (class, _, _, mgr_name, mgr) in manager_order {
                let mut resolved = by_manager[mgr_name].clone();
                // Only an AVAILABLE manager (class 0) can be asked what it
                // holds; a bootstrappable or unknown one is planned in full,
                // exactly as the profile-level planner does for the same case.
                if class == 0
                    && let Some(mgr) = mgr
                {
                    self.retain_uninstalled(mgr, &mut resolved);
                }
                // Nothing left to install is nothing to plan: the module has
                // already converged under this manager, and emitting the action
                // anyway is what made every plan re-list the whole declared set
                // and every apply re-shell to the manager for it.
                if resolved.is_empty() {
                    continue;
                }
                work += 1;
                body.push(routed(
                    module,
                    ModuleActionKind::InstallPackages { resolved },
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
                            body.push(routed_to(
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
                                    body.push(routed_to(
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
                                    body.push(routed_to(
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
                    // Diff each declared file against its deployed target and
                    // plan only the entries a run would really write — the
                    // file counterpart of `retain_uninstalled` above. A
                    // converged entry is elided; all-converged elides the
                    // action entirely, which is what lets a settled machine
                    // read "nothing to do" instead of re-deploying (and
                    // re-hooking) the same six files on every run.
                    //
                    // Elision requires the manifest to already OWN the row:
                    // `cfgd status <module>` and `profile remove-module` read
                    // `module_file_manifest`, and its only writer is the
                    // DeployFiles apply arm — a target that happened to match
                    // its source before cfgd ever deployed it would never be
                    // recorded, so it would be invisible to both. A state-read failure elides
                    // nothing: planning (and re-recording) a converged file is
                    // harmless, an unrecorded owned file is not.
                    let recorded: std::collections::HashSet<String> = self
                        .state
                        .module_deployed_files(&module.name)
                        .map(|rows| rows.into_iter().map(|r| r.file_path).collect())
                        .unwrap_or_default();
                    // A `Patch` entry's convergence is a function of the live
                    // target plus the patch script's environment, which only a
                    // caller holding the config dir can complete. Built once
                    // per module, and only when a Patch entry exists to ask
                    // about — the binding snapshots the script env eagerly.
                    let binding = self.config_dir.as_deref().and_then(|dir| {
                        module
                            .files
                            .iter()
                            .any(|f| {
                                f.strategy.unwrap_or(self.registry.default_file_strategy)
                                    == crate::config::FileStrategy::Patch
                            })
                            .then(|| {
                                super::patch::PatchBinding::module(
                                    dir,
                                    profile_name,
                                    context,
                                    module,
                                )
                            })
                    });
                    let declared_total = module.files.len();
                    let files: Vec<crate::modules::ResolvedFile> = module
                        .files
                        .iter()
                        .filter(|file| {
                            let target = expand_tilde(&file.target);
                            let strategy =
                                file.strategy.unwrap_or(self.registry.default_file_strategy);
                            !(recorded.contains(&crate::to_posix_fs_key(&target))
                                && super::modules::planned_file_converged(
                                    file,
                                    &target,
                                    strategy,
                                    binding.as_ref(),
                                ))
                        })
                        .cloned()
                        .collect();
                    if !files.is_empty() {
                        work += 1;
                        body.push(routed(
                            module,
                            ModuleActionKind::DeployFiles {
                                files,
                                declared_total,
                            },
                        ));
                    }
                }
            }

            // Lifecycle hooks bracket the module's work, so they are planned
            // only when some of it survived elision. A module that DECLARES
            // packages or files and planned none of them is converged, and a
            // converged module runs no hooks and renders nothing — the same
            // rule the render states as "nothing to do". A module declaring
            // no packages and no files keeps its scripts: they are the whole
            // of its content, and there is nothing for it to converge against.
            //
            // Declared env/aliases are a work surface too, but one this
            // planner cannot diff: the env surface is generated as a unit by
            // `plan_env`, later. A module that declares packages or files AND
            // env, all of whose diffable work converged, routes its hooks
            // through `env_gated_hooks` — revived when the module's OWN
            // env/alias entries move between the deployed primary env file
            // and the planned content, dropped when they hold (another
            // layer's change to the shared surface is that layer's work).
            // An unanswerable comparison fails OPEN and revives.
            //
            // "Declares no work" deliberately reads packages/files ALONE, not
            // env: a module with scripts and env but no packages or files
            // keeps its hooks unconditionally, because the scripts are the
            // whole of its content — env-gating it would make the module
            // inert on every run whose env surface happened to converge.
            let declares_work = !module.packages.is_empty() || !module.files.is_empty();
            let declares_env = !module.env.is_empty() || !module.aliases.is_empty();
            let hooks_now = work > 0 || !declares_work;
            let hooks_env_gated = !hooks_now && declares_env;
            let route_hooks = |scripts: &[crate::config::ScriptEntry], phase: &ScriptPhase| {
                scripts
                    .iter()
                    .map(|script| {
                        routed(
                            module,
                            ModuleActionKind::RunScript {
                                script: script.clone(),
                                phase: phase.clone(),
                            },
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let mut gated: RoutedActions = Vec::new();
            if hooks_now {
                actions.extend(route_hooks(pre_scripts, &pre_phase));
            } else if hooks_env_gated {
                gated.extend(route_hooks(pre_scripts, &pre_phase));
            }
            actions.append(&mut body);
            if hooks_now {
                actions.extend(route_hooks(post_scripts, &post_phase));
            } else if hooks_env_gated {
                gated.extend(route_hooks(post_scripts, &post_phase));
            }
            if !gated.is_empty() {
                env_gated_hooks.push((module.name.clone(), gated));
            }
        }

        (actions, env_gated_hooks)
    }

    /// Drop the entries `mgr` already reports installed, leaving the ones a
    /// run would really install.
    ///
    /// A no-op when the caller wired no installed-state reader, and fail-OPEN on
    /// a manager that cannot be queried: a machine cfgd could not observe is one
    /// whose packages must still be planned, never one whose work is silently
    /// dropped. Entries are compared through
    /// [`crate::providers::PackageManager::package_identity`] — the identity
    /// space the profile planner diffs in, and the space
    /// [`crate::providers::InstalledPackages`] folds its listing into — so a
    /// case-insensitive manager or a `go`-style install-vs-listed name split
    /// agrees across both halves of the run.
    ///
    /// A declared `minVersion` is a SECOND question the name comparison cannot
    /// answer. Resolution checked the floor against what the manager currently
    /// OFFERS, never against what the machine holds, so a host carrying an
    /// older copy is installed-by-name and short of the floor at once. Such an
    /// entry is KEPT, and so is one whose installed version cannot be read —
    /// the same fail-open the unreadable-manager arm takes.
    fn retain_uninstalled(
        &self,
        mgr: &dyn PackageManager,
        packages: &mut Vec<crate::modules::ResolvedPackage>,
    ) {
        let Some(cx) = self.installed else {
            return;
        };
        match cx.installed_for(mgr) {
            Ok(installed) => {
                packages.retain(|pkg| Self::package_survives_elision(mgr, &installed, pkg));
            }
            Err(e) => {
                tracing::warn!(
                    manager = mgr.name(),
                    error = %e,
                    "cannot read installed packages; planning the module's declared set in full"
                );
            }
        }
    }

    /// Whether `pkg` survives the installed-state elision: not installed under
    /// `mgr`'s identity fold, or installed short of its declared `minVersion`
    /// floor. The ONE predicate [`Self::retain_uninstalled`] and
    /// [`Self::fill_planned_versions`] both read, so a package can never be
    /// planned-but-unpriced or priced-but-elided.
    fn package_survives_elision(
        mgr: &dyn PackageManager,
        installed: &crate::providers::InstalledPackages,
        pkg: &crate::modules::ResolvedPackage,
    ) -> bool {
        let identity = mgr.package_identity(&pkg.resolved_name);
        if !installed.contains(&identity) {
            return true;
        }
        let Some(min) = pkg.min_version.as_deref() else {
            return false;
        };
        installed
            .listed()
            .iter()
            .find(|p| mgr.listed_identity(&p.name) == identity)
            .is_none_or(|p| !mgr.version_meets_minimum(&p.version, min))
    }

    /// Price the packages this run's plan will actually surface, and no others.
    ///
    /// The survivor-gated form of [`crate::modules::fill_available_versions`],
    /// and what every planning path calls in its place: a package the manager
    /// already reports installed is elided by [`Self::plan_modules`], so it
    /// renders no description, persists no string, and its version query buys
    /// nothing — pricing a converged machine's whole declared set per
    /// invocation was a multi-second silent wait before every plan and apply.
    /// A package that DOES survive is priced through the same memoized query,
    /// so a planned `InstallPackages` renders and persists strings
    /// byte-identical to the unconditional fill's, and the module's recorded
    /// packages hash is unchanged for planned work.
    ///
    /// Elision is judged through [`Self::package_survives_elision`] against
    /// the same enumeration [`Self::retain_uninstalled`] reads. On the success
    /// path both cost one memoized listing per manager; this pass additionally
    /// holds a FAILED read for its whole run, since the memo caches successes
    /// only. It fails OPEN everywhere the planner does: no
    /// installed-state reader wired, or a manager that cannot be queried,
    /// prices the declared set in full exactly as the planner plans it in
    /// full. `cfgd doctor` and `cfgd module show` render a version per
    /// DECLARED package without planning and keep the unconditional fill.
    pub fn fill_planned_versions(
        &self,
        modules: &mut [ResolvedModule],
        managers: &HashMap<String, &dyn PackageManager>,
    ) {
        // One installed listing per manager, a FAILED read held too:
        // `installed_for` memoizes successes only, so retrying per package
        // turns one unreadable manager into one failing subprocess per
        // declared package.
        let mut listings: HashMap<String, Option<Arc<crate::providers::InstalledPackages>>> =
            HashMap::new();
        for module in modules.iter_mut() {
            for pkg in module.packages.iter_mut() {
                let Some(mgr) = crate::modules::priceable_manager(pkg, managers) else {
                    continue;
                };
                if let Some(cx) = self.installed {
                    let listing = listings
                        .entry(mgr.name().to_string())
                        .or_insert_with(|| cx.installed_for(mgr).ok());
                    if let Some(installed) = listing
                        && !Self::package_survives_elision(mgr, installed, pkg)
                    {
                        continue;
                    }
                }
                crate::modules::price_package(pkg, mgr);
            }
        }
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

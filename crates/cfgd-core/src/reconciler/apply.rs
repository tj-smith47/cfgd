use crate::PathDisplayExt;
use crate::config::{ResolvedProfile, ScriptShell};
use crate::errors::{ConfigError, Result};
use crate::modules::ResolvedModule;
use crate::output::{Printer, Role};
use crate::state::ApplyStatus;

use super::format::{
    format_action_description, parse_package_description, parse_resource_from_description,
};
use super::restore::action_target_path;
use super::scripts::{
    MODULE_SCRIPT_TIMEOUT, build_module_script_env, build_script_env, effective_continue_on_error,
    execute_script, script_default_workdir,
};
use super::types::{
    Action, ActionResult, ApplyResult, ModuleAction, ModuleActionKind, PhaseName, Plan,
    ReconcileContext, ScriptAction, ScriptPhase,
};

fn hash_sorted_parts(mut parts: Vec<String>) -> String {
    parts.sort();
    crate::sha256_hex(parts.join("|").as_bytes())
}

/// Whether `action` (residing in `phase_name`) should execute under `filter`.
///
/// `--phase post-scripts` / `--phase pre-scripts` are intentionally inclusive
/// across plan phases: module-level lifecycle scripts are emitted into
/// `PhaseName::Modules` as `Action::Module(RunScript { phase: PostApply | ... })`,
/// not into `PhaseName::PostScripts`. A naive `phase.name == filter` test
/// therefore drops every per-module post/pre script and makes
/// `cfgd apply --module nvim --phase post-scripts` a no-op even when failed
/// module scripts need re-attempting. Other filters keep strict
/// phase-equality semantics.
pub fn action_matches_phase_filter(
    phase_name: &PhaseName,
    action: &Action,
    filter: &PhaseName,
) -> bool {
    if phase_name == filter {
        return true;
    }
    match filter {
        PhaseName::PostScripts => is_post_apply_script(action),
        PhaseName::PreScripts => is_pre_apply_script(action),
        _ => false,
    }
}

fn is_post_apply_script(action: &Action) -> bool {
    matches!(
        action,
        Action::Script(ScriptAction::Run {
            phase: ScriptPhase::PostApply | ScriptPhase::PostReconcile,
            ..
        }) | Action::Module(ModuleAction {
            kind: ModuleActionKind::RunScript {
                phase: ScriptPhase::PostApply | ScriptPhase::PostReconcile,
                ..
            },
            ..
        })
    )
}

fn is_pre_apply_script(action: &Action) -> bool {
    matches!(
        action,
        Action::Script(ScriptAction::Run {
            phase: ScriptPhase::PreApply | ScriptPhase::PreReconcile,
            ..
        }) | Action::Module(ModuleAction {
            kind: ModuleActionKind::RunScript {
                phase: ScriptPhase::PreApply | ScriptPhase::PreReconcile,
                ..
            },
            ..
        })
    )
}

impl<'a> super::Reconciler<'a> {
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

            let packages_hash = hash_sorted_parts(
                module
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
                    .collect(),
            );

            let files_hash = hash_sorted_parts(
                module
                    .files
                    .iter()
                    .map(|f| format!("{}:{}", f.source.display(), f.target.display()))
                    .collect(),
            );

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
    ///
    /// `shell_override` forces every inline lifecycle script to run under the
    /// supplied interpreter, ignoring entries' `shell:` field. Set by
    /// `cfgd apply --shell <shell>` for debugging. File/shebang scripts are
    /// unaffected.
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
        shell_override: Option<ScriptShell>,
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
            // Pre-filter to the actions in this phase that survive `phase_filter`.
            // Restricting the indexed loop below to the surviving subset keeps
            // the `[i/total]` status headers honest about what actually runs.
            let filtered: Vec<&Action> = if let Some(filter) = phase_filter {
                phase
                    .actions
                    .iter()
                    .filter(|a| action_matches_phase_filter(&phase.name, a, filter))
                    .collect()
            } else {
                phase.actions.iter().collect()
            };

            if filtered.is_empty() {
                continue;
            }

            let total = filtered.len();
            for (action_idx, action) in filtered.iter().copied().enumerate() {
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
                    tracing::warn!("failed to store file backup for {}: {}", path.posix(), e);
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
                    shell_override,
                );

                let (desc, success, action_changed, error, should_abort) = match result {
                    Ok((desc, action_changed, script_output)) => {
                        if let Some(jid) = journal_id
                            && let Err(e) =
                                self.state
                                    .journal_complete(jid, None, script_output.as_deref())
                        {
                            tracing::warn!("failed to record journal completion: {e}");
                        }
                        (desc, true, action_changed, None, false)
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
                            printer.status_simple(
                                Role::Warn,
                                format!(
                                    "[{}/{}] Script failed (continueOnError): {} — {}",
                                    action_idx + 1,
                                    total,
                                    desc,
                                    e
                                ),
                            );
                        } else {
                            printer.status_simple(
                                Role::Fail,
                                format!("[{}/{}] Failed: {} — {}", action_idx + 1, total, desc, e),
                            );
                        }
                        if let Some(jid) = journal_id
                            && let Err(je) = self.state.journal_fail(jid, &e.to_string())
                        {
                            tracing::warn!("failed to record journal failure: {je}");
                        }
                        (desc, false, false, Some(e.to_string()), !continue_on_err)
                    }
                };

                let changed = success && action_changed;
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
                resolved.merged.env_scope,
                module_actions,
                &secret_env_collector,
            );
            for env_action in &env_actions {
                if let Action::Env(ea) = env_action {
                    match Self::apply_env_action(ea, printer) {
                        Ok(desc) => {
                            // The `:skipped` substring convention is confined to
                            // env-action descriptions, where it is the actual
                            // data shape produced by `apply_env_action` (a no-op
                            // write marks itself skipped). This path calls
                            // `apply_env_action` directly, so it reads the same
                            // shape — it is not a general description sniff.
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
                            printer.status_simple(
                                Role::Fail,
                                format!("Failed to write secret env vars: {}", e),
                            );
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
            let working = script_default_workdir(config_dir);
            for entry in &resolved.merged.scripts.on_change {
                match execute_script(
                    entry,
                    config_dir,
                    &working,
                    &env_vars,
                    crate::PROFILE_SCRIPT_TIMEOUT,
                    printer,
                    shell_override,
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
                let env_vars = build_module_script_env(
                    config_dir,
                    profile_name,
                    context,
                    &ScriptPhase::OnChange,
                    Some(&module.name),
                    Some(&module.dir),
                    &module.env,
                );
                let working = script_default_workdir(config_dir);
                for entry in &module.on_change_scripts {
                    match execute_script(
                        entry,
                        &module.dir,
                        &working,
                        &env_vars,
                        MODULE_SCRIPT_TIMEOUT,
                        printer,
                        shell_override,
                    ) {
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
            if !result.success {
                continue;
            }

            // Packages track per-resolved-name under "package"/"<mgr>/<pkg>" so the
            // set is usable for declarative prune. The generic parser is lossy for
            // multi-package installs and embeds the verb, so handle them explicitly:
            // install adds a tracking row per package, uninstall deletes it.
            if let Some((manager, verb, packages)) = parse_package_description(&result.description)
            {
                for pkg in &packages {
                    let rid = format!("{manager}/{pkg}");
                    match verb.as_str() {
                        "install" => {
                            // Persist the scripted uninstall command (Some only for
                            // custom managers) so the package can still be pruned
                            // after its manager block leaves the config.
                            let uninstall_cmd = self
                                .registry
                                .package_managers
                                .iter()
                                .find(|m| m.name() == manager)
                                .and_then(|m| m.persisted_uninstall());
                            self.state.upsert_package_resource(
                                &rid,
                                "local",
                                Some(apply_id),
                                uninstall_cmd.as_deref(),
                            )?;
                            self.state.resolve_drift(apply_id, "package", &rid)?;
                        }
                        "uninstall" => {
                            self.state.remove_managed_resource("package", &rid)?;
                            self.state.resolve_drift(apply_id, "package", &rid)?;
                        }
                        _ => {}
                    }
                }
                continue;
            }

            let (rtype, rid) = parse_resource_from_description(&result.description);
            self.state
                .upsert_managed_resource(&rtype, &rid, "local", None, Some(apply_id))?;
            self.state.resolve_drift(apply_id, &rtype, &rid)?;
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
        shell_override: Option<ScriptShell>,
    ) -> Result<(String, bool, Option<String>)> {
        match action {
            Action::System(sys) => self
                .apply_system_action(sys, &resolved.merged, module_actions, printer)
                .map(|d| (d, true, None)),
            Action::Package(pkg) => self
                .apply_package_action(pkg, printer)
                .map(|d| (d, true, None)),
            Action::File(file) => self
                .apply_file_action(file, &resolved.merged, config_dir, printer)
                .map(|d| (d, true, None)),
            Action::Secret(secret) => self
                .apply_secret_action(secret, config_dir, printer, secret_env_collector)
                .map(|d| (d, true, None)),
            Action::Script(script) => self.apply_script_action(
                script,
                resolved,
                config_dir,
                printer,
                context,
                shell_override,
            ),
            Action::Module(module) => self
                .apply_module_action(
                    module,
                    config_dir,
                    printer,
                    apply_id,
                    context,
                    resolved,
                    module_actions,
                    shell_override,
                )
                .map(|(d, c)| (d, c, None)),
            // The `:skipped` substring convention is confined to env-action
            // descriptions, where it is the actual data shape produced by
            // `apply_env_action` (no-op writes mark themselves skipped).
            Action::Env(env) => Self::apply_env_action(env, printer).map(|d| {
                let changed = !d.contains(":skipped");
                (d, changed, None)
            }),
        }
    }
}

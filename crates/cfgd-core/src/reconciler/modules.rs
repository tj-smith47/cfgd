use crate::PathDisplayExt;
use crate::config::{ResolvedProfile, ScriptEntry, ScriptShell};
use crate::errors::{ConfigError, Result};
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::output::Printer;

use super::scripts::{
    MODULE_SCRIPT_TIMEOUT, ScriptEnvContext, ScriptReport, build_module_script_env, execute_script,
    script_default_workdir,
};
use super::types::{ModuleAction, ModuleActionKind, ReconcileContext, ScriptPhase};

impl<'a> super::Reconciler<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_module_action(
        &self,
        action: &ModuleAction,
        config_dir: &std::path::Path,
        printer: &Printer,
        apply_id: i64,
        context: ReconcileContext,
        resolved: &ResolvedProfile,
        module_actions: &[ResolvedModule],
        shell_override: Option<ScriptShell>,
        abort: &crate::AbortFlag,
        notes: &crate::providers::NoteSink,
    ) -> Result<(String, bool)> {
        // Find the resolved module to obtain its dir and declared env vars.
        let resolved_mod = module_actions.iter().find(|m| m.name == action.module_name);
        let module_dir = resolved_mod.map(|m| m.dir.clone());
        let module_env = resolved_mod.map(|m| m.env.as_slice()).unwrap_or(&[]);

        match &action.kind {
            ModuleActionKind::InstallPackages { resolved: pkgs } => {
                // Packages in each InstallPackages action are already grouped by
                // manager in plan_modules(), so just collect names and install.
                let pkg_names: Vec<String> = pkgs.iter().map(|p| p.resolved_name.clone()).collect();

                // A `prefer: [script]` install has no queryable installed-state, so
                // idempotency is the script's own responsibility. When all of a
                // package's guards (creates/onlyIf/unless) say "skip", the install
                // is a clean no-op — `changed` stays false so apply reports it as
                // unchanged rather than a re-run. Without guards the script runs
                // every apply (changed=true), which is the author's responsibility.
                let mut script_changed = false;
                // A manager-backed install always counts as changed (the package
                // managers own their own idempotency at the package level, but the
                // action having reached the install call means work was attempted).
                let mut manager_changed = false;

                if let Some(first) = pkgs.first() {
                    if first.manager == "script" {
                        // Script-based install: run each package's script via execute_script
                        let path_dirs = super::all_recorded_path_dirs(self.state);
                        for pkg in pkgs {
                            if let Some(ref script_content) = pkg.script {
                                let profile_name = resolved
                                    .layers
                                    .last()
                                    .map(|l| l.profile_name.as_str())
                                    .unwrap_or("unknown");
                                let env_vars = build_module_script_env(
                                    &ScriptEnvContext {
                                        config_dir,
                                        profile_name,
                                        context,
                                        phase: &ScriptPhase::PostApply,
                                        module_name: Some(&action.module_name),
                                        module_dir: module_dir.as_deref(),
                                        path_dirs: &path_dirs,
                                    },
                                    module_env,
                                );
                                // Build a Full entry so the package's idempotency
                                // guards run through the same guard-evaluation path
                                // as lifecycle scripts (creates → onlyIf → unless);
                                // a guard that says "skip" yields changed=false.
                                let script_entry = ScriptEntry::Full {
                                    run: script_content.clone(),
                                    timeout: None,
                                    idle_timeout: None,
                                    continue_on_error: None,
                                    shell: ScriptShell::Auto,
                                    only_if: pkg.only_if.clone(),
                                    unless: pkg.unless.clone(),
                                    creates: pkg.creates.clone(),
                                    interactive: false,
                                    workdir: None,
                                };
                                let source = module_dir.as_deref().unwrap_or(config_dir);
                                let working = script_default_workdir(config_dir);
                                let (_label, changed, _captured) = execute_script(
                                    &script_entry,
                                    source,
                                    &working,
                                    &env_vars,
                                    MODULE_SCRIPT_TIMEOUT,
                                    printer,
                                    shell_override,
                                    Some(abort),
                                    ScriptReport::default(),
                                )
                                .map_err(|_| {
                                    crate::errors::CfgdError::Config(ConfigError::Invalid {
                                        message: format!(
                                            "module {} install script for '{}' failed",
                                            action.module_name, pkg.canonical_name
                                        ),
                                    })
                                })?;
                                script_changed |= changed;
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
                            let cx = crate::providers::PackageContext::with_notes(
                                printer, self.state, notes,
                            )
                            .caller_owns_status();

                            // Bootstrap if needed. The manager's PATH directories
                            // are recorded, never appended to `~/.cfgd.env` here:
                            // the generated env file has exactly one writer, and
                            // an out-of-band append would be erased by the next
                            // wholesale rewrite of that file.
                            if !pm.is_available() && pm.can_bootstrap() {
                                pm.bootstrap(&cx)?;
                                self.record_bootstrap_path_dirs(pm.as_ref(), printer);
                            }

                            // Update package index before installing
                            if pm.is_available() {
                                pm.update(&cx)?;
                            }

                            pm.install(&pkg_names, &cx)?;
                            manager_changed = true;
                        }
                    }
                }

                Ok((
                    format!(
                        "module:{}:packages:{}",
                        action.module_name,
                        pkg_names.join(",")
                    ),
                    script_changed || manager_changed,
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

                    // Parse the declared mode BEFORE touching the filesystem so an
                    // invalid value fails fast and never leaves a half-deployed file.
                    let mode = match file.permissions {
                        Some(ref perm_str) => Some(crate::parse_octal_mode(perm_str)?),
                        None => None,
                    };

                    // `Patch` rewrites the target's own content, so the merge is
                    // computed against the live file here rather than against
                    // what planning saw. A failure aborts with the target still
                    // intact.
                    let patched = if strategy == crate::config::FileStrategy::Patch {
                        let spec = file.patch.as_ref().ok_or_else(|| {
                            crate::errors::FileError::PatchBlockMissing {
                                path: target.clone(),
                            }
                        })?;
                        let binding = match resolved_mod {
                            Some(m) => super::patch::PatchBinding::module(
                                config_dir,
                                resolved.profile_name(),
                                context,
                                m,
                            ),
                            None => super::patch::PatchBinding::profile(
                                config_dir,
                                resolved.profile_name(),
                                context,
                            ),
                        };
                        Some(
                            super::patch::evaluate_patch(spec, &target, &binding.context())?
                                .patched,
                        )
                    } else {
                        None
                    };

                    // Backup existing target before overwriting
                    if let Ok(Some(file_state)) = crate::capture_file_state(&target)
                        && let Err(e) = self.state.store_file_backup(
                            apply_id,
                            &crate::to_posix_fs_key(&target),
                            &file_state,
                        )
                    {
                        tracing::warn!("failed to backup module file {}: {}", target.posix(), e);
                    }

                    // Remove existing target before deploying. A `Patch` file is
                    // exempt: the removal clears a stale link ahead of
                    // `create_symlink`/`hard_link`, while `Patch` writes through
                    // `atomic_write_merged`, which replaces by rename and so
                    // keeps the target's mode and follows its symlink.
                    if patched.is_none() && target.symlink_metadata().is_ok() {
                        if target.is_dir() && !target.is_symlink() {
                            std::fs::remove_dir_all(&target)?;
                        } else {
                            std::fs::remove_file(&target)?;
                        }
                    }

                    if let Some(content) = patched {
                        crate::atomic_write_merged(&target, &content)?;
                    } else if file.source.is_dir() {
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
                            | crate::config::FileStrategy::Template
                            // Unreachable: a `Patch` file took the branch above.
                            | crate::config::FileStrategy::Patch => {
                                let content = std::fs::read(&file.source)?;
                                crate::atomic_write(&target, &content)?;
                            }
                        }
                    }

                    // Apply declared permissions after deployment (no-op on Windows).
                    if let Some(mode) = mode {
                        crate::set_file_permissions(&target, mode)?;
                    }

                    // Record in module file manifest
                    let hash = if target.exists() && !target.is_symlink() {
                        match std::fs::read(&target) {
                            Ok(bytes) => crate::sha256_hex(&bytes),
                            Err(e) => {
                                tracing::warn!("cannot read {} for hashing: {e}", target.posix());
                                String::new()
                            }
                        }
                    } else {
                        String::new()
                    };
                    // Persisted key AND a path a later apply reopens, so it folds
                    // with `to_posix_fs_key`: the UNIQUE(module_name, file_path)
                    // row a Windows apply writes is the one every later apply
                    // derives, without renaming a POSIX target whose filename
                    // legitimately contains a backslash.
                    self.state.upsert_module_file(
                        &action.module_name,
                        &crate::to_posix_fs_key(&target),
                        &hash,
                        &format!("{:?}", strategy),
                        apply_id,
                    )?;
                }

                Ok((
                    format!("module:{}:files:{}", action.module_name, files.len()),
                    true,
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
                let env_vars = build_module_script_env(
                    &ScriptEnvContext {
                        config_dir,
                        profile_name,
                        context,
                        phase: script_phase,
                        module_name: Some(&action.module_name),
                        module_dir: module_dir.as_deref(),
                        path_dirs: &super::all_recorded_path_dirs(self.state),
                    },
                    module_env,
                );

                let source = module_dir.as_deref().unwrap_or(config_dir);
                let working = script_default_workdir(config_dir);
                let (_label, changed, _captured) = execute_script(
                    script,
                    source,
                    &working,
                    &env_vars,
                    MODULE_SCRIPT_TIMEOUT,
                    printer,
                    shell_override,
                    Some(abort),
                    ScriptReport {
                        marker: Some(script_phase.display_name()),
                        non_fatal: false,
                    },
                )?;

                Ok((format!("module:{}:script", action.module_name), changed))
            }
            ModuleActionKind::Skip { reason: _ } => {
                // A planned skip did nothing this run, so it must not count as
                // changed and must not fire the module's onChange hooks.
                Ok((format!("module:{}:skip", action.module_name), false))
            }
        }
    }
}

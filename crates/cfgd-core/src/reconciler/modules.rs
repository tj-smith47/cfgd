use crate::PathDisplayExt;
use crate::config::{ResolvedProfile, ScriptShell};
use crate::errors::Result;
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::output::Printer;

use super::scripts::{
    MODULE_SCRIPT_TIMEOUT, ScriptEnvContext, ScriptReport, build_module_script_env, execute_script,
    script_default_workdir,
};
use super::types::{ModuleAction, ModuleActionKind, ReconcileContext};

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
                let exec =
                    super::packages::PackageExec::new(self.registry, self.state, printer, notes);
                let outcome = exec.install_module_packages(
                    action,
                    pkgs,
                    &super::packages::ModuleInstallContext {
                        config_dir,
                        resolved,
                        module_actions,
                        context,
                        shell_override,
                        abort,
                        path_dirs: &super::all_recorded_path_dirs(self.state),
                    },
                );
                self.persist_bootstraps(exec.take_bootstrapped());
                outcome
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
                // The action's ONE display subject, from the same derivation
                // the preview bullet and the phase's alignment column use.
                let subject = super::format::module_script_subject(
                    script.run_str(),
                    script_phase,
                    action.origin.as_deref(),
                );
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
                        subject: super::scripts::ScriptSubject::Planned(&subject),
                        non_fatal: false,
                        ..ScriptReport::default()
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

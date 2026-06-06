use crate::PathDisplayExt;
use crate::config::{ResolvedProfile, ScriptEntry, ScriptShell};
use crate::errors::{ConfigError, Result};
use crate::expand_tilde;
use crate::modules::ResolvedModule;
use crate::output::{Printer, Role};

use super::scripts::{
    MODULE_SCRIPT_TIMEOUT, build_module_script_env, execute_script, script_default_workdir,
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
                                let env_vars = build_module_script_env(
                                    config_dir,
                                    profile_name,
                                    context,
                                    &ScriptPhase::PostApply,
                                    Some(&action.module_name),
                                    module_dir.as_deref(),
                                    module_env,
                                );
                                let script_entry = ScriptEntry::Simple(script_content.clone());
                                let source = module_dir.as_deref().unwrap_or(config_dir);
                                let working = script_default_workdir(config_dir);
                                execute_script(
                                    &script_entry,
                                    source,
                                    &working,
                                    &env_vars,
                                    MODULE_SCRIPT_TIMEOUT,
                                    printer,
                                    shell_override,
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
                                            tracing::warn!("cannot read {}: {e}", env_path.posix());
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

                Ok((
                    format!(
                        "module:{}:packages:{}",
                        action.module_name,
                        pkg_names.join(",")
                    ),
                    true,
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

                    // Backup existing target before overwriting
                    if let Ok(Some(file_state)) = crate::capture_file_state(&target)
                        && let Err(e) = self.state.store_file_backup(
                            apply_id,
                            &target.display().to_string(),
                            &file_state,
                        )
                    {
                        tracing::warn!("failed to backup module file {}: {}", target.posix(), e);
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
                    self.state.upsert_module_file(
                        &action.module_name,
                        &target.display().to_string(),
                        &hash,
                        &format!("{:?}", strategy),
                        apply_id,
                    )?;
                }

                printer.status_simple(
                    Role::Ok,
                    format!(
                        "Module {}: deployed {} file(s)",
                        action.module_name,
                        files.len()
                    ),
                );

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
                    config_dir,
                    profile_name,
                    context,
                    script_phase,
                    Some(&action.module_name),
                    module_dir.as_deref(),
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
                )?;

                Ok((format!("module:{}:script", action.module_name), changed))
            }
            ModuleActionKind::Skip { reason } => {
                printer.status_simple(
                    Role::Warn,
                    format!("Module {}: skipped — {}", action.module_name, reason),
                );
                // A planned skip did nothing this run, so it must not count as
                // changed and must not fire the module's onChange hooks.
                Ok((format!("module:{}:skip", action.module_name), false))
            }
        }
    }
}

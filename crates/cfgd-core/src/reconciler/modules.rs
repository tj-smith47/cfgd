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

/// Whether a module file's deployment would leave `target` exactly as it
/// already stands — the PLAN-time question, answered for every strategy the
/// apply arm below can act on.
///
/// [`converged_content_file`] is its whole-content core; this adds the arms
/// planning needs that apply's write-skip cannot answer: a `Symlink` entry is
/// converged when the target is a link to this very source, a `Hardlink` when
/// the two paths share an inode, a directory source (deployed as
/// remove-then-clone) when the trees are byte-identical BOTH ways — extras
/// included, because a fresh deploy clears them — and a `Patch` entry when the
/// merge `binding` lets this evaluate already reads back what the target holds
/// (the same `is_up_to_date` question diff and verify ask). Every unanswerable
/// case fails OPEN (a `Patch` with no binding or a failing merge; an unreadable
/// path; an invalid declared mode): work is planned rather than silently
/// dropped, and the apply arm decides.
pub(super) fn planned_file_converged(
    file: &crate::modules::ResolvedFile,
    target: &std::path::Path,
    strategy: crate::config::FileStrategy,
    binding: Option<&super::patch::PatchBinding>,
) -> bool {
    use crate::config::FileStrategy;
    let mode = match file.permissions {
        Some(ref perm_str) => match crate::parse_octal_mode(perm_str) {
            Ok(m) => Some(m),
            Err(_) => return false,
        },
        None => None,
    };
    if strategy == FileStrategy::Patch {
        // The merge is a function of the live target, so it is knowable here —
        // the same evaluation diff, verify and compliance already run. Without
        // a binding (a caller with no config dir to anchor a patch script) the
        // question is unanswerable and the deploy is planned.
        let (Some(binding), Some(spec)) = (binding, file.patch.as_ref()) else {
            return false;
        };
        let Ok(outcome) = super::patch::evaluate_patch(spec, target, &binding.context()) else {
            return false;
        };
        return converged_content_file(file, target, strategy, Some(&outcome.patched), mode);
    }
    if strategy == FileStrategy::Symlink {
        let Ok(meta) = target.symlink_metadata() else {
            return false;
        };
        if !meta.file_type().is_symlink() {
            return false;
        }
        let Ok(dest) = std::fs::read_link(target) else {
            return false;
        };
        if dest != file.source {
            return false;
        }
        // A declared mode is applied THROUGH the link, so it is judged on the
        // followed metadata; a dangling link cannot answer and stays planned.
        if let Some(declared) = mode {
            let Ok(followed) = std::fs::metadata(target) else {
                return false;
            };
            if crate::file_permissions_mode_full(&followed).is_some_and(|actual| actual != declared)
            {
                return false;
            }
        }
        return true;
    }
    if file.source.is_dir() {
        // Every non-link strategy deploys a directory as remove-then-clone
        // (`Hardlink` included — see the apply arm's `is_dir` branch).
        let Ok(meta) = target.symlink_metadata() else {
            return false;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return false;
        }
        if let Some(declared) = mode
            && crate::file_permissions_mode_full(&meta).is_some_and(|actual| actual != declared)
        {
            return false;
        }
        return dir_trees_equal(&file.source, target);
    }
    if strategy == FileStrategy::Hardlink {
        let Ok(meta) = target.symlink_metadata() else {
            return false;
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            return false;
        }
        if let Some(declared) = mode
            && crate::file_permissions_mode_full(&meta).is_some_and(|actual| actual != declared)
        {
            return false;
        }
        return crate::is_same_inode(&file.source, target);
    }
    converged_content_file(file, target, strategy, None, mode)
}

/// Whether `dst` already holds exactly the tree a fresh clone of `src` would
/// produce: every non-symlink `src` entry present with identical bytes, and no
/// extras — the deploy removes the target first, so anything the clone would
/// not create is drift the deploy corrects. Symlinks under `src` are skipped
/// to match `copy_dir_recursive`, and any unreadable entry answers false.
fn dir_trees_equal(src: &std::path::Path, dst: &std::path::Path) -> bool {
    // A worklist rather than recursion: the tree's depth is module-supplied,
    // and a comparison must answer false on a pathological nesting, never
    // exhaust the thread's stack on it.
    let mut pending = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((src, dst)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&src) else {
            return false;
        };
        let mut expected: std::collections::BTreeSet<std::ffi::OsString> =
            std::collections::BTreeSet::new();
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            let Ok(ft) = entry.file_type() else {
                return false;
            };
            if ft.is_symlink() {
                continue;
            }
            let deployed = dst.join(entry.file_name());
            expected.insert(entry.file_name());
            let Ok(dmeta) = deployed.symlink_metadata() else {
                return false;
            };
            if ft.is_dir() {
                if dmeta.file_type().is_symlink() || !dmeta.is_dir() {
                    return false;
                }
                pending.push((entry.path(), deployed));
            } else {
                if !dmeta.is_file() {
                    return false;
                }
                // Length first, so a differing leaf answers without reading bytes.
                let Ok(smeta) = entry.metadata() else {
                    return false;
                };
                if smeta.len() != dmeta.len() {
                    return false;
                }
                let (Ok(want), Ok(have)) = (std::fs::read(entry.path()), std::fs::read(&deployed))
                else {
                    return false;
                };
                if want != have {
                    return false;
                }
            }
        }
        let Ok(entries) = std::fs::read_dir(&dst) else {
            return false;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            if !expected.contains(&entry.file_name()) {
                return false;
            }
        }
    }
    true
}

/// Whether a module file's deployment would write bytes the target already
/// holds, with the mode it already carries.
///
/// True only for a whole-content write whose content is knowable before it runs
/// — a `Copy`/`Template` entry, both of which deploy the source verbatim, or a
/// `Patch` entry whose merge has already been evaluated (`patched`). A link
/// entry has no content to compare, and a target that is a symlink or a
/// directory is a thing to replace rather than content to match, so both
/// answer false.
///
/// `strategy` is the RESOLVED strategy — the per-file override or the config's
/// global `fileStrategy` — never `file.strategy`. Read from the field, a module
/// file that declares no strategy of its own under a global `fileStrategy: copy`
/// answers "not a whole-content write" and is rewritten on every apply, which is
/// the exact repetition this check exists to end.
fn converged_content_file(
    file: &crate::modules::ResolvedFile,
    target: &std::path::Path,
    strategy: crate::config::FileStrategy,
    patched: Option<&str>,
    mode: Option<u32>,
) -> bool {
    let Ok(meta) = target.symlink_metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    // A declared mode the target does not already carry is itself drift, and
    // the deployment is what corrects it. Compared over the full `0o7777`,
    // because a declared mode may name a setuid/setgid/sticky bit and a masked
    // actual could then never equal it.
    if let Some(declared) = mode
        && crate::file_permissions_mode_full(&meta).is_some_and(|actual| actual != declared)
    {
        return false;
    }
    // A length mismatch answers without reading either side — this runs per
    // declared file per plan and per daemon tick, not just per apply.
    if let Some(content) = patched {
        if meta.len() != content.len() as u64 {
            return false;
        }
        return std::fs::read(target).is_ok_and(|actual| actual == content.as_bytes());
    }
    if !matches!(
        strategy,
        crate::config::FileStrategy::Copy | crate::config::FileStrategy::Template
    ) {
        return false;
    }
    match file.source.metadata() {
        Ok(smeta) if smeta.len() == meta.len() => {}
        _ => return false,
    }
    let Ok(actual) = std::fs::read(target) else {
        return false;
    };
    std::fs::read(&file.source).is_ok_and(|desired| desired == actual)
}

impl<'a> super::Reconciler<'a> {
    /// Record one deployed module file in the module file manifest.
    ///
    /// Runs for a converged file too: the manifest is the module's inventory of
    /// what it owns, and a row skipped because nothing needed writing would
    /// strand the file at module-removal time.
    fn record_module_file(
        &self,
        action: &ModuleAction,
        target: &std::path::Path,
        strategy: crate::config::FileStrategy,
        apply_id: i64,
    ) -> Result<()> {
        let hash = if target.exists() && !target.is_symlink() {
            match std::fs::read(target) {
                Ok(bytes) => crate::sha256_hex(&bytes),
                Err(e) => {
                    // tracing-ok: the manifest hash degrades to empty; no row carries the read failure
                    tracing::warn!("cannot read {} for hashing: {e}", target.posix());
                    String::new()
                }
            }
        } else {
            String::new()
        };
        // Persisted key AND a path a later apply reopens, so it folds with
        // `to_posix_fs_key`: the UNIQUE(module_name, file_path) row a Windows
        // apply writes is the one every later apply derives, without renaming a
        // POSIX target whose filename legitimately contains a backslash.
        self.state.upsert_module_file(
            &action.module_name,
            &crate::to_posix_fs_key(target),
            &hash,
            &format!("{:?}", strategy),
            apply_id,
        )?;
        Ok(())
    }

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
        sidecars: &mut Vec<super::sidecar::SidecarOutcome>,
    ) -> Result<super::apply::ActionRun> {
        // Find the resolved module to obtain its dir and declared env vars.
        let resolved_mod = module_actions.iter().find(|m| m.name == action.module_name);
        let module_dir = resolved_mod.map(|m| m.dir.clone());
        let module_env = resolved_mod.map(|m| m.env.as_slice()).unwrap_or(&[]);

        match &action.kind {
            ModuleActionKind::InstallPackages { resolved: pkgs } => {
                let unprovisioned = self.unprovisioned.borrow();
                let exec =
                    super::packages::PackageExec::new(self.registry, self.state, printer, notes)
                        .withholding_managers(&unprovisioned);
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
                        provisioned: &self.provisioned.borrow(),
                    },
                );
                self.persist_bootstraps(exec.take_bootstrapped());
                outcome
            }
            ModuleActionKind::DeployFiles {
                files,
                declared_total,
            } => {
                let mut deployed_any = false;
                for file in files {
                    let target = expand_tilde(&file.target);

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

                    // A target already holding the desired bytes needs no backup,
                    // no removal and no write: the sole effect of doing the work
                    // anyway is a redundant `file_backups` row and a run that
                    // claims to have changed a file it did not touch.
                    if converged_content_file(file, &target, strategy, patched.as_deref(), mode) {
                        self.record_module_file(action, &target, strategy, apply_id)?;
                        continue;
                    }

                    // A source that names nothing deploys nothing: leave the
                    // target alone (removing it would trade a broken
                    // declaration for lost data) and claim no change, so the
                    // entry cannot fire `onChange` on every run over a write
                    // that never happens. Not recorded either — the manifest
                    // names what cfgd wrote and `profile remove-module`
                    // deletes what the manifest names, so a row minted here
                    // would mark a file cfgd never touched for deletion.
                    if patched.is_none() && !file.source.exists() {
                        continue;
                    }
                    deployed_any = true;

                    // The parent is created only once a write is certain: a
                    // converged target needs nothing, and a broken declaration
                    // must leave no empty directories behind as the one trace
                    // of a deploy that never happened.
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }

                    // The user's own file at this target, copied aside before it
                    // is displaced — the sidecar an adoption promised, written
                    // here so the write and the line reporting it are one step.
                    sidecars.extend(self.back_up_adopted_target(&target)?);

                    // Backup existing target before overwriting
                    if let Ok(Some(file_state)) = crate::capture_file_state(&target)
                        && let Err(e) = self.state.store_file_backup(
                            apply_id,
                            &crate::to_posix_fs_key(&target),
                            &file_state,
                        )
                    {
                        // tracing-ok: the rollback copy could not be stored; the deploy row says nothing about it
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

                    self.record_module_file(action, &target, strategy, apply_id)?;
                }

                Ok(super::apply::ActionRun::new(
                    super::format::module_files_description(&action.module_name, *declared_total),
                    deployed_any,
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

                Ok(super::apply::ActionRun::new(
                    format!("module:{}:script", action.module_name),
                    changed,
                ))
            }
            ModuleActionKind::Skip { reason: _ } => {
                // A planned skip did nothing this run, so it must not count as
                // changed and must not fire the module's onChange hooks.
                Ok(super::apply::ActionRun::new(
                    format!("module:{}:skip", action.module_name),
                    false,
                ))
            }
        }
    }
}

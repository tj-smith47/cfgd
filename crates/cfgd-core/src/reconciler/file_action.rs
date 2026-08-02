use crate::config::FileStrategy;
use crate::errors::{FileError, Result};
use crate::providers::FileAction;

use super::patch::{PatchBinding, evaluate_patch};
use super::types::ReconcileContext;

pub(super) fn apply_file_action_direct(
    action: &FileAction,
    config_dir: &std::path::Path,
    profile_name: &str,
) -> Result<()> {
    match action {
        FileAction::Create {
            source,
            target,
            strategy,
            patch,
            ..
        }
        | FileAction::Update {
            source,
            target,
            strategy,
            patch,
            ..
        } => {
            // `Patch` rewrites the target's own content, so it is computed
            // before the remove-then-deploy sequence below deletes the very
            // bytes it reads.
            let patched = match strategy {
                FileStrategy::Patch => {
                    let spec = patch.as_ref().ok_or_else(|| FileError::PatchBlockMissing {
                        path: target.clone(),
                    })?;
                    let binding = PatchBinding::profile(
                        config_dir,
                        profile_name,
                        ReconcileContext::Reconcile,
                    );
                    Some(evaluate_patch(spec, target, &binding.context())?.patched)
                }
                _ => None,
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Remove existing target before deploying. `Patch` is exempt: the
            // removal clears a stale link ahead of `create_symlink`/`hard_link`,
            // while `Patch` writes through `atomic_write_merged`, which replaces
            // by rename and so keeps the target's mode and follows its symlink.
            if *strategy != FileStrategy::Patch && target.symlink_metadata().is_ok() {
                std::fs::remove_file(target)?;
            }
            match strategy {
                FileStrategy::Symlink => {
                    crate::create_symlink(source, target)?;
                }
                FileStrategy::Hardlink => {
                    std::fs::hard_link(source, target)?;
                }
                FileStrategy::Copy | FileStrategy::Template => {
                    std::fs::copy(source, target)?;
                }
                FileStrategy::Patch => {
                    crate::atomic_write_merged(target, patched.as_deref().unwrap_or_default())?;
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
            crate::set_file_permissions(target, *mode)?;
            Ok(())
        }
        FileAction::Skip { .. } => Ok(()),
    }
}

// Allow FileAction to be cloned for the trait-based apply path
impl FileAction {
    pub(super) fn clone_action(&self) -> FileAction {
        match self {
            FileAction::Create {
                source,
                target,
                origin,
                strategy,
                source_hash,
                patch,
            } => FileAction::Create {
                source: source.clone(),
                target: target.clone(),
                origin: origin.clone(),
                strategy: *strategy,
                source_hash: source_hash.clone(),
                patch: patch.clone(),
            },
            FileAction::Update {
                source,
                target,
                diff,
                origin,
                strategy,
                source_hash,
                patch,
            } => FileAction::Update {
                source: source.clone(),
                target: target.clone(),
                diff: diff.clone(),
                origin: origin.clone(),
                strategy: *strategy,
                source_hash: source_hash.clone(),
                patch: patch.clone(),
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

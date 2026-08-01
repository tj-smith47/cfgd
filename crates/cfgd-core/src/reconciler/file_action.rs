use crate::config::FileStrategy;
use crate::errors::{FileError, Result};
use crate::providers::FileAction;

use super::modify::{ModifyBinding, evaluate_modify};
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
            modify,
            ..
        }
        | FileAction::Update {
            source,
            target,
            strategy,
            modify,
            ..
        } => {
            // `Modify` rewrites the target's own content, so it is computed
            // before the remove-then-deploy sequence below deletes the very
            // bytes it reads.
            let modified = match strategy {
                FileStrategy::Modify => {
                    let spec = modify
                        .as_ref()
                        .ok_or_else(|| FileError::ModifyBlockMissing {
                            path: target.clone(),
                        })?;
                    let binding = ModifyBinding::profile(
                        config_dir,
                        profile_name,
                        ReconcileContext::Reconcile,
                    );
                    Some(evaluate_modify(spec, target, &binding.context())?.modified)
                }
                _ => None,
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Remove existing target before deploying
            if target.symlink_metadata().is_ok() {
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
                FileStrategy::Modify => {
                    crate::atomic_write_str(target, modified.as_deref().unwrap_or_default())?;
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
                modify,
            } => FileAction::Create {
                source: source.clone(),
                target: target.clone(),
                origin: origin.clone(),
                strategy: *strategy,
                source_hash: source_hash.clone(),
                modify: modify.clone(),
            },
            FileAction::Update {
                source,
                target,
                diff,
                origin,
                strategy,
                source_hash,
                modify,
            } => FileAction::Update {
                source: source.clone(),
                target: target.clone(),
                diff: diff.clone(),
                origin: origin.clone(),
                strategy: *strategy,
                source_hash: source_hash.clone(),
                modify: modify.clone(),
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

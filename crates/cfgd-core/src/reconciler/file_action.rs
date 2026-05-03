use crate::config::MergedProfile;
use crate::errors::Result;
use crate::providers::FileAction;

pub(super) fn apply_file_action_direct(
    action: &FileAction,
    _config_dir: &std::path::Path,
    _profile: &MergedProfile,
) -> Result<()> {
    match action {
        FileAction::Create {
            source,
            target,
            strategy,
            ..
        }
        | FileAction::Update {
            source,
            target,
            strategy,
            ..
        } => {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Remove existing target before deploying
            if target.symlink_metadata().is_ok() {
                std::fs::remove_file(target)?;
            }
            match strategy {
                crate::config::FileStrategy::Symlink => {
                    crate::create_symlink(source, target)?;
                }
                crate::config::FileStrategy::Hardlink => {
                    std::fs::hard_link(source, target)?;
                }
                crate::config::FileStrategy::Copy | crate::config::FileStrategy::Template => {
                    std::fs::copy(source, target)?;
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
            } => FileAction::Create {
                source: source.clone(),
                target: target.clone(),
                origin: origin.clone(),
                strategy: *strategy,
                source_hash: source_hash.clone(),
            },
            FileAction::Update {
                source,
                target,
                diff,
                origin,
                strategy,
                source_hash,
            } => FileAction::Update {
                source: source.clone(),
                target: target.clone(),
                diff: diff.clone(),
                origin: origin.clone(),
                strategy: *strategy,
                source_hash: source_hash.clone(),
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

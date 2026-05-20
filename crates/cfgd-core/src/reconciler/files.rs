use crate::config::MergedProfile;
use crate::errors::Result;
use crate::output::Printer;
use crate::providers::FileAction;

use super::file_action::apply_file_action_direct;

impl<'a> super::Reconciler<'a> {
    pub(super) fn apply_file_action(
        &self,
        action: &FileAction,
        profile: &MergedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
    ) -> Result<String> {
        if let Some(ref fm) = self.registry.file_manager {
            fm.apply(&[action.clone_action()], printer)?;
        } else {
            apply_file_action_direct(action, config_dir, profile)?;
        }

        match action {
            FileAction::Create { target, .. } => Ok(format!("file:create:{}", target.display())),
            FileAction::Update { target, .. } => Ok(format!("file:update:{}", target.display())),
            FileAction::Delete { target, .. } => Ok(format!("file:delete:{}", target.display())),
            FileAction::SetPermissions { target, mode, .. } => {
                Ok(format!("file:chmod:{:#o}:{}", mode, target.display()))
            }
            FileAction::Skip { target, .. } => Ok(format!("file:skip:{}", target.display())),
        }
    }
}

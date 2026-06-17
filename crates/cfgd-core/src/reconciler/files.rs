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

        // Resource ids are drift-correlation keys: this string is matched
        // against the one `format_action_description` records at detection
        // time, which folds via `to_posix_string`. Rendering with `.display()`
        // here emitted host-native `\` on Windows, so the apply-side key never
        // matched the detection-side key and drift never resolved. Fold to the
        // same posix form so the keys agree on every OS.
        use crate::to_posix_string;
        match action {
            FileAction::Create { target, .. } => {
                Ok(format!("file:create:{}", to_posix_string(target)))
            }
            FileAction::Update { target, .. } => {
                Ok(format!("file:update:{}", to_posix_string(target)))
            }
            FileAction::Delete { target, .. } => {
                Ok(format!("file:delete:{}", to_posix_string(target)))
            }
            FileAction::SetPermissions { target, mode, .. } => Ok(format!(
                "file:chmod:{:#o}:{}",
                mode,
                to_posix_string(target)
            )),
            FileAction::Skip { target, .. } => Ok(format!("file:skip:{}", to_posix_string(target))),
        }
    }
}

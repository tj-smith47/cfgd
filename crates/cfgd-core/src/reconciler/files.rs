use crate::errors::Result;
use crate::output::Printer;
use crate::providers::FileAction;

use super::file_action::apply_file_action_direct;

impl<'a> super::Reconciler<'a> {
    /// Bring the recorded content hash of every link-deployed managed file back
    /// in line with the bytes it currently holds, and report how many rows moved.
    ///
    /// Silent by construction: it prints nothing, plans nothing and executes no
    /// action, so a converged run still reports having nothing to do. Symlink and
    /// Hardlink convergence is link IDENTITY — an edit made through the link is
    /// the module source changing, which is never drift — so nothing else in a
    /// run ever revisits `managed_resources.last_hash` for those entries, and the
    /// recorded value would otherwise keep describing the bytes that were there
    /// when the link was first made. The consumer asks "did the user hand-modify
    /// the deployed file since cfgd applied it" by hashing the deployed file, and
    /// for a link the deployed file IS the source, so the value recorded here is
    /// the source's own bytes and the two ends agree.
    ///
    /// A row is written only when its hash actually differs
    /// ([`StateStore::refresh_managed_resource_hash`](crate::state::StateStore::refresh_managed_resource_hash)),
    /// so a machine nobody has touched costs no write however often the daemon
    /// asks. A resource with no row is left alone: an apply that only looked at a
    /// file does not start claiming it.
    ///
    /// `fm` is passed rather than read off the registry because the two apply
    /// paths hold it differently — the CLI registers its file manager so the
    /// reconciler delegates file actions through it, while a daemon tick plans
    /// files through its hooks and leaves the registry slot empty.
    pub fn refresh_link_deployed_hashes(
        &self,
        fm: &dyn crate::providers::FileManager,
        resolved: &crate::config::ResolvedProfile,
    ) -> Result<usize> {
        let deployed = fm.link_deployed_content_hashes(&resolved.merged)?;
        if deployed.is_empty() {
            return Ok(0);
        }
        self.state.in_transaction(|| {
            let mut refreshed = 0;
            for (target, hash) in &deployed {
                if self.state.refresh_managed_resource_hash(
                    "file",
                    &crate::to_posix_string(target),
                    hash,
                )? {
                    refreshed += 1;
                }
            }
            Ok(refreshed)
        })
    }

    pub(super) fn apply_file_action(
        &self,
        action: &FileAction,
        profile_name: &str,
        config_dir: &std::path::Path,
        printer: &Printer,
    ) -> Result<String> {
        if let Some(ref fm) = self.registry.file_manager {
            fm.apply(&[action.clone_action()], printer)?;
        } else {
            apply_file_action_direct(action, config_dir, profile_name)?;
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

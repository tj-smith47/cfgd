use crate::errors::Result;
use crate::output::Printer;
use crate::providers::FileAction;

use super::file_action::apply_file_action_direct;

impl<'a> super::Reconciler<'a> {
    /// Bring the recorded content hash of every link-deployed file back in line
    /// with the bytes it currently holds, and report how many rows moved. Both
    /// halves of the machine are covered: a profile-level `spec.files.managed`
    /// entry has a row of its own, while a module's files share one aggregate row
    /// (see [`Self::module_link_deployed_rows`]).
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
    /// `fm` is passed rather than read off the registry because the three apply
    /// paths hold it differently — the CLI registers its file manager so the
    /// reconciler delegates file actions through it, a daemon tick plans files
    /// through its hooks and leaves the registry slot empty, and a `--module`
    /// run builds none at all. `None` refreshes the module half alone, which is
    /// the half that run has.
    pub fn refresh_link_deployed_hashes(
        &self,
        fm: Option<&dyn crate::providers::FileManager>,
        resolved: &crate::config::ResolvedProfile,
        modules: &[crate::modules::ResolvedModule],
    ) -> Result<usize> {
        let mut rows: Vec<(String, String, String)> = Vec::new();
        if let Some(fm) = fm {
            for (target, hash) in fm.link_deployed_content_hashes(&resolved.merged)? {
                rows.push(("file".to_string(), crate::to_posix_string(&target), hash));
            }
        }
        rows.extend(self.module_link_deployed_rows(modules));
        if rows.is_empty() {
            return Ok(0);
        }
        self.state.in_transaction(|| {
            let mut refreshed = 0;
            for (rtype, rid, hash) in &rows {
                if self.state.refresh_managed_resource_hash(rtype, rid, hash)? {
                    refreshed += 1;
                }
            }
            Ok(refreshed)
        })
    }

    /// The refreshed row of every resolved module deploying at least one file by
    /// Symlink/Hardlink onto a target that is still the link.
    ///
    /// A module records ONE aggregate `managed_resources` row rather than a row
    /// per file, so what is refreshed is an aggregate too: each converged link
    /// contributes `<target>:<content hash>`, and the parts fold through the
    /// same [`hash_sorted_parts`](super::apply::hash_sorted_parts) every other
    /// per-module recorded hash uses, so no second aggregation exists to
    /// disagree with it. The id is minted and parsed by the same pair
    /// `record_managed_resources` writes it with, so a refresh can only land on
    /// the row an apply wrote — and never mints one, since the write is an
    /// `UPDATE`.
    ///
    /// Convergence is asked with the planner's own predicate, so this reports
    /// exactly the entries the plan elided. For a link the deployed file IS the
    /// source file — one inode — so hashing the source yields what hashing the
    /// target would.
    ///
    /// An unreadable source abandons its module's whole aggregate rather than
    /// dropping one part: a digest taken over a partial reading is not "the
    /// question went unanswered", it is a confident wrong answer, and the
    /// recorded value must stand instead.
    fn module_link_deployed_rows(
        &self,
        modules: &[crate::modules::ResolvedModule],
    ) -> Vec<(String, String, String)> {
        use crate::config::FileStrategy;

        let mut rows = Vec::new();
        for module in modules {
            let mut parts = Vec::new();
            for file in &module.files {
                let strategy = file.strategy.unwrap_or(self.registry.default_file_strategy);
                if !matches!(strategy, FileStrategy::Symlink | FileStrategy::Hardlink) {
                    continue;
                }
                // A directory-shaped entry has no one content hash, and a
                // Hardlink one is deployed as a copy rather than as a link.
                if !file.source.is_file() {
                    continue;
                }
                let target = crate::expand_tilde(&file.target);
                if !super::modules::planned_file_converged(file, &target, strategy, None) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&file.source) else {
                    parts.clear();
                    break;
                };
                parts.push(format!(
                    "{}:{}",
                    crate::to_posix_string(&target),
                    crate::sha256_hex(&bytes)
                ));
            }
            if parts.is_empty() {
                continue;
            }
            let (rtype, rid) = super::format::parse_resource_from_description(
                &super::format::module_files_description(&module.name, module.files.len()),
            );
            rows.push((rtype, rid, super::apply::hash_sorted_parts(parts)));
        }
        rows
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

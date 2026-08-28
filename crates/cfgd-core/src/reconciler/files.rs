use crate::errors::Result;
use crate::output::Printer;
use crate::providers::FileAction;

use super::file_action::apply_file_action_direct;

impl<'a> super::Reconciler<'a> {
    /// Bring the recorded content hash of every link-deployed file back in line
    /// with the bytes it currently holds, and report how many rows moved. Both
    /// halves of the machine are covered: a profile-level `spec.files.managed`
    /// entry has a row of its own, while a module's files share one aggregate row
    /// (see `Self::module_link_deployed_rows`).
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
                let target = crate::expand_tilde(&file.target);
                // A Hardlink directory is deployed as a copy, and the
                // convergence predicate already answers false for it.
                if !super::modules::planned_file_converged(file, &target, strategy, None) {
                    continue;
                }
                let Some((digest, _)) = link_deployed_digest(&file.source) else {
                    parts.clear();
                    break;
                };
                parts.push(format!("{}:{digest}", crate::to_posix_string(&target)));
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
        sidecars: &mut Vec<super::sidecar::SidecarOutcome>,
    ) -> Result<String> {
        if let FileAction::Create { target, .. } | FileAction::Update { target, .. } = action {
            sidecars.extend(self.back_up_adopted_target(target)?);
        }
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
        let description = match action {
            FileAction::Create { target, .. } => format!("file:create:{}", to_posix_string(target)),
            FileAction::Update { target, .. } => format!("file:update:{}", to_posix_string(target)),
            FileAction::Delete { target, .. } => format!("file:delete:{}", to_posix_string(target)),
            FileAction::SetPermissions { target, mode, .. } => {
                format!("file:chmod:{:#o}:{}", mode, to_posix_string(target))
            }
            FileAction::Skip { target, .. } => format!("file:skip:{}", to_posix_string(target)),
        };
        Ok(description)
    }
}

/// The content digest of everything a converged link entry deploys, and how
/// many files that is: a file's own sha256 (so a single-file row records the
/// bytes the deployed file holds), or for a directory link the fold of
/// `<relative path>:<sha256>` over every regular file under it — the same
/// tree the deploy walks (symlinks skipped, matching `copy_dir_recursive`).
/// Read by BOTH halves of the recorded-hash refresh, the profile-level
/// `spec.files.managed` rows and a module's aggregate, so a directory
/// entry cannot be visible to one and invisible to the other.
///
/// `None` on any unreadable file: a digest over a partial reading is a
/// confident wrong answer, and the recorded value must stand instead.
pub fn link_deployed_digest(source: &std::path::Path) -> Option<(String, usize)> {
    if !source.is_dir() {
        return std::fs::read(source)
            .ok()
            .map(|bytes| (crate::sha256_hex(&bytes), 1));
    }
    let mut parts = Vec::new();
    // A worklist rather than recursion: the tree's depth is module-supplied.
    let mut pending = vec![source.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).ok()? {
            let entry = entry.ok()?;
            let ft = entry.file_type().ok()?;
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                pending.push(entry.path());
                continue;
            }
            let bytes = std::fs::read(entry.path()).ok()?;
            let relative = entry.path().strip_prefix(source).ok()?.to_path_buf();
            parts.push(format!(
                "{}:{}",
                crate::to_posix_string(&relative),
                crate::sha256_hex(&bytes)
            ));
        }
    }
    let files = parts.len();
    Some((super::apply::hash_sorted_parts(parts), files))
}

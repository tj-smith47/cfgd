use crate::errors::Result;
use crate::output::Printer;
use crate::providers::FileAction;

use super::file_action::apply_file_action_direct;

impl<'a> super::Reconciler<'a> {
    /// Bring the recorded content hash of every link-deployed file back in line
    /// with the bytes it currently holds, and report what moved — as rows AND
    /// as the files that actually changed behind them, counted against the
    /// per-file breakdown each row keeps (see [`RefreshedHashes`]). Both
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
    ) -> Result<RefreshedHashes> {
        let mut rows: Vec<(String, String, String, Vec<String>)> = Vec::new();
        if let Some(fm) = fm {
            for row in fm.link_deployed_content_hashes(&resolved.merged)? {
                rows.push((
                    "file".to_string(),
                    crate::to_posix_string(&row.target),
                    row.hash,
                    row.file_hashes,
                ));
            }
        }
        rows.extend(self.module_link_deployed_rows(modules));
        if rows.is_empty() {
            return Ok(RefreshedHashes::default());
        }
        self.state.in_transaction(|| {
            let mut refreshed = RefreshedHashes::default();
            for (rtype, rid, hash, file_hashes) in &rows {
                let stored = file_hashes_column(file_hashes);
                if let crate::state::HashRefresh::Moved { previous_files } = self
                    .state
                    .refresh_managed_resource_hash(rtype, rid, hash, &stored)?
                {
                    refreshed.rows += 1;
                    refreshed.files = match (refreshed.files, previous_files) {
                        (Some(total), Some(previous)) => {
                            Some(total + moved_file_count(&previous, file_hashes))
                        }
                        // A row with no breakdown yet can prove no count, and
                        // one such row is enough to make the total unprovable.
                        _ => None,
                    };
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
    ) -> Vec<(String, String, String, Vec<String>)> {
        use crate::config::FileStrategy;

        let mut rows = Vec::new();
        for module in modules {
            let mut parts = Vec::new();
            let mut file_hashes = Vec::new();
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
                let Some(digest) = link_deployed_digest(&file.source, &target) else {
                    parts.clear();
                    break;
                };
                parts.push(format!(
                    "{}:{}",
                    crate::to_posix_string(&target),
                    digest.hash
                ));
                file_hashes.extend(digest.file_hashes);
            }
            if parts.is_empty() {
                continue;
            }
            let (rtype, rid) = super::format::parse_resource_from_description(
                &super::format::module_files_description(&module.name, module.files.len()),
            );
            rows.push((
                rtype,
                rid,
                super::apply::hash_sorted_parts(parts),
                file_hashes,
            ));
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

/// What a recorded-hash refresh moved. `rows` is the `managed_resources`
/// writes, `files` the link-deployed files whose bytes actually changed
/// behind those rows — counted entry by entry against the per-file
/// breakdown each row keeps ([`moved_file_count`]), never the row's whole
/// coverage: a module's row is ONE aggregate over every file its entries
/// deploy, so an aggregate that moved by a byte says nothing about how many
/// files did. `None` when some moved row had no breakdown recorded yet (a
/// row written before the breakdown column existed, or by an apply that
/// records no hash): the tick states no number rather than the ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshedHashes {
    pub rows: usize,
    pub files: Option<usize>,
}

impl Default for RefreshedHashes {
    fn default() -> Self {
        Self {
            rows: 0,
            files: Some(0),
        }
    }
}

/// The `managed_resources.file_hashes` column: the breakdown's `<path>:<sha256>`
/// entries, sorted, one per line. Sorted so two readings of one tree store
/// one string, and lines so the inverse is `str::lines`.
fn file_hashes_column(file_hashes: &[String]) -> String {
    let mut lines = file_hashes.to_vec();
    lines.sort();
    lines.join("\n")
}

/// How many entries differ between a stored breakdown and the current one:
/// a file whose digest moved, a file that appeared, and a file that went.
fn moved_file_count(previous: &str, current: &[String]) -> usize {
    let split = |entry: &str| -> Option<(String, String)> {
        entry
            .rsplit_once(':')
            .map(|(path, sha)| (path.to_string(), sha.to_string()))
    };
    let before: std::collections::BTreeMap<String, String> =
        previous.lines().filter_map(split).collect();
    let after: std::collections::BTreeMap<String, String> =
        current.iter().filter_map(|e| split(e)).collect();
    before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|key| before.get(*key) != after.get(*key))
        .count()
}

/// What [`link_deployed_digest`] reads off one converged link entry: the
/// aggregate `hash` its row records, and the `<path>:<sha256>` breakdown
/// behind it, keyed on the deployed target so a module's rows from several
/// entries share one namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkDeployedDigest {
    pub hash: String,
    pub file_hashes: Vec<String>,
}

/// The content digest of everything a converged link entry deploys, and the
/// per-file breakdown behind it: a file's own sha256 (so a single-file row
/// records the bytes the deployed file holds), or for a directory link the
/// fold of `<relative path>:<sha256>` over every regular file under it — the
/// same tree the deploy walks (symlinks skipped, matching
/// `copy_dir_recursive`). The breakdown keys every file on `target`
/// (`<target>` for a file, `<target>/<relative path>` under a directory).
/// Read by BOTH halves of the recorded-hash refresh, the profile-level
/// `spec.files.managed` rows and a module's aggregate, so a directory
/// entry cannot be visible to one and invisible to the other.
///
/// `None` on any unreadable file: a digest over a partial reading is a
/// confident wrong answer, and the recorded value must stand instead.
pub fn link_deployed_digest(
    source: &std::path::Path,
    target: &std::path::Path,
) -> Option<LinkDeployedDigest> {
    let target = crate::to_posix_string(target);
    if !source.is_dir() {
        let sha = crate::sha256_hex(&std::fs::read(source).ok()?);
        return Some(LinkDeployedDigest {
            file_hashes: vec![format!("{target}:{sha}")],
            hash: sha,
        });
    }
    let mut parts = Vec::new();
    let mut file_hashes = Vec::new();
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
            let relative = crate::to_posix_string(&relative);
            let sha = crate::sha256_hex(&bytes);
            parts.push(format!("{relative}:{sha}"));
            file_hashes.push(format!("{target}/{relative}:{sha}"));
        }
    }
    Some(LinkDeployedDigest {
        hash: super::apply::hash_sorted_parts(parts),
        file_hashes,
    })
}

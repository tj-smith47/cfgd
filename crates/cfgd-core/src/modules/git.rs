//! Git URL parsing and git clone/fetch/checkout operations for module file sources.

use std::path::{Path, PathBuf};

use crate::PathDisplayExt;
use crate::errors::{ModuleError, Result};

// ---------------------------------------------------------------------------
// Git file source URL parsing
// ---------------------------------------------------------------------------

/// Parsed git file source URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    /// The repo URL (without tag/ref/subdir suffixes).
    pub repo_url: String,
    /// Tag to checkout (from `@tag` suffix).
    pub tag: Option<String>,
    /// Branch/ref to checkout (from `?ref=branch` suffix).
    pub git_ref: Option<String>,
    /// Subdirectory within the repo (from `//subdir` separator).
    pub subdir: Option<String>,
}

impl GitSource {
    /// The ref this source asks for: its tag, else its `?ref=`. `None` follows
    /// the repository's default branch.
    pub fn requested_ref(&self) -> Option<&str> {
        self.tag.as_deref().or(self.git_ref.as_deref())
    }
}

/// Check whether a source string is a git URL (not a local path).
///
/// The single git-URL predicate for the whole workspace: module file sources,
/// registry-ref disambiguation, and the CLI's `--from` classification all judge
/// a value here, so a URL accepted in one place is accepted in every place.
///
/// Recognises the four remote transports git speaks — `https://`, `http://`,
/// `ssh://`, `git://` — plus the SCP-style `git@host:owner/repo` form.
///
/// `file://` URLs are rejected by default to keep remote-module sources to
/// proper network protocols. Tests can opt into local-file sources by setting
/// `CFGD_ALLOW_LOCAL_SOURCES=1` — same gate `sources/mod.rs` uses for the
/// composed-sources path.
///
/// A local directory that happens to be a git checkout is NOT a git source: a
/// module declaring `source: files/nvim` must stay a path even when the config
/// repo around it is a work tree. The one caller that also accepts a local repo
/// (`cfgd init --from`) layers that probe on top of this predicate rather than
/// widening it for everyone.
pub fn is_git_source(source: &str) -> bool {
    if source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.starts_with("git://")
    {
        return true;
    }
    if source.starts_with("file://") && std::env::var(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV).is_ok() {
        return true;
    }
    false
}

/// Parse a git file source URL into its components.
///
/// Supports:
/// - `https://github.com/user/repo.git` — plain clone
/// - `https://github.com/user/repo.git@v2.1.0` — pin to tag
/// - `https://github.com/user/repo.git?ref=dev` — track branch
/// - `https://github.com/user/repo.git//subdir` — subdirectory
/// - `https://github.com/user/repo.git//subdir@v2.1.0` — subdir at tag
/// - `git@github.com:user/repo.git@v2.1.0` — SSH with tag
pub fn parse_git_source(source: &str) -> Result<GitSource> {
    if !is_git_source(source) {
        return Err(ModuleError::InvalidSpec {
            name: source.to_string(),
            message: "not a git URL".into(),
        }
        .into());
    }

    let mut url = source.to_string();
    let mut tag = None;
    let mut git_ref = None;
    let mut subdir = None;

    // Extract ?ref=... (must be done before @tag extraction since ? is unambiguous)
    // Stop at // (subdir separator) so ?ref=dev//subdir works correctly
    if let Some(ref_pos) = url.find("?ref=") {
        let after_ref = &url[ref_pos + 5..];
        let end = after_ref.find("//").unwrap_or(after_ref.len());
        let ref_val = after_ref[..end].to_string();
        let remainder = &after_ref[end..];
        url = format!("{}{}", &url[..ref_pos], remainder);
        git_ref = Some(ref_val);
    }

    // Extract //subdir (and possibly @tag after the subdir)
    // Skip the :// scheme prefix when looking for // path separator
    let search_start = url.find("://").map(|p| p + 3).unwrap_or(0);
    if let Some(rel_pos) = url[search_start..].find("//") {
        let subdir_pos = search_start + rel_pos;
        let subdir_part = url[subdir_pos + 2..].to_string();
        url = url[..subdir_pos].to_string();

        // The subdir part may have @tag
        if let Some(at_pos) = subdir_part.rfind('@') {
            subdir = Some(subdir_part[..at_pos].to_string());
            tag = Some(subdir_part[at_pos + 1..].to_string());
        } else {
            subdir = Some(subdir_part);
        }
    } else {
        // No subdir — check for @tag on the URL itself
        // For SSH URLs like git@github.com:user/repo.git@v2.1.0,
        // the @tag must be found *after* the .git suffix
        if let Some(git_suffix_pos) = url.find(".git") {
            let after_git = &url[git_suffix_pos + 4..];
            if let Some(at_pos) = after_git.find('@') {
                tag = Some(after_git[at_pos + 1..].to_string());
                url = url[..git_suffix_pos + 4].to_string();
            }
        } else if let Some(at_pos) = url.rfind('@') {
            // No .git in URL — look for last @ that isn't part of the protocol.
            // For https/http/ssh://, skip past ://
            // For git@, skip past the first @
            let skip_to = if url.starts_with("git@") {
                url.find('@').map(|p| p + 1).unwrap_or(0)
            } else {
                url.find("://").map(|p| p + 3).unwrap_or(0)
            };
            if at_pos > skip_to {
                tag = Some(url[at_pos + 1..].to_string());
                url = url[..at_pos].to_string();
            }
        }
    }

    Ok(GitSource {
        repo_url: url,
        tag,
        git_ref,
        subdir,
    })
}

/// The cached clone of a git repository, named by the SHA-256 of its URL.
///
/// It holds the repository's objects, tags and remote-tracking branches, and its
/// working tree is the checkout an unpinned source follows. A source naming a
/// ref reads its files from [`git_checkout_dir`].
pub fn git_cache_dir(cache_base: &Path, repo_url: &str) -> PathBuf {
    let hash = crate::sha256_hex(repo_url.as_bytes());
    cache_base.join(&hash[..32])
}

/// Where cfgd keeps the trees it exports out of a clone, one per commit.
const PINNED_CHECKOUTS_DIR: &str = "cfgd-checkouts";

/// How long after a tree or clone was last written or handed out a sweep
/// leaves it alone.
///
/// A run reading a source holds no lock a sweep could wait on, so a tree
/// exported or handed out within this window may still be in use; an export
/// in progress is younger than this too. [`checkout_ref`] refreshes the
/// in-use stamp of what it hands out, so a long run keeps its trees.
pub(crate) const IN_USE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The directory a source's files are read from: the clone's own working tree
/// for `requested_ref: None`, and otherwise the tree exported for the commit
/// that ref resolves to in the clone.
///
/// The trees are keyed by commit, so every ref naming one commit reads one
/// tree, a ref that moves reads a new tree, and a tree once written never
/// changes. They sit inside the clone's `.git`, out of its working tree, so
/// removing the clone removes them too. [`sweep_pinned_checkouts`] removes the
/// trees nothing reads any more.
///
/// A full object id that no tag or branch of the clone shadows names its own
/// tree, so its path is known before the clone exists. Any other ref is
/// resolved in the clone, and a ref the clone cannot resolve is an error.
pub fn git_checkout_dir(
    cache_base: &Path,
    repo_url: &str,
    requested_ref: Option<&str>,
) -> Result<PathBuf> {
    let clone = git_cache_dir(cache_base, repo_url);
    match requested_ref {
        None => Ok(clone),
        Some(ref_name) => {
            let commit = cached_commit_of(&clone, repo_url, ref_name)?;
            Ok(pinned_checkout_in(&clone, &commit))
        }
    }
}

fn pinned_checkouts_of(clone: &Path) -> PathBuf {
    clone.join(".git").join(PINNED_CHECKOUTS_DIR)
}

pub(crate) fn pinned_checkout_in(clone: &Path, commit: &str) -> PathBuf {
    pinned_checkouts_of(clone).join(commit)
}

/// The commit `ref_name` names in the clone at `clone`, resolved the way
/// [`checkout_ref`] resolves it.
fn cached_commit_of(clone: &Path, repo_url: &str, ref_name: &str) -> Result<String> {
    let repo = git2::Repository::open(clone);
    if is_full_object_id(ref_name)
        && repo
            .as_ref()
            .map_or(true, |repo| !named_ref_exists(repo, ref_name))
    {
        return Ok(ref_name.to_ascii_lowercase());
    }
    let failed = |message: String| -> crate::errors::CfgdError {
        ModuleError::GitFetchFailed {
            module: repo_url.to_string(),
            url: repo_url.to_string(),
            message,
        }
        .into()
    };
    let repo = repo.map_err(|e| {
        failed(if clone.exists() {
            format!("cannot open the cached clone: {e}")
        } else {
            format!("'{ref_name}' cannot be resolved: the repository is not cached yet")
        })
    })?;
    resolve_ref_object(&repo, ref_name)
        .and_then(|obj| obj.peel_to_commit())
        .map(|commit| commit.id().to_string())
        .map_err(|e| {
            failed(format!(
                "cannot resolve '{ref_name}' in the cached clone: {e}"
            ))
        })
}

/// The commit a source's files were materialized from by [`fetch_git_source`]:
/// the clone's HEAD for an unpinned source, the commit its ref resolves to in
/// the clone otherwise.
pub fn checked_out_commit(cache_base: &Path, git_src: &GitSource) -> Result<String> {
    let clone = git_cache_dir(cache_base, &git_src.repo_url);
    match git_src.requested_ref() {
        None => get_head_commit_sha(&clone),
        Some(ref_name) => cached_commit_of(&clone, &git_src.repo_url, ref_name),
    }
}

/// The trees and clones under one cache root that a sweep must keep, reduced
/// to their roots.
///
/// A kept path may be spelled under the cache root as given, under its
/// canonical form, or with a Windows `\\?\` verbatim prefix (what
/// `read_link` and `canonicalize` hand back there). Each is folded to the
/// cache root as given, which is how the sweep names what it finds.
pub(crate) struct KeptCheckouts<'a> {
    cache_base: &'a Path,
    canonical_base: Option<PathBuf>,
    roots: std::collections::HashSet<PathBuf>,
}

impl<'a> KeptCheckouts<'a> {
    pub(crate) fn new(cache_base: &'a Path) -> Self {
        let canonical_base = std::fs::canonicalize(cache_base)
            .ok()
            .map(|path| without_verbatim_prefix(&path).into_owned())
            .filter(|canonical| canonical != cache_base);
        Self {
            cache_base,
            canonical_base,
            roots: std::collections::HashSet::new(),
        }
    }

    /// Keep the clone `path` lies in and, when it lies in an exported tree,
    /// that tree. Returns whether `path` lies under the cache root at all.
    pub(crate) fn keep(&mut self, path: &Path) -> bool {
        let Some(relative) = self.relative(path) else {
            return false;
        };
        let mut parts = relative.components();
        let Some(clone) = parts.next() else {
            return true;
        };
        let clone = self.cache_base.join(clone);
        if let (Some(dot_git), Some(checkouts), Some(commit)) =
            (parts.next(), parts.next(), parts.next())
            && dot_git.as_os_str() == ".git"
            && checkouts.as_os_str() == PINNED_CHECKOUTS_DIR
        {
            self.roots
                .insert(clone.join(".git").join(PINNED_CHECKOUTS_DIR).join(commit));
        }
        self.roots.insert(clone);
        true
    }

    /// Keep what the symlink at `link` points into, given its `target` as
    /// `read_link` returned it. Returns whether it points at something that
    /// exists under the cache root.
    pub(crate) fn keep_link_target(&mut self, link: &Path, target: &Path) -> bool {
        let resolved = match link.parent() {
            Some(parent) => parent.join(target),
            None => target.to_path_buf(),
        };
        if !resolved.exists() {
            return false;
        }
        self.keep(&resolved)
            || std::fs::canonicalize(&resolved).is_ok_and(|canonical| self.keep(&canonical))
    }

    fn relative<'p>(&self, path: &'p Path) -> Option<std::borrow::Cow<'p, Path>> {
        if let Ok(relative) = path.strip_prefix(self.cache_base) {
            return Some(relative.into());
        }
        let folded = without_verbatim_prefix(path);
        let relative = std::iter::once(self.cache_base)
            .chain(self.canonical_base.as_deref())
            .find_map(|base| folded.strip_prefix(base).ok())?
            .to_path_buf();
        Some(relative.into())
    }

    fn holds(&self, path: &Path) -> bool {
        self.roots.contains(path)
    }
}

/// `path` with a Windows `\\?\` verbatim prefix dropped; any other path as is.
fn without_verbatim_prefix(path: &Path) -> std::borrow::Cow<'_, Path> {
    match path.to_str() {
        Some(text) if crate::strip_windows_verbatim(text).len() != text.len() => {
            Path::new(crate::strip_windows_verbatim(text))
                .to_path_buf()
                .into()
        }
        _ => path.into(),
    }
}

/// Where a clone's in-use stamps live: inside its `.git`, which no
/// derivation stamps as an input, so refreshing one leaves a saved plan over
/// the tree current.
const IN_USE_STAMPS_DIR: &str = "cfgd-in-use";

/// The stamp a clone's own handouts refresh. No tree is named like it.
const CLONE_STAMP: &str = "clone";

/// The in-use stamp of the clone at `clone`.
fn clone_stamp(clone: &Path) -> PathBuf {
    clone.join(".git").join(IN_USE_STAMPS_DIR).join(CLONE_STAMP)
}

/// The in-use stamp of the tree at `tree`, a directory of
/// `<clone>/.git/cfgd-checkouts`.
fn tree_stamp(tree: &Path) -> Option<PathBuf> {
    let dot_git = tree.parent()?.parent()?;
    Some(dot_git.join(IN_USE_STAMPS_DIR).join(tree.file_name()?))
}

/// Refresh the in-use stamp at `stamp`, so a sweep inside [`IN_USE_WINDOW`]
/// leaves what it stands for.
///
/// A failure only shortens how long the tree or clone is protected, and the
/// next handout stamps it again, so it is not an error for the run.
fn mark_in_use(stamp: &Path) {
    let touched = stamp
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::File::create(stamp))
        .and_then(|file| file.set_modified(std::time::SystemTime::now()));
    if let Err(e) = touched {
        tracing::debug!(
            path = %crate::to_posix_string(stamp),
            "cannot refresh the in-use stamp of a module checkout: {e}"
        );
    }
}

/// Mark the clone at `clone` as just used.
pub(super) fn mark_clone_in_use(clone: &Path) {
    mark_in_use(&clone_stamp(clone));
}

fn mark_tree_in_use(tree: &Path) {
    if let Some(stamp) = tree_stamp(tree) {
        mark_in_use(&stamp);
    }
}

/// Remove every tree exported under `cache_base` and every clone that `keep`
/// does not hold, and the leftovers of interrupted exports and sweeps. Returns
/// one message per entry that could not be read or removed; the sweep goes on
/// past each.
///
/// A tree or clone changed within `in_use_window` is left alone: a run that
/// holds no apply lock may be reading it. A clone is removed only once no
/// tree is left in it.
///
/// A tree or clone is first renamed to a `.stale-` name in the same directory
/// and only then deleted, so a deletion that stops part way never leaves a
/// partial tree at a commit's path, where a later export would take it as
/// complete. A `.stale-` directory is always removed. Trees laid out by
/// earlier releases under other names are removed like any unreferenced tree.
pub(crate) fn sweep_pinned_checkouts(
    keep: &KeptCheckouts<'_>,
    in_use_window: std::time::Duration,
) -> Vec<String> {
    let mut failures = Vec::new();
    let report = |failures: &mut Vec<String>, what: &str, path: &Path, e: std::io::Error| {
        failures.push(format!(
            "cannot {what} {}: {e}",
            crate::to_posix_string(path)
        ));
    };
    let cache_base = keep.cache_base;
    let clones = match std::fs::read_dir(cache_base) {
        Ok(clones) => clones,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return failures,
        Err(e) => {
            report(&mut failures, "read", cache_base, e);
            return failures;
        }
    };
    for clone in clones {
        let clone = match clone {
            Ok(clone) => clone,
            Err(e) => {
                report(&mut failures, "read", cache_base, e);
                continue;
            }
        };
        let path = clone.path();
        let name = clone.file_name();
        let name = name.to_str().unwrap_or_default();
        if name.starts_with(STALE_PREFIX) {
            if let Err(e) = absent_ok(std::fs::remove_dir_all(&path)) {
                report(&mut failures, "remove", &path, e);
            }
            continue;
        }
        if !is_clone_name(name) {
            continue;
        }
        let trees_left = sweep_trees_of(&path, keep, in_use_window, &mut failures);
        if trees_left || keep.holds(&path) {
            continue;
        }
        match used_within(&path, Some(&clone_stamp(&path)), in_use_window) {
            Ok(true) => {}
            Ok(false) => {
                if let Err(e) = retire(cache_base, &path) {
                    report(&mut failures, "remove", &path, e);
                }
            }
            Err((_, e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err((at, e)) => report(&mut failures, "read", &at, e),
        }
    }
    failures
}

/// Sweep the trees exported in the clone at `clone`, and return whether any
/// tree is left there.
fn sweep_trees_of(
    clone: &Path,
    keep: &KeptCheckouts<'_>,
    in_use_window: std::time::Duration,
    failures: &mut Vec<String>,
) -> bool {
    let mut report = |what: &str, path: &Path, e: std::io::Error| {
        failures.push(format!(
            "cannot {what} {}: {e}",
            crate::to_posix_string(path)
        ));
    };
    let slots = pinned_checkouts_of(clone);
    let entries = match std::fs::read_dir(&slots) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(e) => {
            report("read", &slots, e);
            return true;
        }
    };
    let mut trees_left = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                report("read", &slots, e);
                trees_left = true;
                continue;
            }
        };
        let path = entry.path();
        let name = entry.file_name();
        if name.to_str().unwrap_or_default().starts_with(STALE_PREFIX) {
            if let Err(e) = absent_ok(std::fs::remove_dir_all(&path)) {
                report("remove", &path, e);
                trees_left = true;
            }
            continue;
        }
        if keep.holds(&path) {
            trees_left = true;
            continue;
        }
        let stamp = tree_stamp(&path);
        match used_within(&path, stamp.as_deref(), in_use_window) {
            Ok(true) => trees_left = true,
            Ok(false) => match retire(&slots, &path) {
                Ok(()) => {
                    if let Some(stamp) = &stamp
                        && let Err(e) = absent_ok(std::fs::remove_file(stamp))
                    {
                        report("remove", stamp, e);
                    }
                }
                Err(e) => {
                    report("remove", &path, e);
                    trees_left = true;
                }
            },
            Err((_, e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err((at, e)) => {
                report("read", &at, e);
                trees_left = true;
            }
        }
    }
    trees_left
}

/// Whether the entry at `path`, or its in-use `stamp` when one exists, was
/// modified within `window` of now. A time in the future counts as recent. An
/// error names the path it came from.
fn used_within(
    path: &Path,
    stamp: Option<&Path>,
    window: std::time::Duration,
) -> std::result::Result<bool, (PathBuf, std::io::Error)> {
    let recent = |at: &Path| -> std::result::Result<bool, (PathBuf, std::io::Error)> {
        let modified = std::fs::symlink_metadata(at)
            .and_then(|meta| meta.modified())
            .map_err(|e| (at.to_path_buf(), e))?;
        Ok(modified.elapsed().map_or(true, |age| age < window))
    };
    if recent(path)? {
        return Ok(true);
    }
    match stamp.map(recent) {
        Some(Err((_, e))) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Some(answer) => answer,
        None => Ok(false),
    }
}

/// Whether `name` is how [`git_cache_dir`] names a clone.
fn is_clone_name(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

const STALE_PREFIX: &str = ".stale-";
const EXPORT_PREFIX: &str = ".export-";

/// Move `path` into a fresh `.stale-` directory under `parent`, then delete it.
fn retire(parent: &Path, path: &Path) -> std::io::Result<()> {
    let aside = tempfile::Builder::new()
        .prefix(STALE_PREFIX)
        .tempdir_in(parent)?;
    absent_ok(std::fs::rename(path, aside.path().join("tree")))?;
    let aside = aside.keep();
    std::fs::remove_dir_all(aside)
}

/// A filesystem call whose target was already gone succeeded.
fn absent_ok(removal: std::io::Result<()>) -> std::io::Result<()> {
    match removal {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Default cache directory for module git sources: `<cache-root>/modules` under
/// the single unified cfgd cache root.
///
/// Rebased onto the shared [`crate::default_cache_dir`] resolver so the source
/// cache and module cache share ONE root (Linux `~/.cache/cfgd`, macOS
/// `~/Library/Caches/cfgd`, Windows `%LOCALAPPDATA%\cfgd`). That resolver
/// honors the thread-local test-home override, so tests still redirect module
/// cache writes off the real cache.
pub fn default_module_cache_dir() -> Result<PathBuf> {
    default_module_cache_dir_for(crate::Scope::User)
}

/// Default module cache directory (`<cache-root>/modules`) for the given
/// [`crate::Scope`]: per-user under [`crate::Scope::User`], the FHS / platform
/// system cache root under [`crate::Scope::System`].
pub fn default_module_cache_dir_for(scope: crate::Scope) -> Result<PathBuf> {
    Ok(crate::default_cache_dir_for(scope)
        .map_err(|e| ModuleError::GitFetchFailed {
            module: String::new(),
            url: String::new(),
            message: e.to_string(),
        })?
        .join(crate::MODULE_CACHE_SEGMENT))
}

/// Resolve optional subdir within a cache directory with traversal validation.
pub(super) fn resolve_subdir(
    base: PathBuf,
    subdir: &Option<String>,
    module: &str,
    url: &str,
) -> Result<PathBuf> {
    match subdir {
        Some(sub) => {
            // `subdir: "."` is the repository root — an explicit way to spell
            // the default, and a legitimate answer even though it names nothing
            // of its own.
            crate::validate_no_traversal_allowing_self(std::path::Path::new(sub)).map_err(|e| {
                ModuleError::GitFetchFailed {
                    module: module.to_string(),
                    url: url.to_string(),
                    message: format!("subdir '{sub}' is not usable: {e}"),
                }
            })?;
            Ok(base.join(sub))
        }
        None => Ok(base),
    }
}

// ---------------------------------------------------------------------------
// Git clone / fetch operations
// ---------------------------------------------------------------------------

/// Clone or fetch a git source to the cache, returning the local path.
///
/// If the repo is already cached, fetches updates. Otherwise, clones.
/// Checks out the specified tag/ref if provided.
///
/// Two round-trips are skipped here, and both are skips of work that could not
/// change the answer. A pin the cache already resolves (`cache_answers_pinned_ref`)
/// never fetches at all — the commit is immutable, so there is nothing upstream
/// could tell us. Everything else fetches at most once per repository per
/// refresh window (see `fetch_existing_repo`): a module declaring twenty files
/// out of one repo used to run twenty full fetch cycles of the same refs.
pub fn fetch_git_source(
    git_src: &GitSource,
    cache_base: &Path,
    module_name: &str,
    printer: &crate::output::Printer,
) -> Result<PathBuf> {
    let cache_dir = git_cache_dir(cache_base, &git_src.repo_url);

    if cache_dir.join(".git").exists() || cache_dir.join("HEAD").exists() {
        if !cache_answers_pinned_ref(&cache_dir, git_src) {
            fetch_existing_repo(&cache_dir, git_src, module_name, printer)?;
        }
    } else {
        clone_repo(&cache_dir, git_src, module_name, printer)?;
    }

    let checkout = checkout_ref(&cache_dir, git_src, module_name)?;

    resolve_subdir(checkout, &git_src.subdir, module_name, &git_src.repo_url)
}

/// Whether the cached checkout can already answer `git_src`'s requested ref
/// with no network round-trip.
///
/// True only for a ref that is a full 40-character hex object name — an
/// immutable pin, so no fetch could change what it resolves to — whose commit
/// the cache already holds. A branch or tag name is excluded even when the cache
/// holds one: both move upstream, and learning where they moved to is the entire
/// purpose of the fetch.
///
/// "Holds" is `find_commit` on the parsed object id: the object is in this
/// repository's object database AND is a commit — the git2 spelling of
/// `git cat-file -e <sha>^{commit}`, and no shell-out, so nothing here needs the
/// controlled `git_cmd_*` layer. `revparse_single` is deliberately not used: it
/// would also accept an abbreviated id (ambiguous, and not a pin) and would
/// resolve a REF that merely shares the name.
///
/// That last case is why a ref by the same literal name suppresses the skip.
/// [`checkout_ref`] resolves `refs/tags/<name>` and `refs/remotes/origin/<name>`
/// ahead of the bare revision, so a tag or remote branch literally named with 40
/// hex digits would be checked out after this predicate had judged the pin
/// immutable — the one shape where skipping the fetch could change what lands in
/// the working tree.
fn cache_answers_pinned_ref(repo_path: &Path, git_src: &GitSource) -> bool {
    let Some(ref_name) = git_src.requested_ref() else {
        return false;
    };
    if !is_full_object_id(ref_name) {
        return false;
    }
    let Ok(oid) = git2::Oid::from_str(ref_name) else {
        return false;
    };
    let Ok(repo) = git2::Repository::open(repo_path) else {
        return false;
    };
    if named_ref_exists(&repo, ref_name) {
        return false;
    }
    repo.find_commit(oid).is_ok()
}

/// Whether `value` is a full SHA-1 object name — 40 ASCII hex digits.
///
/// The ONE spelling of "this ref is an immutable pin". Three decisions turn on
/// it and none may re-derive it: the fetch short-circuit, the recovery advice a
/// missing pin earns, and which of a lockfile entry's two refs is checked out.
/// An abbreviated id is deliberately NOT one — it is ambiguous, so it is not a
/// pin.
pub(super) fn is_full_object_id(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The two REF namespaces a pinned name is searched in, in the order
/// [`checkout_ref`] searches them, before it falls back to a bare revision.
///
/// The ONE spelling of those namespaces. Three sites ask about them and all
/// three must agree: the checkout itself, the pinned-SHA short-circuit (which
/// stands down when a ref by the same literal name exists) and
/// [`cache_resolves_ref`] (which asks whether the checkout to come will find
/// anything). A fourth spelling elsewhere would decide a fetch on one rule and
/// perform a checkout under another.
fn named_ref_candidates(ref_name: &str) -> [String; 2] {
    [
        format!("refs/tags/{ref_name}"),
        format!("refs/remotes/origin/{ref_name}"),
    ]
}

/// Whether either named ref exists, without resolving a bare revision.
fn named_ref_exists(repo: &git2::Repository, ref_name: &str) -> bool {
    named_ref_candidates(ref_name)
        .iter()
        .any(|name| repo.refname_to_id(name).is_ok())
}

/// `ref_name` resolved the way [`checkout_ref`] resolves it: tag, then remote
/// branch, then bare revision.
fn resolve_ref_object<'r>(
    repo: &'r git2::Repository,
    ref_name: &str,
) -> std::result::Result<git2::Object<'r>, git2::Error> {
    let [tag, remote_branch] = named_ref_candidates(ref_name);
    repo.revparse_single(&tag)
        .or_else(|_| repo.revparse_single(&remote_branch))
        .or_else(|_| repo.revparse_single(ref_name))
}

/// Whether the cached checkout can resolve `git_src`'s requested ref at all —
/// the second half of the refresh window's condition.
///
/// A window alone is not enough to skip a transfer. `cfgd module upgrade` asks
/// for a ref the cache has never seen, in the same process that just cloned the
/// repository at the version being upgraded FROM, and a window keyed on the
/// repository would answer that ask with the transfer that fetched the old
/// version. The window's claim is only ever "this repository's refs were
/// already brought over"; a ref the cache cannot name is proof that claim does
/// not cover what this caller needs.
///
/// Resolution mirrors [`checkout_ref`] exactly — `refs/tags/<name>`, then
/// `refs/remotes/origin/<name>`, then the bare revision — because the question
/// is precisely "will the checkout that follows find this?". A source with no
/// ref pinned names nothing a transfer has to deliver — it follows wherever its
/// remote-tracking branch already points — so for that one the window alone
/// decides, and inside it the checkout advances only as far as the last
/// transfer reached.
fn cache_resolves_ref(repo_path: &Path, git_src: &GitSource) -> bool {
    let Some(ref_name) = git_src.requested_ref() else {
        return true;
    };
    let Ok(repo) = git2::Repository::open(repo_path) else {
        return false;
    };
    resolve_ref_object(&repo, ref_name).is_ok()
}

/// Repository URLs whose refs were transferred in this process, and when.
///
/// Keyed by URL rather than by (URL, ref) because the transfer is not per-ref:
/// both fetch paths below use the remote's configured refspecs, so one
/// `git fetch origin` brings every branch and tag the remote offers. A second
/// ref of the same repository therefore has nothing left to learn.
static REPO_REFRESHES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::OnceLock::new();

/// Ceiling on how many repositories the window remembers at once, matching
/// [`crate::providers`]'s version memo and `command_path`'s. Losing the map
/// costs at most one redundant fetch per repository still in play, which is
/// exactly what the process did before the window existed.
const REPO_REFRESH_MEMO_CAP: usize = 1024;

/// How long a transfer stands for before the same repository is fetched again.
///
/// The window is what makes "once per run" true without a run being threaded
/// through every resolver: a CLI invocation resolves its modules in one tight
/// pass, so every file of every module sharing a repository lands inside it. It
/// is a ceiling, not a cache lifetime — a daemon ticking on any interval longer
/// than this refreshes on every tick, so a module tracking a mutable ref still
/// converges. Thirty seconds, matching [`crate::command_path`]'s memo and the
/// installed-enumeration memo, for the same reason.
const REPO_REFRESH_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Millisecond override of [`REPO_REFRESH_TTL`], or [`u64::MAX`] for "no
/// override", so a test never depends on wall time: a test whose claim is that
/// one transfer served two asks pins the window out of reach, and one whose
/// claim is that a fetch really transfers refs pins it to zero.
#[cfg(any(test, feature = "test-helpers"))]
static REPO_REFRESH_TTL_OVERRIDE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

/// How long a transfer stands for, honouring the test override.
fn repo_refresh_ttl() -> std::time::Duration {
    #[cfg(any(test, feature = "test-helpers"))]
    {
        let millis = REPO_REFRESH_TTL_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed);
        if millis != u64::MAX {
            return std::time::Duration::from_millis(millis);
        }
    }
    REPO_REFRESH_TTL
}

/// Pin the refresh window, or hand back the default with `None`. Returns what
/// was pinned before, so a guard can put it back.
///
/// Reach for it through `test_helpers::GitRefreshWindowGuard`, never directly.
#[cfg(any(test, feature = "test-helpers"))]
pub(crate) fn set_repo_refresh_ttl_override(millis: Option<u64>) -> Option<u64> {
    let prior = REPO_REFRESH_TTL_OVERRIDE.swap(
        millis.unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    (prior != u64::MAX).then_some(prior)
}

fn repo_refreshes()
-> std::sync::MutexGuard<'static, std::collections::HashMap<String, std::time::Instant>> {
    // A poisoned lock still holds usable bookkeeping: a panic elsewhere is no
    // reason to start fetching the same repository once per declared file.
    REPO_REFRESHES
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Whether `repo_url`'s refs were already transferred inside the current window.
fn repo_refreshed_recently(repo_url: &str) -> bool {
    let ttl = repo_refresh_ttl();
    repo_refreshes()
        .get(repo_url)
        .is_some_and(|at| at.elapsed() < ttl)
}

/// Record that `repo_url`'s refs are now as current as a transfer can make them.
///
/// An expired entry can never be read again — [`repo_refreshed_recently`] tests
/// the age before it answers — so recording is also where the dead ones go. The
/// cap is the backstop for the one shape pruning does not cover: a process that
/// keeps fetching new repositories faster than the window retires the old ones.
fn record_repo_refresh(repo_url: &str) {
    record_repo_refresh_locked(&mut repo_refreshes(), repo_url);
}

/// The body of [`record_repo_refresh`], operating on an already-held lock so a
/// test can record and observe inside ONE critical section — the map is
/// process-global, and any concurrent unpinned fetch prunes under this test's
/// own zero-TTL pin, so an assertion made after the lock is released is a claim
/// about scheduling rather than about the prune.
fn record_repo_refresh_locked(
    map: &mut std::collections::HashMap<String, std::time::Instant>,
    repo_url: &str,
) {
    let ttl = repo_refresh_ttl();
    map.retain(|_, at| at.elapsed() < ttl);
    if map.len() >= REPO_REFRESH_MEMO_CAP {
        map.clear();
    }
    map.insert(repo_url.to_string(), std::time::Instant::now());
}

/// Open a git2 repo with a consistent error mapping.
pub(super) fn open_repo(path: &Path, module: &str, url: &str) -> Result<git2::Repository> {
    git2::Repository::open(path).map_err(|e| {
        ModuleError::GitFetchFailed {
            module: module.to_string(),
            url: url.to_string(),
            message: format!("cannot open repo: {e}"),
        }
        .into()
    })
}

/// Build fetch options with SSH credential callback.
fn git_fetch_options<'a>() -> git2::FetchOptions<'a> {
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(crate::git_ssh_credentials);
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);
    fetch_opts
}

pub(super) fn clone_repo(
    dest: &Path,
    git_src: &GitSource,
    module_name: &str,
    printer: &crate::output::Printer,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("cannot create cache directory: {e}"),
        })?;
    }

    // Try git CLI first with live progress output.
    let mut cmd = crate::git_cmd_safe(Some(&git_src.repo_url), None);
    // absolute-path-ok: git argv, which takes the absolute path
    cmd.args(["clone", &git_src.repo_url, &dest.display().to_string()]);

    // Silent on success, like every other transfer cfgd narrates: the caller
    // already names the module, so a settled row prints the owner twice on two
    // consecutive lines. The libgit2 arm below settles a failure.
    let label = format!("Cloning module:{}", module_name);
    let cli_result = printer.run_silent(&mut cmd, &label);
    if matches!(&cli_result, Ok(output) if output.status.success()) {
        // A fresh clone transferred every ref the remote offers, so the next
        // source out of this repository has nothing to fetch.
        record_repo_refresh(&git_src.repo_url);
        return Ok(());
    }

    // Clean up partial clone before libgit2 retry.
    let _ = std::fs::remove_dir_all(dest);
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Fall back to libgit2 with spinner.
    let spinner = printer.spinner(format!("Cloning module:{} (libgit2)", module_name));

    let result = git2::build::RepoBuilder::new()
        .fetch_options(git_fetch_options())
        .clone(&git_src.repo_url, dest)
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: e.to_string(),
        });

    match &result {
        Ok(_) => {
            let _ = spinner.finish_ok(format!("Cloned module '{}' (libgit2)", module_name));
        }
        Err(e) => {
            let _ = spinner
                .finish_fail(format!(
                    "Failed to clone module '{}' (libgit2)",
                    module_name
                ))
                .detail(crate::output::collapse_to_subject_line(e));
        }
    }
    result?;

    record_repo_refresh(&git_src.repo_url);
    Ok(())
}

/// Transfer `git_src`'s repository refs into the cached checkout, at most once
/// per repository per refresh window.
///
/// The window is what collapses N declared sources out of one repository into
/// one transfer: `resolve_module_files` calls through here once per file entry,
/// `load_locked_modules` once per locked entry, and a registry scan once per
/// registry — all of which routinely name the same repository. See
/// [`REPO_REFRESH_TTL`] for why the bound is a window rather than a run.
///
/// Both halves of the condition are required. The window says the repository's
/// refs were brought over; [`cache_resolves_ref`] says they cover what THIS
/// caller asked for. Skipping on the window alone breaks `cfgd module upgrade`,
/// which asks one process for a ref published after that process cloned.
pub(super) fn fetch_existing_repo(
    repo_path: &Path,
    git_src: &GitSource,
    module_name: &str,
    printer: &crate::output::Printer,
) -> Result<()> {
    if repo_refreshed_recently(&git_src.repo_url) && cache_resolves_ref(repo_path, git_src) {
        return Ok(());
    }

    // Try git CLI first with live progress output.
    let mut cmd = crate::git_cmd_safe(Some(&git_src.repo_url), None);
    // absolute-path-ok: git argv, which takes the absolute path
    cmd.args(["-C", &repo_path.display().to_string(), "fetch", "origin"]);

    // Silent on success, for the reason the clone above is.
    let label = format!("Fetching module:{}", module_name);
    let cli_result = printer.run_silent(&mut cmd, &label);
    if matches!(&cli_result, Ok(output) if output.status.success()) {
        record_repo_refresh(&git_src.repo_url);
        return Ok(());
    }

    // Fall back to libgit2. `open_repo`/`find_remote` are local handle
    // acquisition, not the fetch the spinner narrates — hoisted above it so
    // an early `?` here never leaves a running spinner behind with nothing
    // left to settle it.
    let repo = open_repo(repo_path, module_name, &git_src.repo_url)?;

    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("no 'origin' remote: {e}"),
        })?;

    let spinner = printer.spinner(format!("Fetching module:{} (libgit2)", module_name));

    let refspecs: Vec<String> = remote
        .refspecs()
        .filter_map(|rs| rs.str().ok().map(String::from))
        .collect();
    let refspec_strs: Vec<&str> = refspecs.iter().map(|s| s.as_str()).collect();

    let fetch_result = remote
        .fetch(&refspec_strs, Some(&mut git_fetch_options()), None)
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("fetch failed: {e}"),
        });

    match &fetch_result {
        Ok(_) => {
            let _ = spinner.finish_ok(format!("Fetched module '{}' (libgit2)", module_name));
        }
        Err(e) => {
            let _ = spinner
                .finish_fail(format!(
                    "Failed to fetch module '{}' (libgit2)",
                    module_name
                ))
                .detail(crate::output::collapse_to_subject_line(e));
        }
    }
    fetch_result?;

    record_repo_refresh(&git_src.repo_url);
    Ok(())
}

/// Why a ref would not resolve once the cache is as current as a transfer can
/// make it, and what the reader can do about it.
///
/// A full object id reaching this point is a commit the remote no longer offers
/// — a force-push or a garbage collection — and a bare `cannot find ref
/// '<40 hex>'` hands the reader a hex string with nothing to do about it. The
/// recovery is the one `verify_lockfile_integrity` already names for the same
/// module in the adjacent failure, so the two paths send the user to the same
/// command.
fn unresolvable_ref_message(ref_name: &str, module_name: &str, e: &git2::Error) -> String {
    if is_full_object_id(ref_name) {
        return format!(
            "pinned commit {ref_name} is no longer in the repository or on its remote (history rewritten or garbage-collected) — re-pin with `cfgd module upgrade {module_name}`: {e}"
        );
    }
    format!("cannot find ref '{ref_name}': {e}")
}

/// The remote-tracking branch an unpinned source follows: `origin/HEAD` when the
/// clone recorded one, and otherwise the counterpart of the branch the clone left
/// checked out.
///
/// The fallback answers from the CURRENT branch, and [`detach_and_checkout`]
/// leaves the checkout detached — where the shorthand is `HEAD` and names no
/// branch. Read once and never written back, that arm is therefore good for
/// exactly one advance, and every later resolve of the same checkout silently
/// stops following upstream. So the answer is recorded as `origin/HEAD`, the
/// same symbolic ref a clone would have written, which makes the first arm
/// answer for the rest of the checkout's life.
fn default_tracking_branch(repo: &git2::Repository) -> Option<String> {
    if let Ok(head) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Ok(resolved) = head.resolve()
        && let Ok(name) = resolved.name()
    {
        return Some(name.to_string());
    }
    let head = repo.head().ok()?;
    let branch = head.shorthand().ok()?;
    let candidate = format!("refs/remotes/origin/{branch}");
    repo.refname_to_id(&candidate).ok()?;
    // Forced: reaching here means the existing `origin/HEAD`, if any, does not
    // resolve, and a dangling one would otherwise refuse the write forever.
    let _ = repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        &candidate,
        true,
        "cfgd: record the branch this checkout follows",
    );
    Some(candidate)
}

/// Move an UNPINNED source's working tree onto what the fetch just brought over.
///
/// A source naming no tag and no ref follows its default branch, and a fetch
/// updates `refs/remotes/origin/*` without touching the working tree — so
/// returning here without moving HEAD left the module's deployed files at
/// whatever commit the ORIGINAL clone landed on, for the life of the checkout.
/// Every later fetch paid the network cost and changed nothing the user could
/// see, which reads exactly like cfgd ignoring their upstream.
///
/// Best-effort by design: a repository whose remote-tracking branch cannot be
/// identified stays where it is rather than failing the resolve, because a fresh
/// clone is already at the right commit and an unpinned source has nothing the
/// user asked for that could be missed.
fn advance_to_default_branch(
    repo: &git2::Repository,
    git_src: &GitSource,
    module_name: &str,
) -> Result<()> {
    let Some(tracking) = default_tracking_branch(repo) else {
        return Ok(());
    };
    let Ok(commit) = repo
        .revparse_single(&tracking)
        .and_then(|obj| obj.peel_to_commit())
    else {
        return Ok(());
    };
    if repo.head().ok().and_then(|h| h.target()) == Some(commit.id()) {
        return Ok(());
    }
    detach_and_checkout(repo, commit.id(), &tracking, git_src, module_name)
}

/// Put the working tree on `commit`, discarding whatever is in it.
///
/// Only an unpinned source moves the clone's working tree; a pinned one is
/// exported to a tree of its own by `export_commit`. `label` is what the caller
/// asked for, and names the failure.
fn detach_and_checkout(
    repo: &git2::Repository,
    commit: git2::Oid,
    label: &str,
    git_src: &GitSource,
    module_name: &str,
) -> Result<()> {
    repo.set_head_detached(commit)
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("cannot detach HEAD to '{label}': {e}"),
        })?;

    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("checkout failed for '{label}': {e}"),
        })?;

    Ok(())
}

/// Materialize `git_src`'s files out of the clone at `repo_path` and return the
/// directory holding them.
///
/// An unpinned source advances the clone's own working tree. A source naming a
/// ref is exported to the tree of the commit that ref resolves to (see
/// [`git_checkout_dir`]), and the clone's working tree is left alone.
fn checkout_ref(repo_path: &Path, git_src: &GitSource, module_name: &str) -> Result<PathBuf> {
    let repo = open_repo(repo_path, module_name, &git_src.repo_url)?;
    mark_clone_in_use(repo_path);

    let Some(ref_name) = git_src.requested_ref() else {
        advance_to_default_branch(&repo, git_src, module_name)?;
        return Ok(repo_path.to_path_buf());
    };

    // Try as a tag first, then as a branch
    let obj = resolve_ref_object(&repo, ref_name).map_err(|e| ModuleError::GitFetchFailed {
        module: module_name.to_string(),
        url: git_src.repo_url.clone(),
        message: unresolvable_ref_message(ref_name, module_name, &e),
    })?;

    // Peel to commit
    let commit = obj
        .peel_to_commit()
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("ref '{ref_name}' does not point to a commit: {e}"),
        })?;

    export_commit(&repo, &commit, repo_path).map_err(|message| {
        ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("cannot check out '{ref_name}': {message}"),
        }
        .into()
    })
}

/// Write `commit`'s tree to its directory under the clone at `clone`, unless
/// that directory already exists.
///
/// The tree is built in a directory unique to this call and renamed into
/// place, so the commit's path only ever shows a complete tree. A tree never
/// changes once written, because its path names the commit it holds: when the
/// rename fails because another run put the tree there first, that tree is
/// used.
fn export_commit(
    repo: &git2::Repository,
    commit: &git2::Commit<'_>,
    clone: &Path,
) -> std::result::Result<PathBuf, String> {
    let dest = pinned_checkout_in(clone, &commit.id().to_string());
    if dest.is_dir() {
        mark_tree_in_use(&dest);
        return Ok(dest);
    }
    let io = |what: &str, path: &Path, e: std::io::Error| {
        format!(
            "{what} {}: {e}",
            crate::fold_home_in_text(&crate::to_posix_string(path))
        )
    };
    let slots = pinned_checkouts_of(clone);
    std::fs::create_dir_all(&slots).map_err(|e| io("cannot create", &slots, e))?;

    let staging = tempfile::Builder::new()
        .prefix(EXPORT_PREFIX)
        .tempdir_in(&slots)
        .map_err(|e| io("cannot create a staging directory in", &slots, e))?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout
        .target_dir(staging.path())
        .update_index(false)
        .force();
    repo.checkout_tree(commit.as_object(), Some(&mut checkout))
        .map_err(|e| e.to_string())?;

    match std::fs::rename(staging.path(), &dest) {
        Ok(()) => {
            // The staging path is gone; nothing is left for the guard to delete.
            let _ = staging.keep();
            mark_tree_in_use(&dest);
            Ok(dest)
        }
        Err(_) if dest.is_dir() => {
            mark_tree_in_use(&dest);
            Ok(dest)
        }
        Err(e) => Err(io("cannot move into", &dest, e)),
    }
}

/// Get the HEAD commit SHA from a git repo.
// absolute-path-ok: the string handed back is a commit id, and every path
// render here fills a `ModuleError` field, which keeps the path a reader can
// act on, as every other returned error does.
pub fn get_head_commit_sha(repo_path: &Path) -> Result<String> {
    let path_str = repo_path.display_posix();
    let repo = open_repo(repo_path, &path_str, &path_str)?;
    let head = repo.head().map_err(|e| ModuleError::GitFetchFailed {
        module: path_str.clone(),
        url: path_str.clone(),
        message: format!("cannot read HEAD: {e}"),
    })?;
    let commit = head
        .peel_to_commit()
        .map_err(|e| ModuleError::GitFetchFailed {
            module: path_str.clone(),
            url: path_str,
            message: format!("HEAD is not a commit: {e}"),
        })?;
    Ok(commit.id().to_string())
}

/// Signature status for a git tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagSignatureStatus {
    /// Lightweight tag — cannot carry a signature.
    LightweightTag,
    /// Annotated tag with no signature.
    Unsigned,
    /// Annotated tag with a GPG/SSH signature present.
    SignaturePresent,
    /// Tag not found.
    TagNotFound,
}

/// Check whether a git tag has a GPG/SSH signature.
///
/// Detects signature presence via git2 (no shell-out required).
/// Full GPG verification (cryptographic check) requires `git tag -v` which
/// calls `gpg`; the CLI layer can do that if desired.
pub fn check_tag_signature(
    repo_path: &Path,
    tag_name: &str,
    module_name: &str,
) -> Result<TagSignatureStatus> {
    let repo = open_repo(repo_path, module_name, "")?;

    let tag_ref = match repo.revparse_single(&format!("refs/tags/{tag_name}")) {
        Ok(obj) => obj,
        Err(_) => return Ok(TagSignatureStatus::TagNotFound),
    };

    let tag = match tag_ref.as_tag() {
        Some(t) => t,
        None => return Ok(TagSignatureStatus::LightweightTag),
    };

    let message = match tag.message().ok().flatten() {
        Some(m) => m,
        None => return Ok(TagSignatureStatus::Unsigned),
    };

    if message.contains("-----BEGIN PGP SIGNATURE-----")
        || message.contains("-----BEGIN SSH SIGNATURE-----")
    {
        Ok(TagSignatureStatus::SignaturePresent)
    } else {
        Ok(TagSignatureStatus::Unsigned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_git_source ---

    #[test]
    fn is_git_source_accepts_https() {
        assert!(is_git_source("https://github.com/user/repo.git"));
    }

    #[test]
    fn is_git_source_accepts_http() {
        assert!(is_git_source("http://example.com/repo"));
    }

    #[test]
    fn is_git_source_accepts_ssh() {
        assert!(is_git_source("ssh://git@github.com/user/repo.git"));
    }

    #[test]
    fn is_git_source_accepts_git_at() {
        assert!(is_git_source("git@github.com:user/repo.git"));
    }

    #[test]
    fn is_git_source_accepts_git_protocol() {
        assert!(is_git_source("git://github.com/user/repo.git"));
    }

    #[test]
    fn is_git_source_rejects_local_git_checkout_directory() {
        // A module file source naming a directory that happens to be a work
        // tree stays a path; only `cfgd init --from` layers that probe on top.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("create .git");
        assert!(!is_git_source(&dir.path().display().to_string()));
    }

    #[test]
    fn is_git_source_rejects_bare_repo_suffix() {
        assert!(!is_git_source("/srv/git/repo.git"));
        assert!(!is_git_source("repo.git"));
    }

    #[test]
    fn is_git_source_rejects_local_path() {
        assert!(!is_git_source("/home/user/dotfiles"));
        assert!(!is_git_source("./local/path"));
        assert!(!is_git_source("relative/path"));
    }

    #[test]
    #[serial_test::serial]
    fn is_git_source_rejects_file_url_by_default() {
        let _guard = crate::test_helpers::EnvVarGuard::unset(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV);
        assert!(!is_git_source("file:///tmp/repo"));
    }

    #[test]
    #[serial_test::serial]
    fn is_git_source_accepts_file_url_when_env_set() {
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        assert!(is_git_source("file:///tmp/repo"));
    }

    // --- parse_git_source ---

    #[test]
    fn parse_plain_https_url() {
        let gs = parse_git_source("https://github.com/user/repo.git").unwrap();
        assert_eq!(gs.repo_url, "https://github.com/user/repo.git");
        assert_eq!(gs.tag, None);
        assert_eq!(gs.git_ref, None);
        assert_eq!(gs.subdir, None);
    }

    #[test]
    fn parse_https_with_tag() {
        let gs = parse_git_source("https://github.com/user/repo.git@v2.1.0").unwrap();
        assert_eq!(gs.repo_url, "https://github.com/user/repo.git");
        assert_eq!(gs.tag.as_deref(), Some("v2.1.0"));
    }

    #[test]
    fn parse_https_with_ref() {
        let gs = parse_git_source("https://github.com/user/repo.git?ref=dev").unwrap();
        assert_eq!(gs.repo_url, "https://github.com/user/repo.git");
        assert_eq!(gs.git_ref.as_deref(), Some("dev"));
        assert_eq!(gs.tag, None);
    }

    #[test]
    fn parse_https_with_subdir() {
        let gs = parse_git_source("https://github.com/user/repo.git//configs/base").unwrap();
        assert_eq!(gs.repo_url, "https://github.com/user/repo.git");
        assert_eq!(gs.subdir.as_deref(), Some("configs/base"));
        assert_eq!(gs.tag, None);
    }

    #[test]
    fn parse_https_with_subdir_and_tag() {
        let gs = parse_git_source("https://github.com/user/repo.git//configs/base@v2.1.0").unwrap();
        assert_eq!(gs.repo_url, "https://github.com/user/repo.git");
        assert_eq!(gs.subdir.as_deref(), Some("configs/base"));
        assert_eq!(gs.tag.as_deref(), Some("v2.1.0"));
    }

    #[test]
    fn parse_ssh_with_tag() {
        let gs = parse_git_source("git@github.com:user/repo.git@v1.0.0").unwrap();
        assert_eq!(gs.repo_url, "git@github.com:user/repo.git");
        assert_eq!(gs.tag.as_deref(), Some("v1.0.0"));
    }

    #[test]
    fn parse_ssh_plain() {
        let gs = parse_git_source("git@github.com:user/repo.git").unwrap();
        assert_eq!(gs.repo_url, "git@github.com:user/repo.git");
        assert_eq!(gs.tag, None);
        assert_eq!(gs.git_ref, None);
    }

    #[test]
    fn parse_ref_with_subdir() {
        let gs = parse_git_source("https://github.com/user/repo.git?ref=dev//subdir").unwrap();
        assert_eq!(gs.repo_url, "https://github.com/user/repo.git");
        assert_eq!(gs.git_ref.as_deref(), Some("dev"));
        assert_eq!(gs.subdir.as_deref(), Some("subdir"));
    }

    #[test]
    fn parse_no_dot_git_with_tag() {
        let gs = parse_git_source("https://github.com/user/repo@v3.0").unwrap();
        assert_eq!(gs.repo_url, "https://github.com/user/repo");
        assert_eq!(gs.tag.as_deref(), Some("v3.0"));
    }

    #[test]
    fn parse_rejects_non_git_url() {
        let err = parse_git_source("/local/path").expect_err("local path rejected");
        let msg = err.to_string();
        assert!(msg.contains("not a git URL"), "got: {msg}");
    }

    // --- git_cache_dir ---

    #[test]
    fn git_cache_dir_is_deterministic() {
        let base = Path::new("/tmp/cache");
        let d1 = git_cache_dir(base, "https://github.com/user/repo.git");
        let d2 = git_cache_dir(base, "https://github.com/user/repo.git");
        assert_eq!(d1, d2);
    }

    #[test]
    fn git_cache_dir_differs_for_different_urls() {
        let base = Path::new("/tmp/cache");
        let d1 = git_cache_dir(base, "https://github.com/user/repo-a.git");
        let d2 = git_cache_dir(base, "https://github.com/user/repo-b.git");
        assert_ne!(d1, d2);
    }

    #[test]
    fn git_cache_dir_uses_first_32_hex_chars() {
        let base = Path::new("/cache");
        let d = git_cache_dir(base, "https://example.com/repo");
        let dir_name = d.file_name().unwrap().to_str().unwrap();
        assert_eq!(dir_name.len(), 32);
        assert!(dir_name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // --- resolve_subdir ---

    #[test]
    fn resolve_subdir_none_returns_base() {
        let base = PathBuf::from("/cache/abc123");
        let result = resolve_subdir(base.clone(), &None, "mod", "url").unwrap();
        assert_eq!(result, base);
    }

    #[test]
    fn resolve_subdir_appends_path() {
        let base = PathBuf::from("/cache/abc123");
        let result =
            resolve_subdir(base.clone(), &Some("configs/base".into()), "mod", "url").unwrap();
        assert_eq!(result, base.join("configs/base"));
    }

    #[test]
    fn resolve_subdir_rejects_traversal() {
        let base = PathBuf::from("/cache/abc123");
        let err = resolve_subdir(base, &Some("../escape".into()), "mod", "url")
            .expect_err("traversal rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("traversal"),
            "error must mention traversal, got: {msg}"
        );
    }

    #[test]
    fn resolve_subdir_accepts_a_subdir_that_names_the_cache_base() {
        // `subdir: "."` is an explicit way to spell the default (the whole
        // repository), so it must resolve rather than trip the
        // names-nothing-of-its-own guard `..` shares.
        let base = PathBuf::from("/cache/abc123");
        for candidate in [".", "./"] {
            let resolved = resolve_subdir(base.clone(), &Some(candidate.into()), "mod", "url")
                .unwrap_or_else(|e| panic!("subdir '{candidate}' must resolve, got: {e}"));
            assert_eq!(
                resolved.components().collect::<Vec<_>>(),
                base.components().collect::<Vec<_>>(),
                "a pure `.` subdir names the repository root itself"
            );
        }
        // A leading `./` is ordinary path-writing and still names something.
        assert_eq!(
            resolve_subdir(base.clone(), &Some("./charts".into()), "mod", "url")
                .expect("./charts is a real subdir"),
            base.join("./charts")
        );
    }

    // --- check_tag_signature (with tempdir git repo) ---

    #[test]
    fn check_tag_signature_returns_tag_not_found() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let result = check_tag_signature(dir.path(), "nonexistent", "test-mod").unwrap();
        assert_eq!(result, TagSignatureStatus::TagNotFound);
    }

    #[test]
    fn check_tag_signature_lightweight_tag() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let obj = repo.find_object(commit_oid, None).unwrap();
        repo.tag_lightweight("v1.0.0", &obj, false).unwrap();

        let result = check_tag_signature(dir.path(), "v1.0.0", "test-mod").unwrap();
        assert_eq!(result, TagSignatureStatus::LightweightTag);
    }

    #[test]
    fn check_tag_signature_annotated_unsigned() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let obj = repo.find_object(commit_oid, None).unwrap();
        repo.tag("v2.0.0", &obj, &sig, "release v2.0.0", false)
            .unwrap();

        let result = check_tag_signature(dir.path(), "v2.0.0", "test-mod").unwrap();
        assert_eq!(result, TagSignatureStatus::Unsigned);
    }

    // --- get_head_commit_sha ---

    #[test]
    fn get_head_commit_sha_returns_hex_hash() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let sha = get_head_commit_sha(dir.path()).unwrap();
        assert_eq!(sha, commit_oid.to_string());
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn get_head_commit_sha_errors_on_non_repo() {
        let dir = tempfile::tempdir().unwrap();
        let err = get_head_commit_sha(dir.path()).expect_err("non-repo must error");
        let msg = err.to_string();
        assert!(
            msg.contains("cannot open repo"),
            "error must mention repo open failure, got: {msg}"
        );
    }

    // --- default_module_cache_dir ---

    // `default_module_cache_dir` reads the process-global `CFGD_CACHE_DIR` above
    // the `with_test_home_guard` thread-local, so a concurrent setter hands this
    // test another test's tempdir.
    #[test]
    #[serial_test::serial]
    fn default_module_cache_dir_with_test_home() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::with_test_home_guard(dir.path());
        let cache = default_module_cache_dir().unwrap();
        assert!(
            cache.starts_with(dir.path()),
            "cache dir must be under test home, got: {}",
            cache.display()
        );
        assert!(
            cache.ends_with("cfgd/modules"),
            "must end with cfgd/modules, got: {}",
            cache.display()
        );
    }

    // --- parse_git_source: SSH @tag with no `.git` suffix ---

    #[test]
    fn parse_ssh_without_dot_git_with_tag() {
        // git@host:user/repo@v9.9.9 — no `.git`, so the @tag handling
        // falls through to the rfind('@') branch with skip_to past the
        // first `@` of the SSH prefix.
        let gs = parse_git_source("git@gitlab.example.com:user/repo@v9.9.9").unwrap();
        assert_eq!(gs.repo_url, "git@gitlab.example.com:user/repo");
        assert_eq!(gs.tag.as_deref(), Some("v9.9.9"));
    }

    #[test]
    fn parse_https_no_dot_git_skips_to_scheme_for_at_lookup() {
        // https with no `.git` and `@v3.0` — exercises the `://` skip path
        // inside the no-`.git` branch.
        let gs = parse_git_source("https://internal.host/proj@v3.0").unwrap();
        assert_eq!(gs.repo_url, "https://internal.host/proj");
        assert_eq!(gs.tag.as_deref(), Some("v3.0"));
    }

    #[test]
    fn parse_url_with_no_at_in_path_returns_no_tag() {
        // No `.git`, no `@` after the scheme — must produce repo_url=full URL,
        // tag=None (the rfind('@') yields the scheme '@' but skip_to filters it).
        let gs = parse_git_source("https://example.com/path/to/repo").unwrap();
        assert_eq!(gs.repo_url, "https://example.com/path/to/repo");
        assert_eq!(gs.tag, None);
    }

    // --- fetch_git_source: local file:// + tag checkout ---

    fn build_local_fixture_repo() -> (tempfile::TempDir, String) {
        let src = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(src.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let _commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        // Tag the initial commit so checkout-by-tag tests have a target.
        let head = repo.head().unwrap().target().unwrap();
        let obj = repo.find_object(head, None).unwrap();
        repo.tag_lightweight("v0.1.0", &obj, false).unwrap();
        let url = crate::test_helpers::file_url(src.path());
        (src, url)
    }

    #[test]
    #[serial_test::serial]
    fn fetch_git_source_clones_then_reuses_existing_cache_on_second_call() {
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let (_src, url) = build_local_fixture_repo();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        let git_src = parse_git_source(&url).unwrap();

        // First call: clone branch.
        let path1 = fetch_git_source(&git_src, cache_base.path(), "fixture", &printer)
            .expect("first fetch must clone successfully");
        assert!(path1.join("HEAD").exists() || path1.join(".git").exists());

        // Second call: fetch-existing branch (the cached dir already has .git/HEAD).
        let path2 = fetch_git_source(&git_src, cache_base.path(), "fixture", &printer)
            .expect("second fetch must reuse cache and succeed");
        assert_eq!(path1, path2, "cached path must be stable across calls");
    }

    #[test]
    #[serial_test::serial]
    fn fetch_git_source_with_tag_checks_out_tag() {
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let (_src, url) = build_local_fixture_repo();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        let url_with_tag = format!("{}@v0.1.0", url);
        let git_src = parse_git_source(&url_with_tag).unwrap();
        assert_eq!(git_src.tag.as_deref(), Some("v0.1.0"));

        let result = fetch_git_source(&git_src, cache_base.path(), "fixture", &printer);
        assert!(
            result.is_ok(),
            "checkout-by-tag against local fixture must succeed: {:?}",
            result.err()
        );
    }

    #[test]
    #[serial_test::serial]
    fn fetch_git_source_with_missing_tag_returns_err() {
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let (_src, url) = build_local_fixture_repo();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        let url_with_tag = format!("{}@no-such-tag", url);
        let git_src = parse_git_source(&url_with_tag).unwrap();

        let err = fetch_git_source(&git_src, cache_base.path(), "fixture", &printer)
            .expect_err("missing tag must error");
        let msg = err.to_string();
        assert!(
            msg.contains("cannot find ref") || msg.contains("no-such-tag"),
            "error must mention missing ref, got: {msg}"
        );
    }

    // --- open_repo: non-repo path error message ---

    #[test]
    fn open_repo_errors_on_non_repo() {
        let dir = tempfile::tempdir().unwrap();
        let result = open_repo(dir.path(), "mod", "url");
        let err = match result {
            Ok(_) => panic!("non-repo must error"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("cannot open repo"),
            "error must mention cannot open repo: {err}"
        );
    }

    // --- check_tag_signature: signed-tag and no-message branches ---

    #[test]
    fn check_tag_signature_signature_present_pgp() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        let obj = repo.find_object(commit_oid, None).unwrap();
        // Embed a fake PGP signature footer inside the tag message — the
        // detector is a substring match, no crypto verification.
        let msg =
            "release v3.0.0\n-----BEGIN PGP SIGNATURE-----\nfake\n-----END PGP SIGNATURE-----\n";
        repo.tag("v3.0.0", &obj, &sig, msg, false).unwrap();
        let result = check_tag_signature(dir.path(), "v3.0.0", "mod").unwrap();
        assert_eq!(result, TagSignatureStatus::SignaturePresent);
    }

    #[test]
    fn check_tag_signature_signature_present_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        let obj = repo.find_object(commit_oid, None).unwrap();
        let msg = "release v4\n-----BEGIN SSH SIGNATURE-----\nfake\n-----END SSH SIGNATURE-----\n";
        repo.tag("v4.0.0", &obj, &sig, msg, false).unwrap();
        let result = check_tag_signature(dir.path(), "v4.0.0", "mod").unwrap();
        assert_eq!(result, TagSignatureStatus::SignaturePresent);
    }

    // --- get_head_commit_sha: empty repo (no HEAD) ---

    #[test]
    fn get_head_commit_sha_returns_err_when_repo_has_no_head() {
        let dir = tempfile::tempdir().unwrap();
        // `git init` without any commits — there's no HEAD yet, so .head() errs.
        git2::Repository::init(dir.path()).unwrap();
        let err = get_head_commit_sha(dir.path()).expect_err("no HEAD must error");
        let msg = err.to_string();
        assert!(
            msg.contains("cannot read HEAD") || msg.contains("cannot open repo"),
            "error must mention HEAD or repo: {msg}"
        );
    }

    // --- BareGitRepo-driven end-to-end tests ---
    //
    // These cover the clone + fetch + checkout + signature-detect pipeline by
    // standing up a bare upstream and a working clone, without ever touching
    // the network. They exercise multiple code paths per test for high
    // coverage leverage.

    #[test]
    #[serial_test::serial]
    fn fetch_git_source_with_bare_repo_branch_checks_out_branch() {
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let bare = crate::test_helpers::BareGitRepo::builder()
            .commit("init", &[("README.md", "hello")])
            .branch("feature", &[("feature.txt", "feature-data")])
            .build();

        let cache_base = tempfile::tempdir().expect("cache tempdir");
        let printer = crate::test_helpers::test_printer();

        // Use ?ref=feature so the checkout_ref branch lookup hits the
        // `refs/remotes/origin/<branch>` arm after the tag-lookup misses.
        let url_with_ref = format!("{}?ref=feature", bare.url());
        let git_src = parse_git_source(&url_with_ref).expect("parse ref url");
        assert_eq!(git_src.git_ref.as_deref(), Some("feature"));

        let path = fetch_git_source(&git_src, cache_base.path(), "branchy", &printer)
            .expect("fetch with branch checkout must succeed");

        assert!(path.join("feature.txt").exists(), "branch file must exist");
        assert_eq!(
            std::fs::read_to_string(path.join("feature.txt")).unwrap(),
            "feature-data"
        );
    }

    #[test]
    #[serial_test::serial]
    fn fetch_git_source_with_bare_repo_tag_checks_out_tag() {
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let bare = crate::test_helpers::BareGitRepo::builder()
            .commit("first", &[("a.txt", "first content")])
            .tag("v1.0.0")
            .build();

        let cache_base = tempfile::tempdir().expect("cache tempdir");
        let printer = crate::test_helpers::test_printer();

        let url_with_tag = format!("{}@v1.0.0", bare.url());
        let git_src = parse_git_source(&url_with_tag).expect("parse tag url");
        assert_eq!(git_src.tag.as_deref(), Some("v1.0.0"));

        let path = fetch_git_source(&git_src, cache_base.path(), "tagged", &printer)
            .expect("fetch with tag checkout must succeed");
        assert!(path.join("a.txt").exists());

        // Subsequent call hits the fetch_existing_repo branch.
        let path2 = fetch_git_source(&git_src, cache_base.path(), "tagged", &printer)
            .expect("second fetch (fetch_existing_repo path) must succeed");
        assert_eq!(path, path2);
    }

    #[test]
    fn check_tag_signature_returns_unsigned_when_tag_has_no_message() {
        // Build an annotated tag with an empty message. git2 lets us craft
        // a tag with no message bytes, which exercises the `tag.message()` ->
        // None branch (returns `Unsigned`).
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        let obj = repo.find_object(commit_oid, None).unwrap();
        // Annotate with a single space — git2 requires a non-empty msg but our
        // detector treats it as unsigned (no PGP / SSH header).
        repo.tag("vNoSig", &obj, &sig, " ", false).unwrap();

        let result = check_tag_signature(dir.path(), "vNoSig", "mod").unwrap();
        assert_eq!(result, TagSignatureStatus::Unsigned);
    }

    #[test]
    #[serial_test::serial]
    fn default_module_cache_dir_test_home_uses_home_join() {
        // Confirms the test-home branch composes the path correctly.
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::with_test_home_guard(dir.path());
        let cache = default_module_cache_dir().expect("default_module_cache_dir under test-home");
        assert_eq!(
            cache,
            dir.path().join(".cache").join("cfgd").join("modules")
        );
    }

    // --- fetch_existing_repo: actually transfers new upstream refs ---

    #[test]
    #[serial_test::serial]
    fn fetch_existing_repo_pulls_new_tag_added_after_clone() {
        // Proves fetch_existing_repo is not a no-op: a tag created on the bare
        // upstream *after* the initial clone becomes resolvable on the second
        // fetch, so a checkout against it succeeds. If fetch silently did
        // nothing, the second checkout would fail with "cannot find ref".
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        // Deliberately unpinned: the refresh window is open from the clone, and
        // the second ask still has to transfer because the tag it names is one
        // the cache cannot resolve.
        let bare = crate::test_helpers::BareGitRepo::builder()
            .commit("init", &[("a.txt", "v1")])
            .build();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        // First fetch: clone, no ref pinned (stays on default branch).
        let plain = parse_git_source(&bare.url()).unwrap();
        fetch_git_source(&plain, cache_base.path(), "evolving", &printer)
            .expect("initial clone must succeed");

        // Now add a brand-new lightweight tag to the bare upstream pointing at
        // its current HEAD. The cached clone does not know about it yet.
        let bare_repo = git2::Repository::open_bare(bare.path()).unwrap();
        let head_oid = bare_repo
            .refname_to_id(&format!("refs/heads/{}", bare.head_branch()))
            .unwrap();
        let head_obj = bare_repo.find_object(head_oid, None).unwrap();
        bare_repo
            .tag_lightweight("v-added-later", &head_obj, false)
            .unwrap();
        assert!(bare.has_tag("v-added-later"));

        // Second fetch pinned to the new tag: only succeeds if fetch_existing_repo
        // actually transferred the new ref into the cache.
        let pinned = parse_git_source(&format!("{}@v-added-later", bare.url())).unwrap();
        let path = fetch_git_source(&pinned, cache_base.path(), "evolving", &printer)
            .expect("second fetch must transfer the new tag and check it out");
        assert!(
            path.join("a.txt").exists(),
            "checked-out tree must contain the committed file"
        );
        assert_eq!(std::fs::read_to_string(path.join("a.txt")).unwrap(), "v1");
    }

    // --- pinned-SHA short-circuit and per-repository transfer window ---
    //
    // Both are proven the same way, and without a shim: the fixture's upstream
    // is REMOVED after the cache is materialized, so any attempt to transfer
    // from it fails loudly. A call that still succeeds is a call that did not
    // reach the network — a stronger claim than a fetch count, which cannot
    // distinguish a fetch that ran and found nothing.

    #[test]
    #[serial_test::serial]
    fn a_pinned_sha_the_cache_already_holds_is_never_fetched_again() {
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        // The per-repository window is pinned SHUT, so the pin itself is the
        // only thing that can spare the second call its fetch.
        let _window = crate::test_helpers::GitRefreshWindowGuard::always_expired();
        let mut bare = crate::test_helpers::BareGitRepo::builder()
            .commit("init", &[("a.txt", "v1")])
            .build();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        let plain = parse_git_source(&bare.url()).unwrap();
        let path = fetch_git_source(&plain, cache_base.path(), "pinned", &printer)
            .expect("initial clone must succeed");
        let sha = get_head_commit_sha(&git_cache_dir(cache_base.path(), &plain.repo_url))
            .expect("cached checkout has a HEAD");
        assert_eq!(sha.len(), 40, "the pin under test must be a full object id");

        bare.remove_upstream();

        let pinned = parse_git_source(&format!("{}@{sha}", bare.url())).unwrap();
        assert_eq!(pinned.tag.as_deref(), Some(sha.as_str()));
        let again = fetch_git_source(&pinned, cache_base.path(), "pinned", &printer)
            .expect("a pinned SHA the cache already holds must resolve without the remote");
        assert_ne!(again, path, "a pinned ref reads its own checkout");
        assert_eq!(
            again,
            git_checkout_dir(cache_base.path(), &plain.repo_url, Some(&sha)).unwrap()
        );
        assert_eq!(std::fs::read_to_string(again.join("a.txt")).unwrap(), "v1");
    }

    #[test]
    #[serial_test::serial]
    fn a_tag_pin_is_still_refreshed_because_a_tag_can_move() {
        // The narrow half of the same claim: only a full object id is immutable,
        // so a tag pin keeps its transfer even when the cache can already resolve
        // the name. Without this, the short-circuit widening to "any ref the
        // cache knows" would go unnoticed.
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let _window = crate::test_helpers::GitRefreshWindowGuard::always_expired();
        let mut bare = crate::test_helpers::BareGitRepo::builder()
            .commit("init", &[("a.txt", "v1")])
            .tag("v1.0.0")
            .build();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        let tagged = parse_git_source(&format!("{}@v1.0.0", bare.url())).unwrap();
        fetch_git_source(&tagged, cache_base.path(), "tagged", &printer)
            .expect("initial clone must succeed");

        bare.remove_upstream();

        let err = fetch_git_source(&tagged, cache_base.path(), "tagged", &printer)
            .expect_err("a tag pin must still be refreshed against the remote");
        assert!(
            err.to_string().contains("fetch"),
            "the failure must be the refused transfer, got: {err}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn one_repository_is_transferred_once_however_many_sources_name_it() {
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let _window = crate::test_helpers::GitRefreshWindowGuard::never_expires();
        let mut bare = crate::test_helpers::BareGitRepo::builder()
            .commit("init", &[("a.txt", "v1")])
            .tag("v1.0.0")
            .branch("feature", &[("f.txt", "f")])
            .build();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        // Both refs are MUTABLE, so the pinned-SHA short-circuit cannot be what
        // spares the second source its transfer.
        let first = parse_git_source(&format!("{}@v1.0.0", bare.url())).unwrap();
        fetch_git_source(&first, cache_base.path(), "one", &printer)
            .expect("the first source out of the repository clones it");

        bare.remove_upstream();

        let second = parse_git_source(&format!("{}?ref=feature", bare.url())).unwrap();
        let path = fetch_git_source(&second, cache_base.path(), "two", &printer).expect(
            "a second source out of an already-transferred repository must not fetch again",
        );
        assert!(
            path.join("f.txt").exists(),
            "the branch ref must still check out from the cache"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_ref_the_cache_has_never_seen_is_transferred_even_inside_the_window() {
        // `cfgd module upgrade` is this shape: one process clones the
        // repository at the version being upgraded FROM, then asks for a
        // version that did not exist when it did. A window keyed on the
        // repository alone answers the second ask with the first transfer, and
        // the upgrade fails with "cannot find ref".
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let _window = crate::test_helpers::GitRefreshWindowGuard::never_expires();
        let bare = crate::test_helpers::BareGitRepo::builder()
            .commit("init", &[("a.txt", "v1")])
            .tag("v1.0.0")
            .build();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        let v1 = parse_git_source(&format!("{}@v1.0.0", bare.url())).unwrap();
        fetch_git_source(&v1, cache_base.path(), "upgrading", &printer)
            .expect("the first version clones the repository");

        // The upstream publishes a version the cached checkout has never heard
        // of, exactly as it would between an install and an upgrade.
        let bare_repo = git2::Repository::open_bare(bare.path()).unwrap();
        let head_oid = bare_repo
            .refname_to_id(&format!("refs/heads/{}", bare.head_branch()))
            .unwrap();
        let head_obj = bare_repo.find_object(head_oid, None).unwrap();
        bare_repo
            .tag_lightweight("v2.0.0", &head_obj, false)
            .unwrap();

        let v2 = parse_git_source(&format!("{}@v2.0.0", bare.url())).unwrap();
        fetch_git_source(&v2, cache_base.path(), "upgrading", &printer)
            .expect("a ref the cache cannot resolve must be transferred, window open or not");
    }

    #[test]
    #[serial_test::serial]
    fn an_unpinned_source_advances_to_what_the_fetch_brought_over() {
        // A source naming no tag and no ref follows its default branch. A fetch
        // updates refs/remotes/origin/* without touching the working tree, so a
        // resolve that stopped there left the module's deployed files at the
        // commit the ORIGINAL clone landed on forever — every later run paid the
        // transfer and showed the user nothing for it.
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let _window = crate::test_helpers::GitRefreshWindowGuard::always_expired();
        let bare = crate::test_helpers::BareGitRepo::builder()
            .commit("init", &[("a.txt", "v1")])
            .build();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        let src = parse_git_source(&bare.url()).unwrap();
        assert!(
            src.tag.is_none() && src.git_ref.is_none(),
            "the fixture must be unpinned for this claim to be about the default branch"
        );
        let path = fetch_git_source(&src, cache_base.path(), "tracking", &printer)
            .expect("the first resolve clones the repository");
        assert_eq!(std::fs::read_to_string(path.join("a.txt")).unwrap(), "v1");

        bare.publish_commit("move the branch on", &[("a.txt", "v2")]);

        let again = fetch_git_source(&src, cache_base.path(), "tracking", &printer)
            .expect("the second resolve fetches the moved branch");
        assert_eq!(
            std::fs::read_to_string(again.join("a.txt")).unwrap(),
            "v2",
            "an unpinned source must deploy the commit the fetch brought over"
        );
    }

    #[test]
    fn an_unpinned_checkout_keeps_following_its_branch_after_the_first_advance() {
        // The advance detaches HEAD, and a detached HEAD has no branch
        // shorthand — so the arm that recovers the tracking branch from the
        // checked-out branch can answer exactly once unless what it found is
        // written down. Driven directly against the refs, with no clone and no
        // transport: whether a given git build re-creates `origin/HEAD` on
        // fetch is not what this claim is about, and a checkout that has lost
        // it must keep following its branch either way.
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let first = repo
            .commit(Some("HEAD"), &sig, &sig, "first", &tree, &[])
            .unwrap();
        let first_commit = repo.find_commit(first).unwrap();
        let second = repo
            .commit(None, &sig, &sig, "second", &tree, &[&first_commit])
            .unwrap();
        let second_commit = repo.find_commit(second).unwrap();
        let third = repo
            .commit(None, &sig, &sig, "third", &tree, &[&second_commit])
            .unwrap();

        // A remote-tracking branch one commit ahead of the checked-out branch,
        // and deliberately no `origin/HEAD`.
        let branch = repo.head().unwrap().shorthand().unwrap().to_string();
        let tracking = format!("refs/remotes/origin/{branch}");
        repo.reference(&tracking, second, true, "fixture").unwrap();

        let git_src = GitSource {
            repo_url: "file:///fixture".to_string(),
            tag: None,
            git_ref: None,
            subdir: None,
        };

        advance_to_default_branch(&repo, &git_src, "tracking").expect("the first advance");
        assert!(
            repo.head_detached().unwrap(),
            "the advance must detach HEAD, or this test proves nothing"
        );
        assert_eq!(repo.head().unwrap().target().unwrap(), second);

        repo.reference(&tracking, third, true, "upstream moved")
            .unwrap();
        advance_to_default_branch(&repo, &git_src, "tracking").expect("the second advance");
        assert_eq!(
            repo.head().unwrap().target().unwrap(),
            third,
            "a checkout must keep following its branch after it has been detached once"
        );
    }

    #[test]
    #[serial_test::serial]
    fn the_transfer_window_is_what_spares_the_second_source_its_fetch() {
        // The control for the test above: with the window pinned shut the same
        // sequence fails, which is also what proves the removed upstream really
        // does refuse a transfer rather than quietly succeeding.
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let _window = crate::test_helpers::GitRefreshWindowGuard::always_expired();
        let mut bare = crate::test_helpers::BareGitRepo::builder()
            .commit("init", &[("a.txt", "v1")])
            .tag("v1.0.0")
            .branch("feature", &[("f.txt", "f")])
            .build();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        let first = parse_git_source(&format!("{}@v1.0.0", bare.url())).unwrap();
        fetch_git_source(&first, cache_base.path(), "one", &printer)
            .expect("the first source out of the repository clones it");

        bare.remove_upstream();

        let second = parse_git_source(&format!("{}?ref=feature", bare.url())).unwrap();
        fetch_git_source(&second, cache_base.path(), "two", &printer)
            .expect_err("with the window pinned shut the second source must attempt a transfer");
    }

    #[test]
    fn the_window_forgets_a_repository_it_can_no_longer_answer_for() {
        // An entry past the window is unreadable — `repo_refreshed_recently`
        // checks the age before it answers — so a map that kept one would be
        // holding a URL for the life of the process to say nothing with. A
        // daemon fetching from many repositories is where that accumulates.
        //
        // Asserted on this test's OWN keys, never on the map's size: the map is
        // process-global and every fetch test in the binary writes to it, so a
        // length is a claim about what the rest of the suite was doing.
        let _window = crate::test_helpers::GitRefreshWindowGuard::always_expired();
        let stale = "https://example.invalid/forgets-stale.git";
        let fresh = "https://example.invalid/forgets-fresh.git";
        // One critical section: under this test's own zero-TTL pin, any
        // concurrent unpinned fetch's record would prune `fresh` the moment
        // the lock was released, so both records and both reads happen under
        // a single hold.
        let mut map = repo_refreshes();
        record_repo_refresh_locked(&mut map, stale);
        record_repo_refresh_locked(&mut map, fresh);
        assert!(
            !map.contains_key(stale),
            "an expired entry must be dropped, not carried"
        );
        assert!(
            map.contains_key(fresh),
            "the entry just recorded must survive its own prune"
        );
    }

    #[test]
    fn the_window_never_grows_past_its_ceiling() {
        // The prune above cannot bound a process that reaches new repositories
        // faster than the window retires the old ones, so the cap is what makes
        // the map's size independent of how long the process runs.
        let _window = crate::test_helpers::GitRefreshWindowGuard::never_expires();
        let filler = |i: usize| format!("https://example.invalid/ceiling-{i}.git");
        {
            let mut map = repo_refreshes();
            map.clear();
            for i in 0..REPO_REFRESH_MEMO_CAP {
                map.insert(filler(i), std::time::Instant::now());
            }
        }
        record_repo_refresh("https://example.invalid/ceiling-one-too-many.git");
        let map = repo_refreshes();
        // The property, not the size: with the window pinned open nothing else
        // can retire these, so their absence is the clear and nothing else.
        assert!(
            !map.contains_key(&filler(0)) && !map.contains_key(&filler(REPO_REFRESH_MEMO_CAP - 1)),
            "reaching the ceiling must clear the entries already held"
        );
        assert!(
            map.contains_key("https://example.invalid/ceiling-one-too-many.git"),
            "the entry that reached the ceiling must be the one kept"
        );
    }

    // --- export_commit and the sweep: one whole tree per commit ---

    fn commit_one_file(repo: &git2::Repository, content: &str) -> git2::Oid {
        let workdir = repo.workdir().unwrap().to_path_buf();
        std::fs::write(workdir.join("payload.txt"), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("payload.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let parents: Vec<git2::Commit<'_>> = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, content, &tree, &parent_refs)
            .unwrap()
    }

    fn payload_of(tree: &Path) -> String {
        let read = std::fs::read_to_string(tree.join("payload.txt")).unwrap();
        crate::normalize_line_endings(&read).into_owned()
    }

    fn slot_names(clone: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(pinned_checkouts_of(clone))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn concurrent_exports_of_one_commit_each_end_on_a_whole_tree() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        const RUNS: usize = 8;
        let mut exported = Vec::new();
        for round in 0..6 {
            let content = format!("round {round} bytes\n");
            let commit = commit_one_file(&repo, &content);
            let dest = pinned_checkout_in(dir.path(), &commit.to_string());
            let barrier = std::sync::Barrier::new(RUNS);
            let results: Vec<_> = std::thread::scope(|s| {
                let handles: Vec<_> = (0..RUNS)
                    .map(|_| {
                        s.spawn(|| {
                            let repo = git2::Repository::open(dir.path()).unwrap();
                            let commit = repo.find_commit(commit).unwrap();
                            barrier.wait();
                            let landed = export_commit(&repo, &commit, dir.path());
                            // Each run reads the tree it was handed while the
                            // others may still be landing theirs.
                            let read = landed.as_deref().ok().map(payload_of);
                            (landed, read)
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for (landed, read) in results {
                assert_eq!(landed.as_deref(), Ok(dest.as_path()), "round {round}");
                assert_eq!(read.as_deref(), Some(content.as_str()), "round {round}");
            }
            exported.push(commit.to_string());
            assert_eq!(
                slot_names(dir.path()),
                {
                    let mut sorted = exported.clone();
                    sorted.sort();
                    sorted
                },
                "one tree per commit and no staging left behind"
            );
        }
    }

    #[test]
    fn a_moved_ref_lands_in_a_new_tree_and_the_old_tree_stays() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let first = commit_one_file(&repo, "first\n");
        repo.tag_lightweight("v", &repo.find_object(first, None).unwrap(), false)
            .unwrap();
        let source = GitSource {
            repo_url: "https://example.invalid/moved.git".into(),
            tag: Some("v".into()),
            git_ref: None,
            subdir: None,
        };
        let old = checkout_ref(dir.path(), &source, "moved").unwrap();
        assert_eq!(old, pinned_checkout_in(dir.path(), &first.to_string()));

        let second = commit_one_file(&repo, "second\n");
        repo.tag_lightweight("v", &repo.find_object(second, None).unwrap(), true)
            .unwrap();
        let new = checkout_ref(dir.path(), &source, "moved").unwrap();
        assert_eq!(new, pinned_checkout_in(dir.path(), &second.to_string()));
        assert_eq!(payload_of(&new), "second\n");
        assert_eq!(
            payload_of(&old),
            "first\n",
            "a tree something may still link into is left as it was"
        );
    }

    #[test]
    fn a_ref_resolves_to_the_tree_of_its_commit() {
        let cache_base = tempfile::tempdir().unwrap();
        let url = "https://example.invalid/resolve.git";
        let clone = git_cache_dir(cache_base.path(), url);
        let repo = git2::Repository::init(&clone).unwrap();
        let commit = commit_one_file(&repo, "bytes\n");
        repo.tag_lightweight("v1", &repo.find_object(commit, None).unwrap(), false)
            .unwrap();
        let tree = pinned_checkout_in(&clone, &commit.to_string());
        assert_eq!(
            git_checkout_dir(cache_base.path(), url, Some("v1")).unwrap(),
            tree
        );
        let upper = commit.to_string().to_ascii_uppercase();
        assert_eq!(
            git_checkout_dir(cache_base.path(), url, Some(&upper)).unwrap(),
            tree,
            "an object id names one tree whatever its case"
        );
        assert!(
            git_checkout_dir(cache_base.path(), url, Some("no-such-tag")).is_err(),
            "a ref the clone cannot resolve names no tree"
        );
    }

    /// A clone under `cache_base` holding the trees of two commits, plus the
    /// leftovers a sweep must deal with. Returns the clone and both trees.
    fn clone_with_two_trees(cache_base: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let clone = git_cache_dir(cache_base, "https://example.invalid/sweep.git");
        let repo = git2::Repository::init(&clone).unwrap();
        let mut trees = Vec::new();
        for content in ["kept\n", "unread\n"] {
            let id = commit_one_file(&repo, content);
            trees.push(export_commit(&repo, &repo.find_commit(id).unwrap(), &clone).unwrap());
        }
        let slots = pinned_checkouts_of(&clone);
        for leftover in [".stale-left", ".export-busy"] {
            std::fs::create_dir_all(slots.join(leftover).join("sub")).unwrap();
            std::fs::write(slots.join(leftover).join("sub").join("f"), "x").unwrap();
        }
        // A tree an earlier release laid out under a hash of the ref name.
        let earlier = slots.join("0123456789abcdef0123456789abcdef");
        std::fs::create_dir_all(earlier.join("tree")).unwrap();
        std::fs::write(earlier.join("commit"), "deadbeef").unwrap();
        let unread = trees.pop().unwrap();
        (clone, trees.pop().unwrap(), unread)
    }

    /// Sweep `cache_base` keeping `kept`, with the window every apply uses.
    fn sweep(cache_base: &Path, kept: &[&Path]) -> Vec<String> {
        let mut keep = KeptCheckouts::new(cache_base);
        for path in kept {
            assert!(keep.keep(path), "{} lies under the cache", path.display());
        }
        sweep_pinned_checkouts(&keep, IN_USE_WINDOW)
    }

    /// Move the modification time of the directory at `dir`, and of the
    /// in-use stamp of the tree or clone it is, back past [`IN_USE_WINDOW`].
    fn backdate(dir: &Path) {
        let then = std::time::SystemTime::now() - IN_USE_WINDOW - IN_USE_WINDOW;
        crate::test_helpers::set_dir_modified(dir, then);
        for stamp in [Some(clone_stamp(dir)), tree_stamp(dir)]
            .into_iter()
            .flatten()
        {
            if stamp.is_file() {
                std::fs::File::options()
                    .write(true)
                    .open(&stamp)
                    .unwrap()
                    .set_modified(then)
                    .unwrap();
            }
        }
    }

    fn modified(path: &Path) -> std::time::SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    fn is_young(dir: &Path, stamp: &Path) -> bool {
        used_within(dir, Some(stamp), IN_USE_WINDOW).unwrap()
    }

    #[test]
    fn a_sweep_removes_every_tree_nothing_reads_and_keeps_the_rest() {
        let cache_base = tempfile::tempdir().unwrap();
        let (clone, kept, unread) = clone_with_two_trees(cache_base.path());
        let slots = pinned_checkouts_of(&clone);
        let kept_name = kept.file_name().unwrap().to_str().unwrap().to_string();
        let mut all = slot_names(&clone);
        all.retain(|name| name != ".stale-left");

        let failures = sweep(cache_base.path(), &[]);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(
            slot_names(&clone),
            all,
            "every tree written within the window stays, kept or not; only the \
             stale leftover goes"
        );

        for name in &all {
            backdate(&slots.join(name));
        }
        let (kept_stamp, unread_stamp) = (tree_stamp(&kept).unwrap(), tree_stamp(&unread).unwrap());
        assert!(
            kept_stamp.is_file() && unread_stamp.is_file(),
            "an export stamps its tree"
        );
        let failures = sweep(cache_base.path(), &[&kept.join("payload.txt")]);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(
            slot_names(&clone),
            vec![kept_name],
            "the unread tree, the earlier layout and the abandoned export are gone"
        );
        assert!(!unread.exists());
        assert!(
            !unread_stamp.exists(),
            "a removed tree takes its in-use stamp with it"
        );
        assert!(kept_stamp.is_file(), "a kept tree keeps its in-use stamp");
        assert_eq!(payload_of(&kept), "kept\n");
        assert!(clone.join(".git").is_dir(), "a clone holding a tree stays");
    }

    #[test]
    fn handing_out_a_tree_protects_it_and_its_clone_for_the_window() {
        let cache_base = tempfile::tempdir().unwrap();
        let url = "https://example.invalid/handout.git";
        let clone = git_cache_dir(cache_base.path(), url);
        let repo = git2::Repository::init(&clone).unwrap();
        let commit = commit_one_file(&repo, "bytes\n");
        repo.tag_lightweight("v1", &repo.find_object(commit, None).unwrap(), false)
            .unwrap();
        let source = GitSource {
            repo_url: url.into(),
            tag: Some("v1".into()),
            git_ref: None,
            subdir: None,
        };
        let tree = checkout_ref(&clone, &source, "handout").unwrap();
        backdate(&tree);
        backdate(&clone);
        let (tree_stamp, clone_stamp) = (tree_stamp(&tree).unwrap(), clone_stamp(&clone));
        assert!(!is_young(&tree, &tree_stamp));
        assert!(!is_young(&clone, &clone_stamp));
        let recorded = (modified(&tree), modified(&clone));

        assert_eq!(checkout_ref(&clone, &source, "handout").unwrap(), tree);
        assert!(
            is_young(&tree, &tree_stamp),
            "handing out a tree refreshes its stamp"
        );
        assert!(
            is_young(&clone, &clone_stamp),
            "handing out a tree refreshes its clone's stamp"
        );
        assert_eq!(
            (modified(&tree), modified(&clone)),
            recorded,
            "the stamps live apart from the tree and the clone a derivation records"
        );
        let failures = sweep(cache_base.path(), &[]);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(payload_of(&tree), "bytes\n", "a tree in use is left alone");

        backdate(&tree);
        backdate(&clone);
        let failures = sweep(cache_base.path(), &[]);
        assert!(failures.is_empty(), "{failures:?}");
        assert!(
            !tree.exists(),
            "the tree nothing read for the window is gone"
        );
        assert!(!clone.exists(), "and so is the clone it emptied");
    }

    #[test]
    fn a_sweep_removes_a_clone_nothing_references_and_keeps_the_rest() {
        let cache_base = tempfile::tempdir().unwrap();
        let clone_of = |url: &str| {
            let clone = git_cache_dir(cache_base.path(), url);
            let repo = git2::Repository::init(&clone).unwrap();
            commit_one_file(&repo, url);
            clone
        };
        let referenced = clone_of("https://example.invalid/referenced.git");
        let unreferenced = clone_of("https://example.invalid/unreferenced.git");
        let young = clone_of("https://example.invalid/young.git");
        let not_a_clone = cache_base.path().join("notes");
        std::fs::create_dir_all(&not_a_clone).unwrap();
        for dir in [&referenced, &unreferenced, &not_a_clone] {
            backdate(dir);
        }

        let failures = sweep(cache_base.path(), &[&referenced.join("payload.txt")]);
        assert!(failures.is_empty(), "{failures:?}");
        assert!(!unreferenced.exists(), "a clone nothing references is gone");
        assert!(referenced.join(".git").is_dir(), "a referenced clone stays");
        assert!(
            young.join(".git").is_dir(),
            "a clone used within the window stays"
        );
        assert!(
            not_a_clone.is_dir(),
            "a directory no clone is named like stays"
        );
        let mut left: Vec<String> = std::fs::read_dir(cache_base.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut expected: Vec<String> = [&referenced, &young, &not_a_clone]
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        expected.sort();
        assert_eq!(left, expected, "the removal leaves no `.stale-` directory");
    }

    #[test]
    fn a_sweep_of_a_cache_with_no_trees_does_nothing() {
        let cache_base = tempfile::tempdir().unwrap();
        assert!(sweep(cache_base.path(), &[]).is_empty());
        let missing = cache_base.path().join("missing");
        assert!(sweep(&missing, &[]).is_empty());
    }

    #[test]
    fn a_sweep_reports_every_entry_it_cannot_read_and_goes_on() {
        let cache_base = tempfile::tempdir().unwrap();
        let (clone, kept, unread) = clone_with_two_trees(cache_base.path());
        let unreadable = git_cache_dir(cache_base.path(), "https://example.invalid/odd.git");
        std::fs::create_dir_all(unreadable.join(".git")).unwrap();
        let slots_file = pinned_checkouts_of(&unreadable);
        std::fs::write(&slots_file, "not a directory").unwrap();
        backdate(&unreadable);
        backdate(&unread);

        let failures = sweep(cache_base.path(), &[&kept]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            failures[0].starts_with(&format!(
                "cannot read {}: ",
                crate::to_posix_string(&slots_file)
            )),
            "the failure names the path it could not read: {failures:?}"
        );
        assert!(unreadable.is_dir(), "a clone that could not be read stays");
        assert!(!unread.exists(), "the sweep went on past the failure");
        assert!(clone.join(".git").is_dir());

        let file_base = cache_base.path().join("file");
        std::fs::write(&file_base, "").unwrap();
        let failures = sweep_pinned_checkouts(&KeptCheckouts::new(&file_base), IN_USE_WINDOW);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            failures[0].starts_with(&format!(
                "cannot read {}: ",
                crate::to_posix_string(&file_base)
            )),
            "{failures:?}"
        );
    }

    #[test]
    fn a_kept_path_keeps_its_tree_in_every_spelling_of_the_cache_root() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let tree = |base: &Path| {
            base.join("0123456789abcdef0123456789abcdef")
                .join(".git")
                .join(PINNED_CHECKOUTS_DIR)
                .join("c0ffee")
        };
        std::fs::create_dir_all(tree(&real)).unwrap();

        let mut keep = KeptCheckouts::new(&real);
        assert!(keep.keep(&tree(&real).join("payload.txt")));
        assert!(keep.holds(&tree(&real)));
        assert!(keep.holds(&real.join("0123456789abcdef0123456789abcdef")));
        assert!(
            !keep.keep(root.path()),
            "a path outside the cache keeps nothing"
        );

        let canonical =
            without_verbatim_prefix(&std::fs::canonicalize(&real).unwrap()).into_owned();
        let mut keep = KeptCheckouts::new(&real);
        assert!(keep.keep(&tree(&canonical)), "the canonical spelling");
        assert!(keep.holds(&tree(&real)));

        let verbatim = PathBuf::from(format!(r"\\?\{}", canonical.display()));
        let mut keep = KeptCheckouts::new(&real);
        if cfg!(windows) {
            assert!(keep.keep(&tree(&verbatim)), "the verbatim spelling");
            assert!(keep.holds(&tree(&real)));
        }

        // A cache root reached through a link is matched by its canonical
        // form, the spelling a link's target resolves to.
        let linked = root.path().join("linked");
        crate::create_symlink(&real, &linked).unwrap();
        let mut keep = KeptCheckouts::new(&linked);
        assert!(keep.keep(&tree(&canonical)));
        assert!(keep.holds(&tree(&linked)));
    }

    /// Windows refuses to rename a directory holding an open file. The sweep
    /// reports that tree and leaves it whole.
    #[cfg(windows)]
    #[test]
    fn a_sweep_reports_a_tree_it_cannot_move_and_leaves_it_whole() {
        let cache_base = tempfile::tempdir().unwrap();
        let (clone, kept, unread) = clone_with_two_trees(cache_base.path());
        backdate(&unread);
        let held = std::fs::File::open(unread.join("payload.txt")).unwrap();

        let failures = sweep(cache_base.path(), &[&kept]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].starts_with("cannot remove"), "{failures:?}");
        assert_eq!(payload_of(&unread), "unread\n");
        drop(held);

        backdate(&unread);
        assert!(sweep(cache_base.path(), &[&kept]).is_empty());
        assert!(!unread.exists());
        assert!(
            slot_names(&clone).contains(&kept.file_name().unwrap().to_str().unwrap().to_string())
        );
    }

    #[test]
    fn checkout_ref_errors_when_ref_points_to_non_commit() {
        // A tag pointing directly at a *tree* (not a commit) resolves via
        // revparse_single but fails peel_to_commit. The error must name the ref
        // and the "does not point to a commit" failure mode.
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Tag the bare *tree* object, not a commit.
        let tree_obj = repo.find_object(tree_id, None).unwrap();
        repo.tag_lightweight("tree-tag", &tree_obj, false).unwrap();

        let git_src = GitSource {
            repo_url: "file:///fixture".to_string(),
            tag: Some("tree-tag".to_string()),
            git_ref: None,
            subdir: None,
        };
        let err = checkout_ref(dir.path(), &git_src, "treemod")
            .expect_err("a tag pointing at a tree must fail to checkout");
        let msg = err.to_string();
        assert!(
            msg.contains("does not point to a commit"),
            "error must describe the non-commit peel failure: {msg}"
        );
        assert!(
            msg.contains("tree-tag"),
            "error must name the offending ref: {msg}"
        );
    }

    #[test]
    fn a_pinned_commit_the_remote_no_longer_has_says_how_to_re_pin() {
        // The one failure a locked module can reach after the cache is as
        // current as a transfer can make it: the recorded commit was rewritten
        // or collected upstream. A bare "cannot find ref '<40 hex>'" hands the
        // reader a hex string and no next step, so the resolution path names
        // the same recovery the integrity check already does.
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let missing = "0".repeat(40);
        let git_src = GitSource {
            repo_url: "file:///fixture".to_string(),
            tag: Some(missing.clone()),
            git_ref: None,
            subdir: None,
        };
        let err = checkout_ref(dir.path(), &git_src, "pinned-mod")
            .expect_err("a commit the repository does not hold must fail to checkout");
        let msg = err.to_string();
        assert!(
            msg.contains("cfgd module upgrade pinned-mod"),
            "the message must name the command that re-pins the module: {msg}"
        );
        assert!(
            msg.contains(&missing),
            "the message must still name the commit that is missing: {msg}"
        );
    }

    #[test]
    fn a_missing_tag_is_still_reported_as_a_missing_ref() {
        // The narrow half: only a full object id earns the re-pin advice. A tag
        // that is simply absent is a different failure and keeps its own words,
        // so widening the advice to every ref cannot pass unnoticed.
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let git_src = GitSource {
            repo_url: "file:///fixture".to_string(),
            tag: Some("v9.9.0".to_string()),
            git_ref: None,
            subdir: None,
        };
        let err = checkout_ref(dir.path(), &git_src, "tagged-mod")
            .expect_err("an absent tag must fail to checkout");
        let msg = err.to_string();
        assert!(
            msg.contains("cannot find ref 'v9.9.0'"),
            "an absent tag keeps the plain missing-ref message: {msg}"
        );
        assert!(
            !msg.contains("module upgrade"),
            "re-pin advice belongs to an immutable pin, not to a tag: {msg}"
        );
    }

    // --- checkout_ref: no ref pinned, and no upstream to advance towards ---

    #[test]
    fn an_unpinned_checkout_with_no_tracking_branch_is_left_where_it_is() {
        // An unpinned source advances to its remote-tracking branch, and this
        // repository has none — no origin remote, no origin/HEAD. The degrade
        // is what keeps that best-effort: HEAD stays put, still attached, and
        // the resolve succeeds rather than failing over an upstream the user
        // never asked for.
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        let head_before = repo.head().unwrap().target().unwrap();

        let git_src = GitSource {
            repo_url: "file:///fixture".to_string(),
            tag: None,
            git_ref: None,
            subdir: None,
        };
        checkout_ref(dir.path(), &git_src, "noref")
            .expect("a checkout with no upstream to follow must still resolve");

        // HEAD is unchanged and still attached (not detached).
        let head_after = repo.head().unwrap().target().unwrap();
        assert_eq!(head_before, head_after, "HEAD must not move");
        assert!(
            !repo.head_detached().unwrap(),
            "HEAD must remain attached when there is no tracking branch to move to"
        );
    }

    // --- checkout_ref: tag takes precedence over git_ref ---

    #[test]
    #[serial_test::serial]
    fn fetch_git_source_tag_precedence_over_ref() {
        // When both a tag and a ?ref= are present, the tag wins (checkout_ref
        // reads `tag.or(git_ref)`). The tag points at the root commit;
        // the branch carries an extra commit with feature.txt. Pinning to the
        // tag must yield the tag's tree (no feature.txt), proving precedence.
        let _guard =
            crate::test_helpers::EnvVarGuard::set(crate::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1");
        let bare = crate::test_helpers::BareGitRepo::builder()
            .commit("root", &[("base.txt", "base")])
            .tag("v1.0.0")
            .branch("feature", &[("feature.txt", "feat")])
            .build();

        let cache_base = tempfile::tempdir().unwrap();
        let printer = crate::test_helpers::test_printer();

        // Both ?ref=feature AND @v1.0.0 — the tag must win. `?ref=` is parsed
        // first, leaving `<url>@v1.0.0` for the @tag extractor.
        let url = format!("{}@v1.0.0?ref=feature", bare.url());
        let git_src = parse_git_source(&url).unwrap();
        assert_eq!(git_src.tag.as_deref(), Some("v1.0.0"));
        assert_eq!(git_src.git_ref.as_deref(), Some("feature"));

        let path = fetch_git_source(&git_src, cache_base.path(), "prec", &printer)
            .expect("fetch pinned to tag must succeed");
        assert!(
            path.join("base.txt").exists(),
            "tag tree must contain the root file"
        );
        assert!(
            !path.join("feature.txt").exists(),
            "tag (root commit) must NOT contain the branch-only file — tag won over ref"
        );
    }
}

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
    if source.starts_with("file://") && std::env::var("CFGD_ALLOW_LOCAL_SOURCES").is_ok() {
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

/// Compute the cache directory for a git source URL.
/// Uses SHA-256 hash of the repo URL for uniqueness.
pub fn git_cache_dir(cache_base: &Path, repo_url: &str) -> PathBuf {
    let hash = crate::sha256_hex(repo_url.as_bytes());
    cache_base.join(&hash[..32])
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

    checkout_ref(&cache_dir, git_src, module_name)?;

    resolve_subdir(cache_dir, &git_src.subdir, module_name, &git_src.repo_url)
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
    let Some(ref_name) = git_src.tag.as_deref().or(git_src.git_ref.as_deref()) else {
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
    let Some(ref_name) = git_src.tag.as_deref().or(git_src.git_ref.as_deref()) else {
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
/// The one checkout in this module: both the pinned and the unpinned paths land
/// here, so a module's files are replaced the same way whichever named the
/// commit. `label` is what the caller asked for, and names the failure.
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

fn checkout_ref(repo_path: &Path, git_src: &GitSource, module_name: &str) -> Result<()> {
    let repo = open_repo(repo_path, module_name, &git_src.repo_url)?;

    let target_ref = git_src.tag.as_deref().or(git_src.git_ref.as_deref());

    let Some(ref_name) = target_ref else {
        return advance_to_default_branch(&repo, git_src, module_name);
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

    detach_and_checkout(&repo, commit.id(), ref_name, git_src, module_name)
}

/// Get the HEAD commit SHA from a git repo.
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
        let _guard = crate::test_helpers::EnvVarGuard::unset("CFGD_ALLOW_LOCAL_SOURCES");
        assert!(!is_git_source("file:///tmp/repo"));
    }

    #[test]
    #[serial_test::serial]
    fn is_git_source_accepts_file_url_when_env_set() {
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        assert_eq!(again, path);
        assert_eq!(std::fs::read_to_string(again.join("a.txt")).unwrap(), "v1");
    }

    #[test]
    #[serial_test::serial]
    fn a_tag_pin_is_still_refreshed_because_a_tag_can_move() {
        // The narrow half of the same claim: only a full object id is immutable,
        // so a tag pin keeps its transfer even when the cache can already resolve
        // the name. Without this, the short-circuit widening to "any ref the
        // cache knows" would go unnoticed.
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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

    // --- checkout_ref: ref that does not peel to a commit ---

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
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
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

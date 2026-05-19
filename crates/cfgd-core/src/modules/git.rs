//! Git URL parsing and git clone/fetch/checkout operations for module file sources.

use std::path::{Path, PathBuf};

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

/// Check whether a file source string is a git URL (not a local path).
///
/// `file://` URLs are rejected by default to keep remote-module sources to
/// proper network protocols. Tests can opt into local-file sources by setting
/// `CFGD_ALLOW_LOCAL_SOURCES=1` — same gate `sources/mod.rs` uses for the
/// composed-sources path.
pub fn is_git_source(source: &str) -> bool {
    if source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
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
        // we need to find the @tag *after* the .git suffix
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

/// Default cache directory for module git sources.
///
/// Honors the thread-local test-home override (set via `with_test_home_guard`)
/// so tests can redirect module cache writes off the real `~/.cache/cfgd/`.
/// Without an override this falls through to `directories::BaseDirs` (XDG on
/// Linux, `~/Library/Caches` on macOS, `AppData\Local` on Windows).
pub fn default_module_cache_dir() -> Result<PathBuf> {
    if let Some(home) = crate::util::test_home_override() {
        return Ok(home.join(".cache").join("cfgd").join("modules"));
    }
    let base = directories::BaseDirs::new().ok_or_else(|| ModuleError::GitFetchFailed {
        module: String::new(),
        url: String::new(),
        message: "cannot determine home directory".into(),
    })?;
    Ok(base.cache_dir().join("cfgd").join("modules"))
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
            crate::validate_no_traversal(std::path::Path::new(sub)).map_err(|_| {
                ModuleError::GitFetchFailed {
                    module: module.to_string(),
                    url: url.to_string(),
                    message: format!("subdir contains path traversal: {sub}"),
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
pub fn fetch_git_source(
    git_src: &GitSource,
    cache_base: &Path,
    module_name: &str,
    printer: &crate::output_v2::Printer,
) -> Result<PathBuf> {
    let cache_dir = git_cache_dir(cache_base, &git_src.repo_url);

    if cache_dir.join(".git").exists() || cache_dir.join("HEAD").exists() {
        fetch_existing_repo(&cache_dir, git_src, module_name, printer)?;
    } else {
        clone_repo(&cache_dir, git_src, module_name, printer)?;
    }

    checkout_ref(&cache_dir, git_src, module_name)?;

    resolve_subdir(cache_dir, &git_src.subdir, module_name, &git_src.repo_url)
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
    printer: &crate::output_v2::Printer,
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
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let label = format!("Cloning module '{}'", module_name);
    let cli_result = printer.run(&mut cmd, &label);
    if matches!(&cli_result, Ok(output) if output.status.success()) {
        return Ok(());
    }

    // Clean up partial clone before libgit2 retry.
    let _ = std::fs::remove_dir_all(dest);
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Fall back to libgit2 with spinner.
    let spinner = printer.spinner(format!("Cloning module '{}' (libgit2)...", module_name));

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
                .detail(e.to_string());
        }
    }
    result?;

    Ok(())
}

pub(super) fn fetch_existing_repo(
    repo_path: &Path,
    git_src: &GitSource,
    module_name: &str,
    printer: &crate::output_v2::Printer,
) -> Result<()> {
    // Try git CLI first with live progress output.
    let mut cmd = crate::git_cmd_safe(Some(&git_src.repo_url), None);
    cmd.args(["-C", &repo_path.display().to_string(), "fetch", "origin"]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let label = format!("Fetching module '{}'", module_name);
    let cli_result = printer.run(&mut cmd, &label);
    if matches!(&cli_result, Ok(output) if output.status.success()) {
        return Ok(());
    }

    // Fall back to libgit2 with spinner.
    let spinner = printer.spinner(format!("Fetching module '{}' (libgit2)...", module_name));

    let repo = open_repo(repo_path, module_name, &git_src.repo_url)?;

    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("no 'origin' remote: {e}"),
        })?;

    let refspecs: Vec<String> = remote
        .refspecs()
        .filter_map(|rs| rs.str().map(String::from))
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
                .detail(e.to_string());
        }
    }
    fetch_result?;

    Ok(())
}

fn checkout_ref(repo_path: &Path, git_src: &GitSource, module_name: &str) -> Result<()> {
    let repo = open_repo(repo_path, module_name, &git_src.repo_url)?;

    let target_ref = git_src.tag.as_deref().or(git_src.git_ref.as_deref());

    let Some(ref_name) = target_ref else {
        // No specific ref — stay on default branch
        return Ok(());
    };

    // Try as a tag first, then as a branch
    let obj = repo
        .revparse_single(&format!("refs/tags/{ref_name}"))
        .or_else(|_| repo.revparse_single(&format!("refs/remotes/origin/{ref_name}")))
        .or_else(|_| repo.revparse_single(ref_name))
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("cannot find ref '{ref_name}': {e}"),
        })?;

    // Peel to commit
    let commit = obj
        .peel_to_commit()
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("ref '{ref_name}' does not point to a commit: {e}"),
        })?;

    repo.set_head_detached(commit.id())
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("cannot detach HEAD to '{ref_name}': {e}"),
        })?;

    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .map_err(|e| ModuleError::GitFetchFailed {
            module: module_name.to_string(),
            url: git_src.repo_url.clone(),
            message: format!("checkout failed for '{ref_name}': {e}"),
        })?;

    Ok(())
}

/// Get the HEAD commit SHA from a git repo.
pub fn get_head_commit_sha(repo_path: &Path) -> Result<String> {
    let path_str = repo_path.display().to_string();
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

    let message = match tag.message() {
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

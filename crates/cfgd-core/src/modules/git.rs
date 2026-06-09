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

/// Default cache directory for module git sources: `<cache-root>/modules` under
/// the single unified cfgd cache root.
///
/// Rebased onto the shared [`crate::default_cache_dir`] resolver so the source
/// cache and module cache share ONE root (Linux `~/.cache/cfgd`, macOS
/// `~/Library/Caches/cfgd`, Windows `%LOCALAPPDATA%\cfgd`). That resolver
/// honors the thread-local test-home override, so tests still redirect module
/// cache writes off the real cache.
pub fn default_module_cache_dir() -> Result<PathBuf> {
    Ok(crate::default_cache_dir()
        .map_err(|e| ModuleError::GitFetchFailed {
            module: String::new(),
            url: String::new(),
            message: e.to_string(),
        })?
        .join("modules"))
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
    printer: &crate::output::Printer,
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
                .detail(crate::output::collapse_to_subject_line(e));
        }
    }
    result?;

    Ok(())
}

pub(super) fn fetch_existing_repo(
    repo_path: &Path,
    git_src: &GitSource,
    module_name: &str,
    printer: &crate::output::Printer,
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
                .detail(crate::output::collapse_to_subject_line(e));
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

    #[test]
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
}

// Sources — multi-source config management
// Manages fetching, caching, and tracking external config sources (git repos).
// Dependency rules: depends only on config/, output/, errors/. Must NOT import
// files/, packages/, secrets/, reconciler/, providers/.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{FetchOptions, RemoteCallbacks, Repository};
use semver::{Version, VersionReq};

use crate::config::{
    ConfigSourceDocument, OriginSpec, OriginType, ProfileDocument, SourceSpec, parse_config_source,
};
use crate::errors::{Result, SourceError};
use crate::output::Printer;

const SOURCE_MANIFEST_FILE: &str = "cfgd-source.yaml";
const PROFILES_DIR: &str = "profiles";

/// Cached state for a single config source.
#[derive(Debug, Clone)]
pub struct CachedSource {
    pub name: String,
    pub origin_url: String,
    pub origin_branch: String,
    pub local_path: PathBuf,
    pub manifest: ConfigSourceDocument,
    pub last_commit: Option<String>,
    pub last_fetched: Option<String>,
}

/// Manager for multiple config sources — handles fetching, caching, version checking.
pub struct SourceManager {
    cache_dir: PathBuf,
    sources: HashMap<String, CachedSource>,
    /// When true, skip signature verification even if a source requires it.
    allow_unsigned: bool,
}

impl SourceManager {
    /// Create a new SourceManager using the given cache directory.
    pub fn new(cache_dir: &Path) -> Self {
        Self {
            cache_dir: cache_dir.to_path_buf(),
            sources: HashMap::new(),
            allow_unsigned: false,
        }
    }

    /// Set whether to allow unsigned source content (bypasses signature verification).
    pub fn set_allow_unsigned(&mut self, allow: bool) {
        self.allow_unsigned = allow;
    }

    /// Default cache directory: ~/.local/share/cfgd/sources/
    pub fn default_cache_dir() -> Result<PathBuf> {
        let base = directories::BaseDirs::new().ok_or_else(|| SourceError::CacheError {
            message: "cannot determine home directory".into(),
        })?;
        Ok(base.data_local_dir().join("cfgd").join("sources"))
    }

    /// Load all sources from config, fetching if needed.
    /// Returns an error if sources were specified but none loaded successfully.
    pub fn load_sources(&mut self, sources: &[SourceSpec], printer: &Printer) -> Result<()> {
        let mut loaded = 0;
        for spec in sources {
            match self.load_source(spec, printer) {
                Ok(()) => loaded += 1,
                Err(e) => {
                    printer.warning(&format!("Failed to load source '{}': {}", spec.name, e));
                }
            }
        }
        if !sources.is_empty() && loaded == 0 {
            return Err(SourceError::GitError {
                name: "all".to_string(),
                message: "all sources failed to load".to_string(),
            }
            .into());
        }
        Ok(())
    }

    /// Load a single source — clone or fetch, parse manifest, check version.
    pub fn load_source(&mut self, spec: &SourceSpec, printer: &Printer) -> Result<()> {
        crate::validate_no_traversal(std::path::Path::new(&spec.name)).map_err(|e| {
            SourceError::GitError {
                name: spec.name.clone(),
                message: format!("invalid source name: {e}"),
            }
        })?;

        // Reject local file URLs to prevent local filesystem access from composed sources.
        // CFGD_ALLOW_LOCAL_SOURCES bypasses this for dev/test environments only.
        let url_lower = spec.origin.url.to_lowercase();
        let allow_local = std::env::var("CFGD_ALLOW_LOCAL_SOURCES").is_ok();
        if !allow_local && (url_lower.starts_with("file://") || url_lower.starts_with('/')) {
            return Err(SourceError::GitError {
                name: spec.name.clone(),
                message: "local file:// URLs and absolute paths are not allowed as source origins"
                    .to_string(),
            }
            .into());
        }

        let source_dir = self.cache_dir.join(&spec.name);

        if source_dir.exists() {
            self.fetch_source(spec, &source_dir, printer)?;
        } else {
            self.clone_source(spec, &source_dir, printer)?;
        }

        let manifest = self.parse_manifest(&spec.name, &source_dir)?;

        // Signature verification: if the source requires signed commits, verify HEAD
        self.verify_commit_signature(&spec.name, &source_dir, &manifest.spec.policy.constraints)?;

        // Version pinning check
        if let Some(ref pin) = spec.sync.pin_version {
            self.check_version_pin(&spec.name, &manifest, pin)?;
        }

        let last_commit = Self::head_commit(&source_dir);

        let cached = CachedSource {
            name: spec.name.clone(),
            origin_url: spec.origin.url.clone(),
            origin_branch: spec.origin.branch.clone(),
            local_path: source_dir,
            manifest,
            last_commit,
            last_fetched: Some(crate::utc_now_iso8601()),
        };

        self.sources.insert(spec.name.clone(), cached);
        Ok(())
    }

    /// Fetch (pull) updates for an already-cloned source.
    fn fetch_source(&self, spec: &SourceSpec, source_dir: &Path, printer: &Printer) -> Result<()> {
        // Try git CLI first with live progress output.
        let mut cmd = crate::git_cmd_safe(
            Some(&spec.origin.url),
            Some(spec.origin.ssh_strict_host_key_checking),
        );
        cmd.args([
            "-C",
            &source_dir.display().to_string(),
            "fetch",
            "origin",
            &spec.origin.branch,
        ]);
        // Ensure stderr is captured (git progress goes to stderr)
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let label = format!("Fetching source '{}'", spec.name);
        let cli_result = printer.run_with_output(&mut cmd, &label);
        let cli_ok = matches!(&cli_result, Ok(output) if output.status.success());

        if !cli_ok {
            // Fall back to libgit2 with spinner
            let spinner = printer.spinner(&format!("Fetching source '{}' (libgit2)...", spec.name));

            let repo = Repository::open(source_dir).map_err(|e| SourceError::GitError {
                name: spec.name.clone(),
                message: e.to_string(),
            })?;

            let mut remote = repo
                .find_remote("origin")
                .map_err(|e| SourceError::GitError {
                    name: spec.name.clone(),
                    message: e.to_string(),
                })?;

            let mut fo = FetchOptions::new();
            let mut callbacks = RemoteCallbacks::new();
            callbacks.credentials(crate::git_ssh_credentials);
            fo.remote_callbacks(callbacks);

            let fetch_result = remote
                .fetch(&[&spec.origin.branch], Some(&mut fo), None)
                .map_err(|e| SourceError::FetchFailed {
                    name: spec.name.clone(),
                    message: e.to_string(),
                });

            spinner.finish_and_clear();
            fetch_result?;
        }

        // Fast-forward to FETCH_HEAD
        let repo = Repository::open(source_dir).map_err(|e| SourceError::GitError {
            name: spec.name.clone(),
            message: e.to_string(),
        })?;

        let fetch_head = repo
            .find_reference("FETCH_HEAD")
            .map_err(|e| SourceError::GitError {
                name: spec.name.clone(),
                message: e.to_string(),
            })?;
        let fetch_commit = repo
            .reference_to_annotated_commit(&fetch_head)
            .map_err(|e| SourceError::GitError {
                name: spec.name.clone(),
                message: e.to_string(),
            })?;

        let (analysis, _) =
            repo.merge_analysis(&[&fetch_commit])
                .map_err(|e| SourceError::GitError {
                    name: spec.name.clone(),
                    message: e.to_string(),
                })?;

        if analysis.is_fast_forward() {
            let refname = format!("refs/heads/{}", spec.origin.branch);
            if let Ok(mut reference) = repo.find_reference(&refname) {
                reference
                    .set_target(fetch_commit.id(), "cfgd source fetch")
                    .map_err(|e| SourceError::GitError {
                        name: spec.name.clone(),
                        message: e.to_string(),
                    })?;
            }
            repo.set_head(&refname).map_err(|e| SourceError::GitError {
                name: spec.name.clone(),
                message: e.to_string(),
            })?;
            repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
                .map_err(|e| SourceError::GitError {
                    name: spec.name.clone(),
                    message: e.to_string(),
                })?;
        }

        Ok(())
    }

    /// Clone a new source repo. Tries git CLI first (respects system credential
    /// helpers and SSH config), falls back to libgit2.
    fn clone_source(&self, spec: &SourceSpec, source_dir: &Path, printer: &Printer) -> Result<()> {
        // Ensure parent dir exists but not source_dir itself (git clone creates it)
        if let Some(parent) = source_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SourceError::CacheError {
                message: format!("cannot create cache dir: {}", e),
            })?;
        }

        // Try git CLI first with live progress output.
        // --depth=1: only fetch latest commit (limits repo size for DoS protection)
        // --no-recurse-submodules: prevent malicious submodule URLs (SSRF, credential theft)
        // --single-branch: only fetch the target branch
        let mut cmd = crate::git_cmd_safe(
            Some(&spec.origin.url),
            Some(spec.origin.ssh_strict_host_key_checking),
        );
        cmd.args([
            "clone",
            "--depth=1",
            "--single-branch",
            "--no-recurse-submodules",
            "--branch",
            &spec.origin.branch,
            &spec.origin.url,
            &source_dir.display().to_string(),
        ]);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let label = format!("Cloning source '{}'", spec.name);
        let cli_result = printer.run_with_output(&mut cmd, &label);
        if matches!(&cli_result, Ok(output) if output.status.success()) {
            // Restrict cloned directory to owner-only access
            let _ = crate::set_file_permissions(source_dir, 0o700);
            return Ok(());
        }

        // Clean up partial clone before libgit2 retry
        let _ = std::fs::remove_dir_all(source_dir);

        // Fall back to libgit2 with spinner
        let spinner = printer.spinner(&format!("Cloning source '{}' (libgit2)...", spec.name));

        let mut fo = FetchOptions::new();
        if spec.origin.url.starts_with("git@") || spec.origin.url.starts_with("ssh://") {
            let mut callbacks = RemoteCallbacks::new();
            callbacks.credentials(crate::git_ssh_credentials);
            fo.remote_callbacks(callbacks);
        }

        // Shallow clone with depth 1, disable submodule init
        fo.depth(1);
        let mut builder = git2::build::RepoBuilder::new();
        builder.fetch_options(fo);
        builder.branch(&spec.origin.branch);

        let clone_result =
            builder
                .clone(&spec.origin.url, source_dir)
                .map_err(|e| SourceError::FetchFailed {
                    name: spec.name.clone(),
                    message: e.to_string(),
                });

        spinner.finish_and_clear();
        clone_result?;

        // Restrict cloned directory to owner-only access
        let _ = crate::set_file_permissions(source_dir, 0o700);

        Ok(())
    }

    /// Parse the ConfigSource manifest from a source directory.
    pub fn parse_manifest(&self, name: &str, source_dir: &Path) -> Result<ConfigSourceDocument> {
        read_manifest(name, source_dir)
    }

    /// Verify the HEAD commit of a source repo has a valid GPG or SSH signature.
    /// Checks `allow_unsigned` on this SourceManager and `require_signed_commits`
    /// on the constraints before delegating to `verify_head_signature`.
    pub fn verify_commit_signature(
        &self,
        name: &str,
        source_dir: &Path,
        constraints: &crate::config::SourceConstraints,
    ) -> Result<()> {
        if !constraints.require_signed_commits {
            return Ok(());
        }

        if self.allow_unsigned {
            tracing::info!(
                source = %name,
                "Signature verification skipped for source '{}' (allow-unsigned is set)",
                name
            );
            return Ok(());
        }

        verify_head_signature(name, source_dir)
    }

    /// Check version pin against source manifest version.
    fn check_version_pin(
        &self,
        name: &str,
        manifest: &ConfigSourceDocument,
        pin: &str,
    ) -> Result<()> {
        let version_str = manifest.metadata.version.as_deref().unwrap_or("0.0.0");

        let version = Version::parse(version_str).map_err(|e| SourceError::InvalidManifest {
            name: name.to_string(),
            message: format!("invalid semver '{}': {}", version_str, e),
        })?;

        // Support tilde (~2) as shorthand for ~2.0.0
        let normalized_pin = normalize_semver_pin(pin);
        let req = VersionReq::parse(&normalized_pin).map_err(|_| SourceError::VersionMismatch {
            name: name.to_string(),
            version: version_str.to_string(),
            pin: pin.to_string(),
        })?;

        if !req.matches(&version) {
            return Err(SourceError::VersionMismatch {
                name: name.to_string(),
                version: version_str.to_string(),
                pin: pin.to_string(),
            }
            .into());
        }

        Ok(())
    }

    /// Get the HEAD commit hash for a repo.
    fn head_commit(source_dir: &Path) -> Option<String> {
        let repo = Repository::open(source_dir).ok()?;
        let head = repo.head().ok()?;
        head.target().map(|oid| oid.to_string())
    }

    /// Get a cached source by name.
    pub fn get(&self, name: &str) -> Option<&CachedSource> {
        self.sources.get(name)
    }

    /// Get all cached sources.
    pub fn all_sources(&self) -> &HashMap<String, CachedSource> {
        &self.sources
    }

    /// Load a profile from a source's profiles directory.
    pub fn load_source_profile(
        &self,
        source_name: &str,
        profile_name: &str,
    ) -> Result<ProfileDocument> {
        let cached = self
            .sources
            .get(source_name)
            .ok_or_else(|| SourceError::NotFound {
                name: source_name.to_string(),
            })?;

        let profile_path = cached
            .local_path
            .join(PROFILES_DIR)
            .join(format!("{}.yaml", profile_name));

        if !profile_path.exists() {
            return Err(SourceError::ProfileNotFound {
                name: source_name.to_string(),
                profile: profile_name.to_string(),
            }
            .into());
        }

        crate::config::load_profile(&profile_path)
    }

    /// Get the source profiles directory path.
    pub fn source_profiles_dir(&self, source_name: &str) -> Result<PathBuf> {
        let cached = self
            .sources
            .get(source_name)
            .ok_or_else(|| SourceError::NotFound {
                name: source_name.to_string(),
            })?;
        Ok(cached.local_path.join(PROFILES_DIR))
    }

    /// Get the source files directory path.
    pub fn source_files_dir(&self, source_name: &str) -> Result<PathBuf> {
        let cached = self
            .sources
            .get(source_name)
            .ok_or_else(|| SourceError::NotFound {
                name: source_name.to_string(),
            })?;
        Ok(cached.local_path.join("files"))
    }

    /// Remove a source from cache.
    pub fn remove_source(&mut self, name: &str) -> Result<()> {
        let cached = self
            .sources
            .remove(name)
            .ok_or_else(|| SourceError::NotFound {
                name: name.to_string(),
            })?;

        if cached.local_path.exists() {
            std::fs::remove_dir_all(&cached.local_path).map_err(|e| SourceError::CacheError {
                message: format!("failed to remove cache for '{}': {}", name, e),
            })?;
        }

        Ok(())
    }

    /// Build a SourceSpec for adding a new source.
    pub fn build_source_spec(name: &str, url: &str, profile: Option<&str>) -> SourceSpec {
        SourceSpec {
            name: name.to_string(),
            origin: OriginSpec {
                origin_type: OriginType::Git,
                url: url.to_string(),
                branch: "master".to_string(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: crate::config::SubscriptionSpec {
                profile: profile.map(|s| s.to_string()),
                ..Default::default()
            },
            sync: Default::default(),
        }
    }
}

/// Read and parse a cfgd-source.yaml manifest from a directory.
fn read_manifest(name: &str, source_dir: &Path) -> Result<ConfigSourceDocument> {
    let manifest_path = source_dir.join(SOURCE_MANIFEST_FILE);
    if !manifest_path.exists() {
        return Err(SourceError::InvalidManifest {
            name: name.to_string(),
            message: format!("{} not found", SOURCE_MANIFEST_FILE),
        }
        .into());
    }

    let contents =
        std::fs::read_to_string(&manifest_path).map_err(|e| SourceError::InvalidManifest {
            name: name.to_string(),
            message: e.to_string(),
        })?;

    let doc = parse_config_source(&contents).map_err(|e| SourceError::InvalidManifest {
        name: name.to_string(),
        message: e.to_string(),
    })?;

    if doc.spec.provides.profiles.is_empty() && doc.spec.provides.profile_details.is_empty() {
        return Err(SourceError::NoProfiles {
            name: name.to_string(),
        }
        .into());
    }

    Ok(doc)
}

/// Check if a directory contains a cfgd-source.yaml manifest.
/// Returns Ok(Some(doc)) if found and valid, Ok(None) if not present,
/// Err if file exists but is invalid.
pub fn detect_source_manifest(dir: &Path) -> Result<Option<ConfigSourceDocument>> {
    let manifest_path = dir.join(SOURCE_MANIFEST_FILE);
    if !manifest_path.exists() {
        return Ok(None);
    }
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    read_manifest(name, dir).map(Some)
}

/// Verify that the HEAD commit of a git repo has a valid GPG or SSH signature.
/// Uses `git log --format=%G?` to check signature status. Returns Ok(()) if the
/// signature is valid (G) or valid with unknown trust (U). Returns an error for
/// unsigned, bad, expired, revoked, or unverifiable signatures.
pub fn verify_head_signature(name: &str, repo_dir: &Path) -> Result<()> {
    if !crate::command_available("git") {
        return Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "git CLI is required for signature verification but is not available on PATH"
                .into(),
        }
        .into());
    }

    let output = crate::command_output_with_timeout(
        std::process::Command::new("git")
            .args([
                "-C",
                &repo_dir.display().to_string(),
                "log",
                "--format=%G?",
                "-1",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
        crate::COMMAND_TIMEOUT,
    )
    .map_err(|e| SourceError::SignatureVerificationFailed {
        name: name.to_string(),
        message: format!("failed to run git: {}", e),
    })?;

    if !output.status.success() {
        return Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: format!(
                "git log failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                crate::stderr_lossy_trimmed(&output)
            ),
        }
        .into());
    }

    let status = crate::stdout_lossy_trimmed(&output);

    match status.as_str() {
        // G = good valid signature, U = good signature with unknown validity (untrusted key)
        "G" | "U" => {
            tracing::info!(
                source = %name,
                "Source '{}' HEAD commit signature verified (status: {})",
                name, status
            );
            Ok(())
        }
        "N" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "HEAD commit is not signed — source requires signed commits".into(),
        }
        .into()),
        "B" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "HEAD commit has a bad (invalid) signature".into(),
        }
        .into()),
        "E" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "signature cannot be checked — ensure the signing key is imported".into(),
        }
        .into()),
        "X" | "Y" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "HEAD commit signature or signing key has expired".into(),
        }
        .into()),
        "R" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "HEAD commit was signed with a revoked key".into(),
        }
        .into()),
        other => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: format!("unexpected signature status '{}' from git", other),
        }
        .into()),
    }
}

/// Normalize shorthand semver pins.
/// `~2` means "any 2.x.x" (maps to `>=2.0.0, <3.0.0`, i.e., caret semantics).
/// `~2.1` means "any 2.1.x" (maps to `~2.1.0`, tilde semantics).
/// `~2.1.0` is passed through directly.
/// `^N` uses caret semantics as-is.
fn normalize_semver_pin(pin: &str) -> String {
    let trimmed = pin.trim();

    if let Some(rest) = trimmed.strip_prefix('~') {
        let dots = rest.matches('.').count();
        match dots {
            // ~2 -> ^2.0.0 (user means "any v2")
            0 => format!("^{}.0.0", rest),
            // ~2.1 -> ~2.1.0 (user means "any 2.1.x")
            1 => format!("~{}.0", rest),
            _ => trimmed.to_string(),
        }
    } else if let Some(rest) = trimmed.strip_prefix('^') {
        let dots = rest.matches('.').count();
        match dots {
            0 => format!("^{}.0.0", rest),
            1 => format!("^{}.0", rest),
            _ => trimmed.to_string(),
        }
    } else {
        trimmed.to_string()
    }
}

/// Clone a git repo with git CLI (with live progress), falling back to libgit2.
/// Returns Ok(()) on success, Err with description on failure.
pub fn git_clone_with_fallback(
    url: &str,
    target: &Path,
    printer: &Printer,
) -> std::result::Result<(), String> {
    // Try git CLI first with live progress output.
    let mut cmd = crate::git_cmd_safe(Some(url), None);
    cmd.args([
        "clone",
        "--depth=1",
        "--no-recurse-submodules",
        url,
        &target.display().to_string(),
    ]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let label = format!("Cloning {}", url);
    let cli_result = printer.run_with_output(&mut cmd, &label);
    if matches!(&cli_result, Ok(output) if output.status.success()) {
        return Ok(());
    }

    // Clean up partial clone before libgit2 retry
    let _ = std::fs::remove_dir_all(target);
    let _ = std::fs::create_dir_all(target);

    // Fall back to libgit2 with spinner
    let spinner = printer.spinner("Cloning (libgit2)...");

    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.depth(1);
    if url.starts_with("git@") || url.starts_with("ssh://") {
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(crate::git_ssh_credentials);
        fetch_opts.remote_callbacks(callbacks);
    }
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fetch_opts);

    let result = builder
        .clone(url, target)
        .map(|_| ())
        .map_err(|e| format!("Failed to clone {}: {}", url, e));

    spinner.finish_and_clear();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::test_printer;

    #[test]
    fn normalize_tilde_pin() {
        // ~N -> ^N.0.0 (any version N.x.x)
        assert_eq!(normalize_semver_pin("~2"), "^2.0.0");
        // ~N.M -> ~N.M.0 (any version N.M.x)
        assert_eq!(normalize_semver_pin("~2.1"), "~2.1.0");
        // Full version passed through
        assert_eq!(normalize_semver_pin("~2.1.0"), "~2.1.0");
    }

    #[test]
    fn normalize_caret_pin() {
        assert_eq!(normalize_semver_pin("^3"), "^3.0.0");
        assert_eq!(normalize_semver_pin("^3.1"), "^3.1.0");
        assert_eq!(normalize_semver_pin("^3.1.2"), "^3.1.2");
    }

    #[test]
    fn normalize_exact_pin() {
        assert_eq!(normalize_semver_pin("=1.2.3"), "=1.2.3");
        assert_eq!(normalize_semver_pin("1.2.3"), "1.2.3");
    }

    #[test]
    fn version_pin_matching() {
        let pin = normalize_semver_pin("~2");
        let req = VersionReq::parse(&pin).unwrap();
        assert!(req.matches(&Version::new(2, 0, 0)));
        assert!(req.matches(&Version::new(2, 5, 0)));
        assert!(!req.matches(&Version::new(3, 0, 0)));
        assert!(!req.matches(&Version::new(1, 9, 0)));
    }

    #[test]
    fn source_manager_creates_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());
        assert!(mgr.all_sources().is_empty());
    }

    #[test]
    fn build_source_spec_with_defaults() {
        let spec = SourceManager::build_source_spec(
            "acme",
            "git@github.com:acme/config.git",
            Some("backend"),
        );
        assert_eq!(spec.name, "acme");
        assert_eq!(spec.subscription.priority, 500);
        assert_eq!(spec.subscription.profile.as_deref(), Some("backend"));
        assert_eq!(spec.sync.interval, "1h");
    }

    #[test]
    fn build_source_spec_no_profile() {
        let spec = SourceManager::build_source_spec("test", "https://example.com/config.git", None);
        assert!(spec.subscription.profile.is_none());
    }

    #[test]
    fn remove_nonexistent_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        let err = mgr.remove_source("nonexistent").unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "expected 'not found' error, got: {err}"
        );
    }

    #[test]
    fn get_nonexistent_source() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());
        assert!(mgr.get("nonexistent").is_none());
    }

    #[test]
    fn detect_source_manifest_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: test-source
spec:
  provides:
    profiles:
      - base
  policy: {}
"#,
        )
        .unwrap();

        let result = detect_source_manifest(dir.path()).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().metadata.name, "test-source");
    }

    #[test]
    fn detect_source_manifest_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_source_manifest(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn detect_source_manifest_invalid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SOURCE_MANIFEST_FILE), "not: valid: yaml: [").unwrap();
        let err = detect_source_manifest(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("invalid") || err.to_string().contains("ConfigSource"),
            "expected manifest parse error, got: {err}"
        );
    }

    #[test]
    fn parse_manifest_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());
        let result = mgr.parse_manifest("test", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn parse_manifest_valid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: test-source
  version: "1.0.0"
spec:
  provides:
    profiles:
      - base
  policy:
    required:
      packages:
        brew:
          formulae:
            - git-secrets
    constraints:
      noScripts: true
"#,
        )
        .unwrap();

        let mgr = SourceManager::new(dir.path());
        let manifest = mgr.parse_manifest("test", dir.path()).unwrap();
        assert_eq!(manifest.metadata.name, "test-source");
        assert_eq!(manifest.spec.provides.profiles, vec!["base"]);
    }

    #[test]
    fn check_version_pin_passes() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let manifest = ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "test".into(),
                version: Some("2.1.0".into()),
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        };

        // All three pins should match version 2.1.0 — unwrap to prove success
        mgr.check_version_pin("test", &manifest, "~2")
            .expect("~2 should match 2.1.0");
        mgr.check_version_pin("test", &manifest, "^2")
            .expect("^2 should match 2.1.0");
        mgr.check_version_pin("test", &manifest, "~2.1")
            .expect("~2.1 should match 2.1.0");
    }

    #[test]
    fn check_version_pin_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let manifest = ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "test".into(),
                version: Some("3.0.0".into()),
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        };

        let err = mgr.check_version_pin("test", &manifest, "~2").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("3.0.0") && msg.contains("~2"),
            "expected version mismatch with '3.0.0' and '~2', got: {msg}"
        );
    }

    #[test]
    fn verify_signature_skipped_when_not_required() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());
        let constraints = crate::config::SourceConstraints::default();
        assert!(
            !constraints.require_signed_commits,
            "default should be false"
        );
        // require_signed_commits defaults to false — should return Ok(()) without any repo
        let result = mgr.verify_commit_signature("test", dir.path(), &constraints);
        assert_eq!(
            result.unwrap(),
            (),
            "expected Ok(()) when signatures not required"
        );
    }

    #[test]
    fn verify_signature_skipped_when_allow_unsigned() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        mgr.set_allow_unsigned(true);
        let constraints = crate::config::SourceConstraints {
            require_signed_commits: true,
            ..Default::default()
        };
        assert!(mgr.allow_unsigned, "allow_unsigned should be set");
        assert!(
            constraints.require_signed_commits,
            "require_signed_commits should be true"
        );
        // Even though require_signed_commits is true, allow_unsigned bypasses it
        let result = mgr.verify_commit_signature("test", dir.path(), &constraints);
        assert_eq!(
            result.unwrap(),
            (),
            "expected Ok(()) when allow_unsigned bypasses verification"
        );
    }

    #[test]
    fn verify_signature_fails_on_unsigned_commit() {
        // Create a real git repo with an unsigned commit
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "unsigned commit", &tree, &[])
            .unwrap();

        let mgr = SourceManager::new(dir.path());
        let constraints = crate::config::SourceConstraints {
            require_signed_commits: true,
            ..Default::default()
        };

        let result = mgr.verify_commit_signature("test-source", dir.path(), &constraints);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not signed"),
            "expected 'not signed' in error, got: {}",
            err_msg
        );
    }

    #[test]
    fn set_allow_unsigned_works() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        assert!(!mgr.allow_unsigned);
        mgr.set_allow_unsigned(true);
        assert!(mgr.allow_unsigned);
    }

    #[test]
    fn verify_head_signature_fails_on_unsigned_repo() {
        // Create a git repo with an unsigned commit using git2 directly
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let repo = git2::Repository::init(&repo_dir).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();

        // Create a file and commit it
        std::fs::write(repo_dir.join("README"), "test\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "unsigned commit", &tree, &[])
            .unwrap();

        // The public function verify_head_signature should fail on unsigned commits
        let result = verify_head_signature("test-source", &repo_dir);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not signed") || err_msg.contains("signature"),
            "expected signature-related error, got: {}",
            err_msg
        );
    }

    #[test]
    fn source_profiles_dir_nonexistent_source() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let result = mgr.source_profiles_dir("nonexistent");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found"),
            "expected 'not found' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn source_files_dir_nonexistent_source() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let result = mgr.source_files_dir("nonexistent");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found"),
            "expected 'not found' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn normalize_semver_pin_whitespace() {
        assert_eq!(normalize_semver_pin("  ~2  "), "^2.0.0");
        assert_eq!(normalize_semver_pin(" ^3.1 "), "^3.1.0");
    }

    #[test]
    fn normalize_semver_pin_plain_version() {
        // No prefix — passed through as-is
        assert_eq!(normalize_semver_pin("2.1.0"), "2.1.0");
        assert_eq!(normalize_semver_pin(">=1.0.0"), ">=1.0.0");
    }

    #[test]
    fn check_version_pin_no_manifest_version_uses_zero() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let manifest = ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "test".into(),
                version: None, // No version — defaults to 0.0.0
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        };

        // ~0 matches 0.0.0
        mgr.check_version_pin("test", &manifest, "~0")
            .expect("~0 should match defaulted version 0.0.0");
        // ~1 does NOT match 0.0.0
        let err = mgr.check_version_pin("test", &manifest, "~1").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("0.0.0") && msg.contains("~1"),
            "expected version mismatch with '0.0.0' and '~1', got: {msg}"
        );
    }

    #[test]
    fn check_version_pin_invalid_semver_in_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let manifest = ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "test".into(),
                version: Some("not-a-version".into()),
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        };

        let result = mgr.check_version_pin("test", &manifest, "~1");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("semver") || err.contains("invalid"),
            "expected semver error, got: {err}"
        );
    }

    #[test]
    fn check_version_pin_invalid_pin_format() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let manifest = ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "test".into(),
                version: Some("1.0.0".into()),
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        };

        let err = mgr
            .check_version_pin("test", &manifest, "not-a-pin")
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not-a-pin") && msg.contains("version"),
            "expected version mismatch error mentioning 'not-a-pin', got: {msg}"
        );
    }

    #[test]
    fn read_manifest_no_profiles_is_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: empty-source
spec:
  provides:
    profiles: []
  policy: {}
"#,
        )
        .unwrap();

        let result = read_manifest("empty-source", dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no profiles") || err.contains("NoProfiles"),
            "expected no-profiles error, got: {err}"
        );
    }

    #[test]
    fn detect_source_manifest_no_profiles_is_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: empty-profiles
spec:
  provides:
    profiles: []
  policy: {}
"#,
        )
        .unwrap();

        // detect_source_manifest delegates to read_manifest which should fail
        let err = detect_source_manifest(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("no profiles") || err.to_string().contains("NoProfiles"),
            "expected no-profiles error, got: {err}"
        );
    }

    #[test]
    fn load_source_profile_nonexistent_source() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let result = mgr.load_source_profile("nonexistent", "default");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "expected 'not found' error, got: {err}"
        );
    }

    #[test]
    fn default_cache_dir_returns_path() {
        // This test may fail in environments without a home directory,
        // but in normal test environments it should work.
        let result = SourceManager::default_cache_dir();
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("cfgd"));
        assert!(path.to_string_lossy().contains("sources"));
    }

    #[test]
    fn build_source_spec_defaults() {
        let spec = SourceManager::build_source_spec("test", "https://example.com/config.git", None);
        assert_eq!(spec.origin.branch, "master");
        assert_eq!(spec.origin.url, "https://example.com/config.git");
        assert!(spec.origin.auth.is_none());
        assert!(spec.subscription.profile.is_none());
        // Default sync interval
        assert_eq!(spec.sync.interval, "1h");
        assert!(spec.sync.pin_version.is_none());
    }

    #[test]
    fn subscription_config_from_spec() {
        let spec = crate::config::SourceSpec {
            name: "test".into(),
            origin: crate::config::OriginSpec {
                origin_type: OriginType::Git,
                url: "https://example.com".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: crate::config::SubscriptionSpec {
                profile: Some("backend".into()),
                priority: 500,
                accept_recommended: true,
                opt_in: vec!["extra".into()],
                overrides: serde_yaml::Value::Null,
                reject: serde_yaml::Value::Null,
            },
            sync: Default::default(),
        };

        let config = crate::composition::SubscriptionConfig::from_spec(&spec);
        assert!(config.accept_recommended);
        assert_eq!(config.opt_in, vec!["extra".to_string()]);
    }

    #[test]
    fn version_pin_tilde_two_part() {
        // ~2.1 should match 2.1.x but not 2.2.0
        let pin = normalize_semver_pin("~2.1");
        let req = VersionReq::parse(&pin).unwrap();
        assert!(req.matches(&Version::new(2, 1, 0)));
        assert!(req.matches(&Version::new(2, 1, 9)));
        assert!(!req.matches(&Version::new(2, 2, 0)));
    }

    #[test]
    fn version_pin_caret_matches_minor_bumps() {
        let pin = normalize_semver_pin("^3");
        let req = VersionReq::parse(&pin).unwrap();
        assert!(req.matches(&Version::new(3, 0, 0)));
        assert!(req.matches(&Version::new(3, 9, 0)));
        assert!(!req.matches(&Version::new(4, 0, 0)));
    }

    #[test]
    fn load_source_rejects_traversal_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        let printer = test_printer();

        let spec = crate::config::SourceSpec {
            name: "../evil".into(),
            origin: crate::config::OriginSpec {
                origin_type: OriginType::Git,
                url: "https://example.com/config.git".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: Default::default(),
            sync: Default::default(),
        };

        let result = mgr.load_source(&spec, &printer);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid source name") || err.contains("traversal"),
            "expected traversal error, got: {err}"
        );
    }

    #[test]
    fn remove_source_success() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());

        // Create a fake cached source directory on disk
        let source_path = dir.path().join("test-source");
        std::fs::create_dir_all(&source_path).unwrap();
        std::fs::write(source_path.join("marker.txt"), "exists").unwrap();
        assert!(source_path.exists());

        // Manually insert a CachedSource into the manager's internal map
        let cached = CachedSource {
            name: "test-source".to_string(),
            origin_url: "https://example.com/config.git".to_string(),
            origin_branch: "main".to_string(),
            local_path: source_path.clone(),
            manifest: crate::config::ConfigSourceDocument {
                api_version: crate::API_VERSION.into(),
                kind: "ConfigSource".into(),
                metadata: crate::config::ConfigSourceMetadata {
                    name: "test-source".into(),
                    version: Some("1.0.0".into()),
                    description: None,
                },
                spec: crate::config::ConfigSourceSpec {
                    provides: Default::default(),
                    policy: Default::default(),
                },
            },
            last_commit: None,
            last_fetched: None,
        };
        mgr.sources.insert("test-source".to_string(), cached);

        // Verify the source is present
        assert!(mgr.get("test-source").is_some());

        // Remove the source
        mgr.remove_source("test-source")
            .expect("remove_source should succeed for existing cached source");

        // Verify it was removed from the map
        assert!(mgr.get("test-source").is_none());
        assert!(
            mgr.all_sources().is_empty(),
            "sources map should be empty after removal"
        );

        // Verify the directory and its contents were removed from disk
        assert!(!source_path.exists(), "source directory should be deleted");
        assert!(
            !source_path.join("marker.txt").exists(),
            "files within source directory should be deleted"
        );
    }

    /// Helper: insert a fake CachedSource into a SourceManager for testing
    /// methods that operate on already-cached sources.
    fn insert_fake_source(mgr: &mut SourceManager, name: &str, local_path: PathBuf) {
        let cached = CachedSource {
            name: name.to_string(),
            origin_url: "https://example.com/config.git".to_string(),
            origin_branch: "main".to_string(),
            local_path,
            manifest: crate::config::ConfigSourceDocument {
                api_version: crate::API_VERSION.into(),
                kind: "ConfigSource".into(),
                metadata: crate::config::ConfigSourceMetadata {
                    name: name.into(),
                    version: Some("1.0.0".into()),
                    description: None,
                },
                spec: crate::config::ConfigSourceSpec {
                    provides: Default::default(),
                    policy: Default::default(),
                },
            },
            last_commit: None,
            last_fetched: None,
        };
        mgr.sources.insert(name.to_string(), cached);
    }

    #[test]
    fn load_source_profile_success() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("my-source");
        std::fs::create_dir_all(source_path.join(PROFILES_DIR)).unwrap();

        // Write a valid profile YAML
        std::fs::write(
            source_path.join(PROFILES_DIR).join("default.yaml"),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  packages:
    pipx:
      - ripgrep
"#,
        )
        .unwrap();

        let mut mgr = SourceManager::new(dir.path());
        insert_fake_source(&mut mgr, "my-source", source_path);

        let result = mgr.load_source_profile("my-source", "default");
        assert!(
            result.is_ok(),
            "load_source_profile failed: {:?}",
            result.err()
        );
        let profile = result.unwrap();
        assert_eq!(profile.metadata.name, "default");
    }

    #[test]
    fn load_source_profile_missing_profile_file() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("my-source");
        std::fs::create_dir_all(source_path.join(PROFILES_DIR)).unwrap();
        // No profile file written

        let mut mgr = SourceManager::new(dir.path());
        insert_fake_source(&mut mgr, "my-source", source_path);

        let result = mgr.load_source_profile("my-source", "nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found") || err.contains("ProfileNotFound"),
            "expected profile not found error, got: {err}"
        );
    }

    #[test]
    fn source_profiles_dir_returns_path_for_cached_source() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("src-1");
        std::fs::create_dir_all(&source_path).unwrap();

        let mut mgr = SourceManager::new(dir.path());
        insert_fake_source(&mut mgr, "src-1", source_path.clone());

        let result = mgr.source_profiles_dir("src-1");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), source_path.join(PROFILES_DIR));
    }

    #[test]
    fn source_files_dir_returns_path_for_cached_source() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("src-1");
        std::fs::create_dir_all(&source_path).unwrap();

        let mut mgr = SourceManager::new(dir.path());
        insert_fake_source(&mut mgr, "src-1", source_path.clone());

        let result = mgr.source_files_dir("src-1");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), source_path.join("files"));
    }

    #[test]
    fn head_commit_returns_oid_for_valid_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("repo");

        // Use a manually created repo
        let repo = git2::Repository::init(&repo_dir).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        std::fs::write(repo_dir.join("file.txt"), "content\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let result = SourceManager::head_commit(&repo_dir);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), oid.to_string());
    }

    #[test]
    fn head_commit_returns_none_for_nonexistent_dir() {
        let result = SourceManager::head_commit(std::path::Path::new("/tmp/no-such-repo-xyz"));
        assert!(result.is_none());
    }

    #[test]
    fn load_sources_fails_when_all_sources_fail() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        let printer = test_printer();

        // Create specs that point to non-existent repos
        let specs = vec![
            crate::config::SourceSpec {
                name: "bad1".into(),
                origin: crate::config::OriginSpec {
                    origin_type: OriginType::Git,
                    url: "file:///nonexistent/repo1".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
                subscription: Default::default(),
                sync: Default::default(),
            },
            crate::config::SourceSpec {
                name: "bad2".into(),
                origin: crate::config::OriginSpec {
                    origin_type: OriginType::Git,
                    url: "file:///nonexistent/repo2".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
                subscription: Default::default(),
                sync: Default::default(),
            },
        ];

        let result = mgr.load_sources(&specs, &printer);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("all sources failed"),
            "expected all sources failed error, got: {err}"
        );
    }

    #[test]
    fn load_sources_succeeds_with_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        let printer = test_printer();

        // Empty list should succeed and leave no sources loaded
        mgr.load_sources(&[], &printer)
            .expect("load_sources with empty list should succeed");
        assert!(
            mgr.all_sources().is_empty(),
            "no sources should be loaded from empty list"
        );
    }

    #[test]
    fn git_clone_with_fallback_local_repo() {
        let dir = tempfile::tempdir().unwrap();
        let origin_path = dir.path().join("origin");

        // Create a bare repo as the origin
        let repo = git2::Repository::init(&origin_path).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        std::fs::write(origin_path.join("file.txt"), "hello\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let clone_path = dir.path().join("clone");
        let printer = test_printer();
        git_clone_with_fallback(&origin_path.display().to_string(), &clone_path, &printer)
            .expect("clone of local repo should succeed");

        // Verify the cloned file exists with the correct content
        assert!(
            clone_path.join("file.txt").exists(),
            "cloned file should exist"
        );
        let content = std::fs::read_to_string(clone_path.join("file.txt")).unwrap();
        assert_eq!(
            content, "hello\n",
            "cloned file should have original content"
        );

        // Verify it is a valid git repo
        assert!(
            clone_path.join(".git").exists(),
            "cloned directory should be a git repo"
        );
    }

    #[test]
    fn git_clone_with_fallback_invalid_url() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("clone");
        std::fs::create_dir_all(&target).unwrap();

        let printer = test_printer();
        let err = git_clone_with_fallback("file:///nonexistent/path/repo", &target, &printer)
            .unwrap_err();
        assert!(
            err.contains("Failed to clone") || err.contains("nonexistent"),
            "expected clone failure message, got: {err}"
        );
    }

    #[test]
    fn remove_source_cleans_up_directory() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("removable");
        std::fs::create_dir_all(&source_path).unwrap();
        std::fs::write(source_path.join("data.txt"), "test").unwrap();

        let mut mgr = SourceManager::new(dir.path());
        insert_fake_source(&mut mgr, "removable", source_path.clone());

        // Pre-conditions: source exists on disk and in cache
        assert!(
            source_path.exists(),
            "source directory should exist before removal"
        );
        assert!(
            source_path.join("data.txt").exists(),
            "data file should exist before removal"
        );
        assert!(
            mgr.get("removable").is_some(),
            "source should be in cache before removal"
        );

        mgr.remove_source("removable")
            .expect("remove_source should succeed for existing cached source");

        // Post-conditions: both directory and cache entry are gone
        assert!(
            !source_path.exists(),
            "source directory should be deleted after removal"
        );
        assert!(
            mgr.get("removable").is_none(),
            "source should be removed from cache"
        );
        assert!(
            mgr.all_sources().is_empty(),
            "sources map should be empty after removal"
        );
    }

    #[test]
    fn all_sources_returns_cached() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());

        let path1 = dir.path().join("src-a");
        let path2 = dir.path().join("src-b");
        std::fs::create_dir_all(&path1).unwrap();
        std::fs::create_dir_all(&path2).unwrap();

        insert_fake_source(&mut mgr, "src-a", path1);
        insert_fake_source(&mut mgr, "src-b", path2);

        let all = mgr.all_sources();
        assert_eq!(all.len(), 2);
        assert!(all.contains_key("src-a"));
        assert!(all.contains_key("src-b"));
    }

    // ─── build_source_spec — field verification ──────────────────

    #[test]
    fn build_source_spec_ssh_url() {
        let spec = SourceManager::build_source_spec("corp", "git@gitlab.com:corp/config.git", None);
        assert_eq!(spec.name, "corp");
        assert_eq!(spec.origin.url, "git@gitlab.com:corp/config.git");
        assert_eq!(spec.origin.branch, "master");
        assert!(matches!(spec.origin.origin_type, OriginType::Git));
        assert!(spec.origin.auth.is_none());
        assert!(spec.subscription.profile.is_none());
        assert!(!spec.subscription.accept_recommended);
        assert!(spec.subscription.opt_in.is_empty());
        assert!(!spec.sync.auto_apply);
        assert!(spec.sync.pin_version.is_none());
    }

    #[test]
    fn build_source_spec_with_profile_sets_subscription() {
        let spec = SourceManager::build_source_spec(
            "team",
            "https://github.com/team/dotfiles.git",
            Some("devops"),
        );
        assert_eq!(spec.subscription.profile.as_deref(), Some("devops"));
        assert_eq!(spec.subscription.priority, 500);
        assert_eq!(spec.sync.interval, "1h");
    }

    #[test]
    fn build_source_spec_preserves_url_verbatim() {
        let url = "ssh://git@internal.host:2222/repo.git";
        let spec = SourceManager::build_source_spec("internal", url, None);
        assert_eq!(spec.origin.url, url);
    }

    // ─── parse_manifest — profile_details support ────────────────

    #[test]
    fn parse_manifest_with_profile_details() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: detailed-source
  version: "2.0.0"
  description: "A source with profile details"
spec:
  provides:
    profiles: []
    profileDetails:
      - name: backend
        description: "Backend developer profile"
        inherits:
          - base
      - name: frontend
        description: "Frontend developer profile"
    modules:
      - docker
      - kubernetes
  policy: {}
"#,
        )
        .unwrap();

        let mgr = SourceManager::new(dir.path());
        let manifest = mgr.parse_manifest("detailed", dir.path()).unwrap();
        assert_eq!(manifest.metadata.name, "detailed-source");
        assert_eq!(manifest.metadata.version.as_deref(), Some("2.0.0"));
        assert_eq!(
            manifest.metadata.description.as_deref(),
            Some("A source with profile details")
        );
        assert_eq!(manifest.spec.provides.profile_details.len(), 2);
        assert_eq!(manifest.spec.provides.profile_details[0].name, "backend");
        assert_eq!(
            manifest.spec.provides.profile_details[0]
                .description
                .as_deref(),
            Some("Backend developer profile")
        );
        assert_eq!(
            manifest.spec.provides.profile_details[0].inherits,
            vec!["base"]
        );
        assert_eq!(manifest.spec.provides.profile_details[1].name, "frontend");
        assert_eq!(manifest.spec.provides.modules, vec!["docker", "kubernetes"]);
    }

    #[test]
    fn parse_manifest_with_platform_profiles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: platform-source
spec:
  provides:
    profiles:
      - base
    platformProfiles:
      macos: macos-base
      linux: linux-base
  policy: {}
"#,
        )
        .unwrap();

        let mgr = SourceManager::new(dir.path());
        let manifest = mgr.parse_manifest("plat", dir.path()).unwrap();
        assert_eq!(
            manifest.spec.provides.platform_profiles.get("macos"),
            Some(&"macos-base".to_string())
        );
        assert_eq!(
            manifest.spec.provides.platform_profiles.get("linux"),
            Some(&"linux-base".to_string())
        );
    }

    // ─── ConfigSourceDocument serialization roundtrip ─────────────

    #[test]
    fn config_source_document_serde_roundtrip() {
        let doc = ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "roundtrip-test".into(),
                version: Some("1.2.3".into()),
                description: Some("Test description".into()),
            },
            spec: crate::config::ConfigSourceSpec {
                provides: crate::config::ConfigSourceProvides {
                    profiles: vec!["base".into(), "dev".into()],
                    profile_details: vec![crate::config::ConfigSourceProfileEntry {
                        name: "base".into(),
                        description: Some("Base profile".into()),
                        path: None,
                        inherits: vec![],
                    }],
                    platform_profiles: {
                        let mut m = HashMap::new();
                        m.insert("macos".into(), "macos-base".into());
                        m
                    },
                    modules: vec!["git".into()],
                },
                policy: Default::default(),
            },
        };

        let yaml = serde_yaml::to_string(&doc).expect("serialize should succeed");
        let parsed: ConfigSourceDocument =
            serde_yaml::from_str(&yaml).expect("deserialize should succeed");

        assert_eq!(parsed.metadata.name, "roundtrip-test");
        assert_eq!(parsed.metadata.version.as_deref(), Some("1.2.3"));
        assert_eq!(
            parsed.metadata.description.as_deref(),
            Some("Test description")
        );
        assert_eq!(parsed.spec.provides.profiles, vec!["base", "dev"]);
        assert_eq!(parsed.spec.provides.profile_details.len(), 1);
        assert_eq!(parsed.spec.provides.profile_details[0].name, "base");
        assert_eq!(
            parsed
                .spec
                .provides
                .platform_profiles
                .get("macos")
                .map(String::as_str),
            Some("macos-base")
        );
        assert_eq!(parsed.spec.provides.modules, vec!["git"]);
    }

    #[test]
    fn config_source_document_deserialize_minimal() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: minimal
spec:
  provides:
    profiles:
      - default
"#;
        let doc: ConfigSourceDocument =
            serde_yaml::from_str(yaml).expect("minimal manifest should parse");
        assert_eq!(doc.metadata.name, "minimal");
        assert!(doc.metadata.version.is_none());
        assert!(doc.metadata.description.is_none());
        assert_eq!(doc.spec.provides.profiles, vec!["default"]);
        assert!(doc.spec.provides.profile_details.is_empty());
        assert!(doc.spec.provides.platform_profiles.is_empty());
        assert!(doc.spec.provides.modules.is_empty());
        // Policy defaults
        assert!(!doc.spec.policy.constraints.require_signed_commits);
    }

    // ─── read_manifest — additional edge cases ───────────────────

    #[test]
    fn read_manifest_unreadable_yaml_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            "this is not yaml at all: [[[",
        )
        .unwrap();

        let err = read_manifest("bad-yaml", dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bad-yaml") || msg.contains("invalid") || msg.contains("ConfigSource"),
            "expected manifest parse error mentioning the source name, got: {msg}"
        );
    }

    #[test]
    fn read_manifest_wrong_kind_in_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: wrong-kind
spec: {}
"#,
        )
        .unwrap();

        // parse_config_source validates the kind field — this should fail
        let result = read_manifest("wrong-kind", dir.path());
        assert!(
            result.is_err(),
            "wrong kind should be rejected by parse_config_source"
        );
    }

    #[test]
    fn parse_manifest_with_policy_constraints() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: constrained-source
spec:
  provides:
    profiles:
      - secure
  policy:
    constraints:
      requireSignedCommits: true
      noScripts: true
      noSecretsRead: false
      allowSystemChanges: true
"#,
        )
        .unwrap();

        let mgr = SourceManager::new(dir.path());
        let manifest = mgr.parse_manifest("constrained", dir.path()).unwrap();
        assert!(manifest.spec.policy.constraints.require_signed_commits);
        assert!(manifest.spec.policy.constraints.no_scripts);
        assert!(!manifest.spec.policy.constraints.no_secrets_read);
        assert!(manifest.spec.policy.constraints.allow_system_changes);
    }

    // ─── CachedSource field verification ─────────────────────────

    #[test]
    fn cached_source_fields_via_get() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("my-src");
        std::fs::create_dir_all(&source_path).unwrap();

        let mut mgr = SourceManager::new(dir.path());
        insert_fake_source(&mut mgr, "my-src", source_path.clone());

        let cached = mgr.get("my-src").expect("source should be cached");
        assert_eq!(cached.name, "my-src");
        assert_eq!(cached.origin_url, "https://example.com/config.git");
        assert_eq!(cached.origin_branch, "main");
        assert_eq!(cached.local_path, source_path);
        assert_eq!(cached.manifest.kind, "ConfigSource");
        assert!(cached.last_commit.is_none());
        assert!(cached.last_fetched.is_none());
    }

    // ─── normalize_semver_pin — more edge cases ──────────────────

    #[test]
    fn normalize_semver_pin_tilde_three_part() {
        // Full three-part tilde passed through unchanged
        assert_eq!(normalize_semver_pin("~1.2.3"), "~1.2.3");
    }

    #[test]
    fn normalize_semver_pin_caret_three_part() {
        assert_eq!(normalize_semver_pin("^0.1.2"), "^0.1.2");
    }

    #[test]
    fn normalize_semver_pin_comparison_operators() {
        // Operators other than ~ and ^ are passed through
        assert_eq!(normalize_semver_pin(">1.0.0"), ">1.0.0");
        assert_eq!(normalize_semver_pin("<=2.0.0"), "<=2.0.0");
        assert_eq!(normalize_semver_pin(">=1.5.0, <2.0.0"), ">=1.5.0, <2.0.0");
    }

    #[test]
    fn normalize_semver_pin_wildcard() {
        assert_eq!(normalize_semver_pin("*"), "*");
    }

    // ─── SourceSpec serialization ────────────────────────────────

    #[test]
    fn source_spec_serde_roundtrip() {
        let spec = SourceManager::build_source_spec(
            "my-source",
            "https://github.com/org/config.git",
            Some("engineering"),
        );
        let yaml = serde_yaml::to_string(&spec).expect("serialize should succeed");
        let parsed: crate::config::SourceSpec =
            serde_yaml::from_str(&yaml).expect("deserialize should succeed");

        assert_eq!(parsed.name, "my-source");
        assert_eq!(parsed.origin.url, "https://github.com/org/config.git");
        assert_eq!(parsed.origin.branch, "master");
        assert_eq!(parsed.subscription.profile.as_deref(), Some("engineering"));
        assert_eq!(parsed.subscription.priority, 500);
        assert_eq!(parsed.sync.interval, "1h");
        assert!(!parsed.sync.auto_apply);
    }

    // ─── detect_source_manifest — with profile_details ───────────

    #[test]
    fn detect_source_manifest_with_profile_details_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SOURCE_MANIFEST_FILE),
            r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: details-only
spec:
  provides:
    profiles: []
    profileDetails:
      - name: dev
        description: "Developer profile"
  policy: {}
"#,
        )
        .unwrap();

        let result = detect_source_manifest(dir.path()).unwrap();
        assert!(
            result.is_some(),
            "should accept profile_details as valid profiles"
        );
        let doc = result.unwrap();
        assert_eq!(doc.metadata.name, "details-only");
        assert_eq!(doc.spec.provides.profile_details.len(), 1);
    }

    // --- load_source: local file URL rejection ---

    #[test]
    fn load_source_rejects_file_url() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        let printer = test_printer();

        let spec = crate::config::SourceSpec {
            name: "local-bad".into(),
            origin: crate::config::OriginSpec {
                origin_type: OriginType::Git,
                url: "file:///etc/shadow".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: Default::default(),
            sync: Default::default(),
        };

        let result = mgr.load_source(&spec, &printer);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("local file://") || err.contains("not allowed"),
            "expected file:// rejection, got: {err}"
        );
    }

    #[test]
    fn load_source_rejects_absolute_path_url() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        let printer = test_printer();

        let spec = crate::config::SourceSpec {
            name: "abs-bad".into(),
            origin: crate::config::OriginSpec {
                origin_type: OriginType::Git,
                url: "/tmp/local-repo".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: Default::default(),
            sync: Default::default(),
        };

        let result = mgr.load_source(&spec, &printer);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not allowed") || err.contains("absolute path"),
            "expected absolute path rejection, got: {err}"
        );
    }

    #[test]
    fn load_source_rejects_file_url_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        let printer = test_printer();

        let spec = crate::config::SourceSpec {
            name: "case-bad".into(),
            origin: crate::config::OriginSpec {
                origin_type: OriginType::Git,
                url: "FILE:///etc/passwd".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: Default::default(),
            sync: Default::default(),
        };

        let result = mgr.load_source(&spec, &printer);
        assert!(result.is_err(), "FILE:// should also be rejected");
    }

    // --- remove_source: already-deleted directory ---

    #[test]
    fn remove_source_missing_directory_still_removes_cache_entry() {
        let dir = tempfile::tempdir().unwrap();
        let missing_path = dir.path().join("already-gone");
        // Do NOT create the directory — simulate it being deleted externally

        let mut mgr = SourceManager::new(dir.path());
        insert_fake_source(&mut mgr, "already-gone", missing_path.clone());

        // Should succeed even though directory doesn't exist
        mgr.remove_source("already-gone")
            .expect("remove should succeed when directory is already gone");
        assert!(mgr.get("already-gone").is_none());
    }

    // --- check_version_pin: exact version match ---

    #[test]
    fn check_version_pin_exact_match() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let manifest = ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "test".into(),
                version: Some("1.2.3".into()),
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        };

        mgr.check_version_pin("test", &manifest, "=1.2.3")
            .expect("exact version should match");
    }

    #[test]
    fn check_version_pin_exact_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let manifest = ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "test".into(),
                version: Some("1.2.3".into()),
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        };

        let result = mgr.check_version_pin("test", &manifest, "=2.0.0");
        assert!(result.is_err());
    }

    // --- verify_commit_signature: constraints control ---

    #[test]
    fn verify_signature_required_but_allow_unsigned_skips() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());
        mgr.set_allow_unsigned(true);

        let constraints = crate::config::SourceConstraints {
            require_signed_commits: true,
            ..Default::default()
        };

        // Even though require_signed_commits is true, allow_unsigned bypasses it
        // This should succeed without even checking the repo
        let result = mgr.verify_commit_signature("test", dir.path(), &constraints);
        assert!(result.is_ok());
    }

    // --- head_commit: empty repo ---

    #[test]
    fn head_commit_empty_repo_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("empty-repo");
        git2::Repository::init(&repo_dir).unwrap();
        // No commits yet
        let result = SourceManager::head_commit(&repo_dir);
        assert!(
            result.is_none(),
            "empty repo with no commits should return None"
        );
    }

    // --- SourceManager: multiple operations ---

    #[test]
    fn source_manager_get_and_all_sources_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SourceManager::new(dir.path());

        assert!(mgr.all_sources().is_empty());
        assert!(mgr.get("nonexistent").is_none());

        let path = dir.path().join("src");
        std::fs::create_dir_all(&path).unwrap();
        insert_fake_source(&mut mgr, "src", path);

        assert_eq!(mgr.all_sources().len(), 1);
        assert!(mgr.get("src").is_some());
        assert!(mgr.get("other").is_none());
    }

    // --- load_source_profile: missing profile file variant ---

    #[test]
    fn load_source_profile_no_profiles_directory() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("src-no-profiles");
        std::fs::create_dir_all(&source_path).unwrap();
        // Don't create the profiles subdirectory

        let mut mgr = SourceManager::new(dir.path());
        insert_fake_source(&mut mgr, "src-no-profiles", source_path);

        let result = mgr.load_source_profile("src-no-profiles", "default");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found") || err.contains("ProfileNotFound"),
            "expected profile not found error, got: {err}"
        );
    }
}

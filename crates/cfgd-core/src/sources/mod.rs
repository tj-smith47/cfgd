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
use crate::output::{Printer, Role};

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
                    printer.status_simple(
                        Role::Warn,
                        format!("Failed to load source '{}': {}", spec.name, e),
                    );
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
        let cli_result = printer.run(&mut cmd, &label);
        let cli_ok = matches!(&cli_result, Ok(output) if output.status.success());

        if !cli_ok {
            // Fall back to libgit2 with spinner
            let spinner = printer.spinner(format!("Fetching source '{}' (libgit2)...", spec.name));

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

            match &fetch_result {
                Ok(_) => {
                    let _ = spinner.finish_ok(format!("Fetched source '{}' (libgit2)", spec.name));
                }
                Err(e) => {
                    let _ = spinner
                        .finish_fail(format!("Failed to fetch source '{}' (libgit2)", spec.name))
                        .detail(e.to_string());
                }
            }
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
        let cli_result = printer.run(&mut cmd, &label);
        if matches!(&cli_result, Ok(output) if output.status.success()) {
            // Restrict cloned directory to owner-only access
            let _ = crate::set_file_permissions(source_dir, 0o700);
            return Ok(());
        }

        // Clean up partial clone before libgit2 retry
        let _ = std::fs::remove_dir_all(source_dir);

        // Fall back to libgit2 with spinner
        let spinner = printer.spinner(format!("Cloning source '{}' (libgit2)...", spec.name));

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

        match &clone_result {
            Ok(_) => {
                let _ = spinner.finish_ok(format!("Cloned source '{}' (libgit2)", spec.name));
            }
            Err(e) => {
                let _ = spinner
                    .finish_fail(format!("Failed to clone source '{}' (libgit2)", spec.name))
                    .detail(e.to_string());
            }
        }
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
    classify_signature_status(name, &status)
}

/// Map a `git log --format=%G?` status code to a `Result`.
///
/// Status codes per `git-log(1)`:
/// - `G`: good (valid) signature
/// - `U`: good signature with unknown validity (untrusted key)
/// - `N`: no signature
/// - `B`: bad signature
/// - `E`: signature cannot be checked
/// - `X`: good signature that has expired
/// - `Y`: good signature made by an expired key
/// - `R`: good signature made by a revoked key
///
/// `G` and `U` are accepted (cfgd treats untrusted-key as "key not in keyring
/// yet" rather than a hard failure). Anything else is a verification failure.
pub(super) fn classify_signature_status(name: &str, status: &str) -> Result<()> {
    match status {
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
    let cli_result = printer.run(&mut cmd, &label);
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

    match &result {
        Ok(_) => {
            let _ = spinner.finish_ok(format!("Cloned {} (libgit2)", url));
        }
        Err(msg) => {
            let _ = spinner
                .finish_fail(format!("Failed to clone {} (libgit2)", url))
                .detail(msg.clone());
        }
    }
    result
}

#[cfg(test)]
mod tests;

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
}

impl SourceManager {
    /// Create a new SourceManager using the given cache directory.
    pub fn new(cache_dir: &Path) -> Self {
        Self {
            cache_dir: cache_dir.to_path_buf(),
            sources: HashMap::new(),
        }
    }

    /// Default cache directory: ~/.local/share/cfgd/sources/
    pub fn default_cache_dir() -> Result<PathBuf> {
        let base = directories::BaseDirs::new().ok_or_else(|| SourceError::CacheError {
            message: "cannot determine home directory".into(),
        })?;
        Ok(base.data_local_dir().join("cfgd").join("sources"))
    }

    /// Load all sources from config, fetching if needed.
    pub fn load_sources(&mut self, sources: &[SourceSpec], printer: &Printer) -> Result<()> {
        for spec in sources {
            match self.load_source(spec, printer) {
                Ok(()) => {}
                Err(e) => {
                    printer.warning(&format!("Failed to load source '{}': {}", spec.name, e));
                }
            }
        }
        Ok(())
    }

    /// Load a single source — clone or fetch, parse manifest, check version.
    pub fn load_source(&mut self, spec: &SourceSpec, printer: &Printer) -> Result<()> {
        let source_dir = self.cache_dir.join(&spec.name);

        if source_dir.exists() {
            self.fetch_source(spec, &source_dir, printer)?;
        } else {
            self.clone_source(spec, &source_dir, printer)?;
        }

        let manifest = self.parse_manifest(&spec.name, &source_dir)?;

        // Signature verification is not yet available. Log a warning so operators
        // are aware that source content is trusted without cryptographic verification.
        tracing::warn!(
            source = %spec.name,
            "Source '{}' loaded without signature verification — content is trusted as-is",
            spec.name
        );

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
        printer.info(&format!("Fetching source '{}'...", spec.name));

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

        remote
            .fetch(&[&spec.origin.branch], Some(&mut fo), None)
            .map_err(|e| SourceError::FetchFailed {
                name: spec.name.clone(),
                message: e.to_string(),
            })?;

        // Fast-forward to FETCH_HEAD
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

    /// Clone a new source repo.
    fn clone_source(&self, spec: &SourceSpec, source_dir: &Path, printer: &Printer) -> Result<()> {
        printer.info(&format!(
            "Cloning source '{}' from {}...",
            spec.name, spec.origin.url
        ));

        std::fs::create_dir_all(source_dir).map_err(|e| SourceError::CacheError {
            message: format!("cannot create cache dir: {}", e),
        })?;

        let mut fo = FetchOptions::new();
        let mut callbacks = RemoteCallbacks::new();
        callbacks.credentials(crate::git_ssh_credentials);
        fo.remote_callbacks(callbacks);

        let mut builder = git2::build::RepoBuilder::new();
        builder.fetch_options(fo);
        builder.branch(&spec.origin.branch);

        builder
            .clone(&spec.origin.url, source_dir)
            .map_err(|e| SourceError::FetchFailed {
                name: spec.name.clone(),
                message: e.to_string(),
            })?;

        Ok(())
    }

    /// Parse the ConfigSource manifest from a source directory.
    pub fn parse_manifest(&self, name: &str, source_dir: &Path) -> Result<ConfigSourceDocument> {
        read_manifest(name, source_dir)
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
                branch: "main".to_string(),
                auth: None,
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

/// Clone a git repo with git2, falling back to the git CLI for SSH URLs.
/// Returns Ok(()) on success, Err with description on failure.
pub fn git_clone_with_fallback(url: &str, target: &Path) -> std::result::Result<(), String> {
    match git2::Repository::clone(url, target) {
        Ok(_) => Ok(()),
        Err(e) => {
            let status = std::process::Command::new("git")
                .args(["clone", url, &target.display().to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();

            match status {
                Ok(s) if s.success() => Ok(()),
                Ok(_) => Err(format!("Failed to clone {}: {}", url, e)),
                Err(cli_err) => Err(format!(
                    "Failed to clone {}: {} (git cli: {})",
                    url, e, cli_err
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let result = mgr.remove_source("nonexistent");
        assert!(result.is_err());
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
apiVersion: cfgd/v1
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
        let result = detect_source_manifest(dir.path());
        assert!(result.is_err());
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
apiVersion: cfgd/v1
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
      no-scripts: true
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
            api_version: "cfgd/v1".into(),
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

        assert!(mgr.check_version_pin("test", &manifest, "~2").is_ok());
        assert!(mgr.check_version_pin("test", &manifest, "^2").is_ok());
        assert!(mgr.check_version_pin("test", &manifest, "~2.1").is_ok());
    }

    #[test]
    fn check_version_pin_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SourceManager::new(dir.path());

        let manifest = ConfigSourceDocument {
            api_version: "cfgd/v1".into(),
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

        let result = mgr.check_version_pin("test", &manifest, "~2");
        assert!(result.is_err());
    }
}

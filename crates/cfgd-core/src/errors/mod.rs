// Domain error types — thiserror for library errors, anyhow only at CLI boundary

use std::path::PathBuf;

use crate::PathDisplayExt;

pub type Result<T> = std::result::Result<T, CfgdError>;

/// Render a path list as `'a', 'b', 'c'` (posix separators) for single-line
/// error messages that must name every candidate.
fn join_quoted_posix(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| format!("'{}'", p.posix()))
        .collect::<Vec<_>>()
        .join(", ")
}

// Top-level variants print `"<category>: <inner>"` because `{0}` expands the
// inner error's Display once. `main.rs` formats with `{}`, which emits this
// single-layer message. Do NOT switch `main.rs` to `{:#}` — that also walks
// `source()` (via `#[from]`) and would duplicate the inner text. The
// Composition variant uses `#[source]` (not `#[from]`) because a manual
// `From<CompositionError>` impl exists for error-context wrapping.
#[derive(Debug, thiserror::Error)]
pub enum CfgdError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("file error: {0}")]
    File(#[from] FileError),

    #[error("package error: {0}")]
    Package(#[from] PackageError),

    #[error("secret error: {0}")]
    Secret(#[from] SecretError),

    #[error("state error: {0}")]
    State(#[from] StateError),

    #[error("daemon error: {0}")]
    Daemon(#[from] DaemonError),

    #[error("source error: {0}")]
    Source(#[from] SourceError),

    #[error("composition error: {0}")]
    Composition(#[source] Box<CompositionError>),

    #[error("upgrade error: {0}")]
    Upgrade(#[from] UpgradeError),

    #[error("module error: {0}")]
    Module(#[from] ModuleError),

    #[error("generate error: {0}")]
    Generate(#[from] GenerateError),

    #[error("oci error: {0}")]
    Oci(#[from] OciError),

    #[error("skill error: {0}")]
    Skill(#[from] SkillError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    NotFound { path: PathBuf },

    #[error(
        "cannot resolve home directory (HOME unset) to locate config at {path}; set HOME or pass --config <path>"
    )]
    HomeUnresolved { path: PathBuf },

    #[error("invalid config: {message}")]
    Invalid { message: String },

    #[error(
        "unsupported apiVersion {found:?}; this build supports {}",
        crate::API_VERSION
    )]
    UnsupportedApiVersion { found: String },

    #[error("circular profile inheritance: {chain:?}")]
    CircularInheritance { chain: Vec<String> },

    #[error("profile not found: {name}")]
    ProfileNotFound { name: String },

    #[error(
        "ambiguous profile '{name}': multiple forms exist ({forms}) — delete or rename one of them (the canonical form is '{name}/profile.yaml')",
        forms = join_quoted_posix(.paths)
    )]
    AmbiguousProfile { name: String, paths: Vec<PathBuf> },

    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("source file not found: {path}")]
    SourceNotFound { path: PathBuf },

    #[error("target path not writable: {path}")]
    TargetNotWritable { path: PathBuf },

    #[error("template rendering failed for {path}: {message}")]
    TemplateError { path: PathBuf, message: String },

    #[error("permission denied setting mode {mode:#o} on {path}")]
    PermissionDenied { path: PathBuf, mode: u32 },

    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error(
        "file conflict: {target} is targeted by both '{source_a}' and '{source_b}' with different content"
    )]
    Conflict {
        target: PathBuf,
        source_a: String,
        source_b: String,
    },

    #[error("source file changed between plan and apply: {path}")]
    SourceChanged { path: PathBuf },

    #[error("path {path} escapes root directory {root}")]
    PathTraversal { path: PathBuf, root: PathBuf },

    #[error(
        "source file '{path}' must be encrypted with '{backend}' but appears to be unencrypted"
    )]
    NotEncrypted { path: PathBuf, backend: String },

    #[error("unknown encryption backend '{backend}' — supported: sops, age")]
    UnknownEncryptionBackend { backend: String },

    #[error(
        "encryption mode 'Always' is incompatible with strategy '{strategy}' for '{path}' — use Copy or Template instead"
    )]
    EncryptionStrategyIncompatible { path: PathBuf, strategy: String },
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("package manager '{manager}' not available")]
    ManagerNotAvailable { manager: String },

    #[error("{manager} install failed: {message}")]
    InstallFailed { manager: String, message: String },

    #[error("{manager} uninstall failed: {message}")]
    UninstallFailed { manager: String, message: String },

    #[error("{manager} failed to list installed packages: {message}")]
    ListFailed { manager: String, message: String },

    #[error("{manager} command failed: {source}")]
    CommandFailed {
        manager: String,
        source: std::io::Error,
    },

    #[error("{manager} bootstrap failed: {message}")]
    BootstrapFailed { manager: String, message: String },

    #[error("package manager '{manager}' not found in registry")]
    ManagerNotFound { manager: String },
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("sops not found — install: https://github.com/getsops/sops#install")]
    SopsNotFound,

    #[error("sops encryption failed for {path}: {message}")]
    EncryptionFailed { path: PathBuf, message: String },

    #[error("sops decryption failed for {path}: {message}")]
    DecryptionFailed { path: PathBuf, message: String },

    #[error("secret provider '{provider}' not available — {hint}")]
    ProviderNotAvailable { provider: String, hint: String },

    #[error("secret reference unresolvable: {reference}")]
    UnresolvableRef { reference: String },

    #[error("age key not found at {path}")]
    AgeKeyNotFound { path: PathBuf },
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("state database error: {0}")]
    Database(String),

    #[error("migration failed: {message}")]
    MigrationFailed { message: String },

    #[error("state directory not writable: {path}")]
    DirectoryNotWritable { path: PathBuf },

    #[error("state filesystem I/O failed at {path}: {source}")]
    FilesystemIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("state serialization failed ({context}): {source}")]
    Serialize {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("apply lock held by another process: {holder}")]
    ApplyLockHeld { holder: String },
}

impl From<rusqlite::Error> for StateError {
    fn from(e: rusqlite::Error) -> Self {
        StateError::Database(e.to_string())
    }
}

impl From<CompositionError> for CfgdError {
    fn from(e: CompositionError) -> Self {
        CfgdError::Composition(Box::new(e))
    }
}

impl From<rusqlite::Error> for CfgdError {
    fn from(e: rusqlite::Error) -> Self {
        CfgdError::State(StateError::Database(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("source '{name}' not found")]
    NotFound { name: String },

    #[error("failed to fetch source '{name}': {message}")]
    FetchFailed { name: String, message: String },

    #[error("invalid ConfigSource manifest in '{name}': {message}")]
    InvalidManifest { name: String, message: String },

    #[error("no git ref matched pin '{pin}' for source '{name}'{}", .available.as_ref().map(|a| format!(" (available tags: {a})")).unwrap_or_default())]
    PinRefNotFound {
        name: String,
        pin: String,
        available: Option<String>,
    },

    #[error("source '{name}' provides neither profiles nor modules")]
    EmptyProvides { name: String },

    #[error("profile '{profile}' not found in source '{name}'")]
    ProfileNotFound { name: String, profile: String },

    #[error("source cache error: {message}")]
    CacheError { message: String },

    #[error("git error for source '{name}': {message}")]
    GitError { name: String, message: String },

    #[error("signature verification failed for source '{name}': {message}")]
    SignatureVerificationFailed { name: String, message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum CompositionError {
    #[error("cannot override locked resource '{resource}' from source '{source_name}'")]
    LockedResource {
        source_name: String,
        resource: String,
    },

    #[error("cannot remove required resource '{resource}' from source '{source_name}'")]
    RequiredResource {
        source_name: String,
        resource: String,
    },

    #[error("path '{path}' not in allowed paths for source '{source_name}'")]
    PathNotAllowed { source_name: String, path: String },

    #[error("invalid overrides for source '{source_name}': {message}")]
    InvalidOverrides {
        source_name: String,
        message: String,
    },

    #[error(
        "unknown reject key '{key}' for source '{source_name}' (allowed: packages, env, aliases, modules)"
    )]
    InvalidReject { source_name: String, key: String },

    #[error("source '{source_name}' is not allowed to run scripts")]
    ScriptsNotAllowed { source_name: String },

    #[error(
        "required source '{source_name}' is not available (not synced or failed to load); fix the source or set its sync.required to false"
    )]
    RequiredSourceUnavailable { source_name: String },

    #[error("source '{source_name}' template attempted to access local variable '{variable}'")]
    TemplateSandboxViolation {
        source_name: String,
        variable: String,
    },

    #[error(
        "source '{source_name}' attempted to modify system setting '{setting}' without permission"
    )]
    SystemChangeNotAllowed {
        source_name: String,
        setting: String,
    },

    #[error("conflict on '{resource}' between sources: {source_names:?}")]
    UnresolvableConflict {
        resource: String,
        source_names: Vec<String>,
    },

    #[error(
        "file '{path}' matches required-encryption target '{pattern}' in source '{source_name}' but has no encryption block"
    )]
    EncryptionRequired {
        source_name: String,
        path: String,
        pattern: String,
    },

    #[error(
        "file '{path}' matches required-encryption target '{pattern}' in source '{source_name}' but uses backend '{actual_backend}' instead of required '{required_backend}'"
    )]
    EncryptionBackendMismatch {
        source_name: String,
        path: String,
        pattern: String,
        actual_backend: String,
        required_backend: String,
    },

    #[error(
        "file '{path}' matches required-encryption target '{pattern}' in source '{source_name}' but uses mode '{actual_mode}' instead of required '{required_mode}'"
    )]
    EncryptionModeMismatch {
        source_name: String,
        path: String,
        pattern: String,
        actual_mode: String,
        required_mode: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("failed to query GitHub releases: {message}")]
    ApiError { message: String },

    #[error("no release found for {os}/{arch}")]
    NoAsset { os: String, arch: String },

    #[error("download failed: {message}")]
    DownloadFailed { message: String },

    #[error("checksum verification failed for {file}")]
    ChecksumMismatch { file: String },

    #[error("no checksum (.sha256) published for {file}")]
    ChecksumMissing { file: String },

    #[error("published checksum file was empty or malformed")]
    ChecksumsEmpty,

    #[error("failed to install binary: {message}")]
    InstallFailed { message: String },

    #[error("version parse error: {message}")]
    VersionParse { message: String },

    #[error(
        "strict cosign verification required but unavailable: {reason} — re-run without --require-cosign / unset CFGD_REQUIRE_COSIGN to allow SHA256-only fallback"
    )]
    CosignRequired { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("module not found: {name}")]
    NotFound { name: String },

    #[error(
        "module '{name}' is declared by source '{source_name}' (provides.modules) but its body is missing from that source — the publisher must add modules/{name}/module.yaml, or run 'cfgd source update {source_name}'"
    )]
    OfferedButMissing { name: String, source_name: String },

    #[error("module registry not found: {name}")]
    RegistryNotFound { name: String },

    #[error("module dependency cycle: {chain:?}")]
    DependencyCycle { chain: Vec<String> },

    #[error("module '{module}' depends on '{dependency}' which is not available")]
    MissingDependency { module: String, dependency: String },

    #[error(
        "module '{module}' depends on '{dependency}', which is skipped on this platform ({dependency} requires: {platforms})"
    )]
    DependencyPlatformSkipped {
        module: String,
        dependency: String,
        platforms: String,
    },

    #[error(
        "package '{package}' in module '{module}' cannot be resolved: no available manager satisfies the requirements (minVersion: {min_version})"
    )]
    UnresolvablePackage {
        module: String,
        package: String,
        min_version: String,
    },

    #[error("failed to fetch git source for module '{module}': {url}: {message}")]
    GitFetchFailed {
        module: String,
        url: String,
        message: String,
    },

    #[error("module '{name}' has invalid spec: {message}")]
    InvalidSpec { name: String, message: String },

    #[error(
        "module '{module}' delivered by source '{source_name}' carries {kind}, but that source is not allowed to run scripts (set subscription.allowScripts: true to opt in, or relax the source's constraints.no_scripts)"
    )]
    ScriptsNotAllowed {
        source_name: String,
        module: String,
        kind: String,
    },

    #[error(
        "lockfile integrity check failed for module '{name}': expected {expected}, got {actual}"
    )]
    IntegrityMismatch {
        name: String,
        expected: String,
        actual: String,
    },

    #[error(
        "remote module '{name}' requires a pinned ref (tag or commit) — branch tracking is not allowed for security"
    )]
    UnpinnedRemoteModule { name: String },

    #[error("module source fetch failed for '{url}': {message}")]
    SourceFetchFailed { url: String, message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error("validation failed: {message}")]
    ValidationFailed { message: String },

    #[error("file access denied: {path} — {reason}")]
    FileAccessDenied { path: PathBuf, reason: String },

    #[error("AI provider error: {message}")]
    ProviderError { message: String },

    #[error("API key not found in environment variable '{env_var}'")]
    ApiKeyNotFound { env_var: String },
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("failed to render skill for provider '{provider}': {message}")]
    Render { provider: String, message: String },

    #[error("provider detection failed for '{provider}': {message}")]
    Detect { provider: String, message: String },

    #[error("failed to write skill file: {0}")]
    Write(#[source] std::io::Error),

    #[error("failed to acquire skill-file lock: {0}")]
    Lock(#[source] Box<CfgdError>),
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("daemon already running (pid {pid})")]
    AlreadyRunning { pid: u32 },

    #[error("health socket unavailable: {message}")]
    HealthSocketError { message: String },

    #[error("service install failed: {message}")]
    ServiceInstallFailed { message: String },

    #[error("service error: {message}")]
    ServiceError { message: String },

    #[error("watch error: {message}")]
    WatchError { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum OciError {
    #[error("invalid OCI reference: {reference}")]
    InvalidReference { reference: String },

    #[error("registry authentication failed for {registry}: {message}")]
    AuthFailed { registry: String, message: String },

    #[error("registry request failed: {message}")]
    RequestFailed { message: String },

    #[error("blob upload failed for {digest}: {message}")]
    BlobUploadFailed { digest: String, message: String },

    #[error("manifest push failed: {message}")]
    ManifestPushFailed { message: String },

    #[error("manifest not found: {reference}")]
    ManifestNotFound { reference: String },

    #[error("blob not found: {digest}")]
    BlobNotFound { digest: String },

    #[error("module.yaml not found in {dir}")]
    ModuleYamlNotFound { dir: PathBuf },

    #[error("archive error: {message}")]
    ArchiveError { message: String },

    #[error("build error: {message}")]
    BuildError { message: String },

    #[error("signing error: {message}")]
    SigningError { message: String },

    #[error("signature verification failed for {reference}: {message}")]
    VerificationFailed { reference: String, message: String },

    #[error("attestation error: {message}")]
    AttestationError { message: String },

    #[error("{tool} not found — install it or add it to PATH")]
    ToolNotFound { tool: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table-driven: every `From<SubError> for CfgdError` variant in one test.
    #[test]
    fn all_sub_errors_convert_to_cfgd_error() {
        let cases: Vec<(CfgdError, &str)> = vec![
            (
                ConfigError::ProfileNotFound {
                    name: "test".into(),
                }
                .into(),
                "test",
            ),
            (
                SourceError::NotFound {
                    name: "acme".into(),
                }
                .into(),
                "acme",
            ),
            (
                CompositionError::LockedResource {
                    source_name: "acme".into(),
                    resource: "~/.config/security.yaml".into(),
                }
                .into(),
                "locked",
            ),
            (
                UpgradeError::ChecksumMismatch {
                    file: "cfgd-0.2.0-linux-x86_64.tar.gz".into(),
                }
                .into(),
                "checksum",
            ),
            (
                ModuleError::NotFound {
                    name: "nvim".into(),
                }
                .into(),
                "nvim",
            ),
            (
                GenerateError::ValidationFailed {
                    message: "missing apiVersion".into(),
                }
                .into(),
                "missing apiVersion",
            ),
            (
                std::io::Error::new(std::io::ErrorKind::NotFound, "file missing").into(),
                "file missing",
            ),
        ];
        for (cfgd_err, needle) in &cases {
            assert!(
                cfgd_err.to_string().contains(needle),
                "expected '{}' in: {}",
                needle,
                cfgd_err,
            );
        }
    }

    #[test]
    fn rusqlite_error_converts_to_state_and_cfgd_errors() {
        let state_err: StateError = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(
            matches!(state_err, StateError::Database(_)),
            "rusqlite error must map to StateError::Database",
        );

        let cfgd_err: CfgdError = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(
            matches!(cfgd_err, CfgdError::State(StateError::Database(_))),
            "rusqlite error must map to CfgdError::State(Database)",
        );
    }
}

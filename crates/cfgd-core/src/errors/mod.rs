// Domain error types — thiserror for library errors, anyhow only at CLI boundary

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, CfgdError>;

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
    Composition(#[from] CompositionError),

    #[error("upgrade error: {0}")]
    Upgrade(#[from] UpgradeError),

    #[error("module error: {0}")]
    Module(#[from] ModuleError),

    #[error("generate error: {0}")]
    Generate(#[from] GenerateError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    NotFound { path: PathBuf },

    #[error("invalid config: {message}")]
    Invalid { message: String },

    #[error("circular profile inheritance: {chain:?}")]
    CircularInheritance { chain: Vec<String> },

    #[error("profile not found: {name}")]
    ProfileNotFound { name: String },

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

    #[error("apply lock held by another process: {holder}")]
    ApplyLockHeld { holder: String },
}

impl From<rusqlite::Error> for StateError {
    fn from(e: rusqlite::Error) -> Self {
        StateError::Database(e.to_string())
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

    #[error("source version {version} does not match pin {pin} for '{name}'")]
    VersionMismatch {
        name: String,
        version: String,
        pin: String,
    },

    #[error("source '{name}' contains no profiles")]
    NoProfiles { name: String },

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

    #[error("source '{source_name}' is not allowed to run scripts")]
    ScriptsNotAllowed { source_name: String },

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

    #[error("failed to install binary: {message}")]
    InstallFailed { message: String },

    #[error("version parse error: {message}")]
    VersionParse { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("module not found: {name}")]
    NotFound { name: String },

    #[error("module dependency cycle: {chain:?}")]
    DependencyCycle { chain: Vec<String> },

    #[error("module '{module}' depends on '{dependency}' which is not available")]
    MissingDependency { module: String, dependency: String },

    #[error(
        "package '{package}' in module '{module}' cannot be resolved: no available manager satisfies the requirements (min-version: {min_version})"
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
pub enum DaemonError {
    #[error("daemon already running (pid {pid})")]
    AlreadyRunning { pid: u32 },

    #[error("health socket unavailable: {message}")]
    HealthSocketError { message: String },

    #[error("service install failed: {message}")]
    ServiceInstallFailed { message: String },

    #[error("watch error: {message}")]
    WatchError { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_converts_to_cfgd_error() {
        let err = ConfigError::ProfileNotFound {
            name: "test".into(),
        };
        let cfgd_err: CfgdError = err.into();
        assert!(matches!(cfgd_err, CfgdError::Config(_)));
        assert!(cfgd_err.to_string().contains("test"));
    }

    #[test]
    fn file_error_displays_path() {
        let err = FileError::SourceNotFound {
            path: PathBuf::from("/home/user/.zshrc"),
        };
        assert!(err.to_string().contains("/home/user/.zshrc"));
    }

    #[test]
    fn package_error_includes_manager_name() {
        let err = PackageError::ManagerNotAvailable {
            manager: "brew".into(),
        };
        assert!(err.to_string().contains("brew"));
    }

    #[test]
    fn secret_error_includes_install_hint() {
        let err = SecretError::SopsNotFound;
        assert!(err.to_string().contains("https://"));
    }

    #[test]
    fn source_error_converts_to_cfgd_error() {
        let err = SourceError::NotFound {
            name: "acme".into(),
        };
        let cfgd_err: CfgdError = err.into();
        assert!(matches!(cfgd_err, CfgdError::Source(_)));
        assert!(cfgd_err.to_string().contains("acme"));
    }

    #[test]
    fn composition_error_converts_to_cfgd_error() {
        let err = CompositionError::LockedResource {
            source_name: "acme".into(),
            resource: "~/.config/security.yaml".into(),
        };
        let cfgd_err: CfgdError = err.into();
        assert!(matches!(cfgd_err, CfgdError::Composition(_)));
        assert!(cfgd_err.to_string().contains("locked"));
    }

    #[test]
    fn source_version_mismatch_includes_details() {
        let err = SourceError::VersionMismatch {
            name: "acme".into(),
            version: "3.0.0".into(),
            pin: "~2".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("3.0.0"));
        assert!(msg.contains("~2"));
    }

    #[test]
    fn upgrade_error_converts_to_cfgd_error() {
        let err = UpgradeError::ChecksumMismatch {
            file: "cfgd-0.2.0-linux-x86_64.tar.gz".into(),
        };
        let cfgd_err: CfgdError = err.into();
        assert!(matches!(cfgd_err, CfgdError::Upgrade(_)));
        assert!(cfgd_err.to_string().contains("checksum"));
    }

    #[test]
    fn module_error_converts_to_cfgd_error() {
        let err = ModuleError::NotFound {
            name: "nvim".into(),
        };
        let cfgd_err: CfgdError = err.into();
        assert!(matches!(cfgd_err, CfgdError::Module(_)));
        assert!(cfgd_err.to_string().contains("nvim"));
    }

    #[test]
    fn module_error_cycle_includes_chain() {
        let err = ModuleError::DependencyCycle {
            chain: vec!["a".into(), "b".into(), "a".into()],
        };
        let msg = err.to_string();
        assert!(msg.contains("a"));
        assert!(msg.contains("b"));
    }

    #[test]
    fn io_error_converts_to_cfgd_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let cfgd_err: CfgdError = io_err.into();
        assert!(matches!(cfgd_err, CfgdError::Io(_)));
    }

    #[test]
    fn generate_error_converts_to_cfgd_error() {
        let err = GenerateError::ValidationFailed {
            message: "missing apiVersion".into(),
        };
        let cfgd_err: CfgdError = err.into();
        assert!(matches!(cfgd_err, CfgdError::Generate(_)));
        assert!(cfgd_err.to_string().contains("missing apiVersion"));
    }
}

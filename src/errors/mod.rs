// Domain error types — thiserror for library errors, anyhow only at CLI boundary
// Error variants defined in Phase 1 for use in later phases (4-9)
#![allow(dead_code)]

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
    fn io_error_converts_to_cfgd_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let cfgd_err: CfgdError = io_err.into();
        assert!(matches!(cfgd_err, CfgdError::Io(_)));
    }
}

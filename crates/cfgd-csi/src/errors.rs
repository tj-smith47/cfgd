use thiserror::Error;

#[derive(Debug, Error)]
pub enum CsiError {
    #[error("module not found: {name}:{version}")]
    ModuleNotFound { name: String, version: String },

    #[error("OCI pull failed: {0}")]
    PullFailed(#[from] cfgd_core::errors::OciError),

    #[error("mount failed: {message}")]
    MountFailed { message: String },

    #[error("cache error: {message}")]
    CacheError { message: String },

    #[error("invalid volume attribute: {key}")]
    InvalidAttribute { key: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

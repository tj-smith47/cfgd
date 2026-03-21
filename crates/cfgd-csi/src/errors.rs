use thiserror::Error;

#[derive(Debug, Error)]
pub enum CsiError {
    #[error("OCI pull failed: {0}")]
    PullFailed(Box<cfgd_core::errors::OciError>),

    #[error("invalid volume attribute: {key}")]
    InvalidAttribute { key: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<cfgd_core::errors::OciError> for CsiError {
    fn from(e: cfgd_core::errors::OciError) -> Self {
        CsiError::PullFailed(Box::new(e))
    }
}

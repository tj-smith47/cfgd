use thiserror::Error;

#[derive(Debug, Error)]
pub enum CsiError {
    #[error("OCI pull failed: {0}")]
    PullFailed(Box<cfgd_core::errors::OciError>),

    #[error("invalid volume attribute: {key}")]
    InvalidAttribute { key: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Metrics HTTP server setup or serve failure. Emitted by
    /// `metrics::serve_metrics` on bind / serve errors. In current production
    /// flow this surfaces only through tests because `serve_metrics` is
    /// invoked via fire-and-forget `tokio::spawn` from `app::run`, with errors
    /// logged via `tracing`. The typed variant preserves a clean error
    /// contract should propagation tighten later.
    #[error("metrics server error: {0}")]
    Metrics(String),
}

impl From<cfgd_core::errors::OciError> for CsiError {
    fn from(e: cfgd_core::errors::OciError) -> Self {
        CsiError::PullFailed(Box::new(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::errors::OciError;

    #[test]
    fn pullfailed_display_includes_inner_oci_error_message() {
        let oci_err = OciError::ManifestNotFound {
            reference: "ghcr.io/example/mod:v1".into(),
        };
        let e = CsiError::PullFailed(Box::new(oci_err));
        assert_eq!(
            format!("{e}"),
            "OCI pull failed: manifest not found: ghcr.io/example/mod:v1"
        );
    }

    #[test]
    fn from_oci_error_wraps_in_pullfailed_preserving_inner() {
        let oci_err = OciError::ManifestNotFound {
            reference: "ghcr.io/example/mod:v1".into(),
        };
        let csi: CsiError = oci_err.into();
        match csi {
            CsiError::PullFailed(boxed) => {
                assert_eq!(
                    format!("{boxed}"),
                    "manifest not found: ghcr.io/example/mod:v1"
                );
            }
            other => panic!("expected PullFailed, got {other:?}"),
        }
    }

    #[test]
    fn invalid_attribute_display_uses_exact_key() {
        let e = CsiError::InvalidAttribute {
            key: "csi.cfgd.io/oci-uri".into(),
        };
        assert_eq!(
            format!("{e}"),
            "invalid volume attribute: csi.cfgd.io/oci-uri"
        );
    }

    #[test]
    fn from_io_error_wraps_in_io_variant_preserving_kind_and_message() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such device");
        let csi: CsiError = io_err.into();
        match csi {
            CsiError::Io(io) => {
                assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
                assert_eq!(format!("{io}"), "no such device");
            }
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn io_display_uses_io_error_prefix() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such device");
        let csi: CsiError = io_err.into();
        assert_eq!(format!("{csi}"), "IO error: no such device");
    }
}

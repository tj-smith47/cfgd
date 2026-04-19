//! Exit-code taxonomy for the cfgd CLI.
//!
//! Scripted consumers rely on distinct exit codes to choose follow-up
//! actions without parsing stderr. This module defines every code cfgd
//! itself emits and provides the error-to-code mapping used by the CLI
//! entry point.
//!
//! # Codes
//!
//! | Code | Variant           | Meaning                                                  |
//! |------|-------------------|----------------------------------------------------------|
//! | 0    | [`Success`]       | Operation completed without error.                       |
//! | 1    | [`Error`]         | Generic failure (network, IO, unclassified internal).    |
//! | 2    | [`UpdateAvailable`] | `upgrade --check`: a newer release exists.             |
//! | 3    | [`NoConfig`]      | No cfgd config file at the resolved path.                |
//! | 4    | [`ConfigInvalid`] | Config file exists but failed parse or validation.       |
//! | 5    | [`DriftDetected`] | `diff`/`status` with `--exit-code`: drift present.       |
//!
//! External-process passthrough (e.g. `kubectl exec` forwarded by the
//! `kubectl cfgd` plugin) is out of scope for this enum — those codes
//! belong to the invoked tool, not to cfgd.
//!
//! [`Success`]: ExitCode::Success
//! [`Error`]: ExitCode::Error
//! [`UpdateAvailable`]: ExitCode::UpdateAvailable
//! [`NoConfig`]: ExitCode::NoConfig
//! [`ConfigInvalid`]: ExitCode::ConfigInvalid
//! [`DriftDetected`]: ExitCode::DriftDetected

use crate::errors::{CfgdError, ConfigError};

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Success = 0,
    Error = 1,
    UpdateAvailable = 2,
    NoConfig = 3,
    ConfigInvalid = 4,
    DriftDetected = 5,
}

impl ExitCode {
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Terminate the current process with this exit code.
    pub fn exit(self) -> ! {
        std::process::exit(self.as_i32())
    }
}

impl From<ExitCode> for i32 {
    fn from(code: ExitCode) -> i32 {
        code as i32
    }
}

/// Map a [`CfgdError`] to the most specific exit code available.
///
/// Only config-setup errors are differentiated today — runtime errors
/// (network, filesystem, provider) all collapse to [`ExitCode::Error`]
/// because scripted consumers generally can't act on those without
/// reading the message anyway. Extend this function when a new variant
/// warrants a distinct code.
pub fn exit_code_for_error(err: &CfgdError) -> ExitCode {
    match err {
        CfgdError::Config(ConfigError::NotFound { .. }) => ExitCode::NoConfig,
        CfgdError::Config(_) => ExitCode::ConfigInvalid,
        _ => ExitCode::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn distinct_integer_codes() {
        let codes = [
            ExitCode::Success.as_i32(),
            ExitCode::Error.as_i32(),
            ExitCode::UpdateAvailable.as_i32(),
            ExitCode::NoConfig.as_i32(),
            ExitCode::ConfigInvalid.as_i32(),
            ExitCode::DriftDetected.as_i32(),
        ];
        let mut seen = std::collections::HashSet::new();
        for c in codes {
            assert!(seen.insert(c), "duplicate exit code {}", c);
        }
    }

    #[test]
    fn stable_wire_values() {
        // These are consumed by downstream shell scripts. Changing any of
        // these numbers is a breaking change — update this test and
        // document the change in a release note.
        assert_eq!(ExitCode::Success.as_i32(), 0);
        assert_eq!(ExitCode::Error.as_i32(), 1);
        assert_eq!(ExitCode::UpdateAvailable.as_i32(), 2);
        assert_eq!(ExitCode::NoConfig.as_i32(), 3);
        assert_eq!(ExitCode::ConfigInvalid.as_i32(), 4);
        assert_eq!(ExitCode::DriftDetected.as_i32(), 5);
    }

    #[test]
    fn config_not_found_maps_to_no_config() {
        let err = CfgdError::Config(ConfigError::NotFound {
            path: PathBuf::from("/nonexistent/cfgd.yaml"),
        });
        assert_eq!(exit_code_for_error(&err), ExitCode::NoConfig);
    }

    #[test]
    fn config_invalid_maps_to_config_invalid() {
        let err = CfgdError::Config(ConfigError::Invalid {
            message: "missing apiVersion".into(),
        });
        assert_eq!(exit_code_for_error(&err), ExitCode::ConfigInvalid);
        let err = CfgdError::Config(ConfigError::ProfileNotFound { name: "dev".into() });
        assert_eq!(exit_code_for_error(&err), ExitCode::ConfigInvalid);
    }

    #[test]
    fn non_config_error_maps_to_generic() {
        let err = CfgdError::Io(std::io::Error::other("boom"));
        assert_eq!(exit_code_for_error(&err), ExitCode::Error);
    }

    #[test]
    fn i32_conversion_matches_as_i32() {
        let code: i32 = ExitCode::DriftDetected.into();
        assert_eq!(code, ExitCode::DriftDetected.as_i32());
    }
}

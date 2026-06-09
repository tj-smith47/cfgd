//! Minimal argv surface for the `cfgd-operator` binary.
//!
//! The operator is configured entirely by environment variables and has no
//! subcommands. This root exists solely so the binary answers `--version` and
//! `--help` instantly (without connecting to a cluster) and rejects unknown
//! arguments, instead of silently ignoring argv and hanging on the cluster
//! connect.

use clap::Parser;

/// cfgd Kubernetes operator — CRD-based node configuration.
///
/// Configured via environment variables (e.g. `DEVICE_GATEWAY_ENABLED`,
/// `DEVICE_GATEWAY_STANDALONE`, `HEALTH_PORT`, `METRICS_PORT`,
/// `WEBHOOK_CERT_DIR`); there are no arguments beyond `--version`/`--help`.
#[derive(Parser, Debug)]
#[command(name = "cfgd-operator", version, about, long_about = None)]
pub struct Args {}

/// Parse the operator argv. On `--version`/`--help` or an argv error, clap
/// returns an `Err` whose `kind()`/`print()`/`exit()` carry the intended
/// behavior (print to stdout/stderr, exit 0 or 2); on a normal no-arg
/// invocation it returns `Ok(Args)` and the caller proceeds to `app::run()`.
pub fn parse_args<I, T>(argv: I) -> Result<Args, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    Args::try_parse_from(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn no_args_parses_ok() {
        let parsed = parse_args(["cfgd-operator"]);
        assert!(
            parsed.is_ok(),
            "no-arg invocation must parse Ok so main falls through to app::run(), got {parsed:?}"
        );
    }

    #[test]
    fn version_flag_yields_display_version_error() {
        let err = parse_args(["cfgd-operator", "--version"])
            .expect_err("--version must short-circuit via a clap error, not Ok");
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        assert_eq!(err.exit_code(), 0, "--version exits 0");
        assert!(
            err.to_string().contains(env!("CARGO_PKG_VERSION")),
            "--version output must contain the crate version, got: {err}"
        );
        assert!(
            err.to_string().contains("cfgd-operator"),
            "--version output must contain the binary name, got: {err}"
        );
    }

    #[test]
    fn help_flag_yields_display_help_error() {
        let err = parse_args(["cfgd-operator", "--help"])
            .expect_err("--help must short-circuit via a clap error, not Ok");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        assert_eq!(err.exit_code(), 0, "--help exits 0");
    }

    #[test]
    fn short_help_flag_yields_display_help_error() {
        let err = parse_args(["cfgd-operator", "-h"])
            .expect_err("-h must short-circuit via a clap error, not Ok");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        assert_eq!(err.exit_code(), 0);
    }

    #[test]
    fn unknown_flag_is_rejected_with_nonzero_exit() {
        let err = parse_args(["cfgd-operator", "--nope"])
            .expect_err("an unknown flag must be rejected, not silently ignored");
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
        assert_ne!(err.exit_code(), 0, "argv errors exit non-zero");
    }

    #[test]
    fn unexpected_positional_is_rejected_with_nonzero_exit() {
        let err = parse_args(["cfgd-operator", "bogus"])
            .expect_err("an unexpected positional must be rejected, not silently ignored");
        assert!(
            matches!(
                err.kind(),
                ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand
            ),
            "expected an unexpected-argument error, got {:?}",
            err.kind()
        );
        assert_ne!(err.exit_code(), 0, "argv errors exit non-zero");
    }
}

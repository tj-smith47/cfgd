use clap::{CommandFactory, FromArgMatches};

use cfgd::cli;

/// Examples block appended to `cfgd mcp --help`. brontes mints the `mcp`
/// command without cfgd's help convention (every top-level command carries an
/// `Examples:` block), so cfgd attaches one before mounting the subcommand.
const MCP_HELP_EXAMPLES: &str = "Examples:\n  \
    cfgd mcp start    # run the MCP server over stdio\n  \
    cfgd mcp tools    # list the tools the server exposes\n  \
    cfgd mcp stream   # stream events over the MCP transport";

/// Map an [`anyhow::Error`] to an exit code by downcasting through the
/// `CfgdError` boundary. Returns [`ExitCode::Error`] for errors that did
/// not originate in cfgd's typed domain (e.g. `anyhow::anyhow!(...)` at
/// a CLI callsite). Lives here (not cfgd-core) because Hard Rule #4
/// forbids `anyhow` anywhere but the CLI boundary.
fn exit_code_for_anyhow(err: &anyhow::Error) -> cfgd_core::exit::ExitCode {
    err.downcast_ref::<cfgd_core::errors::CfgdError>()
        .map(cfgd_core::exit::exit_code_for_error)
        .unwrap_or(cfgd_core::exit::ExitCode::Error)
}

/// Map a raw `CFGD_YES` value to the canonical `"true"`/`"false"` that clap's
/// `BoolishValueParser` accepts. Mirrors that parser's accept-set exactly
/// (case-insensitive): TRUE = {1, y, yes, t, true, on}, FALSE = {0, n, no, f,
/// false, off}. Returns `None` for anything else so genuinely-invalid values
/// still surface clap's validation error rather than being silently coerced.
fn canonical_bool_str(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "y" | "yes" | "t" | "true" | "on" => Some("true"),
        "0" | "n" | "no" | "f" | "false" | "off" => Some("false"),
        _ => None,
    }
}

/// Normalize `CFGD_YES` in the process environment to a spelling clap accepts.
/// clap parses the env value with a bool value-parser, which rejects shell-truthy
/// spellings like `1`/`yes`/`on` that users and scripts naturally set. Rewriting
/// the var before any parsing keeps the env binding ergonomic without touching the
/// ten `#[arg(env = "CFGD_YES")]` sites individually.
fn normalize_cfgd_yes_env() {
    if let Ok(raw) = std::env::var("CFGD_YES")
        && let Some(canonical) = canonical_bool_str(&raw)
    {
        // Safe here: runs at the very start of main(), before any threads spawn.
        unsafe {
            std::env::set_var("CFGD_YES", canonical);
        }
    }
}

fn main() -> anyhow::Result<()> {
    // Normalize CFGD_YES before clap reads it (see fn doc).
    normalize_cfgd_yes_env();

    let _ = rustls::crypto::ring::default_provider().install_default();

    // Clean up old binary from Windows upgrade rename-dance (no-op on Unix)
    cfgd_core::upgrade::cleanup_old_binary();

    // kubectl plugin detection — must be before normal CLI parsing
    let argv0 = std::env::args().next().unwrap_or_default();
    if argv0.ends_with("kubectl-cfgd") || argv0.ends_with("kubectl-cfgd.exe") {
        return cli::plugin::plugin_main();
    }

    // Expand aliases before clap parsing
    let raw_args: Vec<String> = std::env::args().collect();
    let expanded = cli::expand_aliases(raw_args);

    let brontes_cfg = brontes::Config::default().tool_name_prefix("cfgd");
    let mcp_command = brontes::command(Some(&brontes_cfg)).after_help(MCP_HELP_EXAMPLES);
    let augmented = cli::Cli::command().subcommand(mcp_command);
    let matches = augmented.clone().get_matches_from(&expanded);

    if let Some(("mcp", sub)) = matches.subcommand() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow::anyhow!("failed to start tokio runtime: {e}"))?;
        return rt
            .block_on(brontes::handle(sub, &augmented, Some(&brontes_cfg)))
            .map_err(|e| anyhow::anyhow!("{e}"));
    }

    let cli = cli::Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    // Resolve output format with --jsonpath backwards compat.
    // NOTE: --jsonpath is deprecated; --output jsonpath=EXPR is canonical.
    // The deprecation warning is emitted after Printer is constructed.
    let jsonpath_deprecated = cli.jsonpath.is_some();
    let mut output_format = cli.output.0.clone();
    if let Some(ref expr) = cli.jsonpath {
        match &output_format {
            cfgd_core::output::OutputFormat::Jsonpath(_) => {
                anyhow::bail!("cannot use both --jsonpath and -o jsonpath=...");
            }
            _ => {
                output_format = cfgd_core::output::OutputFormat::Jsonpath(expr.clone());
            }
        }
    }

    // Determine verbosity. `-v` (count=1) → debug; `-vv` or higher → trace.
    // Verbosity enum stays 3-way (Quiet|Normal|Verbose) — the extra tracing detail
    // from `-vv` goes into the tracing filter, not the user-facing Printer noise.
    let verbosity = if cli.quiet {
        cfgd_core::output::Verbosity::Quiet
    } else if cli.verbose > 0 {
        cfgd_core::output::Verbosity::Verbose
    } else {
        cfgd_core::output::Verbosity::Normal
    };

    // Initialize tracing
    let filter = if cli.quiet {
        "error"
    } else {
        match cli.verbose {
            0 => "info",
            1 => "debug",
            _ => "trace",
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(cfgd_core::tracing_env_filter(filter))
        .with_target(false)
        .without_time()
        .init();

    // Handle --no-color flag. NO_COLOR / TERM=dumb are handled inside
    // Printer::with_format so every Printer (including daemon-owned
    // ones) honors the convention.
    if cli.no_color {
        cfgd_core::output::Printer::disable_colors();
    }

    // Try loading config for theme settings; fall back to default theme if unavailable
    let theme_config = std::path::Path::new(&cli.config)
        .exists()
        .then(|| cfgd_core::config::load_config(std::path::Path::new(&cli.config)).ok())
        .flatten()
        .and_then(|c| c.spec.theme);
    let theme_name = theme_config.as_ref().map(|t| t.name.clone());
    let printer =
        cfgd_core::output::Printer::with_format(verbosity, theme_name.as_deref(), output_format);

    if jsonpath_deprecated {
        printer.status_simple(
            cfgd_core::output::Role::Warn,
            "--jsonpath is deprecated and will be removed in a future release; use --output jsonpath=EXPR instead",
        );
    }

    if let Err(e) = cli::execute(&cli, &printer) {
        render_cli_error(&printer, &e).exit();
    }

    Ok(())
}

/// Render a CLI-boundary error to stderr and return its exit code. For a missing-config
/// failure it also emits a remediation hint: a bare "config file not found" otherwise leaves
/// a first-run user with no path forward.
fn render_cli_error(
    printer: &cfgd_core::output::Printer,
    err: &anyhow::Error,
) -> cfgd_core::exit::ExitCode {
    // Format with `{}` not `{:#}` — CfgdError templates already include `{0}` which expands
    // the inner error, so `{:#}` would walk source() and duplicate the inner text. See
    // errors/mod.rs::CfgdError for the paired contract.
    printer.status_simple(
        cfgd_core::output::Role::Fail,
        cfgd_core::output::collapse_to_subject_line(err),
    );
    let code = exit_code_for_anyhow(err);
    // The hint goes to the same stream (stderr) as the error above.
    if code == cfgd_core::exit::ExitCode::NoConfig {
        printer.hint("run `cfgd init` to create a config, or pass --config <path>");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::{canonical_bool_str, exit_code_for_anyhow};

    #[test]
    fn canonical_bool_str_accepts_truthy_spellings() {
        for raw in [
            "1", "y", "yes", "t", "true", "on", "YES", "Yes", "ON", "True",
        ] {
            assert_eq!(
                canonical_bool_str(raw),
                Some("true"),
                "{raw:?} should normalize to true"
            );
        }
    }

    #[test]
    fn canonical_bool_str_accepts_falsey_spellings() {
        for raw in ["0", "n", "no", "f", "false", "off", "NO", "Off", "FALSE"] {
            assert_eq!(
                canonical_bool_str(raw),
                Some("false"),
                "{raw:?} should normalize to false"
            );
        }
    }

    #[test]
    fn canonical_bool_str_trims_surrounding_whitespace() {
        assert_eq!(canonical_bool_str("  1  "), Some("true"));
        assert_eq!(canonical_bool_str("\toff\n"), Some("false"));
    }

    #[test]
    fn canonical_bool_str_passes_through_canonical_values() {
        assert_eq!(canonical_bool_str("true"), Some("true"));
        assert_eq!(canonical_bool_str("false"), Some("false"));
    }

    #[test]
    fn canonical_bool_str_rejects_unrecognized_values() {
        // Unrecognized values return None so they remain untouched and clap's
        // bool parser still rejects them, preserving validation.
        for raw in ["garbage", "", "2", "tru", "yess", "10"] {
            assert_eq!(
                canonical_bool_str(raw),
                None,
                "{raw:?} should not be recognized as boolish"
            );
        }
    }

    #[test]
    fn render_cli_error_hints_cfgd_init_on_missing_config() {
        // A NoConfig failure must point the user at `cfgd init`, not just print "not found".
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let err: anyhow::Error =
            cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::NotFound {
                path: "/home/u/.config/cfgd/cfgd.yaml".into(),
            })
            .into();
        let code = super::render_cli_error(&printer, &err);
        printer.flush();
        assert_eq!(code, cfgd_core::exit::ExitCode::NoConfig);
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("cfgd init"),
            "expected remediation naming `cfgd init`, got: {out:?}"
        );
    }

    #[test]
    fn exit_code_for_anyhow_falls_back_to_error_for_opaque_anyhow_errors() {
        // anyhow::anyhow! produces an error that doesn't downcast to
        // CfgdError; the helper must return ExitCode::Error.
        let err = anyhow::anyhow!("an opaque CLI-boundary error");
        let code = exit_code_for_anyhow(&err);
        assert_eq!(code, cfgd_core::exit::ExitCode::Error);
    }

    #[test]
    fn exit_code_for_anyhow_propagates_cfgd_error_exit_code_through_downcast() {
        // Errors that downcast to CfgdError should be routed through
        // exit_code_for_error so the typed-domain semantics are preserved.
        let cfgd_err =
            cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                message: "invalid config".to_string(),
            });
        let expected = cfgd_core::exit::exit_code_for_error(&cfgd_err);
        let anyhow_err: anyhow::Error = cfgd_err.into();
        let actual = exit_code_for_anyhow(&anyhow_err);
        assert_eq!(actual, expected);
    }
}

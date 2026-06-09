use clap::{CommandFactory, FromArgMatches};

use cfgd::cli;

/// Examples block appended to `cfgd mcp --help`. brontes mints the `mcp`
/// command without cfgd's help convention (every top-level command carries an
/// `Examples:` block), so cfgd attaches one before mounting the subcommand.
const MCP_HELP_EXAMPLES: &str = "Examples:\n  \
    cfgd mcp start    # run the MCP server over stdio\n  \
    cfgd mcp tools    # list the tools the server exposes\n  \
    cfgd mcp stream   # stream events over the MCP transport";

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

/// Global env vars bound to a `bool` clap arg. Their value-parser rejects
/// shell-truthy spellings (`1`/`yes`/`on`/…), so each is rewritten to the
/// canonical `true`/`false` before parsing — keeping `CFGD_QUIET=1` ergonomic
/// without touching the per-arg `#[arg(env = …)]` sites.
const BOOL_ENV_VARS: &[&str] = &[
    "CFGD_YES",
    "CFGD_QUIET",
    "CFGD_REQUIRE_COSIGN",
    "CFGD_LIST_ENVELOPE",
];

/// Rewrite a boolish env var to the canonical `true`/`false` spelling clap's
/// bool value-parser accepts. No-op when the var is unset or holds a value
/// `canonical_bool_str` doesn't recognize (so genuinely-invalid values still
/// surface clap's validation error).
fn normalize_boolish_env(var: &str) {
    if let Ok(raw) = std::env::var(var)
        && let Some(canonical) = canonical_bool_str(&raw)
    {
        // Safe here: runs at the very start of main(), before any threads spawn.
        unsafe {
            std::env::set_var(var, canonical);
        }
    }
}

/// Normalize `CFGD_VERBOSE` so the documented "on/off flag" behavior holds.
/// `--verbose` is an `ArgAction::Count` (`u8`), so its value-parser accepts bare
/// integers but rejects boolish spellings. Map boolish ON (`y`/`yes`/`on`/…) to
/// `1` and boolish OFF (`n`/`no`/`off`/…) to `0`; leave bare integers untouched
/// (`canonical_bool_str` returns `None` for `2`, so `CFGD_VERBOSE=2` still means
/// trace) and leave unrecognized values for clap to reject.
fn normalize_cfgd_verbose_env() {
    if let Ok(raw) = std::env::var("CFGD_VERBOSE")
        && let Some(canonical) = canonical_bool_str(&raw)
    {
        let count = if canonical == "true" { "1" } else { "0" };
        // Safe here: runs at the very start of main(), before any threads spawn.
        unsafe {
            std::env::set_var("CFGD_VERBOSE", count);
        }
    }
}

fn main() -> anyhow::Result<()> {
    // Normalize boolish env vars before clap reads them (see fn docs).
    for var in BOOL_ENV_VARS {
        normalize_boolish_env(var);
    }
    normalize_cfgd_verbose_env();

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

    // Gates for the macOS config-location migration prompt (evaluated below,
    // after the Printer exists). An explicit `--config`/`CFGD_CONFIG` pins the
    // location, and `--yes`/`CFGD_YES` means "don't prompt".
    let explicit_config = std::env::var_os("CFGD_CONFIG").is_some()
        || expanded
            .iter()
            .any(|a| a == "--config" || a.starts_with("--config="));
    let assume_yes = std::env::var("CFGD_YES")
        .map(|v| v == "true")
        .unwrap_or(false)
        || expanded.iter().any(|a| a == "--yes");

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

    let mut cli = cli::Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    // A `--config <dir>` / `CFGD_CONFIG=<dir>` argument names the config directory;
    // infer the discovery file inside it once, up front, so every downstream
    // consumer (theme load, profiles-dir derivation, dispatch) agrees on the same
    // resolved file rather than load_config silently inferring while config_dir()
    // derives `profiles/` from the wrong parent.
    cli.config = cfgd_core::config::resolve_config_path(&cli.config);

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
    // Route tracing to stderr: stdout is reserved for `-o` machine output
    // (the `-o json` purity contract), mirroring Printer human-on-stderr.
    tracing_subscriber::fmt()
        .with_env_filter(cfgd_core::tracing_env_filter(filter))
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
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
        cfgd_core::output::Printer::with_format(verbosity, theme_name.as_deref(), output_format)
            .with_list_envelope(cli.list_envelope);

    if jsonpath_deprecated {
        // A deprecation notice is a stderr diagnostic, not `-o` data — and
        // `--jsonpath` always forces a structured format, under which the
        // Printer auto-quiets every non-Fail status. `deprecation` force-shows
        // on stderr regardless, keeping the stdout data channel pure.
        printer.deprecation(
            "--jsonpath is deprecated and will be removed in a future release; use --output jsonpath=EXPR instead",
        );
    }

    // One-time macOS prompt to migrate a legacy ~/.config/cfgd to the native
    // location (or pin XDG_CONFIG_HOME). No-op off macOS and in non-interactive
    // sessions; re-resolve the config path when the dir was moved. Skipped for
    // the daemon, which must never block on a prompt when run in the foreground.
    let is_daemon = matches!(cli.command, Some(cli::Command::Daemon { .. }));
    if !is_daemon
        && let Some(new_config) =
            cli::config_migration::maybe_migrate_macos_config(&printer, explicit_config, assume_yes)
    {
        cli.config = cfgd_core::config::resolve_config_path(&new_config);
    }

    if let Err(e) = cli::execute(&cli, &printer) {
        cli::error::render_cli_error(&printer, &e).exit();
    }

    // A data-dependent structured-output failure (template render/context error,
    // or an unreadable template-file) is reported on stderr by `emit` but cannot
    // make `execute` return `Err` without rippling through 175 `emit` call
    // sites. The Printer records it; surface it as a non-zero exit so a `-o`
    // consumer never sees exit 0 over a polluted/empty data channel. The message
    // already went to stderr — do not print again.
    if printer.had_output_error() {
        cfgd_core::exit::ExitCode::Error.exit();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{canonical_bool_str, normalize_boolish_env, normalize_cfgd_verbose_env};
    use cfgd_core::test_helpers::EnvVarGuard;
    use serial_test::serial;

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
    #[serial]
    fn normalize_boolish_env_rewrites_truthy_to_true() {
        for raw in ["1", "yes", "on", "Y", "True"] {
            let _g = EnvVarGuard::set("CFGD_QUIET", raw);
            normalize_boolish_env("CFGD_QUIET");
            assert_eq!(
                std::env::var("CFGD_QUIET").as_deref(),
                Ok("true"),
                "{raw:?} should normalize to true"
            );
        }
    }

    #[test]
    #[serial]
    fn normalize_boolish_env_rewrites_falsey_to_false() {
        for raw in ["0", "no", "off", "N", "False"] {
            let _g = EnvVarGuard::set("CFGD_QUIET", raw);
            normalize_boolish_env("CFGD_QUIET");
            assert_eq!(
                std::env::var("CFGD_QUIET").as_deref(),
                Ok("false"),
                "{raw:?} should normalize to false"
            );
        }
    }

    #[test]
    #[serial]
    fn normalize_boolish_env_leaves_invalid_untouched() {
        let _g = EnvVarGuard::set("CFGD_QUIET", "garbage");
        normalize_boolish_env("CFGD_QUIET");
        assert_eq!(std::env::var("CFGD_QUIET").as_deref(), Ok("garbage"));
    }

    #[test]
    #[serial]
    fn normalize_boolish_env_noop_when_unset() {
        let _g = EnvVarGuard::unset("CFGD_QUIET");
        normalize_boolish_env("CFGD_QUIET");
        assert!(std::env::var("CFGD_QUIET").is_err());
    }

    #[test]
    #[serial]
    fn normalize_cfgd_verbose_env_maps_boolish_on_to_one() {
        for raw in ["on", "yes", "true", "y"] {
            let _g = EnvVarGuard::set("CFGD_VERBOSE", raw);
            normalize_cfgd_verbose_env();
            assert_eq!(
                std::env::var("CFGD_VERBOSE").as_deref(),
                Ok("1"),
                "{raw:?} should map to count 1"
            );
        }
    }

    #[test]
    #[serial]
    fn normalize_cfgd_verbose_env_maps_boolish_off_to_zero() {
        for raw in ["off", "no", "false", "n"] {
            let _g = EnvVarGuard::set("CFGD_VERBOSE", raw);
            normalize_cfgd_verbose_env();
            assert_eq!(
                std::env::var("CFGD_VERBOSE").as_deref(),
                Ok("0"),
                "{raw:?} should map to count 0"
            );
        }
    }

    #[test]
    #[serial]
    fn normalize_cfgd_verbose_env_leaves_integers_untouched() {
        for raw in ["1", "2", "10"] {
            let _g = EnvVarGuard::set("CFGD_VERBOSE", raw);
            normalize_cfgd_verbose_env();
            assert_eq!(
                std::env::var("CFGD_VERBOSE").as_deref(),
                Ok(raw),
                "{raw:?} (bare integer) must pass through unchanged"
            );
        }
    }
}

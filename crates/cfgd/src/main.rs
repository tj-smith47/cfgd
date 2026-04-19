use clap::Parser;

mod ai;
mod cli;
mod files;
mod generate;
mod mcp;
mod packages;
mod secrets;
mod system;

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

/// Stack size for the worker thread that runs `run_main`.
///
/// The clap `Command` tree for cfgd's 29 top-level subcommands (each carrying
/// a `long_about` with `Examples:` blocks, some built via `format!`) is large
/// enough to overflow Windows's default 1 MiB main-thread stack during
/// `Cli::parse_from`. The symptom on Windows CI was `STATUS_STACK_OVERFLOW`
/// (exit code `-1073741571` = `0xC00000FD`) on *every* invocation — every
/// assert_cmd integration test in `tests/cli_integration.rs` failed with a
/// "main has overflowed its stack" panic.
///
/// 8 MiB matches Linux's default and is the standard workaround for
/// clap-heavy Rust CLIs on Windows.
const RUN_STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() -> anyhow::Result<()> {
    // Run the real entry point on a worker thread with a Linux-sized stack so
    // the clap Command tree never overflows on Windows. The main thread just
    // joins and propagates the worker's Result.
    let worker = std::thread::Builder::new()
        .name("cfgd-main".into())
        .stack_size(RUN_STACK_SIZE)
        .spawn(run_main)
        .expect("spawn cfgd-main worker thread");
    match worker.join() {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn run_main() -> anyhow::Result<()> {
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
    let cli = cli::Cli::parse_from(expanded);

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
    let printer =
        cfgd_core::output::Printer::with_format(verbosity, theme_config.as_ref(), output_format);

    if jsonpath_deprecated {
        printer.warning(
            "--jsonpath is deprecated and will be removed in a future release; use --output jsonpath=EXPR instead",
        );
    }

    if let Err(e) = cli::execute(&cli, &printer) {
        // Format with `{}` not `{:#}` — CfgdError templates already include
        // `{0}` which expands the inner error, so `{:#}` would walk source()
        // and duplicate the inner text. See errors/mod.rs::CfgdError for the
        // paired contract.
        printer.error(&format!("{}", e));
        exit_code_for_anyhow(&e).exit();
    }

    Ok(())
}

use clap::Parser;
use tracing_subscriber::EnvFilter;

mod ai;
mod cli;
mod files;
mod generate;
mod mcp;
mod packages;
mod secrets;
mod system;

fn main() -> anyhow::Result<()> {
    if let Err(e) = rustls::crypto::ring::default_provider().install_default() {
        eprintln!("rustls CryptoProvider already installed: {e:?}");
    }

    // kubectl plugin detection — must be before normal CLI parsing
    let argv0 = std::env::args().next().unwrap_or_default();
    if argv0.ends_with("kubectl-cfgd") || argv0.ends_with("kubectl-cfgd.exe") {
        return cli::plugin::plugin_main();
    }

    // Expand aliases before clap parsing
    let raw_args: Vec<String> = std::env::args().collect();
    let expanded = cli::expand_aliases(raw_args);
    let cli = cli::Cli::parse_from(expanded);

    // Parse output format
    let output_format = match cli.output.as_str() {
        "json" => cfgd_core::output::OutputFormat::Json,
        "yaml" => cfgd_core::output::OutputFormat::Yaml,
        "table" => {
            // --jsonpath implies json output
            if cli.jsonpath.is_some() {
                cfgd_core::output::OutputFormat::Json
            } else {
                cfgd_core::output::OutputFormat::Table
            }
        }
        other => {
            eprintln!("Unknown output format '{}'. Use: table, json, yaml", other);
            std::process::exit(1);
        }
    };

    // Determine verbosity
    let verbosity = if cli.quiet {
        cfgd_core::output::Verbosity::Quiet
    } else if cli.verbose {
        cfgd_core::output::Verbosity::Verbose
    } else {
        cfgd_core::output::Verbosity::Normal
    };

    // Initialize tracing
    let filter = match verbosity {
        cfgd_core::output::Verbosity::Quiet => "error",
        cfgd_core::output::Verbosity::Normal => "info",
        cfgd_core::output::Verbosity::Verbose => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter)),
        )
        .with_target(false)
        .without_time()
        .init();

    // Handle --no-color flag or NO_COLOR env var (https://no-color.org/)
    if cli.no_color || std::env::var_os("NO_COLOR").is_some() {
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
    }

    // Try loading config for theme settings; fall back to default theme if unavailable
    let theme_config = std::path::Path::new(&cli.config)
        .exists()
        .then(|| cfgd_core::config::load_config(std::path::Path::new(&cli.config)).ok())
        .flatten()
        .and_then(|c| c.spec.theme);
    let printer = cfgd_core::output::Printer::with_format(
        verbosity,
        theme_config.as_ref(),
        output_format,
        cli.jsonpath.clone(),
    );

    if let Err(e) = cli::execute(&cli, &printer) {
        printer.error(&format!("{:#}", e));
        std::process::exit(1);
    }

    Ok(())
}

use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cli;
mod files;
mod packages;
mod secrets;
mod system;

fn main() -> anyhow::Result<()> {
    // Expand aliases before clap parsing
    let raw_args: Vec<String> = std::env::args().collect();
    let expanded = cli::expand_aliases(raw_args);
    let cli = cli::Cli::parse_from(expanded);

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

    // Handle --no-color
    if cli.no_color {
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
    }

    // Try loading config for theme settings; fall back to default theme if unavailable
    let theme_config = std::path::Path::new(&cli.config)
        .exists()
        .then(|| cfgd_core::config::load_config(std::path::Path::new(&cli.config)).ok())
        .flatten()
        .and_then(|c| c.spec.theme);
    let printer = cfgd_core::output::Printer::with_theme(verbosity, theme_config.as_ref());

    if let Err(e) = cli::execute(&cli, &printer) {
        printer.error(&format!("{:#}", e));
        std::process::exit(1);
    }

    Ok(())
}

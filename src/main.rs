use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cli;
mod composition;
mod config;
mod daemon;
mod errors;
mod files;
mod output;
mod packages;
mod providers;
mod reconciler;
mod secrets;
mod sources;
mod state;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    // Determine verbosity
    let verbosity = if cli.quiet {
        output::Verbosity::Quiet
    } else if cli.verbose {
        output::Verbosity::Verbose
    } else {
        output::Verbosity::Normal
    };

    // Initialize tracing
    let filter = match verbosity {
        output::Verbosity::Quiet => "error",
        output::Verbosity::Normal => "info",
        output::Verbosity::Verbose => "debug",
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

    let printer = output::Printer::new(verbosity);

    if let Err(e) = cli::execute(&cli, &printer) {
        printer.error(&format!("{:#}", e));
        std::process::exit(1);
    }

    Ok(())
}

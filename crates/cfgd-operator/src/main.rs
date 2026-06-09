use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Parse argv BEFORE any cluster/async work so `--version`/`--help` answer
    // instantly and unknown args are rejected. clap handles those exits itself
    // (version/help → stdout, exit 0; bad argv → stderr usage, exit 2); a normal
    // no-arg invocation returns Ok and falls through to the operator lifecycle,
    // which is configured by environment variables, not argv.
    let _args = cfgd_operator::args::parse_args(std::env::args_os()).unwrap_or_else(|e| e.exit());

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    cfgd_operator::app::run().await
}

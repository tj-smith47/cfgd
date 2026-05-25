#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(cfgd_core::tracing_env_filter("info"))
        .json()
        .init();
    cfgd_csi::app::run().await
}

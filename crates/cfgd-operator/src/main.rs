mod controllers;
mod crds;
mod errors;
mod webhook;

use std::path::Path;

use anyhow::Result;
use kube::Client;
use tracing_subscriber::EnvFilter;

use crate::crds::{ConfigPolicy, DriftAlert, MachineConfig};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Starting cfgd-operator");
    log_crd_info();

    let client = Client::try_default().await?;

    let cert_dir = std::env::var("WEBHOOK_CERT_DIR")
        .unwrap_or_else(|_| "/tmp/k8s-webhook-server/serving-certs".to_string());
    let webhook_port: u16 = std::env::var("WEBHOOK_PORT")
        .unwrap_or_else(|_| "9443".to_string())
        .parse()
        .unwrap_or(9443);

    if Path::new(&cert_dir).join("tls.crt").exists() {
        tracing::info!(cert_dir = %cert_dir, port = webhook_port, "Starting webhook server");
        tokio::spawn(async move {
            if let Err(e) = webhook::run_webhook_server(&cert_dir, webhook_port).await {
                tracing::error!(error = %e, "Webhook server failed");
            }
        });
    } else {
        tracing::info!(
            cert_dir = %cert_dir,
            "Webhook certs not found, webhook server disabled"
        );
    }

    controllers::run(client).await?;

    Ok(())
}

fn log_crd_info() {
    use kube::CustomResourceExt;

    tracing::info!(
        crd = %MachineConfig::crd_name(),
        "Registered CRD: MachineConfig"
    );
    tracing::info!(
        crd = %ConfigPolicy::crd_name(),
        "Registered CRD: ConfigPolicy"
    );
    tracing::info!(
        crd = %DriftAlert::crd_name(),
        "Registered CRD: DriftAlert"
    );
}

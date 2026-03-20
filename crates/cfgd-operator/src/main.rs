mod controllers;
mod crds;
mod errors;
mod gateway;
mod health;
mod leader;
mod webhook;

use std::path::Path;

use anyhow::Result;
use kube::Client;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::crds::{ConfigPolicy, DriftAlert, MachineConfig};
use crate::gateway::GatewayConfig;

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Starting cfgd-operator");
    log_crd_info();

    let client = Client::try_default().await?;

    let health_state = health::HealthState::new();
    let health_port: u16 = std::env::var("HEALTH_PORT")
        .unwrap_or_else(|_| "8081".to_string())
        .parse()
        .unwrap_or(8081);

    tokio::spawn({
        let hs = health_state.clone();
        async move {
            if let Err(e) = health::run_health_server(health_port, hs).await {
                tracing::error!(error = %e, "Health server failed");
            }
        }
    });

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

    let leader_enabled = std::env::var("LEADER_ELECTION_ENABLED")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if leader_enabled {
        let namespace = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "cfgd-system".to_string());
        let identity =
            std::env::var("POD_NAME").unwrap_or_else(|_| Uuid::new_v4().to_string());

        tracing::info!(
            namespace = %namespace,
            identity = %identity,
            "Leader election enabled"
        );

        let le = leader::LeaderElection::new(client.clone(), namespace, identity);
        le.run(|| async {
            health_state.set_ready();
            run_operator(client).await.map_err(|e| {
                errors::OperatorError::Leader(format!("Operator run failed: {e}"))
            })
        })
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    } else {
        health_state.set_ready();
        run_operator(client).await?;
    }

    Ok(())
}

async fn run_operator(client: Client) -> Result<()> {
    // Device gateway — optional HTTP server for device checkin, enrollment, drift, web UI
    let gateway_enabled = std::env::var("DEVICE_GATEWAY_ENABLED")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if gateway_enabled {
        let gateway_config = GatewayConfig {
            port: std::env::var("DEVICE_GATEWAY_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8080),
            db_path: std::env::var("CFGD_SERVER_DB_PATH")
                .unwrap_or_else(|_| "/data/cfgd-gateway.db".to_string()),
            kube_client: Some(client.clone()),
            retention_days: std::env::var("CFGD_RETENTION_DAYS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(90),
        };

        tracing::info!("Device gateway enabled");

        // Spawn controllers with retry — if they fail, retry after delay
        tokio::spawn(async move {
            loop {
                match controllers::run(client.clone()).await {
                    Ok(()) => break,
                    Err(e) => {
                        tracing::error!(error = %e, "Controllers failed — retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        });

        // Gateway is the primary process — if it exits, we exit
        gateway::start_gateway(gateway_config)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    } else {
        controllers::run(client).await?;
    }

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

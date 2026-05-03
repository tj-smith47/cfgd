//! DriftAlert CRD creation in Kubernetes, called by the device drift-event handler.

use super::*;
/// Create a DriftAlert CRD in Kubernetes with retry and exponential backoff.
pub(super) async fn create_drift_alert_crd(
    client: &kube::Client,
    device_id: &str,
    hostname: &str,
    details: &[DriftDetailInput],
    timestamp: &str,
) -> Result<(), GatewayError> {
    use crate::crds::{
        DriftAlert, DriftAlertSpec, DriftDetail, DriftSeverity, MachineConfigReference,
    };
    use kube::ResourceExt;
    use kube::api::{Api, PostParams};

    let alerts: Api<DriftAlert> = Api::default_namespaced(client.clone());

    let mc_ref = find_machine_config_for_device(client, hostname).await;

    let alert_name = format!(
        "drift-{}-{}",
        device_id,
        timestamp.replace([':', '-', 'T', 'Z'], "")
    );

    let drift_details: Vec<DriftDetail> = details
        .iter()
        .map(|d| DriftDetail {
            field: d.field.clone(),
            expected: d.expected.clone(),
            actual: d.actual.clone(),
        })
        .collect();

    let mut alert = DriftAlert::new(
        &alert_name,
        DriftAlertSpec {
            device_id: device_id.to_string(),
            machine_config_ref: MachineConfigReference {
                name: mc_ref.clone(),
                namespace: None,
            },
            drift_details,
            severity: DriftSeverity::Medium,
        },
    );
    alert.metadata.labels = Some(std::collections::BTreeMap::from([
        (cfgd_core::LABEL_MACHINE_CONFIG.to_string(), mc_ref),
        (
            cfgd_core::LABEL_DEVICE_ID.to_string(),
            device_id.to_string(),
        ),
    ]));

    let retry = cfgd_core::retry::BackoffConfig::DEFAULT_TRANSIENT;
    let mut last_err = None;

    for attempt in 0..retry.max_attempts {
        let delay = retry.delay_for_attempt(attempt);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }

        match alerts.create(&PostParams::default(), &alert).await {
            Ok(created) => {
                tracing::info!(
                    name = %created.name_any(),
                    device_id = %device_id,
                    "driftAlert CRD created in Kubernetes"
                );
                return Ok(());
            }
            Err(kube::Error::Api(ref resp)) if resp.code == 409 => {
                tracing::debug!(
                    name = %alert_name,
                    "driftAlert already exists, skipping creation"
                );
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    device_id = %device_id,
                    attempt = attempt + 1,
                    max_retries = retry.max_attempts,
                    error = %e,
                    "failed to create DriftAlert CRD, retrying"
                );
                last_err = Some(e);
            }
        }
    }

    if let Some(e) = last_err {
        tracing::error!(
            device_id = %device_id,
            error = %e,
            attempts = retry.max_attempts,
            "failed to create DriftAlert CRD after all attempts — drift recorded in database only"
        );
    }

    Ok(())
}

/// Find the MachineConfig CRD name that corresponds to a device hostname.
pub(super) async fn find_machine_config_for_device(
    client: &kube::Client,
    hostname: &str,
) -> String {
    use crate::crds::MachineConfig;
    use kube::ResourceExt;
    use kube::api::{Api, ListParams};

    let machines: Api<MachineConfig> = Api::all(client.clone());
    match machines.list(&ListParams::default()).await {
        Ok(list) => {
            for mc in &list.items {
                if mc.spec.hostname == hostname {
                    return mc.name_any();
                }
            }
            format!("{}-mc", hostname)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to list MachineConfigs for device lookup");
            format!("{}-mc", hostname)
        }
    }
}

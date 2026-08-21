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

    // A device id and a hostname arrive from the device itself, so neither is a
    // legal Kubernetes name by construction: an underscore or an over-long id
    // makes the API server reject the whole object with a 422, and the retry
    // ladder below then burns every attempt on a body that can never be
    // accepted. The object name and both label values are the outbound copies
    // and are cut to shape here; the drift row the caller writes keeps the id
    // the device sent.
    let device_label = k8s_value(device_id);
    let mc_label = k8s_value(&mc_ref);

    let alert_name = format!(
        "drift-{}-{}",
        device_label,
        cfgd_core::iso8601_to_filename_safe(timestamp)
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
                name: mc_ref,
                namespace: None,
            },
            drift_details,
            severity: DriftSeverity::Medium,
        },
    );
    alert.metadata.labels = Some(std::collections::BTreeMap::from([
        (cfgd_core::LABEL_MACHINE_CONFIG.to_string(), mc_label),
        (cfgd_core::LABEL_DEVICE_ID.to_string(), device_label),
    ]));

    // `Api::create` mints `kube::Error::SerdeError` from two places: serializing
    // this object into the request body, before anything is sent, and
    // deserializing a 2xx response. Only the second means the alert exists, and
    // the error carries nothing that tells them apart — so the first is ruled
    // out here instead of argued about. `serde_json::to_vec` is pure and
    // deterministic over the same value, so once it has succeeded here it
    // cannot fail inside the loop, and every `SerdeError` below is a response.
    if let Err(e) = serde_json::to_vec(&alert) {
        tracing::error!(
            name = %alert_name,
            device_id = %device_id,
            error = %e,
            "driftAlert CRD could not be serialized; not sent — drift recorded in database only"
        );
        return Ok(());
    }

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
            // A response-side deserialize error, and only that: the request-side
            // source is ruled out by the serialization above, and no 4xx/5xx can
            // arrive here because kube turns every one of those into `Api`,
            // including the branch where the error body itself fails to parse.
            // So the object was created and only the server's echo of it is
            // unreadable; a retry would POST a second time and learn from the
            // 409 what this arm already knows.
            Err(kube::Error::SerdeError(e)) => {
                tracing::warn!(
                    name = %alert_name,
                    device_id = %device_id,
                    error = %e,
                    "driftAlert CRD created, but the response body could not be parsed"
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

/// One label-shaped copy of a string the gateway did not author: sanitized to
/// an RFC 1123 label and cut to the 63 bytes a Kubernetes label value allows,
/// trimming a hyphen the cut may have exposed at the end.
fn k8s_value(raw: &str) -> String {
    const LABEL_VALUE_MAX: usize = 63;
    let mut value = cfgd_core::sanitize_k8s_name(raw);
    // `sanitize_k8s_name` yields ASCII only, so a byte cut is a char cut.
    value.truncate(LABEL_VALUE_MAX);
    while value.ends_with('-') {
        value.pop();
    }
    value
}

/// Find the MachineConfig CRD name that corresponds to a device hostname.
pub(super) async fn find_machine_config_for_device(
    client: &kube::Client,
    hostname: &str,
) -> String {
    use crate::crds::MachineConfig;
    use kube::ResourceExt;
    use kube::api::{Api, ListParams};

    // A live read, deliberately: the gateway answers a device's request about
    // the machine it is right now, and it holds no reflector of its own — a
    // cache would have to be built and kept warm for one lookup per API call.
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

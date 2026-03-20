use std::time::Duration;

use chrono::Utc;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::Client;
use kube::api::{Api, Patch, PatchParams, PostParams};

use crate::errors::OperatorError;

const FIELD_MANAGER: &str = "cfgd-operator";
const LEASE_NAME: &str = "cfgd-operator-leader";

fn parse_duration_secs(env_var: &str, default: u64) -> u64 {
    std::env::var(env_var)
        .ok()
        .and_then(|v| {
            let v = v.trim_end_matches('s');
            v.parse().ok()
        })
        .unwrap_or(default)
}

pub struct LeaderElection {
    client: Client,
    namespace: String,
    identity: String,
    lease_duration_secs: i32,
    renew_deadline_secs: u64,
    retry_period_secs: u64,
}

impl LeaderElection {
    pub fn new(client: Client, namespace: String, identity: String) -> Self {
        Self {
            client,
            namespace,
            identity,
            lease_duration_secs: parse_duration_secs("LEADER_LEASE_DURATION", 15) as i32,
            renew_deadline_secs: parse_duration_secs("LEADER_RENEW_DEADLINE", 10),
            retry_period_secs: parse_duration_secs("LEADER_RETRY_PERIOD", 2),
        }
    }

    pub async fn try_acquire(&self) -> Result<bool, OperatorError> {
        let leases: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);
        let now = Utc::now();
        let now_micro = MicroTime(now);

        match leases.get(LEASE_NAME).await {
            Ok(existing) => {
                let spec = existing.spec.as_ref();
                let current_holder = spec
                    .and_then(|s| s.holder_identity.as_deref())
                    .unwrap_or("");

                // Check if lease is expired
                let expired = if let Some(renew_time) = spec.and_then(|s| s.renew_time.as_ref()) {
                    let duration_secs = spec
                        .and_then(|s| s.lease_duration_seconds)
                        .unwrap_or(self.lease_duration_secs);
                    let expiry = renew_time.0 + chrono::Duration::seconds(i64::from(duration_secs));
                    now > expiry
                } else {
                    // No renew time means expired
                    true
                };

                if expired || current_holder == self.identity {
                    // Patch to renew
                    let patch = serde_json::json!({
                        "spec": {
                            "holderIdentity": self.identity,
                            "leaseDurationSeconds": self.lease_duration_secs,
                            "renewTime": now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                            "acquireTime": if expired || current_holder != self.identity {
                                Some(now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
                            } else {
                                spec.and_then(|s| s.acquire_time.as_ref())
                                    .map(|t| t.0.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
                            },
                            "leaseTransitions": if expired || current_holder != self.identity {
                                spec.and_then(|s| s.lease_transitions).unwrap_or(0) + 1
                            } else {
                                spec.and_then(|s| s.lease_transitions).unwrap_or(0)
                            }
                        }
                    });

                    leases
                        .patch(
                            LEASE_NAME,
                            &PatchParams::apply(FIELD_MANAGER),
                            &Patch::Merge(&patch),
                        )
                        .await?;

                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                // Create the Lease
                let lease = serde_json::from_value(serde_json::json!({
                    "apiVersion": "coordination.k8s.io/v1",
                    "kind": "Lease",
                    "metadata": {
                        "name": LEASE_NAME,
                        "namespace": self.namespace,
                    },
                    "spec": {
                        "holderIdentity": self.identity,
                        "leaseDurationSeconds": self.lease_duration_secs,
                        "acquireTime": now_micro.0.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                        "renewTime": now_micro.0.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                        "leaseTransitions": 0
                    }
                }))
                .map_err(|e| OperatorError::Leader(format!("Failed to build Lease: {e}")))?;

                leases.create(&PostParams::default(), &lease).await?;
                Ok(true)
            }
            Err(e) => Err(OperatorError::KubeError(e)),
        }
    }

    pub async fn run<F, Fut>(&self, on_started: F) -> Result<(), OperatorError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), OperatorError>>,
    {
        // Acquisition loop
        loop {
            match self.try_acquire().await {
                Ok(true) => {
                    tracing::info!(identity = %self.identity, "Leader election won");
                    break;
                }
                Ok(false) => {
                    tracing::info!(
                        identity = %self.identity,
                        "Not the leader, retrying in {}s",
                        self.retry_period_secs
                    );
                    tokio::time::sleep(Duration::from_secs(self.retry_period_secs)).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Leader election attempt failed, retrying");
                    tokio::time::sleep(Duration::from_secs(self.retry_period_secs)).await;
                }
            }
        }

        // Spawn background renewal task
        let renewal_client = self.client.clone();
        let renewal_ns = self.namespace.clone();
        let renewal_identity = self.identity.clone();
        let renew_interval = Duration::from_secs(self.renew_deadline_secs / 2);

        tokio::spawn(async move {
            let renewal = LeaderElection::new(renewal_client, renewal_ns, renewal_identity);
            loop {
                tokio::time::sleep(renew_interval).await;
                match renewal.try_acquire().await {
                    Ok(true) => {
                        tracing::debug!("Leader lease renewed");
                    }
                    Ok(false) => {
                        tracing::error!("Lost leader lease — another instance took over");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Leader lease renewal failed");
                    }
                }
            }
        });

        on_started().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_lease_name() {
        assert_eq!(LEASE_NAME, "cfgd-operator-leader");
    }
}

use std::time::Duration;

use chrono::Utc;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::Client;
use kube::api::{Api, Patch, PatchParams, PostParams};
use tokio_util::sync::CancellationToken;

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
                    let expiry = renew_time.0
                        + chrono::TimeDelta::try_seconds(i64::from(duration_secs))
                            .unwrap_or_default();
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

    pub async fn run<F, Fut>(
        &self,
        shutdown: CancellationToken,
        on_started: F,
    ) -> Result<(), OperatorError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), OperatorError>>,
    {
        // Acquisition loop
        loop {
            match self.try_acquire().await {
                Ok(true) => {
                    tracing::info!(identity = %self.identity, "leader election won");
                    break;
                }
                Ok(false) => {
                    tracing::info!(
                        identity = %self.identity,
                        "not the leader, retrying in {}s",
                        self.retry_period_secs
                    );
                    tokio::time::sleep(Duration::from_secs(self.retry_period_secs)).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "leader election attempt failed, retrying");
                    tokio::time::sleep(Duration::from_secs(self.retry_period_secs)).await;
                }
            }
        }

        // Clone existing config fields instead of re-reading env vars
        let renewal_client = self.client.clone();
        let renewal_ns = self.namespace.clone();
        let renewal_identity = self.identity.clone();
        let renewal_lease_duration = self.lease_duration_secs;
        let renewal_renew_deadline = self.renew_deadline_secs;
        let renewal_retry_period = self.retry_period_secs;

        let renew_interval = Duration::from_secs(std::cmp::max(1, self.renew_deadline_secs / 2));
        let renewal_shutdown = shutdown.clone();

        tokio::spawn(async move {
            let renewal = LeaderElection {
                client: renewal_client,
                namespace: renewal_ns,
                identity: renewal_identity,
                lease_duration_secs: renewal_lease_duration,
                renew_deadline_secs: renewal_renew_deadline,
                retry_period_secs: renewal_retry_period,
            };

            let max_failures =
                (renewal.lease_duration_secs as u64) / std::cmp::max(1, renewal.retry_period_secs);
            let mut consecutive_failures: u64 = 0;

            loop {
                tokio::time::sleep(renew_interval).await;
                match renewal.try_acquire().await {
                    Ok(true) => {
                        tracing::debug!("leader lease renewed");
                        consecutive_failures = 0;
                    }
                    Ok(false) => {
                        tracing::error!(
                            "lost leader lease — another instance took over, shutting down"
                        );
                        renewal_shutdown.cancel();
                        return;
                    }
                    Err(e) => {
                        consecutive_failures += 1;
                        tracing::warn!(
                            error = %e,
                            consecutive = consecutive_failures,
                            "leader lease renewal failed"
                        );
                        if consecutive_failures >= max_failures {
                            tracing::error!("too many consecutive renewal failures, shutting down");
                            renewal_shutdown.cancel();
                            return;
                        }
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
    use serial_test::serial;

    // SAFETY: env var mutations are serialized via #[serial]
    unsafe fn set_env(key: &str, val: &str) {
        unsafe { std::env::set_var(key, val) };
    }
    unsafe fn remove_env(key: &str) {
        unsafe { std::env::remove_var(key) };
    }

    #[test]
    #[serial]
    fn parse_duration_secs_with_suffix() {
        unsafe { set_env("TEST_LEADER_DUR_1", "30s") };
        assert_eq!(parse_duration_secs("TEST_LEADER_DUR_1", 10), 30);
        unsafe { remove_env("TEST_LEADER_DUR_1") };
    }

    #[test]
    #[serial]
    fn parse_duration_secs_without_suffix() {
        unsafe { set_env("TEST_LEADER_DUR_2", "45") };
        assert_eq!(parse_duration_secs("TEST_LEADER_DUR_2", 10), 45);
        unsafe { remove_env("TEST_LEADER_DUR_2") };
    }

    #[test]
    #[serial]
    fn parse_duration_secs_invalid_returns_default() {
        unsafe { set_env("TEST_LEADER_DUR_3", "not-a-number") };
        assert_eq!(parse_duration_secs("TEST_LEADER_DUR_3", 15), 15);
        unsafe { remove_env("TEST_LEADER_DUR_3") };
    }

    #[test]
    #[serial]
    fn parse_duration_secs_missing_env_returns_default() {
        unsafe { remove_env("TEST_LEADER_DUR_MISSING") };
        assert_eq!(parse_duration_secs("TEST_LEADER_DUR_MISSING", 20), 20);
    }

    #[test]
    #[serial]
    fn parse_duration_secs_empty_string_returns_default() {
        unsafe { set_env("TEST_LEADER_DUR_4", "") };
        assert_eq!(parse_duration_secs("TEST_LEADER_DUR_4", 25), 25);
        unsafe { remove_env("TEST_LEADER_DUR_4") };
    }

    #[test]
    #[serial]
    fn parse_duration_secs_zero() {
        unsafe { set_env("TEST_LEADER_DUR_5", "0s") };
        assert_eq!(parse_duration_secs("TEST_LEADER_DUR_5", 10), 0);
        unsafe { remove_env("TEST_LEADER_DUR_5") };
    }

    #[test]
    #[serial]
    fn parse_duration_secs_zero_without_suffix() {
        unsafe { set_env("TEST_LEADER_DUR_6", "0") };
        assert_eq!(parse_duration_secs("TEST_LEADER_DUR_6", 10), 0);
        unsafe { remove_env("TEST_LEADER_DUR_6") };
    }

    #[test]
    #[serial]
    fn parse_duration_secs_large_value() {
        unsafe { set_env("TEST_LEADER_DUR_7", "3600s") };
        assert_eq!(parse_duration_secs("TEST_LEADER_DUR_7", 10), 3600);
        unsafe { remove_env("TEST_LEADER_DUR_7") };
    }
}

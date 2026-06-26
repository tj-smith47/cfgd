use std::time::Duration;

use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use k8s_openapi::jiff::{SignedDuration, Timestamp};
use kube::Client;
use kube::api::{Api, Patch, PatchParams, PostParams};
use tokio_util::sync::CancellationToken;

use crate::errors::OperatorError;

const FIELD_MANAGER_PREFIX: &str = "cfgd-operator-leader";
const LEASE_NAME: &str = "cfgd-operator-leader";

/// Format a [`jiff::Timestamp`] as an RFC 3339 string with fixed microsecond
/// precision and a `Z` zulu offset, byte-identical to how `k8s-openapi`'s own
/// `MicroTime` serializer renders the wire value. Used to build the
/// `renewTime`/`acquireTime` strings the Kubernetes API server expects.
fn rfc3339_micros(ts: Timestamp) -> Result<String, OperatorError> {
    // Matches k8s-openapi 0.28's MicroTime::serialize strftime pattern exactly.
    k8s_openapi::jiff::fmt::strtime::format("%Y-%m-%dT%H:%M:%S%.6fZ", ts)
        .map_err(|e| OperatorError::Leader(format!("formatting lease timestamp: {e}")))
}

/// Module-local wrapper around [`cfgd_core::parse_duration_str`] that reads a
/// duration from an env var and returns seconds (u64), with a caller-provided
/// fallback. The caller-specific `default` per lease timing variable
/// (LEASE_DURATION / RENEW_DEADLINE / RETRY_PERIOD) is the reason this doesn't
/// collapse into `cfgd_core::parse_duration_str`: those defaults are an
/// operator leader-election concern, not a core-library one. Kept local and
/// documented per dedup-audit S1 (decision: keep + document).
fn parse_duration_secs(env_var: &str, default: u64) -> u64 {
    std::env::var(env_var)
        .ok()
        .and_then(|v| cfgd_core::parse_duration_str(&v).map(|d| d.as_secs()).ok())
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

    // SSA field manager must be unique per pod — if two operator pods share
    // the same field manager they can both "own" `spec.holderIdentity` and
    // silently split-brain on renewals. Deriving from the pod-scoped
    // `identity` guarantees distinctness.
    fn field_manager(&self) -> String {
        format!("{FIELD_MANAGER_PREFIX}:{}", self.identity)
    }

    pub async fn try_acquire(&self) -> Result<bool, OperatorError> {
        let leases: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);
        let now = Timestamp::now();
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
                    let expiry = renew_time
                        .0
                        .checked_add(SignedDuration::from_secs(i64::from(duration_secs)))
                        .unwrap_or(renew_time.0);
                    now > expiry
                } else {
                    // No renew time means expired
                    true
                };

                if expired || current_holder == self.identity {
                    let taking_over = expired && current_holder != self.identity;
                    let now_str = rfc3339_micros(now)?;
                    let acquire_time = if taking_over {
                        Some(now_str.clone())
                    } else {
                        spec.and_then(|s| s.acquire_time.as_ref())
                            .map(|t| rfc3339_micros(t.0))
                            .transpose()?
                    };
                    // SSA apply body must carry apiVersion/kind so the server
                    // accepts it as an `application/apply-patch+yaml`.
                    let patch = serde_json::json!({
                        "apiVersion": "coordination.k8s.io/v1",
                        "kind": "Lease",
                        "metadata": { "name": LEASE_NAME },
                        "spec": {
                            "holderIdentity": self.identity,
                            "leaseDurationSeconds": self.lease_duration_secs,
                            "renewTime": now_str,
                            "acquireTime": acquire_time,
                            "leaseTransitions": if taking_over {
                                spec.and_then(|s| s.lease_transitions).unwrap_or(0) + 1
                            } else {
                                spec.and_then(|s| s.lease_transitions).unwrap_or(0)
                            }
                        }
                    });

                    let field_manager = self.field_manager();
                    let params = if taking_over {
                        // Takeover needs force=true to strip the stale
                        // previous-holder's field manager from spec.*.
                        PatchParams::apply(&field_manager).force()
                    } else {
                        // Self-renewal: we already own the fields; a plain
                        // SSA apply is a no-conflict renewal. If another pod
                        // silently took over, SSA returns 409 Conflict and
                        // we treat that as lost-lease below.
                        PatchParams::apply(&field_manager)
                    };

                    match leases
                        .patch(LEASE_NAME, &params, &Patch::Apply(&patch))
                        .await
                    {
                        Ok(_) => Ok(true),
                        Err(kube::Error::Api(ref e)) if e.code == 409 => {
                            tracing::warn!(
                                identity = %self.identity,
                                "SSA conflict on lease apply — another pod owns the field, treating as not-acquired"
                            );
                            Ok(false)
                        }
                        Err(e) => Err(OperatorError::KubeError(e)),
                    }
                } else {
                    Ok(false)
                }
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                // Create the Lease
                let now_str = rfc3339_micros(now_micro.0)?;
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
                        "acquireTime": now_str,
                        "renewTime": now_str,
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

    // -----------------------------------------------------------------------
    // try_acquire — kube interactions exercised through the operator's
    // tower_test::mock::pair-backed MockKubeHarness.
    // -----------------------------------------------------------------------

    use crate::controllers::test_kube_harness::{ExpectedCall, MockKubeHarness};

    const TEST_NS: &str = "cfgd-system";
    const TEST_ID: &str = "operator-pod-A";

    fn lease_path() -> String {
        format!("/apis/coordination.k8s.io/v1/namespaces/{TEST_NS}/leases/{LEASE_NAME}")
    }

    fn collection_path() -> String {
        format!("/apis/coordination.k8s.io/v1/namespaces/{TEST_NS}/leases")
    }

    /// Build a Lease JSON payload whose renew_time is `secs_ago` seconds before now
    /// and whose holder is `holder`. `lease_duration_seconds` controls expiry math.
    fn lease_json(holder: &str, lease_duration: i32, secs_ago: i64) -> serde_json::Value {
        let renew_time = Timestamp::now() - SignedDuration::from_secs(secs_ago);
        let renew_str = rfc3339_micros(renew_time).expect("format test timestamp");
        serde_json::json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": { "name": LEASE_NAME, "namespace": TEST_NS },
            "spec": {
                "holderIdentity": holder,
                "leaseDurationSeconds": lease_duration,
                "renewTime": renew_str,
                "acquireTime": renew_str,
                "leaseTransitions": 0_i32,
            }
        })
    }

    #[tokio::test]
    async fn try_acquire_creates_new_lease_when_get_returns_404() {
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_404(LEASE_NAME),
            // POST to the collection endpoint with the new Lease body
            ExpectedCall::post(collection_path()).returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le
            .try_acquire()
            .await
            .expect("404 path should create the Lease");
        assert!(acquired, "create-on-404 must return Ok(true)");

        let report = harness.finish().await;
        assert_eq!(report.captured.len(), 2);
        // The POST body must declare us as holder.
        let post_body = report.captured[1].body_json();
        assert_eq!(post_body["spec"]["holderIdentity"], TEST_ID);
        assert_eq!(post_body["spec"]["leaseTransitions"], 0);
    }

    #[tokio::test]
    async fn try_acquire_takes_over_when_existing_lease_expired() {
        // Existing holder is someone else, renew_time is 60s ago, lease_duration 15s
        // → expired → we patch to take over.
        let existing = lease_json("other-pod", 15, 60);
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&existing),
            ExpectedCall::patch(lease_path())
                .with_query_contains("force=true")
                .returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le
            .try_acquire()
            .await
            .expect("expired-takeover should succeed");
        assert!(acquired);

        let report = harness.finish().await;
        assert_eq!(report.captured.len(), 2);
        let patch_body = report.captured[1].body_json();
        assert_eq!(patch_body["spec"]["holderIdentity"], TEST_ID);
        // Takeover bumps transitions
        assert_eq!(patch_body["spec"]["leaseTransitions"], 1);
    }

    #[tokio::test]
    async fn try_acquire_self_renews_when_currently_holder() {
        // We already hold the lease; SSA apply for renewal must NOT be force=true.
        let existing = lease_json(TEST_ID, 15, 5);
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&existing),
            ExpectedCall::patch(lease_path()).returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le.try_acquire().await.expect("self-renew should succeed");
        assert!(acquired);

        let report = harness.finish().await;
        assert_eq!(report.captured.len(), 2);
        let patch_body = report.captured[1].body_json();
        assert_eq!(patch_body["spec"]["holderIdentity"], TEST_ID);
        // Self-renew preserves transition count
        assert_eq!(patch_body["spec"]["leaseTransitions"], 0);
    }

    #[tokio::test]
    async fn try_acquire_returns_false_when_other_holder_and_unexpired() {
        // Another pod owns the lease and it's still valid → we DO NOT patch.
        let existing = lease_json("other-pod", 60, 5);
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&existing),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le
            .try_acquire()
            .await
            .expect("non-acquire is Ok(false), not Err");
        assert!(
            !acquired,
            "another holder with unexpired lease must yield Ok(false)"
        );

        let report = harness.finish().await;
        assert_eq!(
            report.captured.len(),
            1,
            "must only issue the GET — no patch when not acquiring"
        );
    }

    #[tokio::test]
    async fn try_acquire_returns_false_on_409_ssa_conflict() {
        // Existing lease shows us as the holder; PATCH races against another
        // pod and returns 409 → treat as not-acquired (Ok(false)).
        let existing = lease_json(TEST_ID, 15, 5);
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&existing),
            ExpectedCall::patch(lease_path()).returning_server_error(409, "field manager conflict"),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le
            .try_acquire()
            .await
            .expect("409 SSA conflict must be Ok(false), not Err");
        assert!(!acquired);

        let report = harness.finish().await;
        assert_eq!(report.captured.len(), 2);
    }

    #[tokio::test]
    async fn try_acquire_propagates_unexpected_get_errors() {
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_server_error(500, "etcd fell over"),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let err = le
            .try_acquire()
            .await
            .expect_err("non-404 server error must surface");
        let msg = err.to_string();
        assert!(
            msg.contains("etcd fell over") || msg.to_lowercase().contains("kube"),
            "expected kube error context, got: {msg}"
        );

        let _report = harness.finish().await;
    }

    #[tokio::test]
    async fn field_manager_includes_pod_identity() {
        // Construct a LeaderElection without any kube traffic to assert the
        // SSA field-manager string is identity-scoped (split-brain guard).
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![]);
        let le = LeaderElection::new(
            ctx.client.clone(),
            "any-ns".into(),
            "operator-pod-XYZ".into(),
        );
        let fm = le.field_manager();
        assert_eq!(fm, "cfgd-operator-leader:operator-pod-XYZ");
        let _report = harness.finish().await;
    }

    // -----------------------------------------------------------------------
    // try_acquire — additional branch coverage. Drives:
    //  * existing lease with NO renew_time (lines 79-82: `expired = true`)
    //  * existing lease held by us, NOT-yet-expired (self-renew, no force)
    //  * existing lease with no spec at all (defensive null guard at L65)
    //  * lease_transitions absent (L103/L105: unwrap_or(0))
    // -----------------------------------------------------------------------

    /// Build a Lease JSON whose spec omits `renewTime` so the `expired = true`
    /// no-renew-time branch fires.
    fn lease_json_no_renew_time(holder: &str, lease_duration: i32) -> serde_json::Value {
        serde_json::json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": { "name": LEASE_NAME, "namespace": TEST_NS },
            "spec": {
                "holderIdentity": holder,
                "leaseDurationSeconds": lease_duration,
                "leaseTransitions": 0_i32,
            }
        })
    }

    /// Build a Lease JSON whose spec omits `leaseTransitions` so the
    /// `unwrap_or(0)` arm fires on both branches.
    fn lease_json_no_transitions(
        holder: &str,
        lease_duration: i32,
        secs_ago: i64,
    ) -> serde_json::Value {
        let renew_time = Timestamp::now() - SignedDuration::from_secs(secs_ago);
        let renew_str = rfc3339_micros(renew_time).expect("format test timestamp");
        serde_json::json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": { "name": LEASE_NAME, "namespace": TEST_NS },
            "spec": {
                "holderIdentity": holder,
                "leaseDurationSeconds": lease_duration,
                "renewTime": renew_str,
            }
        })
    }

    #[tokio::test]
    async fn try_acquire_takes_over_when_existing_lease_has_no_renew_time() {
        // Spec carries no renewTime → `expired` is set to true and we take
        // over with force=true.
        let existing = lease_json_no_renew_time("ghost-pod", 15);
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&existing),
            ExpectedCall::patch(lease_path())
                .with_query_contains("force=true")
                .returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le
            .try_acquire()
            .await
            .expect("no-renew-time should be treated as expired");
        assert!(acquired);

        let report = harness.finish().await;
        assert_eq!(report.captured.len(), 2);
        let patch_body = report.captured[1].body_json();
        assert_eq!(patch_body["spec"]["holderIdentity"], TEST_ID);
        // Took over from a ghost → transitions bumped from 0 to 1.
        assert_eq!(patch_body["spec"]["leaseTransitions"], 1);
    }

    #[tokio::test]
    async fn try_acquire_self_renew_with_missing_transitions_defaults_to_zero() {
        // We hold the lease; the existing spec doesn't carry leaseTransitions.
        // The `unwrap_or(0)` arm at L105 must produce 0 in the renewed patch.
        let existing = lease_json_no_transitions(TEST_ID, 15, 3);
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&existing),
            ExpectedCall::patch(lease_path()).returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le
            .try_acquire()
            .await
            .expect("self-renew with no prior transitions should succeed");
        assert!(acquired);

        let report = harness.finish().await;
        assert_eq!(report.captured.len(), 2);
        let patch_body = report.captured[1].body_json();
        // No takeover → transitions stays 0 (unwrap_or path).
        assert_eq!(patch_body["spec"]["leaseTransitions"], 0);
    }

    #[tokio::test]
    async fn try_acquire_takeover_with_no_transitions_bumps_to_one() {
        // Existing holder is someone else, expired, with no leaseTransitions
        // in spec. Takeover should bump the unwrap_or(0) baseline to 1.
        let renew_str = rfc3339_micros(Timestamp::now() - SignedDuration::from_secs(60))
            .expect("format test timestamp");
        let existing = serde_json::json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": { "name": LEASE_NAME, "namespace": TEST_NS },
            "spec": {
                "holderIdentity": "other-pod",
                "leaseDurationSeconds": 15,
                "renewTime": renew_str,
            }
        });
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&existing),
            ExpectedCall::patch(lease_path())
                .with_query_contains("force=true")
                .returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le.try_acquire().await.expect("takeover should succeed");
        assert!(acquired);

        let report = harness.finish().await;
        let patch_body = report.captured[1].body_json();
        assert_eq!(patch_body["spec"]["holderIdentity"], TEST_ID);
        // Takeover from a holder with no transitions in spec → bumps 0+1=1.
        assert_eq!(patch_body["spec"]["leaseTransitions"], 1);
    }

    #[tokio::test]
    async fn try_acquire_takeover_uses_lease_duration_seconds_from_spec_when_present() {
        // Spec carries an explicit leaseDurationSeconds different from the
        // default 15. Pin that the expiry math uses the spec value (60 here),
        // so a 90s-ago renewTime is still expired even with the longer duration.
        let renew_time = Timestamp::now() - SignedDuration::from_secs(90);
        let renew_str = rfc3339_micros(renew_time).expect("format test timestamp");
        let existing = serde_json::json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": { "name": LEASE_NAME, "namespace": TEST_NS },
            "spec": {
                "holderIdentity": "other-pod",
                "leaseDurationSeconds": 60,
                "renewTime": renew_str,
                "leaseTransitions": 7_i32,
            }
        });
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&existing),
            ExpectedCall::patch(lease_path())
                .with_query_contains("force=true")
                .returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le
            .try_acquire()
            .await
            .expect("expired (60s window) should take over");
        assert!(acquired);

        let report = harness.finish().await;
        let patch_body = report.captured[1].body_json();
        // Transitions incremented from 7 to 8.
        assert_eq!(patch_body["spec"]["leaseTransitions"], 8);
    }

    #[tokio::test]
    async fn try_acquire_returns_false_when_lease_has_no_spec_at_all() {
        // A Lease whose spec field is null is still treated as not-held — the
        // current_holder fall-through (empty string) means our identity does
        // NOT match, but the lease IS expired (no renewTime), so we take over.
        let existing = serde_json::json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": { "name": LEASE_NAME, "namespace": TEST_NS }
        });
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&existing),
            ExpectedCall::patch(lease_path())
                .with_query_contains("force=true")
                .returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let acquired = le
            .try_acquire()
            .await
            .expect("missing spec is treated as available");
        assert!(acquired);

        let report = harness.finish().await;
        assert_eq!(report.captured.len(), 2);
    }

    // -----------------------------------------------------------------------
    // LeaderElection::new — wiring coverage. Confirms env-driven timing values
    // flow into the struct so the run/renew loop reads the configured values
    // rather than the hard-coded defaults.
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[serial]
    async fn leader_election_new_reads_lease_duration_from_env() {
        unsafe { set_env("LEADER_LEASE_DURATION", "45s") };
        let (ctx, _reg, _harness) = MockKubeHarness::new(vec![]);
        let le = LeaderElection::new(ctx.client.clone(), "ns".into(), "id".into());
        unsafe { remove_env("LEADER_LEASE_DURATION") };
        assert_eq!(le.lease_duration_secs, 45);
    }

    #[tokio::test]
    #[serial]
    async fn leader_election_new_reads_renew_deadline_from_env() {
        unsafe { set_env("LEADER_RENEW_DEADLINE", "20s") };
        let (ctx, _reg, _harness) = MockKubeHarness::new(vec![]);
        let le = LeaderElection::new(ctx.client.clone(), "ns".into(), "id".into());
        unsafe { remove_env("LEADER_RENEW_DEADLINE") };
        assert_eq!(le.renew_deadline_secs, 20);
    }

    #[tokio::test]
    #[serial]
    async fn leader_election_new_reads_retry_period_from_env() {
        unsafe { set_env("LEADER_RETRY_PERIOD", "5s") };
        let (ctx, _reg, _harness) = MockKubeHarness::new(vec![]);
        let le = LeaderElection::new(ctx.client.clone(), "ns".into(), "id".into());
        unsafe { remove_env("LEADER_RETRY_PERIOD") };
        assert_eq!(le.retry_period_secs, 5);
    }

    #[tokio::test]
    #[serial]
    async fn leader_election_new_defaults_when_env_vars_unset() {
        unsafe {
            remove_env("LEADER_LEASE_DURATION");
            remove_env("LEADER_RENEW_DEADLINE");
            remove_env("LEADER_RETRY_PERIOD");
        }
        let (ctx, _reg, _harness) = MockKubeHarness::new(vec![]);
        let le = LeaderElection::new(ctx.client.clone(), "ns".into(), "id".into());
        // Documented defaults from the constructor body.
        assert_eq!(le.lease_duration_secs, 15);
        assert_eq!(le.renew_deadline_secs, 10);
        assert_eq!(le.retry_period_secs, 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_wins_acquisition_then_calls_on_started_callback() {
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_404(LEASE_NAME),
            ExpectedCall::post(collection_path()).returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        let cancel = tokio_util::sync::CancellationToken::new();
        let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started_for_cb = started.clone();
        let cancel_for_cb = cancel.clone();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            le.run(cancel.clone(), move || {
                let started = started_for_cb;
                let cancel = cancel_for_cb;
                async move {
                    started.store(true, std::sync::atomic::Ordering::SeqCst);
                    cancel.cancel();
                    Ok(())
                }
            }),
        )
        .await
        .expect("run did not complete in time");

        assert!(result.is_ok(), "run returned Err: {result:?}");
        assert!(
            started.load(std::sync::atomic::Ordering::SeqCst),
            "on_started callback must fire after acquisition"
        );

        let report = harness.finish().await;
        assert!(
            report.captured.len() >= 2,
            "expected GET + POST to be captured, got {}",
            report.captured.len()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_retries_when_acquisition_returns_false_then_succeeds() {
        let (ctx, _reg, harness) = MockKubeHarness::new(vec![
            ExpectedCall::get(lease_path()).returning_json(&lease_json("other-holder", 15, 0)),
            ExpectedCall::get(lease_path()).returning_404(LEASE_NAME),
            ExpectedCall::post(collection_path()).returning_json(&lease_json(TEST_ID, 15, 0)),
        ]);

        let mut le = LeaderElection::new(ctx.client.clone(), TEST_NS.into(), TEST_ID.into());
        le.retry_period_secs = 1;

        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_for_cb = cancel.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            le.run(cancel.clone(), move || {
                let cancel = cancel_for_cb;
                async move {
                    cancel.cancel();
                    Ok(())
                }
            }),
        )
        .await
        .expect("run did not complete in time");

        assert!(result.is_ok(), "run returned Err: {result:?}");
        let _ = harness.finish().await;
    }
}

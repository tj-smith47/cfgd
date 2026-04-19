//! Shared retry-with-exponential-backoff policy.
//!
//! Before this module, the same `MAX_RETRIES = 3` and `500ms * 2^n` backoff
//! math was inlined at `server_client::post_with_retry` (sync/ureq) and
//! `gateway::api::create_drift_alert_crd` (async/kube). Three timeouts had
//! already drifted when the dedup audit flagged it.
//!
//! Call sites retain their own error classification — the sync site retries
//! on `ureq::Error::Transport` and 5xx, the async site retries on any kube
//! error and short-circuits on HTTP 409. Only the policy (attempts + delay)
//! lives here.

use std::time::Duration;

/// Retry policy for transient network/API failures.
#[derive(Debug, Clone, Copy)]
pub struct BackoffConfig {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
}

impl BackoffConfig {
    /// Canonical transient-error policy: 3 attempts, 500ms initial delay
    /// doubled each retry (→ 500ms, 1s before the third attempt).
    pub const DEFAULT_TRANSIENT: Self = Self {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(500),
    };

    /// Delay before `attempt` (1-indexed; attempt 0 has no preceding delay).
    /// Returns `Duration::ZERO` for `attempt == 0` so a single tight branch
    /// at the call site handles both the first-attempt and retry cases.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            Duration::ZERO
        } else {
            self.initial_backoff * 2u32.pow(attempt - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_transient_has_three_attempts() {
        assert_eq!(BackoffConfig::DEFAULT_TRANSIENT.max_attempts, 3);
    }

    #[test]
    fn delay_for_attempt_zero_is_zero() {
        assert_eq!(
            BackoffConfig::DEFAULT_TRANSIENT.delay_for_attempt(0),
            Duration::ZERO
        );
    }

    #[test]
    fn delay_for_attempt_doubles() {
        let c = BackoffConfig::DEFAULT_TRANSIENT;
        assert_eq!(c.delay_for_attempt(1), Duration::from_millis(500));
        assert_eq!(c.delay_for_attempt(2), Duration::from_millis(1000));
        assert_eq!(c.delay_for_attempt(3), Duration::from_millis(2000));
    }
}

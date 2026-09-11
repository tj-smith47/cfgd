//! Shared retry-with-exponential-backoff policy.
//!
//! Before this module, the same `MAX_RETRIES = 3` and `500ms * 2^n` backoff
//! math was inlined at `server_client::post_with_retry` (sync/ureq) and
//! `gateway::api::create_drift_alert_crd` (async/kube). Three timeouts had
//! already drifted apart between those copies.
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

    /// Retry policy for a rate-limited response the server gave no advised wait
    /// for: the same three attempts, but measured against the quota that refused
    /// them. The gateway's enrollment bucket hands back one token every
    /// [`crate::ENROLL_RATE_LIMIT_REFILL`], so a ladder measured in milliseconds
    /// exhausts itself before the quota has moved at all. Half that interval, so
    /// the cumulative ladder (`0`, then `1x`, then `2x`) outlasts a full refill
    /// before the last attempt.
    pub const RATE_LIMITED: Self = Self {
        max_attempts: 3,
        initial_backoff: Duration::from_secs(crate::util::ENROLL_RATE_LIMIT_REFILL_SECS / 2),
    };

    /// [`Self::RATE_LIMITED`], honouring the test override.
    ///
    /// The ladder it describes is measured in seconds, which is correct against
    /// a real gateway and far too slow for a test that has to prove WHICH ladder
    /// a 429 chose. Production reads the policy through here so a pin can make
    /// that choice observable in milliseconds.
    pub fn rate_limited() -> Self {
        #[cfg(any(test, feature = "test-helpers"))]
        {
            let millis = RATE_LIMITED_BACKOFF_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed);
            if millis != u64::MAX {
                return Self {
                    initial_backoff: Duration::from_millis(millis),
                    ..Self::RATE_LIMITED
                };
            }
        }
        Self::RATE_LIMITED
    }

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

/// Millisecond override of [`BackoffConfig::RATE_LIMITED`]'s initial backoff, or
/// [`u64::MAX`] for "no override". It exists so a test proving that a 429 moved
/// the client onto the rationed ladder does not have to pay the rationed ladder's
/// real seconds to see it.
#[cfg(any(test, feature = "test-helpers"))]
static RATE_LIMITED_BACKOFF_OVERRIDE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

/// Pin the rate-limited ladder's first step, or hand back the default with
/// `None`. Returns what was pinned before, so a guard can put it back.
///
/// Reach for it through `test_helpers::RateLimitedBackoffGuard`, never directly.
#[cfg(any(test, feature = "test-helpers"))]
pub(crate) fn set_rate_limited_backoff_override(millis: Option<u64>) -> Option<u64> {
    let prior = RATE_LIMITED_BACKOFF_OVERRIDE.swap(
        millis.unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    (prior != u64::MAX).then_some(prior)
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

    /// A ladder that finishes before the quota that refused it has handed back a
    /// token cannot clear a full bucket, so the floor is the gateway's own
    /// refill interval rather than a number copied beside it.
    #[test]
    fn the_rate_limited_ladder_outlasts_the_enrollment_quota() {
        let c = BackoffConfig::RATE_LIMITED;
        let total: Duration = (0..c.max_attempts).map(|a| c.delay_for_attempt(a)).sum();
        assert!(
            total >= crate::ENROLL_RATE_LIMIT_REFILL,
            "rate-limited ladder ends at {total:?}, quota refills in {:?}",
            crate::ENROLL_RATE_LIMIT_REFILL
        );
    }

    /// The pin the 429 tests read the ladder through has to be the same policy
    /// production reads, or those tests prove something about a second ladder.
    #[test]
    #[serial_test::serial(rate_limited_backoff)]
    fn the_rate_limited_accessor_is_the_constant_when_nothing_is_pinned() {
        assert_eq!(
            BackoffConfig::rate_limited().initial_backoff,
            BackoffConfig::RATE_LIMITED.initial_backoff
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

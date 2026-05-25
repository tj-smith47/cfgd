//! In-process IP-keyed token-bucket rate limiter for unauthenticated routes.
//!
//! Bounds CPU + disk-IO amplification on `/enroll/*` and `/checkin`. Each
//! verify call runs `tempfile::tempdir() + gpg/ssh-keygen + fs::write` —
//! ~50–100 ms of compute per attempt. An attacker who hammers these endpoints
//! without limit can drive sustained load with very little outbound bandwidth.
//!
//! The bucket grants `burst` tokens up-front, refills at `refill_per_sec`, and
//! tracks one bucket per peer IP. Buckets idle for more than `idle_evict` are
//! cleared on the next admission to bound memory in long-running deployments.
//!
//! Not distributed — each operator replica enforces its own quota. For fleet
//! deployments this is acceptable as a defense-in-depth measure; deliberate
//! abuse from a single IP still gets ~5 N replicas * 5/min = 25 req/min total.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use parking_lot::Mutex;

use super::errors::GatewayError;

/// Per-IP bucket state.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// Available tokens — fractional so refill granularity is sub-second.
    tokens: f64,
    /// Last refill instant.
    last: Instant,
}

#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<RateLimiterInner>,
}

struct RateLimiterInner {
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
    capacity: f64,
    refill_per_sec: f64,
    idle_evict: Duration,
}

impl RateLimiter {
    /// Build a limiter: `burst` tokens upfront, refilled at `per_minute / 60`
    /// per second.
    pub fn per_minute(burst: u32, per_minute: u32) -> Self {
        Self::with_refill(burst, f64::from(per_minute) / 60.0)
    }

    /// Build a limiter with explicit refill rate (tokens/sec).
    pub fn with_refill(burst: u32, refill_per_sec: f64) -> Self {
        let capacity = f64::from(burst);
        Self {
            inner: Arc::new(RateLimiterInner {
                buckets: Mutex::new(HashMap::new()),
                capacity,
                refill_per_sec,
                idle_evict: Duration::from_secs(300),
            }),
        }
    }

    /// Try to consume one token from `peer`'s bucket. Returns `Ok(())` on
    /// admit, `Err(retry_after_secs)` on reject.
    pub fn check(&self, peer: IpAddr) -> Result<(), u64> {
        let now = Instant::now();
        let mut buckets = self.inner.buckets.lock();

        // Opportunistic eviction — bounds memory if attackers cycle IPs.
        // O(n) but capped via `idle_evict` window; small relative to the
        // request cost we're guarding against.
        if buckets.len() > 1024 {
            let cutoff = self.inner.idle_evict;
            buckets.retain(|_, b| now.duration_since(b.last) < cutoff);
        }

        let bucket = buckets.entry(peer).or_insert(Bucket {
            tokens: self.inner.capacity,
            last: now,
        });

        // Refill since last touch.
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens =
            (bucket.tokens + elapsed * self.inner.refill_per_sec).min(self.inner.capacity);
        bucket.last = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let needed = 1.0 - bucket.tokens;
            let wait_secs = (needed / self.inner.refill_per_sec).ceil() as u64;
            Err(wait_secs.max(1))
        }
    }
}

/// Axum middleware that admits or rejects based on `RateLimiter`. Reads peer
/// IP from `ConnectInfo<SocketAddr>` — the router must be served via
/// `into_make_service_with_connect_info` for this to populate.
///
/// Falls open (admit) if `ConnectInfo` isn't available — better than
/// hard-failing real traffic when ConnectInfo wasn't wired up, while still
/// providing protection in production where it always is.
pub async fn rate_limit_middleware(
    State(limiter): State<RateLimiter>,
    request: Request,
    next: Next,
) -> Result<Response, GatewayError> {
    let peer = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());

    if let Some(ip) = peer {
        if let Err(retry_after) = limiter.check(ip) {
            tracing::warn!(
                peer = %ip,
                retry_after_secs = retry_after,
                "rate-limit: request rejected",
            );
            return Err(GatewayError::RateLimited(retry_after));
        }
    } else {
        tracing::debug!("rate-limit: no ConnectInfo on request — admitting (test/local context)");
    }
    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_n_requests_admitted_then_capped() {
        let limiter = RateLimiter::per_minute(3, 60);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_ok());
        let rejected = limiter.check(ip).unwrap_err();
        assert!(rejected >= 1, "retry_after must be at least 1s");
    }

    #[test]
    fn distinct_ips_are_independent() {
        let limiter = RateLimiter::per_minute(1, 60);
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(limiter.check(a).is_ok());
        assert!(limiter.check(b).is_ok());
        assert!(limiter.check(a).is_err());
        assert!(limiter.check(b).is_err());
    }

    #[test]
    fn bucket_refills_after_elapsed_time() {
        // burst=1, refill=10/s — after 200ms we should have at least one token back.
        let limiter = RateLimiter::with_refill(1, 10.0);
        let ip: IpAddr = "10.0.0.3".parse().unwrap();
        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_err());
        std::thread::sleep(Duration::from_millis(200));
        assert!(limiter.check(ip).is_ok());
    }

    #[test]
    fn retry_after_grows_with_deficit() {
        // burst=1, refill=0.1/s (one per 10s). After exhausting, retry should be ~10s.
        let limiter = RateLimiter::with_refill(1, 0.1);
        let ip: IpAddr = "10.0.0.4".parse().unwrap();
        assert!(limiter.check(ip).is_ok());
        let wait = limiter.check(ip).unwrap_err();
        assert!((5..=11).contains(&wait), "expected ~10s retry, got {wait}s");
    }

    #[test]
    fn refill_caps_at_capacity() {
        let limiter = RateLimiter::with_refill(2, 100.0);
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        // Burn one, sleep long enough to overflow if there were no cap.
        assert!(limiter.check(ip).is_ok());
        std::thread::sleep(Duration::from_millis(100));
        // Should be 2 tokens available again, not 11.
        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_ok());
        assert!(limiter.check(ip).is_err());
    }
}

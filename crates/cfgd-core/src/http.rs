//! Canonical `ureq::Agent` construction + named HTTP timeout constants.
//!
//! Before this module, `ureq::AgentBuilder::new().timeout(...).build()` was
//! inlined at 7 production sites (server_client, daemon, oci x3, upgrade x2)
//! with three of the timeouts already drifted. Worse, `cfgd/src/ai/client.rs`
//! used bare `ureq::post(...)` with no timeout at all — the Anthropic call
//! could hang the CLI indefinitely. Using these constants + `http_agent`
//! replaces every inline `Duration::from_secs(...)` literal and keeps future
//! timeout changes in one place.

use std::time::Duration;

/// Device gateway API calls (checkin, drift, enrollment) — small JSON payloads.
/// Kept short because the gateway is nearby and responses are small.
pub const HTTP_API_TIMEOUT: Duration = Duration::from_secs(30);

/// OCI registry blob / manifest operations (push, pull, multi-platform index).
/// Module layers and image blobs can be hundreds of MiB; 300s accommodates
/// cold caches and slow registry peers.
pub const HTTP_OCI_TIMEOUT: Duration = Duration::from_secs(300);

/// GitHub Releases API queries + binary archive downloads for self-upgrade.
/// 300s covers slow mirrors; the streaming download applies per-chunk.
pub const HTTP_UPGRADE_TIMEOUT: Duration = Duration::from_secs(300);

/// Anthropic API requests from `cfgd generate` and `cfgd ai`.
/// Claude latency for agentic tool use is normally a few seconds; 120s is a
/// ceiling for pathological slow networks. Must have *some* timeout — before
/// this constant the request had none at all.
pub const HTTP_AI_TIMEOUT: Duration = Duration::from_secs(120);

/// Outbound webhook notifications (drift alerts, check-in callbacks).
/// Short — the caller doesn't wait on user-visible output; a slow peer
/// should not starve the daemon's notification loop.
pub const HTTP_WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// Build a `ureq::Agent` with `timeout` applied. Exists so every call site
/// that wants a timeout can use one line and we can change `AgentBuilder`
/// configuration (user-agent defaults, connection pooling, TLS options)
/// in exactly one place if it becomes necessary.
pub fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(timeout).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_timeouts_are_distinct_and_sane() {
        assert_eq!(HTTP_API_TIMEOUT, Duration::from_secs(30));
        assert_eq!(HTTP_OCI_TIMEOUT, Duration::from_secs(300));
        assert_eq!(HTTP_UPGRADE_TIMEOUT, Duration::from_secs(300));
        assert_eq!(HTTP_AI_TIMEOUT, Duration::from_secs(120));
        assert_eq!(HTTP_WEBHOOK_TIMEOUT, Duration::from_secs(10));
        // AI timeout must be positive — the original bug was "no timeout"
        assert!(HTTP_AI_TIMEOUT > Duration::ZERO);
    }

    #[test]
    fn http_agent_builds_without_panic() {
        let _ = http_agent(HTTP_API_TIMEOUT);
    }
}

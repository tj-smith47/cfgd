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

/// The `ureq::Agent` this process uses for `timeout`, built at most once per
/// distinct timeout. Exists so every call site that wants a timeout can use one
/// line and we can change agent configuration (user-agent defaults, connection
/// pooling, TLS options) in exactly one place if it becomes necessary.
///
/// Shared rather than freshly built because an `Agent` OWNS the connection
/// pool: a per-call agent throws its pooled TLS connections away when it drops,
/// so an OCI push of N layers and an upgrade's manifest-then-download pair each
/// paid a full TCP + TLS handshake per request to the same host. `Agent::clone`
/// is documented as allocation-free (the config, pool and resolver are all
/// `Arc`), so handing out a clone costs a refcount bump.
///
/// Keyed by the timeout because that is the only per-call configuration cfgd
/// varies; the five named constants above are the whole key space in practice.
/// A caller that ever needs a genuinely different agent shape (a proxy, a
/// client certificate) must build its own rather than widen this key, since two
/// agents differing in anything but timeout are not interchangeable.
pub fn http_agent(timeout: Duration) -> ureq::Agent {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static AGENTS: OnceLock<Mutex<HashMap<Duration, ureq::Agent>>> = OnceLock::new();
    let agents = AGENTS.get_or_init(|| Mutex::new(HashMap::new()));

    // A poisoned lock means a panic unwound while the map was borrowed. The map
    // holds nothing but agents, so reusing it is safe, and a caller must still
    // get a usable agent — falling back to an unshared build keeps the request
    // working at the cost of this one connection pool.
    let Ok(mut agents) = agents.lock() else {
        return build_agent(timeout);
    };
    agents
        .entry(timeout)
        .or_insert_with(|| build_agent(timeout))
        .clone()
}

/// One agent per timeout also means one COOKIE JAR per timeout: ureq's
/// `cookies` feature is on (it is a default of the version this workspace
/// pins), and an `Agent` stores what a response sets. Sharing it is fine here
/// and stays fine only while both of these hold — a call site that breaks
/// either builds its own agent rather than widening this one:
///
/// - cfgd never authenticates with a cookie. Registry, gateway and GitHub
///   credentials all travel as a per-request `Authorization` header, so no
///   caller's identity can leak into another's request through the jar.
/// - the jar is scoped by domain and path by ureq itself, so an OCI registry
///   cannot read a cookie the device gateway set even though both may share
///   the 300s and 30s agents respectively.
fn build_agent(timeout: Duration) -> ureq::Agent {
    #[cfg(test)]
    record_build(timeout);
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// How many agents this process has actually built for `timeout`.
///
/// Counted per timeout rather than in total so a test can assert on a sentinel
/// timeout of its own while the rest of the suite shares the named ones.
#[cfg(test)]
fn agent_builds(timeout: Duration) -> usize {
    BUILD_COUNTS
        .lock()
        .ok()
        .and_then(|counts| counts.get(&timeout).copied())
        .unwrap_or(0)
}

#[cfg(test)]
fn record_build(timeout: Duration) {
    if let Ok(mut counts) = BUILD_COUNTS.lock() {
        *counts.entry(timeout).or_insert(0) += 1;
    }
}

#[cfg(test)]
static BUILD_COUNTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<Duration, usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

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

    #[test]
    fn one_agent_is_built_per_timeout_however_many_requests_want_it() {
        // A sentinel no production constant uses, so the count belongs to this
        // test whatever else the suite is doing in parallel.
        let sentinel = Duration::from_millis(7717);
        assert_eq!(agent_builds(sentinel), 0);

        // An OCI push of N layers asks per request; a per-call agent would drop
        // its pooled TLS connection each time.
        let agents: Vec<_> = (0..4).map(|_| http_agent(sentinel)).collect();
        assert_eq!(agents.len(), 4);
        assert_eq!(agent_builds(sentinel), 1);

        // A different timeout is a different agent: the two are not
        // interchangeable, so the key really is the timeout.
        let other = Duration::from_millis(7718);
        let _ = http_agent(other);
        assert_eq!(agent_builds(other), 1);
        assert_eq!(agent_builds(sentinel), 1);
    }
}

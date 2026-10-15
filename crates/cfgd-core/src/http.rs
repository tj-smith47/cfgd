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
/// Model latency for agentic tool use is normally a few seconds; 120s is a
/// ceiling for pathological slow networks. Must have *some* timeout — before
/// this constant the request had none at all.
pub const HTTP_AI_TIMEOUT: Duration = Duration::from_secs(120);

/// Outbound webhook notifications (drift alerts, check-in callbacks).
/// Short — the caller doesn't wait on user-visible output; a slow peer
/// should not starve the daemon's notification loop.
pub const HTTP_WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// The `ureq::Agent` this process uses for `timeout`, built at most once per
/// distinct timeout. Exists so every call site that wants a timeout can use one
/// line and agent configuration (user-agent defaults, connection pooling, TLS
/// options) has exactly one place to change.
///
/// Shared rather than freshly built because an `Agent` OWNS the connection
/// pool: a per-call agent throws its pooled TLS connections away when it drops,
/// so an OCI push of N layers and an upgrade's manifest-then-download pair each
/// paid a full TCP + TLS handshake per request to the same host. `Agent::clone`
/// is documented as allocation-free (the config, pool and resolver are all
/// `Arc`), so handing out a clone costs a refcount bump.
///
/// Keyed by the timeout because that is the only per-call configuration cfgd
/// varies; production uses the five named constants above, and a test passes
/// whatever timeout it needs. Every agent is built in this file, so a caller
/// needing a different agent shape (a proxy of its own, a client certificate)
/// gets a second builder here with a key of its own: two agents differing in
/// anything but timeout are not interchangeable.
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
        .proxy(agent_proxy())
        .build()
        .into()
}

/// Every environment variable the HTTP agent reads to choose a proxy, in the
/// order ureq 3.3's `Proxy::try_from_env` asks: the first of the six proxy
/// variables that holds a usable URI wins, and `NO_PROXY` / `no_proxy` name the
/// hosts that bypass it. Nothing else in the agent reads the environment.
pub const PROXY_ENV: [&str; 8] = [
    "ALL_PROXY",
    "all_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// The proxy the host's environment names through [`PROXY_ENV`], or `None`
/// when no variable there names one. The one place cfgd reads a proxy.
pub fn proxy_from_env() -> Option<ureq::Proxy> {
    ureq::Proxy::try_from_env()
}

/// The proxy every agent this process builds routes through.
#[cfg(not(any(test, feature = "test-helpers")))]
fn agent_proxy() -> Option<ureq::Proxy> {
    proxy_from_env()
}

/// A test build routes no agent through a proxy. Agents are shared per process,
/// so one built while a host proxy was exported would carry it into every later
/// test whatever that test's own environment holds, and a mock registry on
/// loopback would then be reached through the proxy or not at all.
#[cfg(any(test, feature = "test-helpers"))]
fn agent_proxy() -> Option<ureq::Proxy> {
    None
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

    /// Each proxy variable [`PROXY_ENV`] lists is one [`proxy_from_env`]
    /// reads, and each `NO_PROXY` spelling exempts the host it names, so the
    /// list `cfgd_bin()` and `isolate_registry_host_env` scrub is the list the
    /// agent reads.
    #[test]
    #[serial_test::serial]
    fn proxy_from_env_reads_every_variable_proxy_env_lists() {
        let _clear = PROXY_ENV.map(crate::test_helpers::EnvVarGuard::unset);
        let (proxies, exemptions) = PROXY_ENV.split_at(6);
        for (n, name) in proxies.iter().enumerate() {
            let port = 4100 + n;
            let _set =
                crate::test_helpers::EnvVarGuard::set(name, &format!("http://127.0.0.1:{port}"));
            let proxy =
                proxy_from_env().unwrap_or_else(|| panic!("{name} set, yet no proxy was read"));
            assert_eq!(
                proxy.port(),
                port as u16,
                "{name} was not the variable read"
            );
        }
        for name in exemptions {
            let _proxy =
                crate::test_helpers::EnvVarGuard::set("HTTP_PROXY", "http://127.0.0.1:4200");
            let _exempt = crate::test_helpers::EnvVarGuard::set(name, "registry.example");
            let proxy = proxy_from_env().expect("HTTP_PROXY set, yet no proxy was read");
            let exempt: ureq::http::Uri = "http://registry.example/v2/".parse().expect("uri");
            let routed: ureq::http::Uri = "http://other.example/v2/".parse().expect("uri");
            assert!(
                proxy.is_no_proxy(&exempt),
                "{name} did not exempt the host it names"
            );
            assert!(
                !proxy.is_no_proxy(&routed),
                "{name} exempted a host it does not name"
            );
        }
    }

    /// An agent a test build makes reaches a loopback server directly while a
    /// proxy that answers nothing is exported, so a developer or a CI runner
    /// behind a proxy runs every mock-registry test the same way.
    #[test]
    #[serial_test::serial]
    fn a_test_built_agent_ignores_an_exported_proxy() {
        let _clear = PROXY_ENV.map(crate::test_helpers::EnvVarGuard::unset);
        let _proxy = crate::test_helpers::EnvVarGuard::set("HTTP_PROXY", "http://127.0.0.1:1");
        let mut server = mockito::Server::new();
        let reached = server
            .mock("GET", "/v2/")
            .with_status(200)
            .expect(1)
            .create();
        let agent = build_agent(Duration::from_millis(7719));
        let response = agent.get(format!("{}/v2/", server.url())).call();
        assert!(
            response.is_ok(),
            "the request went through the proxy: {response:?}"
        );
        reached.assert();
    }

    /// Every `ureq` agent in the workspace, a test's included, is built by
    /// [`build_agent`], so none reads the host's proxy under a test build and
    /// none goes without a timeout in production. A spelling that builds an
    /// agent anywhere else fails here; `http.rs`'s own one is the walk's proof
    /// that it reads code.
    #[test]
    fn every_agent_is_built_in_http_rs() {
        const TELLS: [&str; 12] = [
            "Agent::config_builder(",
            "Agent::new_with_",
            ".new_agent(",
            "ureq::get(",
            "ureq::post(",
            "ureq::put(",
            "ureq::delete(",
            "ureq::head(",
            "ureq::patch(",
            "ureq::options(",
            "ureq::trace(",
            "ureq::run(",
        ];
        let root = crate::test_helpers::workspace_root();
        let own = root.join("crates/cfgd-core/src/http.rs");
        let mut own_builds = 0usize;
        let mut files = 0usize;
        let mut offenders = Vec::new();
        for path in crate::test_helpers::rust_sources_under(&root.join("crates")) {
            files += 1;
            // unfloored-slice-ok: an agent built anywhere, tests included, is the subject.
            let body = crate::test_helpers::walked_file_body(&path);
            let code = crate::test_helpers::blank_non_code(&body);
            for (i, line) in code.lines().enumerate() {
                for tell in TELLS.iter().filter(|tell| line.contains(**tell)) {
                    if path == own {
                        own_builds += 1;
                    } else {
                        offenders.push(format!("{}:{}: {tell}", path.display(), i + 1));
                    }
                }
            }
        }
        assert!(
            files >= 500,
            "the walk read {files} files; it has gone blind"
        );
        assert_eq!(
            own_builds, 1,
            "build_agent is the one place an agent is built"
        );
        assert!(
            offenders.is_empty(),
            "an agent is built outside http.rs; take `http_agent(timeout)`:\n{}",
            offenders.join("\n")
        );
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

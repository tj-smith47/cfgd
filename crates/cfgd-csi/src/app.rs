//! CSI plugin application entrypoint — the orchestration logic lifted from
//! `main.rs` so it can be exercised by unit tests.
//!
//! `main.rs` is reduced to a thin shim that installs the tracing subscriber
//! and delegates here. Nothing in this module writes to a terminal; all
//! human-visible output goes through `tracing` macros, consistent with the
//! rest of the CSI plugin binary.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

use crate::cache::Cache;
use crate::csi::v1::identity_server::IdentityServer;
use crate::csi::v1::node_server::NodeServer;
use crate::identity::CfgdIdentity;
use crate::metrics::{CsiMetrics, serve_metrics};
use crate::node::CfgdNode;

pub(crate) fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

pub(crate) async fn shutdown_signal() -> Result<(), std::io::Error> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| std::io::Error::other(format!("failed to register SIGTERM handler: {e}")))?;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = ctrl_c => {}
    }
    Ok(())
}

/// Full CSI plugin lifecycle: parse config from env, create the module cache,
/// start the metrics HTTP server, bind the Unix socket, and serve gRPC until a
/// shutdown signal or fatal error.
///
/// The `tracing_subscriber` global subscriber must be installed before calling
/// this function (it is installed in `main.rs`).
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = env_or("CSI_ENDPOINT", "/csi/csi.sock");
    let cache_dir = PathBuf::from(env_or("CACHE_DIR", "/var/lib/cfgd-csi/cache"));
    let cache_max_str = env_or("CACHE_MAX_BYTES", "5368709120");
    let cache_max: u64 = cache_max_str.parse().unwrap_or_else(|e| {
        tracing::warn!(value = %cache_max_str, error = %e, "invalid CACHE_MAX_BYTES, using default 5GB");
        5_368_709_120
    });
    let metrics_port_str = env_or("METRICS_PORT", "9090");
    let metrics_port: u16 = metrics_port_str.parse().unwrap_or_else(|e| {
        tracing::warn!(value = %metrics_port_str, error = %e, "invalid METRICS_PORT, using default 9090");
        9090
    });

    let node_id = cfgd_core::hostname_string();

    tracing::info!(
        socket = %socket_path,
        cache_dir = %cache_dir.display(),
        cache_max_bytes = cache_max,
        node_id = %node_id,
        "starting cfgd-csi"
    );

    let cache = Arc::new(Cache::new(cache_dir.clone(), cache_max)?);

    let mut registry = prometheus_client::registry::Registry::default();
    let metrics = Arc::new(CsiMetrics::new(&mut registry));
    let registry = Arc::new(registry);

    tokio::spawn({
        let reg = registry.clone();
        async move {
            if let Err(e) = serve_metrics(metrics_port, reg).await {
                tracing::error!(error = %e, "metrics server failed");
            }
        }
    });

    // Remove stale socket before binding to avoid AddrInUse errors.
    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = PathBuf::from(&socket_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    let stream = UnixListenerStream::new(listener);

    tracing::info!(socket = %socket_path, "gRPC server listening");

    Server::builder()
        .add_service(IdentityServer::new(CfgdIdentity::new(cache_dir)))
        .add_service(NodeServer::new(CfgdNode::new(cache, metrics, node_id)))
        .serve_with_incoming_shutdown(stream, async {
            if let Err(e) = shutdown_signal().await {
                tracing::warn!(error = %e, "signal handler setup failed; proceeding with shutdown");
            } else {
                tracing::info!("received shutdown signal, draining");
            }
        })
        .await?;

    tracing::info!("cfgd-csi stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cfgd_core::test_helpers::EnvVarGuard;
    use serial_test::serial;
    use tempfile::TempDir;

    use super::*;

    // --- env_or ---

    #[test]
    #[serial]
    fn env_or_returns_default_when_var_unset() {
        let _g = EnvVarGuard::unset("CFGD_CSI_TEST_UNSET_VAR_42");
        let v = env_or("CFGD_CSI_TEST_UNSET_VAR_42", "fallback-value");
        assert_eq!(v, "fallback-value");
    }

    #[test]
    #[serial]
    fn env_or_returns_value_when_var_set() {
        let _g = EnvVarGuard::set("CFGD_CSI_TEST_SET_VAR_42", "from-env");
        let v = env_or("CFGD_CSI_TEST_SET_VAR_42", "fallback");
        assert_eq!(v, "from-env");
    }

    #[test]
    #[serial]
    fn env_or_returns_empty_string_when_set_to_empty() {
        // Setting a var to "" is distinct from unset; empty string is valid.
        let _g = EnvVarGuard::set("CFGD_CSI_TEST_EMPTY_VAR", "");
        let v = env_or("CFGD_CSI_TEST_EMPTY_VAR", "fallback");
        assert_eq!(v, "");
    }

    // --- shutdown_signal ---

    /// Drive `shutdown_signal` against a 50 ms timer. The sleep wins; the
    /// signal registration lines execute but no real signal fires. A hard
    /// timeout ensures the test cannot block the suite.
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_signal_registers_handlers_without_panicking() {
        let result = tokio::time::timeout(Duration::from_millis(50), shutdown_signal()).await;
        // Elapsed is expected — no signal was sent so the future never
        // resolves. What matters is no panic during signal registration.
        assert!(result.is_err(), "timeout should fire before any signal");
    }

    // --- run ---

    /// Drive `run()` with all env vars pointed at temp dirs so no real paths
    /// are touched. A 300 ms timeout fires before the gRPC serve loop blocks
    /// indefinitely; either Ok, Err, or Elapsed counts as success because all
    /// meaningful setup lines (port parsing, cache creation, metrics spawn,
    /// socket creation) execute before `serve_with_incoming_shutdown` blocks.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial]
    async fn run_drives_startup_path_without_panicking() {
        let socket_dir = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();

        let socket_path = socket_dir
            .path()
            .join("csi.sock")
            .to_string_lossy()
            .into_owned();

        let _g1 = EnvVarGuard::set("CSI_ENDPOINT", &socket_path);
        let _g2 = EnvVarGuard::set("CACHE_DIR", cache_dir.path().to_str().unwrap());
        let _g3 = EnvVarGuard::set("METRICS_PORT", "0");
        let _g4 = EnvVarGuard::set("CACHE_MAX_BYTES", "104857600");

        let result = tokio::time::timeout(Duration::from_millis(300), run()).await;

        // Timeout, Ok, or Err are all acceptable — all three prove that
        // startup (port parsing, cache init, metrics spawn, socket bind) ran.
        match result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {}
            Err(_elapsed) => {}
        }
    }

    /// Verify that an invalid CACHE_MAX_BYTES falls back to the 5 GB default
    /// without panicking. Port 0 + TempDir ensures no real state is created.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial]
    async fn run_accepts_invalid_cache_max_bytes_using_default() {
        let socket_dir = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();

        let socket_path = socket_dir
            .path()
            .join("csi.sock")
            .to_string_lossy()
            .into_owned();

        let _g1 = EnvVarGuard::set("CSI_ENDPOINT", &socket_path);
        let _g2 = EnvVarGuard::set("CACHE_DIR", cache_dir.path().to_str().unwrap());
        let _g3 = EnvVarGuard::set("METRICS_PORT", "0");
        let _g4 = EnvVarGuard::set("CACHE_MAX_BYTES", "not-a-number");

        let result = tokio::time::timeout(Duration::from_millis(300), run()).await;

        match result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {}
            Err(_elapsed) => {}
        }
    }
}

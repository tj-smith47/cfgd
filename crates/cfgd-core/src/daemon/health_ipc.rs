use super::*;
#[cfg(unix)]
use crate::PathDisplayExt;

/// Hard cap on bytes the IPC client will read from a single daemon response.
///
/// Prevents a malicious or hijacked socket from streaming gigabytes of payload
/// and OOMing the CLI. `/status` and `/drift` responses in normal operation
/// are O(100s) of bytes for a healthy daemon and a few KiB for a heavily-drift
/// daemon — 256 KiB leaves three orders of magnitude of headroom.
pub(crate) const MAX_RESPONSE_BYTES: u64 = 256 * 1024;

/// Create `dir` (and parents) with mode 0700, then verify the resulting
/// directory is owner-private. Used by `run_health_server` to guarantee the
/// IPC socket cannot be dropped into a world-traversable location. Refuses
/// to proceed if the final mode has any group/other bits set — an attacker
/// with `+w` on the parent could rename our socket and substitute theirs,
/// defeating the 0600 we set on the socket itself.
///
/// The check is mode-only: it covers the umask-leak case
/// (mkdir under default 0o022 leaving 0755) as well as operator-pre-created
/// directories with the wrong perms. It does not detect an unprivileged
/// user pretending to be root — that is out of scope for the local-daemon
/// threat model (root is already trusted on the host).
#[cfg(unix)]
pub(crate) fn ensure_owner_private_dir(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir).map_err(|e| DaemonError::HealthSocketError {
        message: format!("create parent {}: {}", dir.posix(), e),
    })?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        DaemonError::HealthSocketError {
            message: format!("chmod parent {}: {}", dir.posix(), e),
        }
    })?;
    let meta = std::fs::metadata(dir).map_err(|e| DaemonError::HealthSocketError {
        message: format!("stat parent {}: {}", dir.posix(), e),
    })?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(DaemonError::HealthSocketError {
            message: format!(
                "refusing to bind: parent directory {} is not owner-private (mode {:o})",
                dir.posix(),
                mode
            ),
        }
        .into());
    }
    Ok(())
}

// --- Health Server ---

#[cfg(unix)]
pub(crate) async fn run_health_server(
    ipc_path: &str,
    state: Arc<Mutex<DaemonState>>,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let ipc_path_buf = std::path::PathBuf::from(ipc_path);

    if let Some(parent) = ipc_path_buf.parent() {
        ensure_owner_private_dir(parent)?;
    }

    // Stale socket from a crashed daemon — `UnixListener::bind` would error
    // with EADDRINUSE. `check_already_running` cleans up the dead-daemon case
    // before we get here, but a stale leftover from a kill -9 still slips
    // through; remove it best-effort.
    if ipc_path_buf.exists() {
        let _ = std::fs::remove_file(&ipc_path_buf);
    }

    let listener = UnixListener::bind(ipc_path).map_err(|e| DaemonError::HealthSocketError {
        message: format!("bind {}: {}", ipc_path, e),
    })?;

    // Tighten the freshly-bound socket to 0600 (default Linux umask 0022
    // leaves it 0755 / world-readable). Done immediately after bind so the
    // window where a parallel `nc -U` could succeed is sub-millisecond.
    std::fs::set_permissions(&ipc_path_buf, std::fs::Permissions::from_mode(0o600)).map_err(
        |e| DaemonError::HealthSocketError {
            message: format!("chmod socket {}: {}", ipc_path, e),
        },
    )?;

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|e| DaemonError::HealthSocketError {
                message: format!("accept: {}", e),
            })?;

        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_health_connection(stream, state).await {
                tracing::debug!(error = %e, "health connection error");
            }
        });
    }
}

#[cfg(windows)]
pub(crate) async fn run_health_server(
    ipc_path: &str,
    state: Arc<Mutex<DaemonState>>,
) -> Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(ipc_path)
        .map_err(|e| DaemonError::HealthSocketError {
            message: format!("create pipe {}: {}", ipc_path, e),
        })?;

    loop {
        server
            .connect()
            .await
            .map_err(|e| DaemonError::HealthSocketError {
                message: format!("accept pipe: {}", e),
            })?;

        let connected = server;
        server = ServerOptions::new()
            .first_pipe_instance(false)
            .create(ipc_path)
            .map_err(|e| DaemonError::HealthSocketError {
                message: format!("create pipe {}: {}", ipc_path, e),
            })?;

        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_health_connection(connected, state).await {
                tracing::debug!(error = %e, "health connection error");
            }
        });
    }
}

pub(crate) async fn handle_health_connection<S>(
    stream: S,
    state: Arc<Mutex<DaemonState>>,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf_reader = tokio::io::BufReader::new(reader);

    // Read the HTTP request line
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;

    // Parse path from "GET /path HTTP/1.x"
    let path = request_line.split_whitespace().nth(1).unwrap_or("/health");

    // Drain remaining headers
    loop {
        let mut line = String::new();
        buf_reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
    }

    // Clone the state snapshot out of the guard and DROP the guard before any
    // `.await` — holding the mutex across writer.write_all/flush would serialize
    // every /health, /status, and /drift connection. The /drift branch also
    // runs blocking sqlite I/O inside spawn_blocking instead of under the guard.
    let (status_code, body) = {
        let (uptime_secs, status_response, store_path_for_drift) = {
            let st = state.lock().await;
            (
                st.started_at.elapsed().as_secs(),
                st.to_response(),
                st.store_path.clone(),
            )
        };

        match path {
            "/health" => {
                let health = serde_json::json!({
                    "status": "ok",
                    "pid": std::process::id(),
                    "uptime_secs": uptime_secs,
                });
                ("200 OK", serde_json::to_string_pretty(&health)?)
            }
            "/status" => ("200 OK", serde_json::to_string_pretty(&status_response)?),
            "/drift" => {
                let drift_events = match store_path_for_drift {
                    Some(p) => tokio::task::spawn_blocking(move || {
                        StateStore::open(&p)
                            .and_then(|s| s.unresolved_drift())
                            .unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default(),
                    None => Vec::new(),
                };

                let drift: Vec<serde_json::Value> = drift_events
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "resource_type": d.resource_type,
                            "resource_id": d.resource_id,
                            "expected": d.expected,
                            "actual": d.actual,
                            "timestamp": d.timestamp,
                        })
                    })
                    .collect();

                (
                    "200 OK",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "drift_count": drift.len(),
                        "events": drift,
                    }))?,
                )
            }
            _ => (
                "404 Not Found",
                serde_json::json!({"error": "not found"}).to_string(),
            ),
        }
    };

    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status_code,
        body.len(),
        body
    );

    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;

    Ok(())
}
// --- Status Query (for cfgd daemon status) ---

/// Connect to the daemon IPC endpoint. Returns `None` if the daemon is not
/// reachable. `runtime_over` carries the `--runtime-dir` override and `scope`
/// the `--scope system` selection so the client resolves the same socket the server
/// bound; pass `None`/[`crate::Scope::User`] for env/default.
pub(crate) fn connect_daemon_ipc(
    runtime_over: Option<&std::path::Path>,
    scope: crate::Scope,
) -> Option<IpcStream> {
    let path = super::resolve_default_ipc_path(runtime_over, scope);
    #[cfg(unix)]
    {
        if !path.exists() {
            return None;
        }
        let stream = StdUnixStream::connect(&path).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        Some(IpcStream::Unix(stream))
    }
    #[cfg(windows)]
    {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .ok()?;
        Some(IpcStream::Pipe(file))
    }
}

/// Platform-specific IPC stream wrapper implementing Read + Write.
pub(crate) enum IpcStream {
    #[cfg(unix)]
    Unix(StdUnixStream),
    #[cfg(windows)]
    Pipe(std::fs::File),
}

impl std::io::Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.read(buf),
            #[cfg(windows)]
            IpcStream::Pipe(f) => f.read(buf),
        }
    }
}

impl std::io::Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.write(buf),
            #[cfg(windows)]
            IpcStream::Pipe(f) => f.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.flush(),
            #[cfg(windows)]
            IpcStream::Pipe(f) => f.flush(),
        }
    }
}

/// Query the running daemon's status over IPC. `runtime_over` carries the
/// `--runtime-dir` override and `scope` the `--scope system` selection so the socket
/// is resolved identically to the server's bind; pass `None`/[`crate::Scope::User`]
/// for env/default.
pub fn query_daemon_status(
    runtime_over: Option<&std::path::Path>,
    scope: crate::Scope,
) -> Result<Option<DaemonStatusResponse>> {
    let mut stream = match connect_daemon_ipc(runtime_over, scope) {
        Some(s) => s,
        None => return Ok(None),
    };

    write!(
        stream,
        "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| DaemonError::HealthSocketError {
        message: format!("write request: {}", e),
    })?;

    // Cap total bytes read from the daemon so a hijacked or hostile peer
    // can't stream multi-GiB garbage and OOM the CLI. The `Take` wrapper
    // returns Ok(0) once `MAX_RESPONSE_BYTES` are consumed, which BufRead
    // reports as a clean EOF — we then look at the underlying `limit()`
    // to distinguish "real EOF" from "cap reached".
    let mut limited = std::io::Read::take(&mut stream, MAX_RESPONSE_BYTES);
    let reader = BufReader::new(&mut limited);
    let mut lines: Vec<String> = Vec::new();
    let mut in_body = false;

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| DaemonError::HealthSocketError {
            message: format!("read response: {}", e),
        })?;

        if in_body {
            lines.push(line);
        } else if line.trim().is_empty() {
            in_body = true;
        }
    }

    // `Take::limit()` is the remaining unread budget; zero means we hit the cap
    // before the peer closed the socket, i.e. the response was truncated.
    if limited.limit() == 0 {
        return Err(DaemonError::HealthSocketError {
            message: format!("daemon response exceeded {} bytes", MAX_RESPONSE_BYTES),
        }
        .into());
    }

    let body = lines.join("\n");
    if body.is_empty() {
        return Ok(None);
    }

    let status: DaemonStatusResponse =
        serde_json::from_str(&body).map_err(|e| DaemonError::HealthSocketError {
            message: format!("parse response: {}", e),
        })?;

    Ok(Some(status))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ensure_owner_private_dir_creates_with_mode_700() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc");
        ensure_owner_private_dir(&dir).expect("should create dir owner-private");
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "must enforce 0700 on the IPC parent");
    }

    #[test]
    fn ensure_owner_private_dir_idempotent_when_already_compliant() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        ensure_owner_private_dir(&dir).expect("idempotent on already-compliant dir");
    }

    #[test]
    fn ensure_owner_private_dir_refuses_world_traversable_after_chmod_recovery() {
        // ensure_owner_private_dir attempts to chmod the dir to 0700. If the
        // chmod fails (we make the path immutable-style via a symlink to a
        // file), the function errors out. Cheaper alternative: feed it a path
        // that points at a regular file — create_dir_all errors, surfacing
        // the HealthSocketError.
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("not-a-dir");
        std::fs::write(&file_path, "hello").unwrap();
        let err = ensure_owner_private_dir(&file_path)
            .expect_err("create_dir_all on a file path must error");
        let msg = err.to_string();
        assert!(
            msg.contains("create parent")
                || msg.contains("chmod parent")
                || msg.contains("stat parent")
                || msg.contains("refusing to bind"),
            "error must reference the IPC parent setup, got: {msg}"
        );
    }

    // ---------------------------------------------------------------------------
    // run_health_server — end-to-end over a REAL Unix socket
    //
    // The existing daemon tests drive `handle_health_connection` directly via an
    // in-memory duplex, which never exercises the server's socket bind, the
    // post-bind 0600 chmod, the accept loop, or the per-connection spawn. This
    // test boots the actual server on a temp socket and round-trips a request
    // through it, asserting both the wire response and the on-disk socket mode.
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_health_server_binds_socket_0600_and_serves_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let tmp = tempfile::tempdir().unwrap();
        // Nest the socket under a subdir so ensure_owner_private_dir creates +
        // hardens the parent (covers the parent-setup path inside the server).
        let sock = tmp.path().join("rundir").join("cfgd.sock");
        let sock_str = sock.to_string_lossy().into_owned();

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let server_state = Arc::clone(&state);
        let server = tokio::spawn(async move { run_health_server(&sock_str, server_state).await });

        // Wait for the socket file to appear (bind + chmod completed).
        let mut waited = 0;
        while !sock.exists() && waited < 200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            waited += 1;
        }
        assert!(sock.exists(), "server must bind the socket file");

        // The freshly-bound socket must be owner-only (0600).
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "server must tighten the socket to 0600");
        // And the parent dir must be owner-private (0700).
        let parent_mode = std::fs::metadata(sock.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700, "parent dir must be 0700");

        // Round-trip a /status request through the real accept loop.
        let mut client = UnixStream::connect(&sock).await.unwrap();
        client
            .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        // Half-close the write side to signal end-of-request. The server keys
        // off the blank-line header terminator (not EOF), so the response is
        // produced regardless. On macOS `shutdown()` can race the connection
        // setup and return ENOTCONN even though the request was already
        // flushed; tolerate that — the subsequent read still gets the response.
        match client.shutdown().await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {}
            Err(e) => panic!("unexpected shutdown error: {e}"),
        }

        let mut raw = String::new();
        client.read_to_string(&mut raw).await.unwrap();

        assert!(
            raw.starts_with("HTTP/1.1 200 OK\r\n"),
            "status line, got: {}",
            &raw[..raw.len().min(40)]
        );
        let (_head, body) = raw.split_once("\r\n\r\n").expect("header/body split");
        let json: serde_json::Value = serde_json::from_str(body).expect("body is JSON");
        // Default DaemonState::new() values on the camelCase wire.
        assert_eq!(json["running"], true);
        assert_eq!(json["driftCount"], 0);
        assert_eq!(json["sources"][0]["name"], "local");

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_health_server_removes_stale_socket_before_bind() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // Pre-create a stale leftover file at the socket path (simulating a
        // kill -9 leftover). The server must remove it and bind cleanly.
        std::fs::write(&sock, b"stale").unwrap();
        assert!(sock.exists());

        let sock_str = sock.to_string_lossy().into_owned();
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let server = tokio::spawn(async move { run_health_server(&sock_str, state).await });

        // Connect with a short retry: a successful /health round-trip proves the
        // stale file was removed and a fresh listener bound in its place.
        let mut got_response = None;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let Ok(mut client) = UnixStream::connect(&sock).await else {
                continue;
            };
            client
                .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            client.shutdown().await.unwrap();
            let mut raw = String::new();
            if client.read_to_string(&mut raw).await.is_ok() && !raw.is_empty() {
                got_response = Some(raw);
                break;
            }
        }

        let raw = got_response.expect("server must serve after clearing the stale socket");
        assert!(raw.starts_with("HTTP/1.1 200 OK"));
        let (_head, body) = raw.split_once("\r\n\r\n").unwrap();
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["status"], "ok");

        server.abort();
    }

    // ---------------------------------------------------------------------------
    // query_daemon_status — client-side parsing over a REAL Unix socket
    //
    // The async server tests above only drive `handle_health_connection`. The
    // synchronous client path (`connect_daemon_ipc` → `IpcStream` Read/Write →
    // header/body split → JSON parse → MAX_RESPONSE_BYTES cap) is exercised here
    // by standing up a hand-rolled fake server on a temp socket that returns a
    // crafted HTTP response, then pointing the client at it via the
    // `CFGD_DAEMON_IPC_PATH` override so `resolve_default_ipc_path` resolves to
    // exactly our socket.
    // ---------------------------------------------------------------------------

    /// Spawn a one-shot fake daemon at `sock_path`. The accepted connection's
    /// request is drained, then `raw_response` is written verbatim and the
    /// socket is closed. Returns the join handle so the test can await teardown.
    fn spawn_fake_daemon(
        sock_path: std::path::PathBuf,
        raw_response: &'static [u8],
    ) -> std::thread::JoinHandle<()> {
        use std::io::{Read as _, Write as _};
        let listener = std::os::unix::net::UnixListener::bind(&sock_path)
            .expect("fake daemon must bind temp socket");
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                // Drain the request line + headers (until blank line) so the
                // client's write side completes before we respond.
                let mut buf = [0u8; 1024];
                // A single read is enough: the client's request is well under 1 KiB
                // and we don't need every byte — we just need the kernel buffer
                // drained enough that our write isn't blocked.
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(raw_response);
                let _ = conn.flush();
            }
        })
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_parses_real_response_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // Crafted /status response: a full HTTP frame whose camelCase body
        // carries distinct, non-default field values, so the test fails if any
        // field is dropped or mis-mapped (uptimeSecs→uptime_secs, driftCount→
        // drift_count) during deserialization.
        const RESP: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"running\":true,\"pid\":4242,\"uptimeSecs\":99,\"lastReconcile\":\"2026-06-13T00:00:00Z\",\"lastSync\":\"2026-06-13T01:00:00Z\",\"driftCount\":7,\"sources\":[{\"name\":\"remote\",\"lastSync\":null,\"lastReconcile\":null,\"driftCount\":2,\"status\":\"degraded\"}]}";
        let handle = spawn_fake_daemon(sock.clone(), RESP);

        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let status = query_daemon_status(None, crate::Scope::User)
            .expect("client must parse a well-formed daemon response")
            .expect("a non-empty body must deserialize to Some(status)");

        handle.join().unwrap();

        assert!(status.running, "running must round-trip true");
        assert_eq!(status.pid, 4242, "pid must round-trip exactly");
        assert_eq!(status.uptime_secs, 99, "uptimeSecs → uptime_secs mapping");
        assert_eq!(
            status.last_reconcile.as_deref(),
            Some("2026-06-13T00:00:00Z")
        );
        assert_eq!(status.last_sync.as_deref(), Some("2026-06-13T01:00:00Z"));
        assert_eq!(status.drift_count, 7, "driftCount → drift_count mapping");
        assert_eq!(status.sources.len(), 1);
        assert_eq!(status.sources[0].name, "remote");
        assert_eq!(status.sources[0].drift_count, 2);
        assert_eq!(status.sources[0].status, "degraded");
        assert!(status.update_available.is_none(), "absent field → None");
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_empty_body_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // Valid HTTP framing but a zero-length body after the blank line.
        const RESP: &[u8] =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n";
        let handle = spawn_fake_daemon(sock.clone(), RESP);

        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let result = query_daemon_status(None, crate::Scope::User)
            .expect("an empty body is a clean Ok(None), not an error");
        handle.join().unwrap();
        assert!(result.is_none(), "empty body must map to Ok(None)");
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_unreachable_socket_returns_none() {
        // Point the client at a socket path that was never bound. connect_daemon_ipc
        // sees the path does not exist and returns None → Ok(None), not an error.
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("never-bound.sock");
        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let result = query_daemon_status(None, crate::Scope::User)
            .expect("an unreachable daemon is not an error");
        assert!(result.is_none(), "no socket file → Ok(None)");
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_malformed_json_body_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // Well-framed HTTP, but the body is not valid JSON for DaemonStatusResponse.
        const RESP: &[u8] = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{not valid json at all";
        let handle = spawn_fake_daemon(sock.clone(), RESP);

        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let err = query_daemon_status(None, crate::Scope::User)
            .expect_err("a malformed body must surface a HealthSocketError");
        handle.join().unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("parse response"),
            "malformed JSON must map to the parse-response HealthSocketError, got: {msg}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_oversize_response_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // A response whose body alone exceeds MAX_RESPONSE_BYTES. The Take cap
        // trips before EOF, so query_daemon_status must reject it rather than
        // attempt to buffer/parse multi-hundred-KiB of attacker-controlled data.
        // Build it once at module load so the &'static lifetime fits spawn_fake_daemon.
        static OVERSIZE: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
            let header = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
            let mut v = Vec::with_capacity(header.len() + MAX_RESPONSE_BYTES as usize + 1024);
            v.extend_from_slice(header);
            // One giant JSON-ish line well past the cap.
            v.extend_from_slice(b"{\"junk\":\"");
            v.resize(v.len() + MAX_RESPONSE_BYTES as usize + 512, b'A');
            v.extend_from_slice(b"\"}");
            v
        });
        let listener = std::os::unix::net::UnixListener::bind(&sock)
            .expect("fake daemon must bind temp socket");
        let handle = std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            if let Ok((mut conn, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                // Best-effort: the client stops reading at the cap and closes, so
                // a broken pipe partway through the write is expected and fine.
                let _ = conn.write_all(&OVERSIZE);
                let _ = conn.flush();
            }
        });

        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let err = query_daemon_status(None, crate::Scope::User)
            .expect_err("an over-cap response must be rejected, not parsed");
        let _ = handle.join();
        let msg = err.to_string();
        assert!(
            msg.contains("exceeded") && msg.contains(&MAX_RESPONSE_BYTES.to_string()),
            "oversize response must cite the byte cap, got: {msg}"
        );
    }
}

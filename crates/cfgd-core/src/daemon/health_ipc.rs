use super::*;

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
        message: format!("create parent {}: {}", dir.display(), e),
    })?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        DaemonError::HealthSocketError {
            message: format!("chmod parent {}: {}", dir.display(), e),
        }
    })?;
    let meta = std::fs::metadata(dir).map_err(|e| DaemonError::HealthSocketError {
        message: format!("stat parent {}: {}", dir.display(), e),
    })?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(DaemonError::HealthSocketError {
            message: format!(
                "refusing to bind: parent directory {} is not owner-private (mode {:o})",
                dir.display(),
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

/// Connect to the daemon IPC endpoint. Returns `None` if the daemon is not reachable.
pub(crate) fn connect_daemon_ipc() -> Option<IpcStream> {
    let path = super::resolve_default_ipc_path();
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

pub fn query_daemon_status() -> Result<Option<DaemonStatusResponse>> {
    let mut stream = match connect_daemon_ipc() {
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

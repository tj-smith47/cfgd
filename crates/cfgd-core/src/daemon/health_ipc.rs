use super::*;

// --- Health Server ---

#[cfg(unix)]
pub(crate) async fn run_health_server(
    ipc_path: &str,
    state: Arc<Mutex<DaemonState>>,
) -> Result<()> {
    let listener = UnixListener::bind(ipc_path).map_err(|e| DaemonError::HealthSocketError {
        message: format!("bind {}: {}", ipc_path, e),
    })?;

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

    let reader = BufReader::new(&mut stream);
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

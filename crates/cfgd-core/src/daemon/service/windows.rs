use super::super::*;

/// Install cfgd as a Windows Service via sc.exe.
///
/// `enable_event_log` controls whether the Windows Event Log subscriber is
/// installed alongside the file appender. When `true`, `--enable-event-log`
/// is baked into the service's binPath and the `cfgd` event source is
/// registered under
/// `HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\cfgd`.
#[cfg(windows)]
pub(crate) fn install_windows_service(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    enable_event_log: bool,
    scope: crate::Scope,
) -> Result<()> {
    // A test with a scoped HOME override must never run real `sc.exe create`
    // against the runner — that mutates the host's service database and
    // collides when a `cfgd` service already exists there. The pure
    // binPath/argv construction is unit-tested separately; skip the
    // side-effecting calls. Mirrors the Unix seam in `start_service`.
    if crate::test_home_override().is_some() {
        return Ok(());
    }
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let config_str = config_abs.display().to_string();
    let config_str = crate::strip_windows_verbatim(&config_str);

    let binary_str = binary.display().to_string();
    let binary_str = crate::strip_windows_verbatim(&binary_str);

    let mut bin_args = format!(
        "\"{}\" daemon service --config \"{}\"",
        binary_str, config_str,
    );
    if let Some(p) = profile {
        bin_args.push_str(&format!(" --profile \"{}\"", p));
    }
    if scope == crate::Scope::System {
        bin_args.push_str(" --scope system");
    }
    if enable_event_log {
        bin_args.push_str(" --enable-event-log");
    }

    // sc.exe requires key= and value as separate arguments
    let output = std::process::Command::new("sc.exe")
        .args([
            "create",
            "cfgd",
            "binPath=",
            &bin_args,
            "start=",
            "auto",
            "DisplayName=",
            "cfgd Configuration Manager",
        ])
        .output()
        .map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("sc.exe create failed: {}", e),
        })?;

    if !output.status.success() {
        return Err(DaemonError::ServiceInstallFailed {
            message: format!(
                "sc.exe create failed: {}",
                crate::stdout_lossy_trimmed(&output)
            ),
        }
        .into());
    }

    // Set service description
    if let Err(e) = std::process::Command::new("sc.exe")
        .args([
            "description",
            "cfgd",
            "Declarative machine configuration management daemon",
        ])
        .output()
    {
        tracing::warn!(error = %e, "failed to set Windows Service description");
    }

    if enable_event_log {
        register_event_source();
    }

    // Start the service
    if let Err(e) = std::process::Command::new("sc.exe")
        .args(["start", "cfgd"])
        .output()
    {
        tracing::warn!(error = %e, "failed to start Windows Service");
    }

    tracing::info!(
        event_log = enable_event_log,
        "installed Windows Service: cfgd"
    );
    Ok(())
}

/// Register the `cfgd` source in the Application Event Log so Event Viewer
/// renders ReportEventW messages cleanly. Best-effort — the service install
/// already succeeded by the time we get here, and a missing source only
/// degrades to "the description for Event ID X cannot be found" warnings in
/// Event Viewer rather than dropping events.
///
/// `EventCreate.exe` ships with every supported Windows version and contains
/// generic message templates (`%1`...`%n`) that just echo the inserted
/// strings — so we get readable Event Viewer rendering without owning a
/// resource DLL.
#[cfg(windows)]
fn register_event_source() {
    let key = r"HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\cfgd";
    let msg_file = r"%SystemRoot%\System32\EventCreate.exe";

    let _ = std::process::Command::new("reg.exe")
        .args([
            "add",
            key,
            "/v",
            "EventMessageFile",
            "/t",
            "REG_EXPAND_SZ",
            "/d",
            msg_file,
            "/f",
        ])
        .output();

    // TypesSupported = 0x7 → ERROR | WARNING | INFORMATION (the three the
    // Layer emits). Higher bits would cover audit success/failure if we ever
    // surface those.
    let _ = std::process::Command::new("reg.exe")
        .args([
            "add",
            key,
            "/v",
            "TypesSupported",
            "/t",
            "REG_DWORD",
            "/d",
            "0x7",
            "/f",
        ])
        .output();
}

/// Uninstall cfgd Windows Service via sc.exe.
#[cfg(windows)]
pub(crate) fn uninstall_windows_service() -> Result<()> {
    // A test with a scoped HOME override must never run real `sc.exe` against
    // the runner — deleting a host `cfgd` service that the test did not create.
    // Mirrors the install-side and Unix-side seam.
    if crate::test_home_override().is_some() {
        return Ok(());
    }
    // Stop service first (best-effort — may not be running)
    if let Err(e) = std::process::Command::new("sc.exe")
        .args(["stop", "cfgd"])
        .output()
    {
        tracing::debug!(error = %e, "sc.exe stop (pre-uninstall)");
    }

    let output = std::process::Command::new("sc.exe")
        .args(["delete", "cfgd"])
        .output()
        .map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("sc.exe delete failed: {}", e),
        })?;

    if !output.status.success() {
        let stdout = crate::stdout_lossy_trimmed(&output);
        // Error 1060 = "The specified service does not exist as an installed service."
        // Treat this as a noop — uninstalling a non-existent service is idempotent.
        if stdout.contains("1060") || stdout.contains("does not exist") {
            tracing::debug!("cfgd Windows Service not found; nothing to remove");
            return Ok(());
        }
        return Err(DaemonError::ServiceInstallFailed {
            message: format!("sc.exe delete failed: {}", stdout),
        }
        .into());
    }

    // Best-effort: drop the Event Log source registration. Idempotent —
    // `reg delete` on a non-existent key returns non-zero but causes no harm.
    let _ = std::process::Command::new("reg.exe")
        .args([
            "delete",
            r"HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\cfgd",
            "/f",
        ])
        .output();

    tracing::info!("removed Windows Service: cfgd");
    Ok(())
}

/// Hooks stored before dispatching to the SCM so `windows_service_main` can retrieve them.
#[cfg(windows)]
static SERVICE_HOOKS: std::sync::OnceLock<Arc<dyn DaemonHooks>> = std::sync::OnceLock::new();

/// Run the daemon as a Windows Service. Called by the SCM (Service Control Manager),
/// not directly by users. `hooks` provides the binary-specific provider implementations.
#[cfg(windows)]
pub fn run_as_windows_service(hooks: Arc<dyn DaemonHooks>) -> Result<()> {
    use windows_service::service_dispatcher;
    // Store hooks before dispatching — ffi_service_main retrieves them via OnceLock.
    let _ = SERVICE_HOOKS.set(hooks);
    service_dispatcher::start("cfgd", ffi_service_main).map_err(|e| DaemonError::ServiceError {
        message: format!("failed to start service dispatcher: {}", e),
    })?;
    Ok(())
}

/// Windows Service mode is only available on Windows.
#[cfg(not(windows))]
pub fn run_as_windows_service(_hooks: Arc<dyn DaemonHooks>) -> Result<()> {
    Err(DaemonError::ServiceError {
        message: "Windows Service mode is only available on Windows".to_string(),
    }
    .into())
}

#[cfg(windows)]
extern "system" fn ffi_service_main(_argc: u32, _argv: *mut *mut u16) {
    if let Err(e) = windows_service_main() {
        tracing::error!(error = %e, "windows service main failed");
    }
}

/// True when this process should mirror tracing events into the Windows
/// Event Log in addition to the file appender. Set by either:
///
/// * the `--enable-event-log` argument that `install_windows_service` bakes
///   into the service binPath when `daemon.windowsEventLog: true`, or
/// * the `CFGD_WINDOWS_EVENT_LOG=1` environment variable (for ad-hoc testing
///   without reinstalling the service).
///
/// The file appender is always installed; this only adds a *second* sink.
#[cfg(windows)]
fn event_log_requested() -> bool {
    if std::env::var("CFGD_WINDOWS_EVENT_LOG")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return true;
    }
    std::env::args().any(|a| a == "--enable-event-log")
}

#[cfg(windows)]
pub(crate) fn init_windows_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let log_dir = std::env::var("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("cfgd"))
        .unwrap_or_else(|_| crate::default_config_dir());

    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("daemon.log");

    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        // No log destination available — don't install a partial subscriber.
        // tracing macros become no-ops and the daemon continues running.
        Err(_) => return,
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .with_target(false);

    let event_log_layer = if event_log_requested() {
        Some(super::windows_eventlog::EventLogLayer)
    } else {
        None
    };

    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(event_log_layer)
        .try_init();
}

#[cfg(windows)]
pub(crate) fn windows_service_main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    use windows_service::service::*;
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

    init_windows_logging();

    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register("cfgd", event_handler)?;

    // Report StartPending while we initialize
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: std::time::Duration::from_secs(10),
        process_id: None,
    })?;

    // Parse config/profile from process args.
    // SCM invokes: cfgd.exe daemon service --config "C:\..." [--profile "name"]
    let args: Vec<String> = std::env::args().collect();
    let mut config_path = crate::default_config_dir().join("config.yaml");
    let mut profile_override: Option<String> = None;
    let mut scope = crate::Scope::User;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" if i + 1 < args.len() => {
                config_path = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--profile" if i + 1 < args.len() => {
                profile_override = Some(args[i + 1].clone());
                i += 2;
            }
            // `install_windows_service` bakes `--scope system` into the binPath
            // for a machine-wide install (mirroring the systemd unit / launchd
            // plist ExecStart), so the SCM-launched daemon resolves the same
            // %ProgramData% roots the install registered against.
            "--scope" if i + 1 < args.len() => {
                scope = crate::Scope::from_system_flag(args[i + 1] == "system");
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Retrieve hooks stored by run_as_windows_service
    let hooks = SERVICE_HOOKS
        .get()
        .ok_or("SERVICE_HOOKS not initialized — run_as_windows_service must be called first")?
        .clone();

    // Create the tokio runtime on the main service thread so we can shut it down gracefully
    let rt = tokio::runtime::Runtime::new()?;
    let printer = Arc::new(crate::output::Printer::new(crate::output::Verbosity::Quiet));

    // Spawn the daemon loop on the runtime
    rt.spawn(async move {
        // Windows service has no CLI runtime override; env/default socket.
        if let Err(e) = run_daemon(config_path, profile_override, None, printer, hooks, scope).await
        {
            tracing::error!(error = %e, "daemon error");
        }
    });

    // Report Running — daemon loop is now active
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: std::time::Duration::default(),
        process_id: None,
    })?;

    // Block until the SCM sends a stop/shutdown signal
    let _ = shutdown_rx.recv();

    // Report StopPending
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StopPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: std::time::Duration::from_secs(5),
        process_id: None,
    })?;

    // Gracefully shut down the runtime, giving in-flight operations time to complete
    rt.shutdown_timeout(std::time::Duration::from_secs(5));

    // Drop the Event Log source handle if one was registered this run.
    // No-op if the file-only sink was used.
    super::windows_eventlog::deregister_source();

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: std::time::Duration::default(),
        process_id: None,
    })?;

    Ok(())
}

use super::super::*;

/// Install cfgd as a Windows Service via sc.exe.
#[cfg(windows)]
pub(crate) fn install_windows_service(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
) -> Result<()> {
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let config_str = config_abs.display().to_string();
    let config_str = config_str.strip_prefix(r"\\?\").unwrap_or(&config_str);

    let binary_str = binary.display().to_string();
    let binary_str = binary_str.strip_prefix(r"\\?\").unwrap_or(&binary_str);

    let mut bin_args = format!(
        "\"{}\" daemon service --config \"{}\"",
        binary_str, config_str,
    );
    if let Some(p) = profile {
        bin_args.push_str(&format!(" --profile \"{}\"", p));
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

    // Start the service
    if let Err(e) = std::process::Command::new("sc.exe")
        .args(["start", "cfgd"])
        .output()
    {
        tracing::warn!(error = %e, "failed to start Windows Service");
    }

    tracing::info!("installed Windows Service: cfgd");
    Ok(())
}

/// Uninstall cfgd Windows Service via sc.exe.
#[cfg(windows)]
pub(crate) fn uninstall_windows_service() -> Result<()> {
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

#[cfg(windows)]
pub(crate) fn init_windows_logging() {
    let log_dir = std::env::var("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("cfgd"))
        .unwrap_or_else(|_| crate::default_config_dir());

    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("daemon.log");

    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .with_target(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    }
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
        if let Err(e) = run_daemon(config_path, profile_override, printer, hooks).await {
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

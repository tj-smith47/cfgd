use super::super::*;

/// Build the service binPath argv — the tokens AFTER the cfgd binary that the
/// SCM re-launches: `daemon service --config <path> [--profile <p>]
/// [--scope system] [--enable-event-log]`.
///
/// Kept pure and platform-independent (not `#[cfg(windows)]`) so the CLI-parse
/// contract can be unit-tested on the Linux CI host, not only on Windows:
/// **every token produced here MUST be accepted by the `daemon service` clap
/// parser.** A flag added here without a matching clap arg makes the
/// SCM-launched process die at argument validation (exit 2) BEFORE the service
/// dispatcher runs, so the service never starts — Windows reports error 1053
/// ("did not respond to the start request in a timely fashion") and `sc query`
/// shows STOPPED. `windows_service_binpath_argv_parses_via_cli` guards this.
pub fn service_binpath_argv(
    config_path: &Path,
    profile: Option<&str>,
    enable_event_log: bool,
    scope: crate::Scope,
    dirs: &DaemonDirOverrides,
) -> Vec<String> {
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    // native-ok: argv tokens the SCM hands back to this host's own binary, so
    // they must carry this host's separators — the one place a cfgd path
    // string is deliberately NOT folded to `/`.
    let config_str = crate::strip_windows_verbatim(&config_abs.display().to_string()).to_string();

    let mut argv = vec![
        "daemon".to_string(),
        "service".to_string(),
        "--config".to_string(),
        config_str,
    ];
    if let Some(p) = profile {
        argv.push("--profile".to_string());
        argv.push(p.to_string());
    }
    if scope == crate::Scope::System {
        argv.push("--scope".to_string());
        argv.push("system".to_string());
    }
    for (flag, dir) in super::service_dir_flags(dirs) {
        argv.push(flag.to_string());
        // native-ok: argv token for this host (see `config_str` above)
        argv.push(crate::strip_windows_verbatim(&dir.display().to_string()).to_string());
    }
    if enable_event_log {
        argv.push("--enable-event-log".to_string());
    }
    argv
}

/// What the SCM-launched `daemon service` process reads back off its argv.
#[cfg(any(windows, test))]
pub(crate) struct ServiceLaunch {
    pub config_path: PathBuf,
    pub profile_override: Option<String>,
    pub scope: crate::Scope,
    pub dirs: DaemonDirOverrides,
}

/// The inverse of [`service_binpath_argv`]: the service settings its tokens
/// carry, read off the process argv the SCM hands back.
///
/// Platform-independent for the same reason as its twin, so a token the
/// install bakes in and the service never reads back fails on the Linux CI
/// host.
#[cfg(any(windows, test))]
pub(crate) fn parse_service_argv(args: &[String]) -> ServiceLaunch {
    let mut config_path: Option<PathBuf> = None;
    let mut profile_override: Option<String> = None;
    let mut scope = crate::Scope::User;
    let mut dirs = DaemonDirOverrides::default();
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1);
        match (args[i].as_str(), value) {
            ("--config", Some(v)) => config_path = Some(PathBuf::from(v)),
            ("--profile", Some(v)) => profile_override = Some(v.clone()),
            ("--scope", Some(v)) => scope = crate::Scope::from_system_flag(v == "system"),
            ("--state-dir", Some(v)) => dirs.state_dir = Some(PathBuf::from(v)),
            ("--runtime-dir", Some(v)) => dirs.runtime_dir = Some(PathBuf::from(v)),
            ("--cache-dir", Some(v)) => dirs.cache_dir = Some(PathBuf::from(v)),
            _ => {
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    ServiceLaunch {
        config_path: config_path
            .unwrap_or_else(|| crate::config::config_document_in(&crate::default_config_dir())),
        profile_override,
        scope,
        dirs,
    }
}

/// The binPath string `sc.exe create` stores: `binary`, then each
/// [`service_binpath_argv`] token, every one quoted so the SCM-launched
/// process's argv split hands back the tokens that were written.
///
/// The binary goes through the same quoter although the split reads argv[0]
/// by a simpler rule (up to the closing quote, no escapes): a Windows path
/// holds no `"` and does not end in `\`, the two cases where the rules differ.
#[cfg(windows)]
pub(crate) fn service_binpath_command_line(binary: &str, argv: &[String]) -> String {
    std::iter::once(binary)
        .chain(argv.iter().map(String::as_str))
        .map(crate::msvc_argv_quoted)
        .collect::<Vec<_>>()
        .join(" ")
}

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
    dirs: &DaemonDirOverrides,
) -> Result<()> {
    // A test with a scoped HOME override must never run real `sc.exe create`
    // against the runner — that mutates the host's service database and
    // collides when a `cfgd` service already exists there. The pure
    // binPath/argv construction is unit-tested separately; skip the
    // side-effecting calls. Mirrors the Unix seam in `start_service`.
    if crate::test_home_override().is_some() {
        return Ok(());
    }
    let binary_str = binary.display().to_string();
    let binary_str = crate::strip_windows_verbatim(&binary_str);

    // Single source of truth for the SCM-launched argv (see service_binpath_argv):
    // rebuild the sc.exe binPath command-line string from those exact tokens so
    // the parsed contract the test pins and the string sc.exe stores never drift.
    let argv = service_binpath_argv(config_path, profile, enable_event_log, scope, dirs);
    let bin_args = service_binpath_command_line(binary_str, &argv);

    // sc.exe requires key= and value as separate arguments
    let output = crate::command_output(std::process::Command::new("sc.exe").args([
        "create",
        "cfgd",
        "binPath=",
        &bin_args,
        "start=",
        "auto",
        "DisplayName=",
        "cfgd Configuration Manager",
    ]))
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
    if let Err(e) = crate::command_output(std::process::Command::new("sc.exe").args([
        "description",
        "cfgd",
        "Declarative machine configuration management daemon",
    ])) {
        tracing::warn!(error = %e, "daemon: failed to set the Windows Service description");
    }

    if enable_event_log {
        register_event_source();
    }

    // Starting is `start_service`'s job (mirroring the Unix seam), so it can
    // poll the SCM and report the TRUE post-start state — an install that also
    // fired `sc start` here would swallow the outcome and force callers to
    // over-claim "started".
    tracing::debug!(
        event_log = enable_event_log,
        "daemon: Windows Service install options"
    );
    tracing::info!("daemon: installed Windows Service cfgd");
    Ok(())
}

/// Start the `cfgd` Windows Service and report whether it actually reached
/// RUNNING. `sc start` returns as soon as the SCM accepts the request, well
/// before the service transitions, so the true state is only knowable by
/// polling `sc query`. A service that dies at startup (e.g. a binPath clap
/// rejects) leaves STOPPED — this returns `Ok(false)` for it rather than
/// claiming success, so `cfgd daemon install` reports the real state.
#[cfg(windows)]
pub(crate) fn start_windows_service() -> Result<bool> {
    let _ = crate::command_output(std::process::Command::new("sc.exe").args(["start", "cfgd"]))
        .map_err(
            |e| tracing::warn!(error = %e, "daemon: failed to issue sc start for the cfgd service"),
        );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if windows_service_is_running() {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                "daemon: Windows Service cfgd did not reach RUNNING within the start timeout"
            );
            return Ok(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// True when `sc query cfgd` reports the service is RUNNING.
#[cfg(windows)]
fn windows_service_is_running() -> bool {
    crate::command_output(std::process::Command::new("sc.exe").args(["query", "cfgd"]))
        .ok()
        .map(|o| crate::stdout_lossy_trimmed(&o).contains("RUNNING"))
        .unwrap_or(false)
}

/// Register the `cfgd` source in the Application Event Log so Event Viewer
/// renders ReportEventW messages cleanly. Best-effort — the service install
/// already succeeded by the time this runs, and a missing source only
/// degrades to "the description for Event ID X cannot be found" warnings in
/// Event Viewer rather than dropping events.
///
/// `EventCreate.exe` ships with every supported Windows version and contains
/// generic message templates (`%1`...`%n`) that just echo the inserted
/// strings — giving readable Event Viewer rendering without owning a
/// resource DLL.
#[cfg(windows)]
fn register_event_source() {
    let key = r"HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\cfgd";
    let msg_file = r"%SystemRoot%\System32\EventCreate.exe";

    let _ = crate::command_output(std::process::Command::new("reg.exe").args([
        "add",
        key,
        "/v",
        "EventMessageFile",
        "/t",
        "REG_EXPAND_SZ",
        "/d",
        msg_file,
        "/f",
    ]));

    // TypesSupported = 0x7 → ERROR | WARNING | INFORMATION (the three the
    // Layer emits). Higher bits would cover audit success/failure if those
    // are ever surfaced.
    let _ = crate::command_output(std::process::Command::new("reg.exe").args([
        "add",
        key,
        "/v",
        "TypesSupported",
        "/t",
        "REG_DWORD",
        "/d",
        "0x7",
        "/f",
    ]));
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
    if let Err(e) =
        crate::command_output(std::process::Command::new("sc.exe").args(["stop", "cfgd"]))
    {
        tracing::debug!(error = %e, "daemon: sc.exe stop (pre-uninstall)");
    }

    let output =
        crate::command_output(std::process::Command::new("sc.exe").args(["delete", "cfgd"]))
            .map_err(|e| DaemonError::ServiceInstallFailed {
                message: format!("sc.exe delete failed: {}", e),
            })?;

    if !output.status.success() {
        let stdout = crate::stdout_lossy_trimmed(&output);
        // Error 1060 = "The specified service does not exist as an installed service."
        // Treat this as a noop — uninstalling a non-existent service is idempotent.
        if stdout.contains("1060") || stdout.contains("does not exist") {
            tracing::debug!("daemon: Windows Service cfgd not found, nothing to remove");
            return Ok(());
        }
        return Err(DaemonError::ServiceInstallFailed {
            message: format!("sc.exe delete failed: {}", stdout),
        }
        .into());
    }

    // Best-effort: drop the Event Log source registration. Idempotent —
    // `reg delete` on a non-existent key returns non-zero but causes no harm.
    let _ = crate::command_output(std::process::Command::new("reg.exe").args([
        "delete",
        r"HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\cfgd",
        "/f",
    ]));

    tracing::info!("daemon: removed Windows Service cfgd");
    Ok(())
}

/// Hooks stored before dispatching to the SCM so `windows_service_main` can retrieve them.
#[cfg(windows)]
static SERVICE_HOOKS: std::sync::OnceLock<Arc<dyn DaemonHooks>> = std::sync::OnceLock::new();

/// Running binary's version, stored beside [`SERVICE_HOOKS`] for the same reason:
/// the SCM re-enters through `ffi_service_main`, which takes no arguments, so
/// everything the daemon loop needs has to cross that boundary through a static.
/// It cannot fall back to this crate's own `CARGO_PKG_VERSION` — cfgd-core
/// versions independently of the binary, and the daemon reports the binary's.
#[cfg(windows)]
static SERVICE_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Run the daemon as a Windows Service. Called by the SCM (Service Control Manager),
/// not directly by users. `hooks` provides the binary-specific provider implementations,
/// and `cfgd_version` the running binary's `env!("CARGO_PKG_VERSION")`.
#[cfg(windows)]
pub fn run_as_windows_service(hooks: Arc<dyn DaemonHooks>, cfgd_version: &str) -> Result<()> {
    use windows_service::service_dispatcher;
    // Store hooks before dispatching — ffi_service_main retrieves them via OnceLock.
    let _ = SERVICE_HOOKS.set(hooks);
    let _ = SERVICE_VERSION.set(cfgd_version.to_string());
    service_dispatcher::start("cfgd", ffi_service_main).map_err(|e| DaemonError::ServiceError {
        message: format!("failed to start service dispatcher: {}", e),
    })?;
    Ok(())
}

/// Windows Service mode is only available on Windows.
#[cfg(not(windows))]
pub fn run_as_windows_service(_hooks: Arc<dyn DaemonHooks>, _cfgd_version: &str) -> Result<()> {
    Err(DaemonError::ServiceError {
        message: "Windows Service mode is only available on Windows".to_string(),
    }
    .into())
}

#[cfg(windows)]
extern "system" fn ffi_service_main(_argc: u32, _argv: *mut *mut u16) {
    if let Err(e) = windows_service_main() {
        tracing::error!(error = %e, "daemon: Windows Service main failed");
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
    if std::env::var(crate::CFGD_WINDOWS_EVENT_LOG_ENV)
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
        // long-line-ok: a hatch is read off its own line, so it cannot wrap
        // unfolded-writer-ok: a log FILE the service writes under its own state dir, never a terminal
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

    // Report StartPending during initialization
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: std::time::Duration::from_secs(10),
        process_id: None,
    })?;

    let ServiceLaunch {
        config_path,
        profile_override,
        scope,
        dirs,
    } = parse_service_argv(&std::env::args().collect::<Vec<_>>());

    // Retrieve hooks stored by run_as_windows_service
    let hooks = SERVICE_HOOKS
        .get()
        .ok_or("SERVICE_HOOKS not initialized — run_as_windows_service must be called first")?
        .clone();
    let cfgd_version = SERVICE_VERSION
        .get()
        .ok_or("SERVICE_VERSION not initialized — run_as_windows_service must be called first")?
        .clone();

    // Create the tokio runtime on the main service thread so it can shut down gracefully
    let rt = tokio::runtime::Runtime::new()?;
    let printer = Arc::new(crate::output::Printer::silent());

    // Spawn the daemon loop on the runtime
    rt.spawn(async move {
        if let Err(e) = run_daemon(
            config_path,
            profile_override,
            dirs,
            printer,
            hooks,
            scope,
            // `install_windows_service` bakes no `--update-policy` into the
            // binPath, so a service's posture is `spec.update.policy`, re-read
            // on every tick.
            None,
            &cfgd_version,
        )
        .await
        {
            tracing::error!(error = %e, "daemon: run failed");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every setting the install bakes into the binPath reaches the service
    /// that reads it back: config, profile, scope and all three directories.
    #[test]
    fn the_service_reads_back_every_setting_its_install_baked_in() {
        let config = PathBuf::from("/srv/cfgd/cfgd.yaml");
        let dirs = DaemonDirOverrides {
            state_dir: Some(PathBuf::from("/srv/cfgd/state")),
            runtime_dir: Some(PathBuf::from("/srv/cfgd/run")),
            cache_dir: Some(PathBuf::from("/srv/cfgd/cache")),
        };
        let argv = service_binpath_argv(&config, Some("srv"), true, crate::Scope::System, &dirs);
        let args: Vec<String> = std::iter::once("cfgd".to_string()).chain(argv).collect();
        let launch = parse_service_argv(&args);
        assert_eq!(launch.config_path, config, "--config");
        assert_eq!(launch.profile_override.as_deref(), Some("srv"), "--profile");
        assert_eq!(launch.scope, crate::Scope::System, "--scope system");
        assert_eq!(launch.dirs.state_dir, dirs.state_dir, "--state-dir");
        assert_eq!(launch.dirs.runtime_dir, dirs.runtime_dir, "--runtime-dir");
        assert_eq!(
            launch.dirs.cache_dir, dirs.cache_dir,
            "--cache-dir: the service composes sources from the cache its install named"
        );
    }

    /// The whole producer-to-consumer path on the OS that runs it: the binPath
    /// string `sc.exe` stores, split by the same `CommandLineToArgvW` rules the
    /// SCM-launched process's runtime applies, read back by the service. The
    /// values carry a trailing `\`, spaces and a `"`, the cases a bare wrap in
    /// quotes loses.
    #[cfg(windows)]
    #[test]
    fn the_service_reads_back_every_setting_through_the_binpath_string() {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

        let config = PathBuf::from(r"C:\cfgd conf\cfgd.yaml");
        let dirs = DaemonDirOverrides {
            state_dir: Some(PathBuf::from(r"C:\cfgd\state\")),
            runtime_dir: Some(PathBuf::from(r"C:\cfgd run\")),
            cache_dir: Some(PathBuf::from(r"C:\cfgd cache\\")),
        };
        let profile = r#"my "srv" \"#;
        let argv = service_binpath_argv(&config, Some(profile), true, crate::Scope::System, &dirs);
        let line = service_binpath_command_line(r"C:\Program Files\cfgd\cfgd.exe", &argv);

        let wide: Vec<u16> = line.encode_utf16().chain(std::iter::once(0)).collect();
        let mut count = 0i32;
        // SAFETY: `wide` is NUL-terminated and outlives the call; the returned
        // block is read within `count` and freed once with `LocalFree`.
        let split: Vec<String> = unsafe {
            let raw = CommandLineToArgvW(wide.as_ptr(), &mut count);
            assert!(!raw.is_null(), "CommandLineToArgvW failed on {line}");
            let args = (0..count as usize)
                .map(|i| {
                    let p = *raw.add(i);
                    let len = (0..).take_while(|&n| *p.add(n) != 0).count();
                    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
                })
                .collect();
            LocalFree(raw.cast());
            args
        };

        assert_eq!(
            split[0], r"C:\Program Files\cfgd\cfgd.exe",
            "the binary is argv[0]: {line}"
        );
        assert_eq!(
            split[1..],
            argv[..],
            "every token splits back as written: {line}"
        );
        let launch = parse_service_argv(&split);
        assert_eq!(launch.config_path, config, "--config");
        assert_eq!(
            launch.profile_override.as_deref(),
            Some(profile),
            "--profile"
        );
        assert_eq!(launch.scope, crate::Scope::System, "--scope system");
        assert_eq!(launch.dirs.state_dir, dirs.state_dir, "--state-dir");
        assert_eq!(launch.dirs.runtime_dir, dirs.runtime_dir, "--runtime-dir");
        assert_eq!(launch.dirs.cache_dir, dirs.cache_dir, "--cache-dir");
    }
}

use super::*;

#[cfg(unix)]
mod launchd;
#[cfg(unix)]
mod systemd;
mod windows;
#[cfg(windows)]
mod windows_eventlog;

#[cfg(unix)]
pub(crate) use launchd::*;
#[cfg(unix)]
pub(crate) use systemd::*;
#[cfg(windows)]
pub(crate) use windows::*;

pub use windows::run_as_windows_service;

// --- Service Management ---
// launchd on macOS, systemd on Linux, Windows Service on Windows.

pub fn install_service(config_path: &Path, profile: Option<&str>) -> Result<()> {
    let cfgd_binary = std::env::current_exe().map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("cannot determine binary path: {}", e),
    })?;
    #[cfg(windows)]
    {
        let enable_event_log = read_event_log_flag(config_path);
        install_windows_service(&cfgd_binary, config_path, profile, enable_event_log)
    }
    #[cfg(unix)]
    {
        if cfg!(target_os = "macos") {
            install_launchd_service(&cfgd_binary, config_path, profile)
        } else {
            install_systemd_service(&cfgd_binary, config_path, profile)
        }
    }
}

/// Best-effort lookup for `daemon.windowsEventLog` in the user config.
/// Failure to read or parse the config silently defaults to `false` —
/// `cfgd daemon install` keeps working when the config has not yet been
/// scaffolded, just without Event Log mirroring.
#[cfg(windows)]
fn read_event_log_flag(config_path: &Path) -> bool {
    use crate::config;
    config::load_config(config_path)
        .ok()
        .and_then(|cfg| cfg.spec.daemon)
        .map(|d| d.windows_event_log)
        .unwrap_or(false)
}

/// Enable and start the just-installed service so the daemon runs immediately,
/// not only after the next login or reboot.
///
/// On Linux this enables and starts the systemd user unit; on macOS it
/// bootstraps the LaunchAgent into the user GUI domain. Both degrade to a
/// warning plus an actionable hint (lingering / GUI-login) when the session
/// cannot host the service, so a headless `cfgd init --install-daemon` reports
/// the gap instead of silently leaving the daemon down. On Windows this is a
/// no-op: `install_service` already starts the Windows Service.
///
/// Returns `Ok(true)` when the daemon is actually running after this call,
/// `Ok(false)` when it was installed but could not be started now. Callers use
/// the boolean to report a truthful `started` state rather than over-claiming.
#[cfg(any(unix, windows))]
pub fn start_service(printer: &crate::output::Printer) -> Result<bool> {
    #[cfg(windows)]
    {
        let _ = printer;
        // `install_windows_service` already issues `sc start`, so the service
        // is running by the time this is reached.
        Ok(true)
    }
    #[cfg(unix)]
    {
        // A test that scoped a HOME override never wants a real
        // `systemctl --user` / `launchctl` against the runner — that would
        // mutate the host's session manager. The pure argv builders
        // (`systemd_start_argv` / `launchd_*_argv`) carry the unit-test
        // coverage for command construction; skip the side-effecting call here
        // and report not-started, since nothing was actually started.
        if crate::test_home_override().is_some() {
            return Ok(false);
        }
        if cfg!(target_os = "macos") {
            start_launchd_service(printer)
        } else {
            start_systemd_service(printer)
        }
    }
}

pub fn uninstall_service() -> Result<()> {
    #[cfg(windows)]
    {
        uninstall_windows_service()
    }
    #[cfg(unix)]
    {
        if cfg!(target_os = "macos") {
            uninstall_launchd_service()
        } else {
            uninstall_systemd_service()
        }
    }
}

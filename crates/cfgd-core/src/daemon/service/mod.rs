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

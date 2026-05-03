use super::*;

#[cfg(unix)]
mod launchd;
#[cfg(unix)]
mod systemd;
mod windows;

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
        install_windows_service(&cfgd_binary, config_path, profile)
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

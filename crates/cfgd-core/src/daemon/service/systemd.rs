use super::super::*;

/// Generate systemd unit file content for the daemon service.
#[cfg(unix)]
pub(crate) fn generate_systemd_unit(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
) -> String {
    let mut exec_start = format!(
        "{} --config {} daemon",
        binary.display(),
        config_path.display()
    );
    if let Some(p) = profile {
        exec_start = format!(
            "{} --config {} --profile {} daemon",
            binary.display(),
            config_path.display(),
            p
        );
    }

    format!(
        r#"[Unit]
Description=cfgd configuration daemon
After=network.target

[Service]
Type=simple
ExecStart={exec_start}
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target"#
    )
}

#[cfg(unix)]
pub(crate) fn install_systemd_service(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
) -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let unit_dir = home.join(SYSTEMD_USER_DIR);
    std::fs::create_dir_all(&unit_dir).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("create systemd user dir: {}", e),
    })?;

    let unit_path = unit_dir.join("cfgd.service");
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());

    let unit = generate_systemd_unit(binary, &config_abs, profile);

    crate::atomic_write_str(&unit_path, &unit).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("write unit file: {}", e),
    })?;

    tracing::info!(path = %unit_path.display(), "installed systemd user service");
    Ok(())
}

#[cfg(unix)]
pub(crate) fn uninstall_systemd_service() -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let unit_path = home.join(SYSTEMD_USER_DIR).join("cfgd.service");

    if unit_path.exists() {
        std::fs::remove_file(&unit_path).map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("remove unit file: {}", e),
        })?;
        tracing::info!(path = %unit_path.display(), "removed systemd user service");
    }

    Ok(())
}

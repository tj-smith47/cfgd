use super::super::*;

/// Generate systemd unit file content for the daemon service.
#[cfg(unix)]
pub(crate) fn generate_systemd_unit(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
) -> String {
    let mut args = vec![
        binary.display().to_string(),
        "--config".to_string(),
        config_path.display().to_string(),
    ];
    if let Some(p) = profile {
        args.push("--profile".to_string());
        args.push(p.to_string());
    }
    args.push("--quiet".to_string());
    args.push("daemon".to_string());
    let exec_start = args.join(" ");

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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn generate_systemd_unit_includes_binary_config_and_quiet_daemon_flags() {
        let unit = generate_systemd_unit(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &PathBuf::from("/etc/cfgd/config.yaml"),
            None,
        );
        assert!(unit.contains("Description=cfgd configuration daemon"));
        assert!(unit.contains("After=network.target"));
        assert!(unit.contains(
            "ExecStart=/usr/local/bin/cfgd --config /etc/cfgd/config.yaml --quiet daemon"
        ));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(!unit.contains("--profile"));
    }

    #[test]
    fn generate_systemd_unit_emits_profile_args_when_set() {
        let unit = generate_systemd_unit(
            &PathBuf::from("/cfgd"),
            &PathBuf::from("/c.yaml"),
            Some("workstation"),
        );
        assert!(
            unit.contains("ExecStart=/cfgd --config /c.yaml --profile workstation --quiet daemon")
        );
    }
}

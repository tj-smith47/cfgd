use super::super::*;

/// Generate launchd plist content for the daemon service.
#[cfg(unix)]
pub(crate) fn generate_launchd_plist(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    home: &Path,
) -> String {
    let mut args = vec![
        format!("<string>{}</string>", binary.display()),
        "<string>--config</string>".to_string(),
        format!("<string>{}</string>", config_path.display()),
        "<string>--quiet</string>".to_string(),
        "<string>daemon</string>".to_string(),
    ];

    if let Some(p) = profile {
        args.push("<string>--profile</string>".to_string());
        args.push(format!("<string>{}</string>", p));
    }

    let args_xml = args.join("\n            ");
    let label = LAUNCHD_LABEL;
    let home_display = home.display();

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
            {args_xml}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home_display}/Library/Logs/cfgd.log</string>
    <key>StandardErrorPath</key>
    <string>{home_display}/Library/Logs/cfgd.err</string>
</dict>
</plist>"#
    )
}

#[cfg(unix)]
pub(crate) fn install_launchd_service(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
) -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let plist_dir = home.join(LAUNCHD_AGENTS_DIR);
    std::fs::create_dir_all(&plist_dir).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("create LaunchAgents dir: {}", e),
    })?;

    let plist_path = plist_dir.join(format!("{}.plist", LAUNCHD_LABEL));
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());

    let plist = generate_launchd_plist(binary, &config_abs, profile, &home);

    crate::atomic_write_str(&plist_path, &plist).map_err(|e| {
        DaemonError::ServiceInstallFailed {
            message: format!("write plist: {}", e),
        }
    })?;

    tracing::info!(path = %plist_path.display(), "installed launchd service");
    Ok(())
}

#[cfg(unix)]
pub(crate) fn uninstall_launchd_service() -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let plist_path = home
        .join(LAUNCHD_AGENTS_DIR)
        .join(format!("{}.plist", LAUNCHD_LABEL));

    if plist_path.exists() {
        std::fs::remove_file(&plist_path).map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("remove plist: {}", e),
        })?;
        tracing::info!(path = %plist_path.display(), "removed launchd service");
    }

    Ok(())
}

use super::super::*;
use crate::PathDisplayExt;

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

    tracing::info!(path = %unit_path.posix(), "installed systemd user service");
    Ok(())
}

/// Build the `systemctl --user` argv vectors that reload the unit cache and
/// enable+start the service. Split out so the command construction is testable
/// without a running user systemd bus.
#[cfg(unix)]
pub(crate) fn systemd_start_argv() -> [Vec<&'static str>; 2] {
    [
        vec!["--user", "daemon-reload"],
        vec!["--user", "enable", "--now", "cfgd.service"],
    ]
}

/// Enable and start the just-installed systemd user service so the daemon runs
/// immediately rather than only after the next login. Best-effort: when the
/// user has no session systemd (no lingering, no active login session) the
/// enable-now cannot take effect, so the failure is surfaced as a warning plus
/// an actionable hint rather than aborting the calling init flow.
///
/// Returns `Ok(true)` when the service was enabled and started, `Ok(false)`
/// when it was installed but could not be started now (the caller reports the
/// real state instead of over-claiming `started`).
#[cfg(unix)]
pub(crate) fn start_systemd_service(printer: &Printer) -> Result<bool> {
    if !crate::command_available("systemctl") {
        printer.status_simple(
            Role::Warn,
            "systemctl not found — daemon installed but not started",
        );
        printer.hint("Start it later with: systemctl --user enable --now cfgd.service");
        return Ok(false);
    }

    for args in systemd_start_argv() {
        let mut cmd = std::process::Command::new("systemctl");
        cmd.args(&args);
        match crate::command_output_with_timeout(&mut cmd, crate::COMMAND_TIMEOUT) {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let detail = crate::stderr_lossy_trimmed(&output);
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "systemctl {} failed: {}",
                        args.join(" "),
                        crate::output::collapse_to_subject_line(&detail)
                    ),
                );
                printer.hint(
                    "If you have no active login session, enable lingering: loginctl enable-linger $USER",
                );
                return Ok(false);
            }
            Err(e) => {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "systemctl {} failed: {}",
                        args.join(" "),
                        crate::output::collapse_to_subject_line(&e)
                    ),
                );
                printer.hint(
                    "If you have no active login session, enable lingering: loginctl enable-linger $USER",
                );
                return Ok(false);
            }
        }
    }

    printer.status_simple(Role::Ok, "Daemon service started");
    Ok(true)
}

/// Build the `systemctl --user` argv vectors that uninstall mirrors against
/// install: stop+disable the unit, then (after the file is gone) reload the unit
/// cache. Returned together so command construction is testable without a
/// running user systemd bus; the caller invokes them at the correct points
/// around the file removal (disable BEFORE the rm, daemon-reload AFTER).
#[cfg(unix)]
pub(crate) fn systemd_stop_argv() -> [Vec<&'static str>; 2] {
    [
        vec!["--user", "disable", "--now", "cfgd.service"],
        vec!["--user", "daemon-reload"],
    ]
}

/// Stop and disable the running systemd user unit so `uninstall` leaves no
/// orphan daemon process behind — the inverse of `start_systemd_service`.
/// Best-effort: a missing `systemctl` or a session with no user systemd is
/// surfaced as a warning plus an actionable hint rather than aborting the
/// uninstall flow. Must run while the unit file still exists (before removal).
#[cfg(unix)]
pub(crate) fn stop_systemd_service(printer: &Printer) {
    // A test with a scoped HOME override must never run a real
    // `systemctl --user` against the runner's session manager. The argv builder
    // (`systemd_stop_argv`) carries command-construction coverage; skip the
    // side-effecting call here.
    if crate::test_home_override().is_some() {
        return;
    }
    if !crate::command_available("systemctl") {
        printer.status_simple(
            Role::Warn,
            "systemctl not found — unit file removed but daemon may still be running",
        );
        printer.hint("Stop it manually with: systemctl --user disable --now cfgd.service");
        return;
    }

    let [disable, _reload] = systemd_stop_argv();
    let mut cmd = std::process::Command::new("systemctl");
    cmd.args(&disable);
    match crate::command_output_with_timeout(&mut cmd, crate::COMMAND_TIMEOUT) {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let detail = crate::stderr_lossy_trimmed(&output);
            printer.status_simple(
                Role::Warn,
                format!(
                    "systemctl {} failed: {}",
                    disable.join(" "),
                    crate::output::collapse_to_subject_line(&detail)
                ),
            );
        }
        Err(e) => {
            printer.status_simple(
                Role::Warn,
                format!(
                    "systemctl {} failed: {}",
                    disable.join(" "),
                    crate::output::collapse_to_subject_line(&e)
                ),
            );
        }
    }
}

#[cfg(unix)]
pub(crate) fn uninstall_systemd_service(printer: &Printer) -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let unit_path = home.join(SYSTEMD_USER_DIR).join("cfgd.service");

    // Stop+disable BEFORE removing the unit file so systemd can act on a unit it
    // still knows about — otherwise the running daemon is orphaned.
    stop_systemd_service(printer);

    if unit_path.exists() {
        std::fs::remove_file(&unit_path).map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("remove unit file: {}", e),
        })?;
        tracing::info!(path = %unit_path.posix(), "removed systemd user service");
    }

    // Reload AFTER removal so systemd drops the now-deleted unit from its view.
    if crate::test_home_override().is_none() && crate::command_available("systemctl") {
        let [_disable, reload] = systemd_stop_argv();
        let mut cmd = std::process::Command::new("systemctl");
        cmd.args(&reload);
        if let Ok(output) = crate::command_output_with_timeout(&mut cmd, crate::COMMAND_TIMEOUT)
            && !output.status.success()
        {
            let detail = crate::stderr_lossy_trimmed(&output);
            printer.status_simple(
                Role::Warn,
                format!(
                    "systemctl {} failed: {}",
                    reload.join(" "),
                    crate::output::collapse_to_subject_line(&detail)
                ),
            );
        }
    }

    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    #[serial_test::serial]
    fn install_then_uninstall_systemd_service_writes_and_removes_unit() {
        let home_dir = TempDir::new().expect("tempdir");
        let _home_g = crate::with_test_home_guard(home_dir.path());

        let config = home_dir.path().join("cfgd.yaml");
        std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\n").expect("write config");

        install_systemd_service(&PathBuf::from("/usr/local/bin/cfgd"), &config, Some("ws"))
            .expect("install");

        let unit_path = home_dir.path().join(SYSTEMD_USER_DIR).join("cfgd.service");
        let unit = std::fs::read_to_string(&unit_path).expect("read unit");
        assert!(unit.contains("ExecStart=/usr/local/bin/cfgd"));
        assert!(unit.contains("--profile ws"));

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        uninstall_systemd_service(&printer).expect("uninstall");
        assert!(!unit_path.exists());

        uninstall_systemd_service(&printer).expect("idempotent uninstall");
    }

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
    fn systemd_start_argv_reloads_then_enables_now() {
        let [reload, enable] = systemd_start_argv();
        assert_eq!(reload, ["--user", "daemon-reload"]);
        assert_eq!(enable, ["--user", "enable", "--now", "cfgd.service"]);
    }

    #[test]
    fn systemd_stop_argv_disables_now_then_reloads() {
        let [disable, reload] = systemd_stop_argv();
        assert_eq!(disable, ["--user", "disable", "--now", "cfgd.service"]);
        assert_eq!(reload, ["--user", "daemon-reload"]);
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

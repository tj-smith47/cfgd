use super::*;

pub(super) fn cmd_daemon(
    cli: &Cli,
    printer: &Printer,
    command: Option<&DaemonCommand>,
) -> anyhow::Result<()> {
    match command {
        Some(DaemonCommand::Status) => return cmd_daemon_status(printer),
        Some(DaemonCommand::Install) => return cmd_daemon_install(cli, printer),
        Some(DaemonCommand::Uninstall) => return cmd_daemon_uninstall(printer),
        Some(DaemonCommand::Service) => return cmd_daemon_service(),
        Some(DaemonCommand::Run) | None => {}
    }

    // Run daemon in foreground
    let config_path = std::fs::canonicalize(&cli.config).unwrap_or_else(|_| cli.config.clone());
    let profile_override = cli.profile.clone();
    let printer = std::sync::Arc::new(cfgd_core::output::Printer::new(if cli.quiet {
        cfgd_core::output::Verbosity::Quiet
    } else if cli.verbose > 0 {
        cfgd_core::output::Verbosity::Verbose
    } else {
        cfgd_core::output::Verbosity::Normal
    }));

    let hooks: std::sync::Arc<dyn cfgd_core::daemon::DaemonHooks> =
        std::sync::Arc::new(WorkstationDaemonHooks);
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async {
        cfgd_core::daemon::run_daemon(config_path, profile_override, printer, hooks).await
    });
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    result?;

    Ok(())
}

pub(super) fn cmd_daemon_status(printer: &Printer) -> anyhow::Result<()> {
    let status = cfgd_core::daemon::query_daemon_status()?;

    if printer.is_structured() {
        match &status {
            Some(s) => printer.write_structured(s),
            None => printer.write_structured(&cfgd_core::daemon::DaemonStatusResponse {
                running: false,
                pid: 0,
                uptime_secs: 0,
                last_reconcile: None,
                last_sync: None,
                drift_count: 0,
                sources: vec![],
                update_available: None,
                module_reconcile: vec![],
            }),
        };
        return Ok(());
    }

    printer.header("Daemon Status");

    match status {
        Some(status) => {
            printer.success("Daemon is running");
            printer.key_value("PID", &status.pid.to_string());
            printer.key_value("Uptime", &format!("{}s", status.uptime_secs));
            printer.key_value("Drift count", &status.drift_count.to_string());

            if let Some(ref last) = status.last_reconcile {
                printer.key_value("Last reconcile", last);
            }
            if let Some(ref last) = status.last_sync {
                printer.key_value("Last sync", last);
            }

            if let Some(ref version) = status.update_available {
                printer.newline();
                printer.warning(&format!(
                    "Update available: {} — run 'cfgd upgrade' to install",
                    version
                ));
            }

            printer.newline();
            printer.subheader("Sources");
            printer.table(
                &["Name", "Status", "Drift", "Last Sync"],
                &status
                    .sources
                    .iter()
                    .map(|s| {
                        vec![
                            s.name.clone(),
                            s.status.clone(),
                            s.drift_count.to_string(),
                            s.last_sync.clone().unwrap_or_else(|| "-".to_string()),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
        }
        None => {
            printer.warning("Daemon is not running");
            printer.info("Start with: cfgd daemon");
            printer.info("Install as service: cfgd daemon install");
        }
    }

    Ok(())
}

pub(super) fn cmd_daemon_install(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    printer.header("Install Daemon Service");

    cfgd_core::daemon::install_service(&cli.config, cli.profile.as_deref())?;

    #[cfg(windows)]
    {
        printer.success("cfgd service installed and started");
        printer.info("The service will start automatically on boot");
        printer.info("Logs: %LOCALAPPDATA%\\cfgd\\daemon.log");
    }
    #[cfg(unix)]
    {
        print_daemon_install_success(printer);
    }

    Ok(())
}

pub(super) fn cmd_daemon_uninstall(printer: &Printer) -> anyhow::Result<()> {
    printer.header("Uninstall Daemon Service");

    if cfg!(windows) {
        printer.info("Stopping and removing Windows Service: cfgd");
    } else if cfg!(target_os = "macos") {
        printer.info("Unloading: launchctl unload ~/Library/LaunchAgents/com.cfgd.daemon.plist");
    } else {
        printer.info("Stopping: systemctl --user disable --now cfgd.service");
    }

    cfgd_core::daemon::uninstall_service()?;
    printer.success("Daemon service removed");

    Ok(())
}

pub(super) fn cmd_daemon_service() -> anyhow::Result<()> {
    let hooks: std::sync::Arc<dyn cfgd_core::daemon::DaemonHooks> =
        std::sync::Arc::new(WorkstationDaemonHooks);
    cfgd_core::daemon::run_as_windows_service(hooks)?;
    Ok(())
}

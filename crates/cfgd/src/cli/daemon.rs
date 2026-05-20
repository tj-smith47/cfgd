use super::*;
use cfgd_core::output::renderer::Table;
use cfgd_core::output::{Doc, Printer, Role};
use serde::Serialize;

/// JSON payload for `cfgd daemon install`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInstallOutput {
    pub platform: String,
    pub service: String,
    pub path: String,
    pub started: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows_event_log: Option<bool>,
}

/// JSON payload for `cfgd daemon uninstall`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonUninstallOutput {
    pub platform: String,
    pub service: String,
    pub removed: bool,
}

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

    let config_path = std::fs::canonicalize(&cli.config).unwrap_or_else(|_| cli.config.clone());
    let profile_override = cli.profile.clone();
    let daemon_printer = std::sync::Arc::new(cfgd_core::output::Printer::new(if cli.quiet {
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
        cfgd_core::daemon::run_daemon(config_path, profile_override, daemon_printer, hooks).await
    });
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    if let Err(e) = result {
        printer.emit(cfgd_core::output::error_doc(
            "cfgd",
            "runtime_failed",
            format!("Daemon reconcile loop failed: {}", e),
            serde_json::Value::Null,
        ));
        return Err(e.into());
    }

    Ok(())
}

pub fn cmd_daemon_status(printer: &Printer) -> anyhow::Result<()> {
    let status = match cfgd_core::daemon::query_daemon_status() {
        Ok(s) => s,
        Err(e) => {
            printer.emit(cfgd_core::output::error_doc(
                "cfgd",
                "status_unavailable",
                format!("Failed to query daemon status: {}", e),
                serde_json::Value::Null,
            ));
            return Err(e.into());
        }
    };
    printer.emit(build_daemon_status_doc(status.as_ref()));
    Ok(())
}

fn placeholder_status() -> cfgd_core::daemon::DaemonStatusResponse {
    cfgd_core::daemon::DaemonStatusResponse {
        running: false,
        pid: 0,
        uptime_secs: 0,
        last_reconcile: None,
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: None,
        module_reconcile: vec![],
    }
}

/// Build the Doc emitted for `cfgd daemon status`. Pulled out so integration
/// tests can construct the Doc deterministically without standing up IPC.
pub fn build_daemon_status_doc(status: Option<&cfgd_core::daemon::DaemonStatusResponse>) -> Doc {
    let mut doc = Doc::new().heading("Daemon Status");

    match status {
        Some(s) => {
            doc = doc.status(Role::Ok, "Daemon is running");
            doc = doc.kv_block([
                ("PID", s.pid.to_string()),
                ("Uptime", format!("{}s", s.uptime_secs)),
                ("Drift count", s.drift_count.to_string()),
            ]);
            if let Some(ref last) = s.last_reconcile {
                doc = doc.kv("Last reconcile", last);
            }
            if let Some(ref last) = s.last_sync {
                doc = doc.kv("Last sync", last);
            }

            if let Some(ref version) = s.update_available {
                doc = doc.status(
                    Role::Warn,
                    format!(
                        "Update available: {} — run 'cfgd upgrade' to install",
                        version
                    ),
                );
            }

            let rows: Vec<Vec<String>> = s
                .sources
                .iter()
                .map(|src| {
                    vec![
                        src.name.clone(),
                        src.status.clone(),
                        src.drift_count.to_string(),
                        src.last_sync.clone().unwrap_or_else(|| "-".to_string()),
                    ]
                })
                .collect();
            let mut table = Table::new(["Name", "Status", "Drift", "Last Sync"]);
            for row in rows {
                table = table.row(row);
            }
            doc = doc.section("Sources", |sec| sec.table(table));
            doc.with_data(s)
        }
        None => {
            let placeholder = placeholder_status();
            doc.status(Role::Warn, "Daemon is not running")
                .status(Role::Info, "Start with: cfgd daemon")
                .status(Role::Info, "Install as service: cfgd daemon install")
                .with_data(&placeholder)
        }
    }
}

pub(super) fn cmd_daemon_install(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    // Runtime cfg! so the install_failed error_doc has the platform+service
    // strings available before the lib call. The success payload uses
    // compile-time #[cfg] further down because the Windows branch loads
    // platform-specific config that cannot link on other targets.
    let (platform, service) = if cfg!(windows) {
        ("windows", "cfgd")
    } else if cfg!(target_os = "macos") {
        ("macos", "com.cfgd.daemon")
    } else {
        ("linux", "cfgd.service")
    };

    if let Err(e) = cfgd_core::daemon::install_service(&cli.config, cli.profile.as_deref()) {
        printer.emit(cfgd_core::output::error_doc(
            "cfgd",
            "install_failed",
            format!("Failed to install daemon service: {}", e),
            serde_json::json!({ "platform": platform, "service": service }),
        ));
        return Err(e.into());
    }

    #[cfg(windows)]
    let payload = {
        let event_log_on = cfgd_core::config::load_config(&cli.config)
            .ok()
            .and_then(|cfg| cfg.spec.daemon)
            .map(|d| d.windows_event_log)
            .unwrap_or(false);
        DaemonInstallOutput {
            platform: "windows".to_string(),
            service: "cfgd".to_string(),
            path: "%LOCALAPPDATA%\\cfgd\\daemon.log".to_string(),
            started: true,
            windows_event_log: Some(event_log_on),
        }
    };

    #[cfg(unix)]
    let payload = if cfg!(target_os = "macos") {
        DaemonInstallOutput {
            platform: "macos".to_string(),
            service: "com.cfgd.daemon".to_string(),
            path: "~/Library/LaunchAgents/com.cfgd.daemon.plist".to_string(),
            started: false,
            windows_event_log: None,
        }
    } else {
        DaemonInstallOutput {
            platform: "linux".to_string(),
            service: "cfgd.service".to_string(),
            path: "~/.config/systemd/user/cfgd.service".to_string(),
            started: false,
            windows_event_log: None,
        }
    };

    printer.emit(build_daemon_install_doc(&payload));
    Ok(())
}

/// Build the Doc emitted for `cfgd daemon install`. Carries the heading,
/// platform-specific success messages, and `with_data(payload)` so structured
/// consumers see a stable shape.
pub fn build_daemon_install_doc(payload: &DaemonInstallOutput) -> Doc {
    let mut doc = Doc::new().heading("Install Daemon Service");
    match payload.platform.as_str() {
        "windows" => {
            doc = doc
                .status(Role::Ok, "cfgd service installed and started")
                .status(Role::Info, "The service will start automatically on boot")
                .status(Role::Info, format!("Logs: {}", payload.path));
            if payload.windows_event_log.unwrap_or(false) {
                doc = doc.status(
                    Role::Info,
                    "Event Log mirror: Application → Source 'cfgd' (also at the file path above)",
                );
            } else {
                doc = doc.status(
                    Role::Info,
                    "Set spec.daemon.windowsEventLog: true in cfgd.yaml + reinstall to mirror logs into the Windows Event Log",
                );
            }
        }
        "macos" => {
            doc = doc
                .status(
                    Role::Ok,
                    format!("Installed launchd service: {}", payload.service),
                )
                .status(
                    Role::Info,
                    format!("Load with: launchctl load {}", payload.path),
                );
        }
        _ => {
            doc = doc
                .status(
                    Role::Ok,
                    format!("Installed systemd user service: {}", payload.service),
                )
                .status(
                    Role::Info,
                    format!(
                        "Enable with: systemctl --user enable --now {}",
                        payload.service
                    ),
                );
        }
    }
    doc.with_data(payload)
}

pub(super) fn cmd_daemon_uninstall(printer: &Printer) -> anyhow::Result<()> {
    let (platform, service) = if cfg!(windows) {
        ("windows", "cfgd")
    } else if cfg!(target_os = "macos") {
        ("macos", "com.cfgd.daemon")
    } else {
        ("linux", "cfgd.service")
    };

    if let Err(e) = cfgd_core::daemon::uninstall_service() {
        printer.emit(cfgd_core::output::error_doc(
            "cfgd",
            "uninstall_failed",
            format!("Failed to uninstall daemon service: {}", e),
            serde_json::json!({ "platform": platform, "service": service }),
        ));
        return Err(e.into());
    }

    let payload = DaemonUninstallOutput {
        platform: platform.to_string(),
        service: service.to_string(),
        removed: true,
    };
    printer.emit(build_daemon_uninstall_doc(&payload));
    Ok(())
}

/// Build the Doc emitted for `cfgd daemon uninstall`.
pub fn build_daemon_uninstall_doc(payload: &DaemonUninstallOutput) -> Doc {
    let mut doc = Doc::new().heading("Uninstall Daemon Service");
    let detail = match payload.platform.as_str() {
        "windows" => format!("Stopping and removing Windows Service: {}", payload.service),
        "macos" => format!(
            "Unloading: launchctl unload ~/Library/LaunchAgents/{}.plist",
            payload.service
        ),
        _ => format!(
            "Stopping: systemctl --user disable --now {}",
            payload.service
        ),
    };
    doc = doc.status(Role::Info, detail);
    doc = doc.status(Role::Ok, "Daemon service removed");
    doc.with_data(payload)
}

pub(super) fn cmd_daemon_service() -> anyhow::Result<()> {
    let hooks: std::sync::Arc<dyn cfgd_core::daemon::DaemonHooks> =
        std::sync::Arc::new(WorkstationDaemonHooks);
    cfgd_core::daemon::run_as_windows_service(hooks)?;
    Ok(())
}

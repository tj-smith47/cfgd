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
        Some(DaemonCommand::Status) => return cmd_daemon_status(cli, printer),
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
    let runtime_override = cli.runtime_dir.clone();
    let result = rt.block_on(async {
        cfgd_core::daemon::run_daemon(
            config_path,
            profile_override,
            runtime_override,
            daemon_printer,
            hooks,
        )
        .await
    });
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    if let Err(e) = result {
        let msg = format!("Daemon reconcile loop failed: {}", e);
        return Err(crate::cli::cli_error_ctx(
            e.into(),
            "cfgd",
            "runtime_failed",
            msg,
            serde_json::Value::Null,
        ));
    }

    Ok(())
}

pub fn cmd_daemon_status(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let status = match cfgd_core::daemon::query_daemon_status(cli.runtime_dir.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!(
                "Failed to query daemon status: {}",
                cfgd_core::output::collapse_to_subject_line(&e),
            );
            return Err(crate::cli::cli_error_ctx(
                e.into(),
                "cfgd",
                "status_unavailable",
                msg,
                serde_json::Value::Null,
            ));
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
        let msg = format!(
            "Failed to install daemon service: {}",
            cfgd_core::output::collapse_to_subject_line(&e),
        );
        return Err(crate::cli::cli_error_ctx(
            e.into(),
            "cfgd",
            "install_failed",
            msg,
            serde_json::json!({ "platform": platform, "service": service }),
        ));
    }

    // Writing the unit/plist alone leaves the daemon down; enable and start it
    // so `cfgd daemon install` actually runs the service. Degrades to a warning
    // plus hint (lingering / GUI login) when the session cannot host it, in which
    // case `started` reports false so `-o json` consumers see the real state.
    let started = cfgd_core::daemon::start_service(printer)?;

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
            started,
            windows_event_log: Some(event_log_on),
        }
    };

    #[cfg(unix)]
    let payload = if cfg!(target_os = "macos") {
        DaemonInstallOutput {
            platform: "macos".to_string(),
            service: "com.cfgd.daemon".to_string(),
            path: "~/Library/LaunchAgents/com.cfgd.daemon.plist".to_string(),
            started,
            windows_event_log: None,
        }
    } else {
        DaemonInstallOutput {
            platform: "linux".to_string(),
            service: "cfgd.service".to_string(),
            path: "~/.config/systemd/user/cfgd.service".to_string(),
            started,
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
            doc = doc.status(
                Role::Ok,
                format!("Installed launchd service: {}", payload.service),
            );
            if !payload.started {
                doc = doc.status(
                    Role::Info,
                    format!("Load with: launchctl load {}", payload.path),
                );
            }
        }
        _ => {
            doc = doc.status(
                Role::Ok,
                format!("Installed systemd user service: {}", payload.service),
            );
            if !payload.started {
                doc = doc.status(
                    Role::Info,
                    format!(
                        "Enable with: systemctl --user enable --now {}",
                        payload.service
                    ),
                );
            }
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

    if let Err(e) = cfgd_core::daemon::uninstall_service(printer) {
        let msg = format!(
            "Failed to uninstall daemon service: {}",
            cfgd_core::output::collapse_to_subject_line(&e),
        );
        return Err(crate::cli::cli_error_ctx(
            e.into(),
            "cfgd",
            "uninstall_failed",
            msg,
            serde_json::json!({ "platform": platform, "service": service }),
        ));
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
            "Unloading: launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/{}.plist",
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

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::test_helpers::test_printer as make_printer;

    fn make_status(running: bool) -> cfgd_core::daemon::DaemonStatusResponse {
        cfgd_core::daemon::DaemonStatusResponse {
            running,
            pid: if running { 12345 } else { 0 },
            uptime_secs: if running { 300 } else { 0 },
            last_reconcile: if running {
                Some("2026-05-22T10:00:00Z".to_string())
            } else {
                None
            },
            last_sync: if running {
                Some("2026-05-22T10:01:00Z".to_string())
            } else {
                None
            },
            drift_count: if running { 2 } else { 0 },
            sources: vec![],
            update_available: None,
            module_reconcile: vec![],
        }
    }

    fn make_cli() -> Cli {
        let dir = tempfile::tempdir().unwrap();
        Cli {
            config: dir.path().join("cfgd.yaml"),
            profile: None,
            no_color: true,
            verbose: 0,
            quiet: true,
            output: crate::cli::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            list_envelope: false,
            jsonpath: None,
            state_dir: None,
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            command: None,
        }
    }

    #[test]
    fn placeholder_status_defaults() {
        let s = placeholder_status();
        assert!(!s.running);
        assert_eq!(s.pid, 0);
        assert_eq!(s.uptime_secs, 0);
        assert!(s.last_reconcile.is_none());
        assert!(s.last_sync.is_none());
        assert_eq!(s.drift_count, 0);
        assert!(s.sources.is_empty());
        assert!(s.update_available.is_none());
        assert!(s.module_reconcile.is_empty());
    }

    #[test]
    fn build_daemon_status_doc_none_contains_not_running() {
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(None);
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("not running"),
            "expected 'not running' in output, got: {human}"
        );
    }

    #[test]
    fn build_daemon_status_doc_none_json_payload() {
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(None);
        printer.emit(doc);
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["running"], false);
        assert_eq!(json["pid"], 0);
    }

    #[test]
    fn build_daemon_status_doc_some_contains_pid() {
        let status = make_status(true);
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status));
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("12345"),
            "expected PID 12345 in output, got: {human}"
        );
    }

    #[test]
    fn build_daemon_status_doc_some_json_payload() {
        let status = make_status(true);
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status));
        printer.emit(doc);
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["running"], true);
        assert_eq!(json["pid"], 12345);
        assert_eq!(json["driftCount"], 2);
    }

    #[test]
    fn build_daemon_status_doc_update_available_renders() {
        let mut status = make_status(true);
        status.update_available = Some("v1.2.3".to_string());
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status));
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("v1.2.3"),
            "expected version in output, got: {human}"
        );
    }

    #[test]
    fn build_daemon_install_doc_linux() {
        let payload = DaemonInstallOutput {
            platform: "linux".to_string(),
            service: "cfgd.service".to_string(),
            path: "~/.config/systemd/user/cfgd.service".to_string(),
            started: false,
            windows_event_log: None,
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_install_doc(&payload);
        printer.emit(doc);
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["platform"], "linux");
        assert_eq!(json["service"], "cfgd.service");
        assert_eq!(json["started"], false);
        assert!(json.get("windowsEventLog").is_none());
        let human = cap.human();
        assert!(
            human.contains("cfgd.service"),
            "expected service name in output, got: {human}"
        );
    }

    #[test]
    fn build_daemon_install_doc_macos() {
        let payload = DaemonInstallOutput {
            platform: "macos".to_string(),
            service: "com.cfgd.daemon".to_string(),
            path: "~/Library/LaunchAgents/com.cfgd.daemon.plist".to_string(),
            started: false,
            windows_event_log: None,
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_install_doc(&payload);
        printer.emit(doc);
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["platform"], "macos");
        assert_eq!(json["service"], "com.cfgd.daemon");
        let human = cap.human();
        assert!(
            human.contains("launchctl"),
            "expected launchctl hint in output, got: {human}"
        );
    }

    #[test]
    fn build_daemon_install_doc_windows_event_log_some() {
        let payload = DaemonInstallOutput {
            platform: "windows".to_string(),
            service: "cfgd".to_string(),
            path: "%LOCALAPPDATA%\\cfgd\\daemon.log".to_string(),
            started: true,
            windows_event_log: Some(true),
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_install_doc(&payload);
        printer.emit(doc);
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["platform"], "windows");
        assert_eq!(json["started"], true);
        assert_eq!(json["windowsEventLog"], true);
        let human = cap.human();
        assert!(
            human.contains("Event Log"),
            "expected event-log mention in output, got: {human}"
        );
    }

    #[test]
    fn build_daemon_install_doc_windows_event_log_disabled() {
        let payload = DaemonInstallOutput {
            platform: "windows".to_string(),
            service: "cfgd".to_string(),
            path: "%LOCALAPPDATA%\\cfgd\\daemon.log".to_string(),
            started: true,
            windows_event_log: Some(false),
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_install_doc(&payload);
        printer.emit(doc);
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["windowsEventLog"], false);
        let human = cap.human();
        assert!(
            human.contains("windowsEventLog"),
            "expected event-log hint in output, got: {human}"
        );
    }

    #[test]
    fn build_daemon_uninstall_doc_linux() {
        let payload = DaemonUninstallOutput {
            platform: "linux".to_string(),
            service: "cfgd.service".to_string(),
            removed: true,
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_uninstall_doc(&payload);
        printer.emit(doc);
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["platform"], "linux");
        assert_eq!(json["service"], "cfgd.service");
        assert_eq!(json["removed"], true);
        let human = cap.human();
        assert!(
            human.contains("systemctl"),
            "expected systemctl in output, got: {human}"
        );
    }

    #[test]
    fn build_daemon_uninstall_doc_macos() {
        let payload = DaemonUninstallOutput {
            platform: "macos".to_string(),
            service: "com.cfgd.daemon".to_string(),
            removed: true,
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_uninstall_doc(&payload);
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("launchctl"),
            "expected launchctl in output, got: {human}"
        );
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["platform"], "macos");
    }

    #[test]
    fn build_daemon_uninstall_doc_windows() {
        let payload = DaemonUninstallOutput {
            platform: "windows".to_string(),
            service: "cfgd".to_string(),
            removed: true,
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_uninstall_doc(&payload);
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("Windows Service"),
            "expected Windows Service in output, got: {human}"
        );
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["platform"], "windows");
    }

    #[test]
    fn daemon_install_output_serde_roundtrip_without_event_log() {
        let original = DaemonInstallOutput {
            platform: "linux".to_string(),
            service: "cfgd.service".to_string(),
            path: "~/.config/systemd/user/cfgd.service".to_string(),
            started: false,
            windows_event_log: None,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        assert!(
            json.contains("\"platform\""),
            "camelCase key missing: {json}"
        );
        assert!(
            json.contains("\"service\""),
            "camelCase key missing: {json}"
        );
        assert!(
            !json.contains("windowsEventLog"),
            "None field must be skipped: {json}"
        );
    }

    #[test]
    fn daemon_install_output_serde_roundtrip_with_event_log() {
        let original = DaemonInstallOutput {
            platform: "windows".to_string(),
            service: "cfgd".to_string(),
            path: "%LOCALAPPDATA%\\cfgd\\daemon.log".to_string(),
            started: true,
            windows_event_log: Some(true),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        assert!(
            json.contains("\"windowsEventLog\""),
            "windowsEventLog key missing: {json}"
        );
        assert!(json.contains("true"), "value missing: {json}");
    }

    #[test]
    fn daemon_uninstall_output_serde_roundtrip() {
        let original = DaemonUninstallOutput {
            platform: "linux".to_string(),
            service: "cfgd.service".to_string(),
            removed: true,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        assert!(
            json.contains("\"platform\""),
            "camelCase key missing: {json}"
        );
        assert!(
            json.contains("\"service\""),
            "camelCase key missing: {json}"
        );
        assert!(
            json.contains("\"removed\""),
            "camelCase key missing: {json}"
        );
    }

    #[test]
    fn cmd_daemon_status_returns_ok_when_no_daemon() {
        let cli = make_cli();
        let printer = make_printer();
        let result = cmd_daemon_status(&cli, &printer);
        result.expect("cmd_daemon_status must succeed when daemon is not running");
    }

    #[test]
    fn cmd_daemon_dispatches_status() {
        let cli = make_cli();
        let printer = make_printer();
        let result = cmd_daemon(&cli, &printer, Some(&DaemonCommand::Status));
        result.expect("daemon status dispatch must succeed");
    }

    #[test]
    #[serial_test::serial]
    fn cmd_daemon_install_writes_unit_file() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let cli = make_cli();
        let printer = make_printer();
        let result = cmd_daemon(&cli, &printer, Some(&DaemonCommand::Install));
        result.expect("install must succeed with user-level systemd dir");
    }

    #[test]
    #[serial_test::serial]
    fn cmd_daemon_install_reports_started_false_when_start_degrades() {
        // The test-home override short-circuits start_service to "not started"
        // (no real systemctl/launchctl against the runner), exercising the
        // degraded path: the payload must report started == false rather than
        // over-claiming a running daemon to -o json consumers.
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let cli = make_cli();
        let (printer, cap) = Printer::for_test_doc();
        cmd_daemon(&cli, &printer, Some(&DaemonCommand::Install)).expect("install must succeed");
        let json = cap.json().expect("install doc must carry JSON payload");
        assert_eq!(
            json["started"], false,
            "degraded start must report started == false, got: {json}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn cmd_daemon_uninstall_ok_when_no_service() {
        let tmp_home = tempfile::tempdir().unwrap();
        let _home = cfgd_core::with_test_home_guard(tmp_home.path());
        let cli = make_cli();
        let printer = make_printer();
        let result = cmd_daemon(&cli, &printer, Some(&DaemonCommand::Uninstall));
        result.expect("uninstall must succeed when service file is absent");
    }

    // -----------------------------------------------------------------------
    // build_daemon_status_doc — content coverage for the populated-status arm.
    // Pins the human output and the data payload field names for downstream
    // consumers that parse `cfgd daemon status -o json`.
    // -----------------------------------------------------------------------

    #[test]
    fn build_daemon_status_doc_with_sources_emits_table_rows() {
        let mut status = make_status(true);
        status.sources = vec![
            cfgd_core::daemon::SourceStatus {
                name: "infra".into(),
                status: "synced".into(),
                drift_count: 0,
                last_sync: Some("2026-05-22T10:00:00Z".into()),
                last_reconcile: None,
            },
            cfgd_core::daemon::SourceStatus {
                name: "apps".into(),
                status: "stale".into(),
                drift_count: 3,
                last_sync: None,
                last_reconcile: None,
            },
        ];
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status));
        printer.emit(doc);
        let human = cap.human();
        assert!(human.contains("infra"), "infra source must appear: {human}");
        assert!(human.contains("apps"), "apps source must appear: {human}");
        assert!(
            human.contains("synced") || human.contains("stale"),
            "source status must appear: {human}"
        );
    }

    #[test]
    fn build_daemon_status_doc_emits_last_reconcile_when_present() {
        let mut status = make_status(true);
        status.last_reconcile = Some("2026-05-22T10:00:00Z".into());
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status));
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("2026-05-22T10:00:00Z"),
            "last reconcile timestamp must appear: {human}"
        );
    }

    #[test]
    fn build_daemon_status_doc_emits_last_sync_when_present() {
        let mut status = make_status(true);
        status.last_sync = Some("2026-05-22T11:00:00Z".into());
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status));
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("2026-05-22T11:00:00Z"),
            "last sync timestamp must appear: {human}"
        );
    }

    // -----------------------------------------------------------------------
    // build_daemon_install_doc — extra branch coverage for windows no-event-log
    // and arbitrary "unknown" platform falling through to systemd default.
    // -----------------------------------------------------------------------

    #[test]
    fn build_daemon_install_doc_unknown_platform_falls_to_systemd_arm() {
        // The `_ =>` arm in build_daemon_install_doc renders systemd hints
        // regardless of the platform string — pin that fallback so a future
        // refactor that introduces explicit platform handling doesn't silently
        // drop the fallback.
        let payload = DaemonInstallOutput {
            platform: "bsd-unknown".to_string(),
            service: "cfgd.service".to_string(),
            path: "/etc/init.d/cfgd".to_string(),
            started: false,
            windows_event_log: None,
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_install_doc(&payload);
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("systemctl"),
            "unknown platform must fall through to systemd hint: {human}"
        );
    }

    #[test]
    fn build_daemon_install_doc_windows_event_log_none_emits_disabled_hint() {
        let payload = DaemonInstallOutput {
            platform: "windows".to_string(),
            service: "cfgd".to_string(),
            path: "%LOCALAPPDATA%\\cfgd\\daemon.log".to_string(),
            started: true,
            windows_event_log: None,
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_install_doc(&payload);
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("Event Log") || human.contains("windowsEventLog"),
            "windows with windows_event_log=None must still render event-log hint: {human}"
        );
    }

    // -----------------------------------------------------------------------
    // build_daemon_uninstall_doc — pin platform-specific detail wording so a
    // future refactor that re-shapes the message doesn't silently change the
    // operator-visible hint.
    // -----------------------------------------------------------------------

    #[test]
    fn build_daemon_uninstall_doc_unknown_platform_falls_to_systemd_default() {
        let payload = DaemonUninstallOutput {
            platform: "bsd-unknown".to_string(),
            service: "cfgd.service".to_string(),
            removed: true,
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_uninstall_doc(&payload);
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("systemctl"),
            "unknown platform must fall through to systemd disable hint: {human}"
        );
    }

    #[test]
    fn build_daemon_uninstall_doc_includes_ok_status_line() {
        let payload = DaemonUninstallOutput {
            platform: "linux".to_string(),
            service: "cfgd.service".to_string(),
            removed: true,
        };
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_uninstall_doc(&payload);
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("Daemon service removed"),
            "uninstall must emit the Ok-status confirmation line: {human}"
        );
    }
}

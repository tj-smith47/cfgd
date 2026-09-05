use super::source::list::last_sync_display;
use super::*;
use cfgd_core::output::{Doc, KvPair, Printer, Role};
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
        Some(DaemonCommand::Uninstall) => return cmd_daemon_uninstall(cli, printer),
        Some(DaemonCommand::Service { .. }) => return cmd_daemon_service(),
        Some(DaemonCommand::Run) | None => {}
    }

    let config_path = std::fs::canonicalize(&cli.config).unwrap_or_else(|_| cli.config.clone());
    let profile_override = cli.profile.clone();
    // Derived from the process printer, never rebuilt: a fresh `Printer::new`
    // re-resolves colour from the terminal, so `--no-color` reached the process
    // printer and nothing else and the daemon drew a fully coloured reconcile
    // tree into journald. It also re-resolves the theme from nothing, dropping
    // the configured `spec.theme` on the way.
    let daemon_printer = std::sync::Arc::new(printer.at_verbosity(if cli.quiet {
        cfgd_core::output::Verbosity::Quiet
    } else if cli.verbose > 0 {
        cfgd_core::output::Verbosity::Verbose
    } else {
        cfgd_core::output::Verbosity::Normal
    }));

    let hooks: std::sync::Arc<dyn cfgd_core::daemon::DaemonHooks> =
        std::sync::Arc::new(WorkstationDaemonHooks);
    let rt = tokio::runtime::Runtime::new()?;
    let dirs = cli.daemon_dir_overrides();
    let result = rt.block_on(async {
        cfgd_core::daemon::run_daemon(
            config_path,
            profile_override,
            dirs,
            daemon_printer,
            hooks,
            cli.scope(),
            env!("CARGO_PKG_VERSION"),
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
    let status =
        match cfgd_core::daemon::query_daemon_status(cli.runtime_dir.as_deref(), cli.scope()) {
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
    // The daemon reports which sources it is tracking and how each is doing;
    // the config and the state store hold everything else the shared `Sources`
    // table shows. A machine with no readable config still renders the table —
    // the daemon's own rows, with the config-side columns reading `-`.
    let (catalog, declared_sources) = configured_source_catalog(cli);
    printer.emit(build_daemon_status_doc(
        status.as_ref(),
        &declared_sources,
        &catalog,
        &cfgd_core::utc_now_iso8601(),
        printer.arrow(),
    ));
    Ok(())
}

/// The `spec.sources[]` this machine declares, twice over: the table rows
/// carrying the columns the daemon does not report, and the subscriptions the
/// header names. Both empty when the config or the state store cannot be read:
/// the daemon's status is still worth printing without them.
fn configured_source_catalog(
    cli: &Cli,
) -> (
    Vec<SourceListEntry>,
    Vec<cfgd_core::reconciler::ComposedSource>,
) {
    let Ok(cfg) = config::load_config(&cli.config) else {
        return (Vec::new(), Vec::new());
    };
    let declared = cfgd_core::reconciler::ComposedSource::from_declared(&cfg.spec.sources);
    let Ok(state) = open_state_store(cli.state_dir.as_deref(), cli.scope()) else {
        return (Vec::new(), declared);
    };
    (
        super::source::list::configured_source_entries(&cfg, &state),
        declared,
    )
}

pub(super) fn placeholder_status() -> cfgd_core::daemon::DaemonStatusResponse {
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
        reconcile_interval_secs: None,
        sync_interval_secs: None,
        config_path: None,
        profile: None,
        modules: vec![],
        profile_inherits: vec![],
    }
}

/// Build the Doc emitted for `cfgd daemon status`. Pulled out so integration
/// tests can construct the Doc deterministically without standing up IPC.
///
/// `now` is a parameter rather than a clock read, so every stamp this render
/// ages against is the caller's one instant and a captured render pins.
///
/// `catalog` supplies the `Sources` columns the daemon does not report (the
/// origin, the priority, the checked-out commit, the signature demand), matched
/// to the daemon's own rows by name. A running daemon tracks exactly the
/// `spec.sources[]` it started with, so a row with no match is one the config
/// lost since then: it keeps the daemon's live facts and reads `-` for the rest
/// rather than disappearing from a dashboard that is reporting on it.
pub fn build_daemon_status_doc(
    status: Option<&cfgd_core::daemon::DaemonStatusResponse>,
    declared_sources: &[cfgd_core::reconciler::ComposedSource],
    catalog: &[SourceListEntry],
    now: &str,
    arrow: &str,
) -> Doc {
    let mut doc = Doc::new().heading("Daemon Status");

    match status {
        Some(s) => {
            // The config, the sources, the profile and what that profile
            // resolves to — through the one builder `cfgd status` and every run
            // header read, ahead of the facts about the process. The modules
            // are the loop's own resolution, carried on the wire; the sources
            // are what this machine's config subscribes to, this reader holding
            // no composition of its own.
            let mut rows =
                cfgd_core::output::config_header_rows(&cfgd_core::output::ConfigHeader {
                    config_path: s.config_path.as_deref().map(std::path::Path::new),
                    sources: declared_sources,
                    profile: s.profile.as_deref(),
                    profile_inherits: &s.profile_inherits,
                    modules: &s.modules,
                    arrow,
                });
            rows.push(KvPair::new("PID", s.pid.to_string()));
            // A measured duration, not a declared one: the intervals below
            // are the operator's own literals and stay verbatim.
            rows.push(KvPair::new(
                "Uptime",
                cfgd_core::humanize_duration_secs(s.uptime_secs),
            ));
            // Omitted rather than guessed when the daemon did not report them:
            // the loop's cadence is whatever it reloaded last, and this command
            // holds no config of its own to answer from.
            if let Some(secs) = s.reconcile_interval_secs {
                rows.push(KvPair::new("Reconcile Interval", format!("{secs}s")));
            }
            if let Some(secs) = s.sync_interval_secs {
                rows.push(KvPair::new("Sync Interval", format!("{secs}s")));
            }
            rows.push(KvPair::new("Drift Count", s.drift_count.to_string()));
            // The stored instant stays in the `-o json` payload below; a
            // person reading the dashboard is asking how stale the loop is.
            //
            // No `Last Sync` counterpart: the daemon syncs per source and the
            // Sources table below carries a `Last Sync` column, so a single
            // top-level row is the most recent of them wearing a name that
            // reads as all of them.
            if let Some(ref last) = s.last_reconcile {
                rows.push(KvPair::new(
                    "Last Reconcile",
                    last_sync_display(Some(last), now),
                ));
            }
            doc = doc.kv_rows(rows);

            // After the facts, the way every other surface reporting on a
            // resolved configuration orders them: the header block binds to
            // the heading above it, and a verdict about the run reads at the
            // report's own depth below. The update notice stays beside the
            // running verdict — two verdicts about the daemon read as one
            // report when nothing sits between them.
            // verdict-row-ok: reports the service's state, not something this run did
            doc = doc.status(Role::Ok, "Daemon running");
            if let Some(ref version) = s.update_available {
                doc = doc.status(
                    Role::Warn,
                    format!(
                        "Update available: {} — run `cfgd upgrade` to install",
                        version
                    ),
                );
            }

            let rows: Vec<SourceListEntry> = s
                .sources
                .iter()
                .map(|src| daemon_source_row(src, catalog))
                .collect();
            doc = doc.section_if_nonempty(
                super::source::list::SOURCES_SECTION,
                &rows,
                |sec, rows| sec.table(super::source::list::sources_table(rows, false, now)),
            );
            doc.with_data(s)
        }
        None => {
            let placeholder = placeholder_status();
            doc.status(Role::Warn, "Daemon not running")
                .status(Role::Info, "Start with: `cfgd daemon`")
                .status(Role::Info, "Install as service: `cfgd daemon install`")
                .with_data(&placeholder)
        }
    }
}

/// One daemon-reported source as a `Sources` row: the daemon's live facts
/// (status, drift, last sync) over the declared entry of the same name.
fn daemon_source_row(
    src: &cfgd_core::daemon::SourceStatus,
    catalog: &[SourceListEntry],
) -> SourceListEntry {
    let declared = catalog.iter().find(|e| e.name == src.name);
    // Every catalog-sourced slot stays absent for a row the catalog does not
    // hold (the implicit `local` layer, a source the config dropped): a
    // substituted default reads as a declared fact.
    SourceListEntry {
        name: src.name.clone(),
        url: declared.and_then(|e| e.url.clone()),
        priority: declared.and_then(|e| e.priority),
        version: declared.and_then(|e| e.version.clone()),
        status: src.status.clone(),
        last_fetched: src.last_sync.clone(),
        signed: declared.and_then(|e| e.signed),
        require_signed_commits: declared.and_then(|e| e.require_signed_commits),
        // The daemon holds the commit its own pull landed on; the catalog's
        // is what the last `cfgd sync` recorded, and may be older.
        last_commit: src
            .last_commit
            .clone()
            .or_else(|| declared.and_then(|e| e.last_commit.clone())),
        // Straight through: the daemon cannot attribute drift to one source
        // (see `SourceStatus::drift_count`), so the `Drift` column is dropped
        // rather than filled with the machine-wide total.
        drift_count: src.drift_count,
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

    let scope = cli.scope();

    if scope == cfgd_core::Scope::System && !cfgd_core::is_root() {
        printer.status_simple(Role::Fail, "System-scope install requires root privileges");
        printer.hint("Re-run with `sudo cfgd --scope system daemon install`");
        return Err(anyhow::anyhow!(
            "insufficient privileges for system-scope install"
        ));
    }

    let dirs = cli.daemon_dir_overrides();
    if let Err(e) =
        cfgd_core::daemon::install_service(&cli.config, cli.profile.as_deref(), scope, &dirs)
    {
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
    let started = cfgd_core::daemon::start_service(printer, scope)?;

    #[cfg(windows)]
    let payload = {
        let event_log_on = match cfgd_core::config::load_config(&cli.config) {
            Ok(mut cfg) => {
                drain_config_deprecations(printer, &mut cfg);
                cfg.spec
                    .daemon
                    .map(|d| d.windows_event_log)
                    .unwrap_or(false)
            }
            Err(_) => false,
        };
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
        let path = if scope == cfgd_core::Scope::System {
            "/Library/LaunchDaemons/com.cfgd.daemon.plist".to_string()
        } else {
            "~/Library/LaunchAgents/com.cfgd.daemon.plist".to_string()
        };
        DaemonInstallOutput {
            platform: "macos".to_string(),
            service: "com.cfgd.daemon".to_string(),
            path,
            started,
            windows_event_log: None,
        }
    } else {
        let path = if scope == cfgd_core::Scope::System {
            "/etc/systemd/system/cfgd.service".to_string()
        } else {
            "~/.config/systemd/user/cfgd.service".to_string()
        };
        DaemonInstallOutput {
            platform: "linux".to_string(),
            service: "cfgd.service".to_string(),
            path,
            started,
            windows_event_log: None,
        }
    };

    printer.emit(build_daemon_install_doc(&payload, printer.arrow()));
    Ok(())
}

/// Build the Doc emitted for `cfgd daemon install`. Carries the heading,
/// platform-specific success messages, and `with_data(payload)` so structured
/// consumers see a stable shape.
pub fn build_daemon_install_doc(payload: &DaemonInstallOutput, arrow: &str) -> Doc {
    let mut doc = Doc::new().heading("Install Daemon Service");
    match payload.platform.as_str() {
        "windows" => {
            if payload.started {
                doc = doc.status(Role::Ok, "Installed and started the cfgd service");
            } else {
                doc = doc
                    .status(
                        Role::Warn,
                        "Installed the cfgd service but it is not yet running",
                    )
                    .status(
                        Role::Info,
                        "Start it with `sc start cfgd` — it is also set to auto-start on boot",
                    );
            }
            doc = doc
                .status(Role::Info, "The service will start automatically on boot")
                .status_with(Role::Info, "Logs", |f| f.qualifier(payload.path.clone()));
            if payload.windows_event_log.unwrap_or(false) {
                doc = doc.status(
                    Role::Info,
                    format!(
                        "Event Log mirror: Application {arrow} Source 'cfgd' (also at the file path above)"
                    ),
                );
            } else {
                doc = doc.status(
                    Role::Info,
                    "Set spec.daemon.windowsEventLog: true in cfgd.yaml + reinstall to mirror logs into the Windows Event Log",
                );
            }
        }
        "macos" => {
            doc = doc.status_with(Role::Ok, "Installed launchd service", |f| {
                f.qualifier(payload.service.clone())
            });
            if !payload.started {
                doc = doc.status(
                    Role::Info,
                    format!("Load with `launchctl load {}`", payload.path),
                );
            }
        }
        _ => {
            doc = doc.status_with(Role::Ok, "Installed systemd user service", |f| {
                f.qualifier(payload.service.clone())
            });
            if !payload.started {
                doc = doc.status(
                    Role::Info,
                    format!(
                        "Enable with `systemctl --user enable --now {}`",
                        payload.service
                    ),
                );
            }
        }
    }
    doc.with_data(payload)
}

pub(super) fn cmd_daemon_uninstall(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    let (platform, service) = if cfg!(windows) {
        ("windows", "cfgd")
    } else if cfg!(target_os = "macos") {
        ("macos", "com.cfgd.daemon")
    } else {
        ("linux", "cfgd.service")
    };

    let scope = cli.scope();

    if scope == cfgd_core::Scope::System && !cfgd_core::is_root() {
        printer.status_simple(
            Role::Fail,
            "System-scope uninstall requires root privileges",
        );
        printer.hint("Re-run with `sudo cfgd --scope system daemon uninstall`");
        return Err(anyhow::anyhow!(
            "insufficient privileges for system-scope uninstall"
        ));
    }

    if let Err(e) = cfgd_core::daemon::uninstall_service(printer, scope) {
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
    printer.emit(build_daemon_uninstall_doc(&payload, scope));
    Ok(())
}

/// Build the Doc emitted for `cfgd daemon uninstall`.
pub fn build_daemon_uninstall_doc(payload: &DaemonUninstallOutput, scope: cfgd_core::Scope) -> Doc {
    let mut doc = Doc::new().heading("Uninstall Daemon Service");
    let detail = match payload.platform.as_str() {
        "windows" => format!("Stopping and removing Windows Service: {}", payload.service),
        "macos" => {
            if scope == cfgd_core::Scope::System {
                format!(
                    "Unloading: launchctl bootout system /Library/LaunchDaemons/{}.plist",
                    payload.service
                )
            } else {
                format!(
                    "Unloading: launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/{}.plist",
                    payload.service
                )
            }
        }
        _ => {
            if scope == cfgd_core::Scope::System {
                format!("Stopping: systemctl disable --now {}", payload.service)
            } else {
                format!(
                    "Stopping: systemctl --user disable --now {}",
                    payload.service
                )
            }
        }
    };
    doc = doc.status(Role::Info, detail);
    doc = doc.status(Role::Ok, "Removed daemon service");
    doc.with_data(payload)
}

pub(super) fn cmd_daemon_service() -> anyhow::Result<()> {
    let hooks: std::sync::Arc<dyn cfgd_core::daemon::DaemonHooks> =
        std::sync::Arc::new(WorkstationDaemonHooks);
    cfgd_core::daemon::run_as_windows_service(hooks, env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

#[cfg(test)]
mod tests {

    /// The instant every daemon-status render in this suite ages its stamps
    /// against, so a captured age is a fact about the fixture rather than about
    /// the day the suite ran.
    const DAEMON_STATUS_NOW: &str = "2026-05-14T12:00:00Z";
    use super::*;
    use cfgd_core::test_helpers::test_printer as make_printer;

    fn make_status(running: bool) -> cfgd_core::daemon::DaemonStatusResponse {
        cfgd_core::daemon::DaemonStatusResponse {
            running,
            pid: if running { 12345 } else { 0 },
            uptime_secs: if running { 300 } else { 0 },
            last_reconcile: if running {
                Some("2026-05-14T10:00:00Z".to_string())
            } else {
                None
            },
            last_sync: if running {
                Some("2026-05-14T10:01:00Z".to_string())
            } else {
                None
            },
            drift_count: if running { 2 } else { 0 },
            sources: vec![],
            update_available: None,
            module_reconcile: vec![],
            reconcile_interval_secs: None,
            sync_interval_secs: None,
            config_path: None,
            profile: None,
            modules: vec![],
            profile_inherits: vec![],
        }
    }

    fn make_cli() -> Cli {
        let dir = tempfile::tempdir().unwrap();
        Cli {
            config: dir.path().join("cfgd.yaml"),
            config_explicit: false,
            profile: None,
            no_color: true,
            color: crate::cli::ColorWhen::Auto,
            verbose: 0,
            quiet: true,
            output: crate::cli::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            list_envelope: false,
            no_hints: false,
            theme: None,
            jsonpath: None,
            yes: false,
            state_dir: None,
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    // Producer/consumer contract: the argv `install_windows_service` bakes into
    // the Windows service binPath MUST parse through the real clap `Cli`. The SCM
    // re-launches `cfgd.exe daemon service …` with those exact tokens; if clap
    // rejects any of them the process exits (code 2) before the service
    // dispatcher runs, so the service never starts (Windows error 1053). Runs on
    // the Linux CI host — `service_binpath_argv` is deliberately platform-neutral.
    // Serial: this parses through the real clap `Cli`, whose globals are
    // env-bound (`CFGD_STATE_DIR` and friends). A concurrent test that sets one
    // would be read as this test's own input.
    #[test]
    #[serial_test::serial]
    fn windows_service_binpath_argv_parses_via_cli() {
        use clap::Parser;
        let cfg = std::path::Path::new("C:/ProgramData/cfgd/cfgd.yaml");
        let no_dirs = cfgd_core::daemon::DaemonDirOverrides::default();
        let both_dirs = cfgd_core::daemon::DaemonDirOverrides {
            state_dir: Some(std::path::PathBuf::from("C:/cfgd-state")),
            runtime_dir: Some(std::path::PathBuf::from("C:/cfgd-run")),
            ..Default::default()
        };
        let state_only = cfgd_core::daemon::DaemonDirOverrides {
            state_dir: Some(std::path::PathBuf::from("C:/cfgd-state")),
            runtime_dir: None,
            ..Default::default()
        };
        let cases = [
            (None, false, cfgd_core::Scope::User, &no_dirs),
            (Some("laptop"), true, cfgd_core::Scope::User, &no_dirs),
            (None, true, cfgd_core::Scope::System, &no_dirs),
            (Some("srv"), true, cfgd_core::Scope::System, &no_dirs),
            (None, false, cfgd_core::Scope::User, &both_dirs),
            (Some("srv"), true, cfgd_core::Scope::System, &both_dirs),
            (None, false, cfgd_core::Scope::System, &state_only),
        ];
        for (profile, event_log, scope, dirs) in cases {
            let argv =
                cfgd_core::daemon::service_binpath_argv(cfg, profile, event_log, scope, dirs);
            if let Some(dir) = dirs.state_dir.as_deref() {
                assert!(
                    argv.windows(2)
                        .any(|w| w[0] == "--state-dir" && w[1] == cfgd_core::to_posix_string(dir)),
                    "argv {argv:?} dropped the --state-dir the install ran under",
                );
            }
            if let Some(dir) = dirs.runtime_dir.as_deref() {
                assert!(
                    argv.windows(2).any(
                        |w| w[0] == "--runtime-dir" && w[1] == cfgd_core::to_posix_string(dir)
                    ),
                    "argv {argv:?} dropped the --runtime-dir the install ran under",
                );
            }
            let full = std::iter::once("cfgd".to_string()).chain(argv.iter().cloned());
            let cli = Cli::try_parse_from(full).unwrap_or_else(|e| {
                panic!(
                    "baked service argv {argv:?} rejected by the daemon-service clap parser: {e}"
                )
            });
            assert!(
                matches!(
                    cli.command,
                    Some(crate::cli::Command::Daemon {
                        command: Some(DaemonCommand::Service { .. })
                    })
                ),
                "argv {argv:?} did not parse to `daemon service`",
            );
            assert_eq!(
                cli.state_dir.as_deref(),
                dirs.state_dir.as_deref(),
                "argv {argv:?} did not round-trip --state-dir through clap",
            );
            assert_eq!(
                cli.runtime_dir.as_deref(),
                dirs.runtime_dir.as_deref(),
                "argv {argv:?} did not round-trip --runtime-dir through clap",
            );
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
        let doc = build_daemon_status_doc(None, &[], &[], DAEMON_STATUS_NOW, "->");
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
        let doc = build_daemon_status_doc(None, &[], &[], DAEMON_STATUS_NOW, "->");
        printer.emit(doc);
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["running"], false);
        assert_eq!(json["pid"], 0);
    }

    #[test]
    fn build_daemon_status_doc_some_contains_pid() {
        let status = make_status(true);
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status), &[], &[], DAEMON_STATUS_NOW, "->");
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
        let doc = build_daemon_status_doc(Some(&status), &[], &[], DAEMON_STATUS_NOW, "->");
        printer.emit(doc);
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["running"], true);
        assert_eq!(json["pid"], 12345);
        assert_eq!(json["driftCount"], 2);
    }

    #[test]
    fn build_daemon_status_doc_renders_the_reported_intervals() {
        let mut status = make_status(true);
        status.reconcile_interval_secs = Some(300);
        status.sync_interval_secs = Some(900);
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_daemon_status_doc(
            Some(&status),
            &[],
            &[],
            DAEMON_STATUS_NOW,
            printer.arrow(),
        ));
        let human = cap.human();
        assert!(
            human.contains("Reconcile Interval") && human.contains("300s"),
            "expected the reconcile interval row, got: {human}"
        );
        assert!(
            human.contains("Sync Interval") && human.contains("900s"),
            "expected the sync interval row, got: {human}"
        );
    }

    #[test]
    fn build_daemon_status_doc_omits_intervals_the_daemon_did_not_report() {
        let status = make_status(true);
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_daemon_status_doc(
            Some(&status),
            &[],
            &[],
            DAEMON_STATUS_NOW,
            printer.arrow(),
        ));
        let human = cap.human();
        assert!(
            !human.contains("Reconcile Interval") && !human.contains("Sync Interval"),
            "an unreported cadence must not be rendered at all, got: {human}"
        );
    }

    #[test]
    fn build_daemon_status_doc_update_available_renders() {
        let mut status = make_status(true);
        status.update_available = Some("v1.2.3".to_string());
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status), &[], &[], DAEMON_STATUS_NOW, "->");
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
        let doc = build_daemon_install_doc(&payload, printer.arrow());
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
        let doc = build_daemon_install_doc(&payload, printer.arrow());
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
        let doc = build_daemon_install_doc(&payload, printer.arrow());
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
        let doc = build_daemon_install_doc(&payload, printer.arrow());
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
        let doc = build_daemon_uninstall_doc(&payload, cfgd_core::Scope::User);
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
        let doc = build_daemon_uninstall_doc(&payload, cfgd_core::Scope::User);
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
        let doc = build_daemon_uninstall_doc(&payload, cfgd_core::Scope::User);
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
    fn build_daemon_install_doc_windows_not_started_is_honest() {
        // A Windows install whose service did not reach RUNNING must NOT claim
        // "started" — it reports the real state plus a manual-start hint, so
        // `-o json` consumers and the human reader both see the truth.
        let payload = DaemonInstallOutput {
            platform: "windows".to_string(),
            service: "cfgd".to_string(),
            path: "%LOCALAPPDATA%\\cfgd\\daemon.log".to_string(),
            started: false,
            windows_event_log: Some(true),
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_daemon_install_doc(&payload, printer.arrow()));
        let human = cap.human();
        assert!(
            human.contains("Installed the cfgd service but it is not yet running")
                && human.contains("sc start cfgd"),
            "not-started install must report the real state + start hint, got: {human}"
        );
        assert!(
            !human.contains("Installed and started"),
            "must NOT over-claim 'started' when the service is not running, got: {human}"
        );
        let json = cap.json().expect("doc must carry JSON payload");
        assert_eq!(json["started"], false);
    }

    #[test]
    fn build_daemon_install_doc_windows_started_reports_started() {
        let payload = DaemonInstallOutput {
            platform: "windows".to_string(),
            service: "cfgd".to_string(),
            path: "%LOCALAPPDATA%\\cfgd\\daemon.log".to_string(),
            started: true,
            windows_event_log: Some(false),
        };
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_daemon_install_doc(&payload, printer.arrow()));
        let human = cap.human();
        assert!(
            human.contains("Installed and started the cfgd service"),
            "a running service must report 'started', got: {human}"
        );
        assert_eq!(cap.json().expect("payload")["started"], true);
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
            // Stored tokens, from the constants: `synced` and `stale` are
            // spellings nothing writes into this field, so a row seeded with
            // either proves only what an unrecognised token renders as.
            cfgd_core::daemon::SourceStatus {
                name: "infra".into(),
                status: cfgd_core::state::SOURCE_STATUS_ACTIVE.into(),
                drift_count: None,
                last_sync: Some("2026-05-14T10:00:00Z".into()),
                last_commit: None,
            },
            cfgd_core::daemon::SourceStatus {
                name: "apps".into(),
                status: cfgd_core::state::SOURCE_STATUS_ERROR.into(),
                drift_count: None,
                last_sync: None,
                last_commit: None,
            },
        ];
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status), &[], &[], DAEMON_STATUS_NOW, "->");
        printer.emit(doc);
        let human = cap.human();
        assert!(human.contains("infra"), "infra source must appear: {human}");
        assert!(human.contains("apps"), "apps source must appear: {human}");
        // The words, not the stored tokens: the cell renders through
        // `source_status_display` like every other source-status cell.
        assert!(
            human.contains("Active") && human.contains("Failed"),
            "each source's status must render as its display word: {human}"
        );
    }

    /// The daemon reports the implicit `local` layer, which no `spec.sources[]`
    /// entry declares. Its row read `Priority 0` / `Requires Signed no` —
    /// two facts nobody stated — because the row type could not say absent.
    /// Alone, the columns nothing can fill are dropped; beside a declared
    /// source they read `-`; and the row on the wire carries `null`.
    #[test]
    fn the_implicit_local_source_declares_no_priority_origin_or_signing_demand() {
        let mut status = make_status(true);
        status.sources = vec![cfgd_core::daemon::SourceStatus {
            name: cfgd_core::config::LOCAL_LAYER.into(),
            status: cfgd_core::state::SOURCE_STATUS_ACTIVE.into(),
            drift_count: None,
            last_sync: None,
            last_commit: None,
        }];
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_daemon_status_doc(
            Some(&status),
            &[],
            &[],
            DAEMON_STATUS_NOW,
            printer.arrow(),
        ));
        let human = cap.human();
        let header = human
            .lines()
            .find(|l| l.trim_start().starts_with("Name"))
            .unwrap_or_else(|| panic!("a Sources header: {human}"));
        for column in ["Source", "Priority", "Requires Signed"] {
            assert!(
                !header.contains(column),
                "`{column}` has nothing to say about a row nothing declared and is dropped: {human}"
            );
        }

        let declared = SourceListEntry {
            name: "team".into(),
            url: Some("https://github.com/team/config".into()),
            priority: Some(100),
            version: None,
            status: cfgd_core::state::SOURCE_STATUS_ACTIVE.into(),
            last_fetched: None,
            signed: None,
            require_signed_commits: Some(true),
            last_commit: None,
            drift_count: None,
        };
        status.sources.push(cfgd_core::daemon::SourceStatus {
            name: "team".into(),
            status: cfgd_core::state::SOURCE_STATUS_ACTIVE.into(),
            drift_count: None,
            last_sync: None,
            last_commit: None,
        });
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_daemon_status_doc(
            Some(&status),
            &[],
            std::slice::from_ref(&declared),
            DAEMON_STATUS_NOW,
            printer.arrow(),
        ));
        let human = cap.human();
        let local_row = human
            .lines()
            .find(|l| l.trim_start().starts_with("local"))
            .unwrap_or_else(|| panic!("a local row: {human}"));
        let cells: Vec<&str> = local_row.split_whitespace().collect();
        // Name, Source, Priority, Status, Last Sync, Requires Signed —
        // `Drift` is gone: no row can fill it (see `SourceStatus::drift_count`).
        assert_eq!(
            cells,
            vec!["local", "-", "-", "Active", "never", "-"],
            "every undeclared slot reads absent, never a default: {human}"
        );

        let row = daemon_source_row(&status.sources[0], std::slice::from_ref(&declared));
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["url"], serde_json::Value::Null);
        assert_eq!(json["priority"], serde_json::Value::Null);
        assert_eq!(json["requiresSignedCommits"], serde_json::Value::Null);
    }

    /// The daemon names the commit its own pull landed on, so the `Commit`
    /// column is filled from the live row (shortened) while the payload keeps
    /// the full id; the catalog's recorded commit is only the fallback.
    #[test]
    fn the_commit_column_reads_the_daemons_own_pull_in_full_on_the_wire() {
        const LANDED: &str = "719956f7587f0a1b2c3d4e5f60718293a4b5c6d7";
        let mut status = make_status(true);
        status.sources = vec![cfgd_core::daemon::SourceStatus {
            name: cfgd_core::config::LOCAL_LAYER.into(),
            status: cfgd_core::state::SOURCE_STATUS_ACTIVE.into(),
            drift_count: None,
            last_sync: Some("2026-05-14T11:00:00Z".into()),
            last_commit: Some(LANDED.into()),
        }];
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_daemon_status_doc(
            Some(&status),
            &[],
            &[],
            DAEMON_STATUS_NOW,
            printer.arrow(),
        ));
        let human = cap.human();
        assert!(human.contains("Commit"), "the column is filled: {human}");
        assert!(
            human.contains(cfgd_core::short_commit(LANDED)) && !human.contains(LANDED),
            "the cell is the short form: {human}"
        );
        let json = cap.json().expect("doc captured json");
        assert_eq!(json["sources"][0]["lastCommit"], LANDED);

        let stale = SourceListEntry {
            name: cfgd_core::config::LOCAL_LAYER.into(),
            url: None,
            priority: None,
            version: None,
            status: cfgd_core::state::SOURCE_STATUS_ACTIVE.into(),
            last_fetched: None,
            signed: None,
            require_signed_commits: None,
            last_commit: Some("0000000000000000000000000000000000000000".into()),
            drift_count: None,
        };
        let row = daemon_source_row(&status.sources[0], std::slice::from_ref(&stale));
        assert_eq!(
            row.last_commit.as_deref(),
            Some(LANDED),
            "live beats recorded"
        );
    }

    #[test]
    fn build_daemon_status_doc_emits_last_reconcile_when_present() {
        let mut status = make_status(true);
        status.last_reconcile = Some("2026-05-14T10:00:00Z".into());
        let (printer, cap) = Printer::for_test_doc();
        let doc = build_daemon_status_doc(Some(&status), &[], &[], DAEMON_STATUS_NOW, "->");
        printer.emit(doc);
        let human = cap.human();
        let age = cfgd_core::humanize_age_since("2026-05-14T10:00:00Z", DAEMON_STATUS_NOW)
            .expect("the fixture stamp precedes the pinned now");
        assert!(
            human.contains(&format!("Last Reconcile  {age}")),
            "the last-reconcile row must carry the humanized age {age}: {human}"
        );
    }

    #[test]
    fn the_sync_age_is_reported_per_source_and_not_a_second_time_at_the_top() {
        let mut status = make_status(true);
        status.last_sync = Some("2026-05-14T11:00:00Z".into());
        status.sources = vec![cfgd_core::daemon::SourceStatus {
            name: "local".into(),
            last_sync: Some("2026-05-14T11:00:00Z".into()),
            drift_count: None,
            status: "Active".into(),
            last_commit: None,
        }];
        let (printer, cap) = Printer::for_test_doc();
        printer.emit(build_daemon_status_doc(
            Some(&status),
            &[],
            &[],
            DAEMON_STATUS_NOW,
            printer.arrow(),
        ));
        let human = cap.human();
        assert_eq!(
            human.matches("Last Sync").count(),
            1,
            "the Sources table's column is the only sync label: a top-level \
             row would say the same thing twice: {human}"
        );
        let age = cfgd_core::humanize_age_since("2026-05-14T11:00:00Z", DAEMON_STATUS_NOW)
            .expect("the fixture stamp is parseable");
        assert!(
            human.contains(&age),
            "the Sources table must carry the humanized age {age}: {human}"
        );
        assert!(
            !human.contains("2026-05-14T11:00:00Z"),
            "no surface renders the raw stamp once its age is computable: {human}"
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
        let doc = build_daemon_install_doc(&payload, printer.arrow());
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
        let doc = build_daemon_install_doc(&payload, printer.arrow());
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
        let doc = build_daemon_uninstall_doc(&payload, cfgd_core::Scope::User);
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
        let doc = build_daemon_uninstall_doc(&payload, cfgd_core::Scope::User);
        printer.emit(doc);
        let human = cap.human();
        assert!(
            human.contains("Removed daemon service"),
            "uninstall must emit the Ok-status confirmation line: {human}"
        );
    }
}

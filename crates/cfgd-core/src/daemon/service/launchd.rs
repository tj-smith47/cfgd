use super::super::*;
use crate::PathDisplayExt;

/// Generate launchd plist content for the daemon service.
///
/// `dirs` carries the process-level `--state-dir` / `--runtime-dir` — see
/// [`super::generate_systemd_unit`] for why dropping them silently splits the
/// daemon's state from the CLI's.
#[cfg(unix)]
pub(crate) fn generate_launchd_plist(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    home: &Path,
    scope: crate::Scope,
    dirs: &DaemonDirOverrides,
) -> String {
    // Every value interpolated into the plist is XML-escaped: a path or
    // profile name carrying `&` or `<` otherwise produces a plist launchd
    // refuses to parse, and the service silently never loads.
    //
    // native-ok: every path below renders as an argv token this host's own
    // binary receives back from launchd, so it must carry this host's
    // separators rather than being folded to `/`.
    let mut args = vec![
        format!(
            "<string>{}</string>",
            crate::xml_escape(&binary.display().to_string()) // native-ok: argv token for this host
        ),
        "<string>--config</string>".to_string(),
        format!(
            "<string>{}</string>",
            crate::xml_escape(&config_path.display().to_string()) // native-ok: argv token for this host
        ),
    ];
    if let Some(p) = profile {
        args.push("<string>--profile</string>".to_string());
        args.push(format!("<string>{}</string>", crate::xml_escape(p)));
    }
    if scope == crate::Scope::System {
        args.push("<string>--scope</string>".to_string());
        args.push("<string>system</string>".to_string());
    }
    for (flag, dir) in service_dir_flags(dirs) {
        args.push(format!("<string>{}</string>", flag));
        args.push(format!(
            "<string>{}</string>",
            crate::xml_escape(&dir.display().to_string()) // native-ok: argv token for this host
        ));
    }
    args.push("<string>--quiet</string>".to_string());
    args.push("<string>daemon</string>".to_string());

    let args_xml = args.join("\n            ");
    let label = LAUNCHD_LABEL;

    let (stdout_path, stderr_path) = if scope == crate::Scope::System {
        (
            "/var/log/cfgd.log".to_string(),
            "/var/log/cfgd.err".to_string(),
        )
    } else {
        let home_display = home.display();
        (
            format!("{home_display}/Library/Logs/cfgd.log"),
            format!("{home_display}/Library/Logs/cfgd.err"),
        )
    };

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
    <string>{stdout_path}</string>
    <key>StandardErrorPath</key>
    <string>{stderr_path}</string>
</dict>
</plist>"#
    )
}

#[cfg(unix)]
pub(crate) fn install_launchd_service(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    scope: crate::Scope,
    dirs: &DaemonDirOverrides,
) -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let (plist_dir, plist_path) = if scope == crate::Scope::System {
        let dir = std::path::PathBuf::from(LAUNCHD_DAEMONS_DIR);
        let path = dir.join(format!("{}.plist", LAUNCHD_LABEL));
        (dir, path)
    } else {
        let dir = home.join(LAUNCHD_AGENTS_DIR);
        let path = dir.join(format!("{}.plist", LAUNCHD_LABEL));
        (dir, path)
    };

    std::fs::create_dir_all(&plist_dir).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("create LaunchAgents/LaunchDaemons dir: {}", e),
    })?;

    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());

    let plist = generate_launchd_plist(binary, &config_abs, profile, &home, scope, dirs);

    crate::atomic_write_str(&plist_path, &plist).map_err(|e| {
        DaemonError::ServiceInstallFailed {
            message: format!("write plist: {}", e),
        }
    })?;

    tracing::info!(path = %plist_path.posix(), "installed launchd service");
    Ok(())
}

/// Build the `launchctl bootstrap` argv that loads the service. Split out so
/// the command construction is testable without invoking `launchctl`.
///
/// User scope targets `gui/<uid>`; system scope targets `system`.
#[cfg(unix)]
pub(crate) fn launchd_bootstrap_argv(
    uid: u32,
    plist_path: &Path,
    scope: crate::Scope,
) -> Vec<String> {
    if scope == crate::Scope::System {
        vec![
            "bootstrap".to_string(),
            "system".to_string(),
            plist_path.display().to_string(),
        ]
    } else {
        vec![
            "bootstrap".to_string(),
            format!("gui/{uid}"),
            plist_path.display().to_string(),
        ]
    }
}

/// Build the `launchctl enable` argv that marks the service enabled so it
/// survives a later bootout/bootstrap cycle.
///
/// User scope targets `gui/<uid>/...`; system scope targets `system/...`.
#[cfg(unix)]
pub(crate) fn launchd_enable_argv(uid: u32, scope: crate::Scope) -> Vec<String> {
    if scope == crate::Scope::System {
        vec!["enable".to_string(), format!("system/{LAUNCHD_LABEL}")]
    } else {
        vec!["enable".to_string(), format!("gui/{uid}/{LAUNCHD_LABEL}")]
    }
}

/// Bootstrap and enable the just-installed launchd service into the appropriate
/// domain so the daemon runs immediately rather than only after the next
/// login/boot. Best-effort: a headless session (no `gui/<uid>` domain, e.g.
/// over plain SSH) cannot host a LaunchAgent, so the failure is surfaced as a
/// warning plus an actionable hint rather than aborting the calling init flow.
///
/// Returns `Ok(true)` when the agent was bootstrapped and enabled, `Ok(false)`
/// when it was installed but could not be started now (the caller reports the
/// real state instead of over-claiming `started`).
#[cfg(unix)]
pub(crate) fn start_launchd_service(printer: &Printer, scope: crate::Scope) -> Result<bool> {
    if !crate::command_available("launchctl") {
        printer
            .status(Role::Warn, "launchctl not found")
            .detail(super::INSTALLED_NOT_STARTED);
        printer.hint("Start it later from a GUI login session with: cfgd daemon install");
        return Ok(false);
    }

    let home = crate::expand_tilde(Path::new("~"));
    let plist_path = if scope == crate::Scope::System {
        std::path::PathBuf::from(LAUNCHD_DAEMONS_DIR).join(format!("{}.plist", LAUNCHD_LABEL))
    } else {
        home.join(LAUNCHD_AGENTS_DIR)
            .join(format!("{}.plist", LAUNCHD_LABEL))
    };
    let uid = nix::unistd::getuid().as_raw();

    let bootstrap = launchd_bootstrap_argv(uid, &plist_path, scope);
    let mut cmd = std::process::Command::new("launchctl");
    cmd.args(&bootstrap);
    match crate::command_output_with_timeout(&mut cmd, crate::COMMAND_TIMEOUT) {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let detail = crate::stderr_lossy_trimmed(&output);
            printer.status_simple(
                Role::Warn,
                format!(
                    "launchctl bootstrap failed: {}",
                    crate::output::collapse_to_subject_line(&detail)
                ),
            );
            printer.hint("Run from a GUI login session, or start later with: cfgd daemon install");
            return Ok(false);
        }
        Err(e) => {
            printer.status_simple(
                Role::Warn,
                format!(
                    "launchctl bootstrap failed: {}",
                    crate::output::collapse_to_subject_line(&e)
                ),
            );
            printer.hint("Run from a GUI login session, or start later with: cfgd daemon install");
            return Ok(false);
        }
    }

    let enable = launchd_enable_argv(uid, scope);
    let mut cmd = std::process::Command::new("launchctl");
    cmd.args(&enable);
    match crate::command_output_with_timeout(&mut cmd, crate::COMMAND_TIMEOUT) {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let detail = crate::stderr_lossy_trimmed(&output);
            printer.status_simple(
                Role::Warn,
                format!(
                    "launchctl enable failed: {}",
                    crate::output::collapse_to_subject_line(&detail)
                ),
            );
            return Ok(false);
        }
        Err(e) => {
            printer.status_simple(
                Role::Warn,
                format!(
                    "launchctl enable failed: {}",
                    crate::output::collapse_to_subject_line(&e)
                ),
            );
            return Ok(false);
        }
    }

    printer.status_simple(Role::Ok, "Daemon service started");
    Ok(true)
}

/// Build the `launchctl bootout` argv that unloads and stops the service —
/// the inverse of `launchd_bootstrap_argv`. Split out so the command
/// construction is testable without invoking `launchctl`.
///
/// `bootout gui/<uid> <plist>` mirrors the user bootstrap form;
/// `bootout system <plist>` mirrors the system bootstrap form.
#[cfg(unix)]
pub(crate) fn launchd_bootout_argv(
    uid: u32,
    plist_path: &Path,
    scope: crate::Scope,
) -> Vec<String> {
    if scope == crate::Scope::System {
        vec![
            "bootout".to_string(),
            "system".to_string(),
            plist_path.display().to_string(),
        ]
    } else {
        vec![
            "bootout".to_string(),
            format!("gui/{uid}"),
            plist_path.display().to_string(),
        ]
    }
}

/// Unload and stop the running launchd service so `uninstall` leaves no orphan
/// daemon process behind — the inverse of `start_launchd_service`. Best-effort:
/// a missing `launchctl` or a headless session (nothing was ever loaded) is
/// surfaced as a warning rather than aborting the uninstall flow. Must run
/// while the plist still exists (before removal).
#[cfg(unix)]
pub(crate) fn stop_launchd_service(printer: &Printer, scope: crate::Scope) {
    // A test with a scoped HOME override must never run a real `launchctl`
    // against the runner's session. The argv builder (`launchd_bootout_argv`)
    // carries command-construction coverage; skip the side-effecting call here.
    if crate::test_home_override().is_some() {
        return;
    }
    if !crate::command_available("launchctl") {
        printer
            .status(Role::Warn, "launchctl not found")
            .detail("plist removed but daemon may still be running");
        let hint = if scope == crate::Scope::System {
            "Stop it later with: launchctl bootout system /Library/LaunchDaemons/com.cfgd.daemon.plist".to_string()
        } else {
            "Stop it later from a GUI login session with: launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.cfgd.daemon.plist".to_string()
        };
        printer.hint(hint);
        return;
    }

    let home = crate::expand_tilde(Path::new("~"));
    let plist_path = if scope == crate::Scope::System {
        std::path::PathBuf::from(LAUNCHD_DAEMONS_DIR).join(format!("{}.plist", LAUNCHD_LABEL))
    } else {
        home.join(LAUNCHD_AGENTS_DIR)
            .join(format!("{}.plist", LAUNCHD_LABEL))
    };
    let uid = nix::unistd::getuid().as_raw();

    let bootout = launchd_bootout_argv(uid, &plist_path, scope);
    let mut cmd = std::process::Command::new("launchctl");
    cmd.args(&bootout);
    match crate::command_output_with_timeout(&mut cmd, crate::COMMAND_TIMEOUT) {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let detail = crate::stderr_lossy_trimmed(&output);
            printer.status_simple(
                Role::Warn,
                format!(
                    "launchctl bootout failed: {}",
                    crate::output::collapse_to_subject_line(&detail)
                ),
            );
        }
        Err(e) => {
            printer.status_simple(
                Role::Warn,
                format!(
                    "launchctl bootout failed: {}",
                    crate::output::collapse_to_subject_line(&e)
                ),
            );
        }
    }
}

#[cfg(unix)]
pub(crate) fn uninstall_launchd_service(printer: &Printer, scope: crate::Scope) -> Result<()> {
    let home = crate::expand_tilde(Path::new("~"));
    let plist_path = if scope == crate::Scope::System {
        std::path::PathBuf::from(LAUNCHD_DAEMONS_DIR).join(format!("{}.plist", LAUNCHD_LABEL))
    } else {
        home.join(LAUNCHD_AGENTS_DIR)
            .join(format!("{}.plist", LAUNCHD_LABEL))
    };

    // Bootout BEFORE removing the plist so launchd can unload the agent/daemon it
    // still knows about — otherwise the running daemon is orphaned.
    stop_launchd_service(printer, scope);

    if plist_path.exists() {
        std::fs::remove_file(&plist_path).map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("remove plist: {}", e),
        })?;
        tracing::info!(path = %plist_path.posix(), "removed launchd service");
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
    fn install_then_uninstall_launchd_service_writes_and_removes_plist() {
        let home_dir = TempDir::new().expect("tempdir");
        let _home_g = crate::with_test_home_guard(home_dir.path());

        let config = home_dir.path().join("cfgd.yaml");
        std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\n").expect("write config");

        install_launchd_service(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &config,
            Some("ws"),
            crate::Scope::User,
            &DaemonDirOverrides::default(),
        )
        .expect("install");

        let plist_path = home_dir
            .path()
            .join(LAUNCHD_AGENTS_DIR)
            .join(format!("{}.plist", LAUNCHD_LABEL));
        let plist = std::fs::read_to_string(&plist_path).expect("read plist");
        assert!(plist.contains("<string>/usr/local/bin/cfgd</string>"));
        assert!(plist.contains("<string>--profile</string>"));
        assert!(plist.contains("<string>ws</string>"));

        let printer = Printer::for_test().0;
        uninstall_launchd_service(&printer, crate::Scope::User).expect("uninstall");
        assert!(!plist_path.exists());

        // Second uninstall is a no-op (file absent).
        uninstall_launchd_service(&printer, crate::Scope::User).expect("idempotent uninstall");
    }

    #[test]
    fn generate_launchd_plist_includes_binary_and_config_paths() {
        let plist = generate_launchd_plist(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &PathBuf::from("/etc/cfgd/config.yaml"),
            None,
            &PathBuf::from("/Users/tj"),
            crate::Scope::User,
            &DaemonDirOverrides::default(),
        );
        assert!(plist.contains("<string>/usr/local/bin/cfgd</string>"));
        assert!(plist.contains("<string>/etc/cfgd/config.yaml</string>"));
        assert!(plist.contains("<string>--quiet</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("/Users/tj/Library/Logs/cfgd.log"));
        assert!(plist.contains("/Users/tj/Library/Logs/cfgd.err"));
        assert!(!plist.contains("--profile"));
    }

    #[test]
    fn generate_launchd_plist_emits_profile_args_when_set() {
        let plist = generate_launchd_plist(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &PathBuf::from("/etc/cfgd/config.yaml"),
            Some("workstation"),
            &PathBuf::from("/Users/tj"),
            crate::Scope::User,
            &DaemonDirOverrides::default(),
        );
        assert!(plist.contains("<string>--profile</string>"));
        assert!(plist.contains("<string>workstation</string>"));
    }

    #[test]
    fn launchd_bootstrap_argv_targets_gui_domain_with_plist() {
        let argv = launchd_bootstrap_argv(
            501,
            &PathBuf::from("/Users/tj/Library/LaunchAgents/x.plist"),
            crate::Scope::User,
        );
        assert_eq!(
            argv,
            [
                "bootstrap",
                "gui/501",
                "/Users/tj/Library/LaunchAgents/x.plist",
            ]
        );
    }

    #[test]
    fn launchd_bootout_argv_targets_gui_domain_with_plist() {
        let argv = launchd_bootout_argv(
            501,
            &PathBuf::from("/Users/tj/Library/LaunchAgents/x.plist"),
            crate::Scope::User,
        );
        assert_eq!(
            argv,
            [
                "bootout",
                "gui/501",
                "/Users/tj/Library/LaunchAgents/x.plist",
            ]
        );
    }

    #[test]
    fn launchd_enable_argv_targets_label_in_gui_domain() {
        let argv = launchd_enable_argv(501, crate::Scope::User);
        assert_eq!(argv, ["enable", &format!("gui/501/{LAUNCHD_LABEL}")]);
    }

    #[test]
    fn generate_launchd_plist_emits_required_launchd_keys() {
        let plist = generate_launchd_plist(
            &PathBuf::from("/cfgd"),
            &PathBuf::from("/c.yaml"),
            None,
            &PathBuf::from("/h"),
            crate::Scope::User,
            &DaemonDirOverrides::default(),
        );
        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains("<key>ProgramArguments</key>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>StandardOutPath</key>"));
        assert!(plist.contains("<key>StandardErrorPath</key>"));
    }

    #[test]
    fn generate_launchd_plist_system_scope_adds_scope_flag_and_system_log_paths() {
        let plist = generate_launchd_plist(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &PathBuf::from("/etc/cfgd/config.yaml"),
            None,
            &PathBuf::from("/root"),
            crate::Scope::System,
            &DaemonDirOverrides::default(),
        );
        assert!(plist.contains("<string>--scope</string>"));
        assert!(plist.contains("<string>system</string>"));
        assert!(plist.contains("/var/log/cfgd.log"));
        assert!(plist.contains("/var/log/cfgd.err"));
        assert!(!plist.contains("/root/Library/Logs"));
    }

    #[test]
    fn launchd_bootstrap_argv_system_scope_targets_system_domain() {
        let argv = launchd_bootstrap_argv(
            0,
            &PathBuf::from("/Library/LaunchDaemons/com.cfgd.daemon.plist"),
            crate::Scope::System,
        );
        assert_eq!(
            argv,
            [
                "bootstrap",
                "system",
                "/Library/LaunchDaemons/com.cfgd.daemon.plist"
            ]
        );
    }

    #[test]
    fn launchd_bootout_argv_system_scope_targets_system_domain() {
        let argv = launchd_bootout_argv(
            0,
            &PathBuf::from("/Library/LaunchDaemons/com.cfgd.daemon.plist"),
            crate::Scope::System,
        );
        assert_eq!(
            argv,
            [
                "bootout",
                "system",
                "/Library/LaunchDaemons/com.cfgd.daemon.plist"
            ]
        );
    }

    #[test]
    fn launchd_enable_argv_system_scope_targets_system_domain() {
        let argv = launchd_enable_argv(0, crate::Scope::System);
        assert_eq!(argv, ["enable", &format!("system/{LAUNCHD_LABEL}")]);
    }

    /// Same contract as the systemd unit: the plist is the installed daemon's
    /// only source of argv.
    #[test]
    fn the_plist_carries_the_state_and_runtime_dir_flags() {
        let plist = generate_launchd_plist(
            Path::new("/usr/local/bin/cfgd"),
            Path::new("/Users/t/.config/cfgd/cfgd.yaml"),
            None,
            Path::new("/Users/t"),
            crate::Scope::User,
            &DaemonDirOverrides {
                state_dir: Some(PathBuf::from("/Users/t/state")),
                runtime_dir: Some(PathBuf::from("/Users/t/run")),
            },
        );
        assert!(plist.contains("<string>--state-dir</string>"), "{plist}");
        assert!(plist.contains("<string>/Users/t/state</string>"), "{plist}");
        assert!(plist.contains("<string>--runtime-dir</string>"), "{plist}");
        assert!(plist.contains("<string>/Users/t/run</string>"), "{plist}");
    }

    #[test]
    fn the_plist_omits_the_dir_flags_that_were_not_set() {
        let plist = generate_launchd_plist(
            Path::new("/usr/local/bin/cfgd"),
            Path::new("/Users/t/.config/cfgd/cfgd.yaml"),
            None,
            Path::new("/Users/t"),
            crate::Scope::User,
            &DaemonDirOverrides::default(),
        );
        assert!(!plist.contains("--state-dir"));
        assert!(!plist.contains("--runtime-dir"));
    }

    #[test]
    fn plist_values_are_xml_escaped() {
        let plist = generate_launchd_plist(
            Path::new("/usr/local/bin/cfgd"),
            Path::new("/Users/t/a&b/cfgd.yaml"),
            Some("work<1>"),
            Path::new("/Users/t"),
            crate::Scope::User,
            &DaemonDirOverrides::default(),
        );
        // An unescaped `&` makes the plist unparseable, and launchd then fails
        // to load a service that looks installed.
        assert!(plist.contains("/Users/t/a&amp;b/cfgd.yaml"), "{plist}");
        assert!(plist.contains("work&lt;1&gt;"), "{plist}");
    }
}

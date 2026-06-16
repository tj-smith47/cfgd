use super::super::*;
use crate::PathDisplayExt;

/// Generate systemd unit file content for the daemon service.
#[cfg(unix)]
pub(crate) fn generate_systemd_unit(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    scope: crate::Scope,
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
    if scope == crate::Scope::System {
        args.push("--scope".to_string());
        args.push("system".to_string());
    }
    args.push("--quiet".to_string());
    args.push("daemon".to_string());
    let exec_start = args.join(" ");

    if scope == crate::Scope::System {
        format!(
            r#"[Unit]
Description=cfgd configuration daemon
After=network.target

[Service]
Type=simple
ExecStart={exec_start}
Restart=on-failure
RestartSec=10
ConfigurationDirectory=cfgd
StateDirectory=cfgd
CacheDirectory=cfgd
RuntimeDirectory=cfgd

[Install]
WantedBy=multi-user.target"#
        )
    } else {
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
}

#[cfg(unix)]
pub(crate) fn install_systemd_service(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    scope: crate::Scope,
) -> Result<()> {
    let unit_dir = if scope == crate::Scope::System {
        std::path::PathBuf::from(SYSTEMD_SYSTEM_DIR)
    } else {
        let home = crate::expand_tilde(Path::new("~"));
        home.join(SYSTEMD_USER_DIR)
    };
    std::fs::create_dir_all(&unit_dir).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("create systemd unit dir: {}", e),
    })?;

    let unit_path = unit_dir.join("cfgd.service");
    let config_abs =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());

    let unit = generate_systemd_unit(binary, &config_abs, profile, scope);

    crate::atomic_write_str(&unit_path, &unit).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("write unit file: {}", e),
    })?;

    tracing::info!(path = %unit_path.posix(), "installed systemd service");
    Ok(())
}

/// Build the `systemctl` argv vectors that reload the unit cache and
/// enable+start the service. Split out so the command construction is testable
/// without a running systemd bus.
#[cfg(unix)]
pub(crate) fn systemd_start_argv(scope: crate::Scope) -> [Vec<&'static str>; 2] {
    if scope == crate::Scope::System {
        [
            vec!["daemon-reload"],
            vec!["enable", "--now", "cfgd.service"],
        ]
    } else {
        [
            vec!["--user", "daemon-reload"],
            vec!["--user", "enable", "--now", "cfgd.service"],
        ]
    }
}

/// How `start_systemd_service` should source `XDG_RUNTIME_DIR` for the
/// `systemctl --user` calls. A headless bootstrap (ssh non-login shell, CI,
/// provisioning script) typically has no `XDG_RUNTIME_DIR`, so `systemctl --user`
/// cannot find the user bus; resolving the dir up front lets the caller either
/// self-set it or warn clearly instead of surfacing the cryptic bus error.
#[cfg(unix)]
#[derive(Debug)]
enum RuntimeDirPlan {
    /// `XDG_RUNTIME_DIR` is already set to an existing directory — inherit it.
    AlreadySet,
    /// `XDG_RUNTIME_DIR` was absent/invalid but `/run/user/<uid>` exists — set it.
    Derived(std::path::PathBuf),
    /// No usable runtime dir exists; no user session bus is available.
    Missing,
}

/// Decide how to source `XDG_RUNTIME_DIR` for `systemctl --user`. Pure so the
/// decision is unit-testable without touching the real `/run/user` or env:
/// a non-empty `xdg` pointing at an existing dir wins; otherwise the
/// `/run/user/<uid>` `fallback` is used when it exists; otherwise nothing.
#[cfg(unix)]
fn resolve_runtime_dir(xdg: Option<&str>, fallback: &std::path::Path) -> RuntimeDirPlan {
    if let Some(dir) = xdg
        && !dir.is_empty()
        && std::path::Path::new(dir).is_dir()
    {
        return RuntimeDirPlan::AlreadySet;
    }
    if fallback.is_dir() {
        return RuntimeDirPlan::Derived(fallback.to_path_buf());
    }
    RuntimeDirPlan::Missing
}

/// Enable and start the just-installed systemd service so the daemon runs
/// immediately rather than only after the next login/boot. Best-effort: when the
/// user has no session systemd (no lingering, no active login session) the
/// enable-now cannot take effect, so the failure is surfaced as a warning plus
/// an actionable hint rather than aborting the calling init flow.
///
/// Returns `Ok(true)` when the service was enabled and started, `Ok(false)`
/// when it was installed but could not be started now (the caller reports the
/// real state instead of over-claiming `started`).
#[cfg(unix)]
pub(crate) fn start_systemd_service(printer: &Printer, scope: crate::Scope) -> Result<bool> {
    if !crate::command_available("systemctl") {
        let hint_cmd = if scope == crate::Scope::System {
            "systemctl enable --now cfgd.service"
        } else {
            "systemctl --user enable --now cfgd.service"
        };
        printer.status_simple(
            Role::Warn,
            "systemctl not found — daemon installed but not started",
        );
        printer.hint(format!("Start it later with: {}", hint_cmd));
        return Ok(false);
    }

    // XDG_RUNTIME_DIR resolution is only needed for user-scope systemctl --user.
    let runtime_dir = if scope == crate::Scope::User {
        let fallback =
            std::path::PathBuf::from(format!("/run/user/{}", nix::unistd::geteuid().as_raw()));
        match resolve_runtime_dir(std::env::var("XDG_RUNTIME_DIR").ok().as_deref(), &fallback) {
            RuntimeDirPlan::AlreadySet => None,
            RuntimeDirPlan::Derived(dir) => {
                printer.status_simple(
                    Role::Info,
                    format!(
                        "XDG_RUNTIME_DIR unset — using {} for the user service bus",
                        dir.posix()
                    ),
                );
                Some(dir)
            }
            RuntimeDirPlan::Missing => {
                printer.status_simple(
                    Role::Warn,
                    "no user session bus (XDG_RUNTIME_DIR unset and /run/user/<uid> absent) — daemon installed but not started",
                );
                printer.hint(
                    "Enable lingering so the user service can run without an active login: loginctl enable-linger $USER, then re-run cfgd daemon install",
                );
                return Ok(false);
            }
        }
    } else {
        None
    };

    for args in systemd_start_argv(scope) {
        let mut cmd = std::process::Command::new("systemctl");
        cmd.args(&args);
        if let Some(dir) = &runtime_dir {
            cmd.env("XDG_RUNTIME_DIR", dir);
        }
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
                if scope == crate::Scope::User {
                    printer.hint(
                        "If you have no active login session, enable lingering: loginctl enable-linger $USER",
                    );
                }
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
                if scope == crate::Scope::User {
                    printer.hint(
                        "If you have no active login session, enable lingering: loginctl enable-linger $USER",
                    );
                }
                return Ok(false);
            }
        }
    }

    printer.status_simple(Role::Ok, "Daemon service started");
    Ok(true)
}

/// Build the `systemctl` argv vectors that uninstall mirrors against
/// install: stop+disable the unit, then (after the file is gone) reload the unit
/// cache. Returned together so command construction is testable without a
/// running systemd bus; the caller invokes them at the correct points
/// around the file removal (disable BEFORE the rm, daemon-reload AFTER).
#[cfg(unix)]
pub(crate) fn systemd_stop_argv(scope: crate::Scope) -> [Vec<&'static str>; 2] {
    if scope == crate::Scope::System {
        [
            vec!["disable", "--now", "cfgd.service"],
            vec!["daemon-reload"],
        ]
    } else {
        [
            vec!["--user", "disable", "--now", "cfgd.service"],
            vec!["--user", "daemon-reload"],
        ]
    }
}

/// Stop and disable the running systemd unit so `uninstall` leaves no
/// orphan daemon process behind — the inverse of `start_systemd_service`.
/// Best-effort: a missing `systemctl` or a session with no user systemd is
/// surfaced as a warning plus an actionable hint rather than aborting the
/// uninstall flow. Must run while the unit file still exists (before removal).
#[cfg(unix)]
pub(crate) fn stop_systemd_service(printer: &Printer, scope: crate::Scope) {
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
        let hint_cmd = if scope == crate::Scope::System {
            "systemctl disable --now cfgd.service".to_string()
        } else {
            "systemctl --user disable --now cfgd.service".to_string()
        };
        printer.hint(format!("Stop it manually with: {}", hint_cmd));
        return;
    }

    let [disable, _reload] = systemd_stop_argv(scope);
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
pub(crate) fn uninstall_systemd_service(printer: &Printer, scope: crate::Scope) -> Result<()> {
    let unit_path = if scope == crate::Scope::System {
        std::path::PathBuf::from(SYSTEMD_SYSTEM_DIR).join("cfgd.service")
    } else {
        let home = crate::expand_tilde(Path::new("~"));
        home.join(SYSTEMD_USER_DIR).join("cfgd.service")
    };

    // Stop+disable BEFORE removing the unit file so systemd can act on a unit it
    // still knows about — otherwise the running daemon is orphaned.
    stop_systemd_service(printer, scope);

    if unit_path.exists() {
        std::fs::remove_file(&unit_path).map_err(|e| DaemonError::ServiceInstallFailed {
            message: format!("remove unit file: {}", e),
        })?;
        tracing::info!(path = %unit_path.posix(), "removed systemd service");
    }

    // Reload AFTER removal so systemd drops the now-deleted unit from its view.
    if crate::test_home_override().is_none() && crate::command_available("systemctl") {
        let [_disable, reload] = systemd_stop_argv(scope);
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

    /// The deb/rpm/apk packages ship `packaging/systemd/cfgd.service`, which must
    /// stay byte-identical to what `generate_systemd_unit` emits for a system-scope
    /// `/usr/bin/cfgd` install. If the generator gains a directive or changes the
    /// ExecStart shape, this fails until the packaged unit is regenerated — the
    /// packaged service and `cfgd daemon install` can never silently drift apart.
    #[test]
    fn packaged_systemd_unit_matches_generator() {
        let generated = generate_systemd_unit(
            &PathBuf::from("/usr/bin/cfgd"),
            &PathBuf::from("/etc/cfgd/config.yaml"),
            None,
            crate::Scope::System,
        );
        let packaged_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/systemd/cfgd.service");
        let packaged = std::fs::read_to_string(&packaged_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", packaged_path.display()));
        assert_eq!(
            packaged.trim_end(),
            generated.trim_end(),
            "packaging/systemd/cfgd.service drifted from generate_systemd_unit(System); regenerate it"
        );
    }

    #[test]
    #[serial_test::serial]
    fn install_then_uninstall_systemd_service_writes_and_removes_unit() {
        let home_dir = TempDir::new().expect("tempdir");
        let _home_g = crate::with_test_home_guard(home_dir.path());

        let config = home_dir.path().join("cfgd.yaml");
        std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\n").expect("write config");

        install_systemd_service(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &config,
            Some("ws"),
            crate::Scope::User,
        )
        .expect("install");

        let unit_path = home_dir.path().join(SYSTEMD_USER_DIR).join("cfgd.service");
        let unit = std::fs::read_to_string(&unit_path).expect("read unit");
        assert!(unit.contains("ExecStart=/usr/local/bin/cfgd"));
        assert!(unit.contains("--profile ws"));

        let printer = Printer::new(crate::output::Verbosity::Quiet);
        uninstall_systemd_service(&printer, crate::Scope::User).expect("uninstall");
        assert!(!unit_path.exists());

        uninstall_systemd_service(&printer, crate::Scope::User).expect("idempotent uninstall");
    }

    #[test]
    fn generate_systemd_unit_includes_binary_config_and_quiet_daemon_flags() {
        let unit = generate_systemd_unit(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &PathBuf::from("/etc/cfgd/config.yaml"),
            None,
            crate::Scope::User,
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
    fn resolve_runtime_dir_already_set_when_xdg_dir_exists() {
        let xdg = TempDir::new().expect("tempdir");
        let fallback = PathBuf::from("/nonexistent/run/user/0");
        assert!(matches!(
            resolve_runtime_dir(Some(&xdg.path().display().to_string()), &fallback),
            RuntimeDirPlan::AlreadySet
        ));
    }

    #[test]
    fn resolve_runtime_dir_derives_when_xdg_empty() {
        let fallback = TempDir::new().expect("tempdir");
        match resolve_runtime_dir(Some(""), fallback.path()) {
            RuntimeDirPlan::Derived(dir) => assert_eq!(dir, fallback.path()),
            other => panic!("expected Derived, got {other:?}"),
        }
    }

    #[test]
    fn resolve_runtime_dir_derives_when_xdg_points_at_missing_dir() {
        let fallback = TempDir::new().expect("tempdir");
        match resolve_runtime_dir(Some("/nonexistent/xdg/abc"), fallback.path()) {
            RuntimeDirPlan::Derived(dir) => assert_eq!(dir, fallback.path()),
            other => panic!("expected Derived, got {other:?}"),
        }
    }

    #[test]
    fn resolve_runtime_dir_derives_when_xdg_none_and_fallback_exists() {
        let fallback = TempDir::new().expect("tempdir");
        match resolve_runtime_dir(None, fallback.path()) {
            RuntimeDirPlan::Derived(dir) => assert_eq!(dir, fallback.path()),
            other => panic!("expected Derived, got {other:?}"),
        }
    }

    #[test]
    fn resolve_runtime_dir_missing_when_xdg_none_and_fallback_absent() {
        let fallback = PathBuf::from("/nonexistent/run/user/4242");
        assert!(matches!(
            resolve_runtime_dir(None, &fallback),
            RuntimeDirPlan::Missing
        ));
    }

    #[test]
    fn resolve_runtime_dir_prefers_xdg_over_fallback_when_both_exist() {
        let xdg = TempDir::new().expect("tempdir");
        let fallback = TempDir::new().expect("tempdir");
        assert!(matches!(
            resolve_runtime_dir(Some(&xdg.path().display().to_string()), fallback.path()),
            RuntimeDirPlan::AlreadySet
        ));
    }

    #[test]
    fn systemd_start_argv_reloads_then_enables_now() {
        let [reload, enable] = systemd_start_argv(crate::Scope::User);
        assert_eq!(reload, ["--user", "daemon-reload"]);
        assert_eq!(enable, ["--user", "enable", "--now", "cfgd.service"]);
    }

    #[test]
    fn systemd_stop_argv_disables_now_then_reloads() {
        let [disable, reload] = systemd_stop_argv(crate::Scope::User);
        assert_eq!(disable, ["--user", "disable", "--now", "cfgd.service"]);
        assert_eq!(reload, ["--user", "daemon-reload"]);
    }

    #[test]
    fn generate_systemd_unit_emits_profile_args_when_set() {
        let unit = generate_systemd_unit(
            &PathBuf::from("/cfgd"),
            &PathBuf::from("/c.yaml"),
            Some("workstation"),
            crate::Scope::User,
        );
        assert!(
            unit.contains("ExecStart=/cfgd --config /c.yaml --profile workstation --quiet daemon")
        );
    }

    #[test]
    fn generate_systemd_unit_system_scope_adds_scope_flag_and_resource_dirs() {
        let unit = generate_systemd_unit(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &PathBuf::from("/etc/cfgd/config.yaml"),
            None,
            crate::Scope::System,
        );
        assert!(unit.contains(
            "ExecStart=/usr/local/bin/cfgd --config /etc/cfgd/config.yaml --scope system --quiet daemon"
        ));
        assert!(unit.contains("ConfigurationDirectory=cfgd"));
        assert!(unit.contains("StateDirectory=cfgd"));
        assert!(unit.contains("CacheDirectory=cfgd"));
        assert!(unit.contains("RuntimeDirectory=cfgd"));
        assert!(unit.contains("WantedBy=multi-user.target"));
        assert!(!unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn systemd_start_argv_system_scope_omits_user_flag() {
        let [reload, enable] = systemd_start_argv(crate::Scope::System);
        assert_eq!(reload, ["daemon-reload"]);
        assert_eq!(enable, ["enable", "--now", "cfgd.service"]);
    }

    #[test]
    fn systemd_stop_argv_system_scope_omits_user_flag() {
        let [disable, reload] = systemd_stop_argv(crate::Scope::System);
        assert_eq!(disable, ["disable", "--now", "cfgd.service"]);
        assert_eq!(reload, ["daemon-reload"]);
    }

    #[test]
    fn install_systemd_service_canonicalizes_relative_config_path_in_unit() {
        let home_dir = TempDir::new().expect("tempdir");
        let _home_g = crate::with_test_home_guard(home_dir.path());

        // A bare relative filename must land in the unit as an absolute path so
        // the daemon resolves the same config regardless of its launch CWD.
        let config = home_dir.path().join("relcfg.yaml");
        std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\n").expect("write config");
        let canon = std::fs::canonicalize(&config).expect("canonicalize config");

        install_systemd_service(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &config,
            None,
            crate::Scope::User,
        )
        .expect("install");

        let unit_path = home_dir.path().join(SYSTEMD_USER_DIR).join("cfgd.service");
        let unit = std::fs::read_to_string(&unit_path).expect("read unit");
        assert!(
            unit.contains(&format!(
                "ExecStart=/usr/local/bin/cfgd --config {} --quiet daemon",
                canon.display()
            )),
            "ExecStart must carry the canonicalized config path: {unit}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn start_systemd_service_user_scope_warns_and_hints_when_systemctl_missing() {
        // Point PATH at an empty dir so `command_available("systemctl")` is false,
        // driving the not-found branch without any real systemctl shell-out.
        let empty = TempDir::new().expect("tempdir");
        let empty_path = empty.path().display().to_string();
        let _path_g = crate::test_helpers::EnvVarGuard::set("PATH", &empty_path);

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let started = start_systemd_service(&printer, crate::Scope::User).expect("ok(false)");
        assert!(!started, "missing systemctl cannot start the service");

        let out = buf.lock().expect("lock buf").clone();
        assert!(
            out.contains("systemctl not found — daemon installed but not started"),
            "expected not-found warning: {out}"
        );
        assert!(
            out.contains("Start it later with: systemctl --user enable --now cfgd.service"),
            "user-scope hint must carry the --user form: {out}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn start_systemd_service_system_scope_hint_omits_user_flag_when_systemctl_missing() {
        let empty = TempDir::new().expect("tempdir");
        let empty_path = empty.path().display().to_string();
        let _path_g = crate::test_helpers::EnvVarGuard::set("PATH", &empty_path);

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let started = start_systemd_service(&printer, crate::Scope::System).expect("ok(false)");
        assert!(!started);

        let out = buf.lock().expect("lock buf").clone();
        assert!(
            out.contains("Start it later with: systemctl enable --now cfgd.service"),
            "system-scope hint must be the bare (no --user) form: {out}"
        );
        assert!(
            !out.contains("--user"),
            "system-scope hint must not contain --user: {out}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn stop_systemd_service_is_a_noop_under_test_home_override() {
        // The test-home guard sets the override that makes stop_systemd_service
        // skip the side-effecting systemctl call entirely — it must emit nothing.
        let home_dir = TempDir::new().expect("tempdir");
        let _home_g = crate::with_test_home_guard(home_dir.path());

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        stop_systemd_service(&printer, crate::Scope::User);

        let out = buf.lock().expect("lock buf").clone();
        assert!(
            out.is_empty(),
            "stop under test-home override must produce no output, got: {out}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn stop_systemd_service_user_scope_warns_and_hints_when_systemctl_missing() {
        // No test-home override (so the early-return is not taken) plus an empty
        // PATH drives the systemctl-not-found branch without a real shell-out.
        let empty = TempDir::new().expect("tempdir");
        let empty_path = empty.path().display().to_string();
        let _path_g = crate::test_helpers::EnvVarGuard::set("PATH", &empty_path);

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        stop_systemd_service(&printer, crate::Scope::User);

        let out = buf.lock().expect("lock buf").clone();
        assert!(
            out.contains("systemctl not found — unit file removed but daemon may still be running"),
            "expected not-found warning: {out}"
        );
        assert!(
            out.contains("Stop it manually with: systemctl --user disable --now cfgd.service"),
            "user-scope hint must carry the --user form: {out}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn stop_systemd_service_system_scope_hint_omits_user_flag_when_systemctl_missing() {
        let empty = TempDir::new().expect("tempdir");
        let empty_path = empty.path().display().to_string();
        let _path_g = crate::test_helpers::EnvVarGuard::set("PATH", &empty_path);

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        stop_systemd_service(&printer, crate::Scope::System);

        let out = buf.lock().expect("lock buf").clone();
        assert!(
            out.contains("Stop it manually with: systemctl disable --now cfgd.service"),
            "system-scope hint must be the bare (no --user) form: {out}"
        );
        assert!(
            !out.contains("--user"),
            "system-scope hint must not contain --user: {out}"
        );
    }
}

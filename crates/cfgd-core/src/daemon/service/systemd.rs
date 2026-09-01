use super::super::*;
use crate::PathDisplayExt;
use crate::output::Role;

/// Render one `ExecStart` token so systemd passes it to the daemon verbatim.
///
/// systemd splits `ExecStart` on whitespace unless a token is quoted, so an
/// operator-chosen `--state-dir '/srv/my state'` would otherwise reach clap as
/// two arguments and the unit would die at argument validation before the
/// daemon ever started. Three separate systemd rules apply:
///
/// - `%` introduces a unit specifier (`%h`, `%i`) — doubled to pass through.
/// - `$` introduces variable expansion — doubled to pass through.
/// - whitespace, quotes, and backslashes need a double-quoted string with
///   C-style escapes.
pub(crate) fn systemd_quote(token: &str) -> String {
    let literal = token.replace('%', "%%").replace('$', "$$");
    let needs_quotes = literal.is_empty()
        || literal
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'));
    if !needs_quotes {
        return literal;
    }
    let escaped = literal.replace('\\', r"\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Generate systemd unit file content for the daemon service.
///
/// `dirs` carries the process-level `--state-dir` / `--runtime-dir` the install
/// was invoked with. They MUST reach the unit: the installed service is a fresh
/// process with none of the invoking shell's flags, so a daemon installed from
/// `cfgd --state-dir /srv/cfgd daemon install` would otherwise write its drift
/// events and backups under the scope default while the operator's CLI reads
/// the directory they named — and the two apply locks would stop excluding
/// each other.
#[cfg(unix)]
pub(crate) fn generate_systemd_unit(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    scope: crate::Scope,
    dirs: &DaemonDirOverrides,
) -> String {
    let mut args = vec![
        binary.display().to_string(), // native-ok: argv token for this host
        "--config".to_string(),
        config_path.display().to_string(), // native-ok: argv token for this host
    ];
    if let Some(p) = profile {
        args.push("--profile".to_string());
        args.push(p.to_string());
    }
    if scope == crate::Scope::System {
        args.push("--scope".to_string());
        args.push("system".to_string());
    }
    for (flag, dir) in service_dir_flags(dirs) {
        args.push(flag.to_string());
        args.push(dir.display().to_string()); // native-ok: argv token for this host
    }
    args.push("--quiet".to_string());
    args.push("daemon".to_string());
    let exec_start = args
        .iter()
        .map(|a| systemd_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

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
    dirs: &DaemonDirOverrides,
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

    let unit = generate_systemd_unit(binary, &config_abs, profile, scope, dirs);

    crate::atomic_write_str(&unit_path, &unit).map_err(|e| DaemonError::ServiceInstallFailed {
        message: format!("write unit file: {}", e),
    })?;

    tracing::info!("daemon: installed systemd service at {}", unit_path.posix());
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
    if !crate::systemctl_available() {
        let hint_cmd = if scope == crate::Scope::System {
            "systemctl enable --now cfgd.service"
        } else {
            "systemctl --user enable --now cfgd.service"
        };
        printer
            .status(Role::Warn, "systemctl not found") // name-row-ok: the init system's own tool name, which is lowercase
            .detail(super::INSTALLED_NOT_STARTED);
        printer.hint(format!("Start it later with `{hint_cmd}`"));
        return Ok(false);
    }

    // XDG_RUNTIME_DIR resolution is only needed for user-scope systemctl --user.
    let runtime_dir = if scope == crate::Scope::User {
        let fallback =
            std::path::PathBuf::from(format!("/run/user/{}", nix::unistd::geteuid().as_raw()));
        match resolve_runtime_dir(std::env::var("XDG_RUNTIME_DIR").ok().as_deref(), &fallback) {
            RuntimeDirPlan::AlreadySet => None,
            RuntimeDirPlan::Derived(dir) => {
                printer
                    .status(Role::Info, "XDG_RUNTIME_DIR unset")
                    .detail(format!("using {} for the user service bus", dir.posix()));
                Some(dir)
            }
            RuntimeDirPlan::Missing => {
                printer
                    .status(
                        Role::Warn,
                        "No user session bus (XDG_RUNTIME_DIR unset and /run/user/<uid> absent)",
                    )
                    .detail(super::INSTALLED_NOT_STARTED);
                printer.hint(
                    "Enable lingering so the user service can run without an active login: loginctl enable-linger $USER, then re-run `cfgd daemon install`",
                );
                return Ok(false);
            }
        }
    } else {
        None
    };

    for args in systemd_start_argv(scope) {
        let mut cmd = crate::systemctl_cmd();
        cmd.args(&args);
        if let Some(dir) = &runtime_dir {
            cmd.env("XDG_RUNTIME_DIR", dir);
        }
        match crate::command_output_with_timeout(&mut cmd, crate::COMMAND_TIMEOUT) {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let detail = crate::stderr_lossy_trimmed(&output);
                printer.status_simple(
                    Role::Warn, // name-row-ok: the init system's own tool name, which is lowercase
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
                    Role::Warn, // name-row-ok: the init system's own tool name, which is lowercase
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
    if !crate::systemctl_available() {
        printer
            .status(Role::Warn, "systemctl not found") // name-row-ok: the init system's own tool name, which is lowercase
            .detail("unit file removed but daemon may still be running");
        let hint_cmd = if scope == crate::Scope::System {
            "systemctl disable --now cfgd.service".to_string()
        } else {
            "systemctl --user disable --now cfgd.service".to_string()
        };
        printer.hint(format!("Stop it manually with `{hint_cmd}`"));
        return;
    }

    let [disable, _reload] = systemd_stop_argv(scope);
    let mut cmd = crate::systemctl_cmd();
    cmd.args(&disable);
    match crate::command_output_with_timeout(&mut cmd, crate::COMMAND_TIMEOUT) {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let detail = crate::stderr_lossy_trimmed(&output);
            printer.status_simple(
                Role::Warn, // name-row-ok: the init system's own tool name, which is lowercase
                format!(
                    "systemctl {} failed: {}",
                    disable.join(" "),
                    crate::output::collapse_to_subject_line(&detail)
                ),
            );
        }
        Err(e) => {
            printer.status_simple(
                Role::Warn, // name-row-ok: the init system's own tool name, which is lowercase
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
        tracing::info!("daemon: removed systemd service at {}", unit_path.posix());
    }

    // Reload AFTER removal so systemd drops the now-deleted unit from its view.
    if crate::test_home_override().is_none() && crate::systemctl_available() {
        let [_disable, reload] = systemd_stop_argv(scope);
        let mut cmd = crate::systemctl_cmd();
        cmd.args(&reload);
        if let Ok(output) = crate::command_output_with_timeout(&mut cmd, crate::COMMAND_TIMEOUT)
            && !output.status.success()
        {
            let detail = crate::stderr_lossy_trimmed(&output);
            printer.status_simple(
                Role::Warn, // name-row-ok: the init system's own tool name, which is lowercase
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
            &DaemonDirOverrides::default(),
        );
        let packaged_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/systemd/cfgd.service");
        let packaged = std::fs::read_to_string(&packaged_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", packaged_path.display()));
        assert_eq!(
            packaged.trim_end(),
            generated.trim_end(),
            "packaging/systemd/cfgd.service drifted from generate_systemd_unit(System, &DaemonDirOverrides::default()); regenerate it"
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
            &DaemonDirOverrides::default(),
        )
        .expect("install");

        let unit_path = home_dir.path().join(SYSTEMD_USER_DIR).join("cfgd.service");
        let unit = std::fs::read_to_string(&unit_path).expect("read unit");
        assert!(unit.contains("ExecStart=/usr/local/bin/cfgd"));
        assert!(unit.contains("--profile ws"));

        let printer = Printer::for_test().0;
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
            &DaemonDirOverrides::default(),
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
    fn systemd_quote_leaves_an_ordinary_token_untouched() {
        // The packaging golden and every default install depend on this: a
        // plain path must render byte-identically to the unquoted form.
        for tok in [
            "/usr/local/bin/cfgd",
            "--config",
            "daemon",
            "/etc/cfgd/config.yaml",
        ] {
            assert_eq!(systemd_quote(tok), tok);
        }
    }

    #[test]
    fn systemd_quote_wraps_a_token_systemd_would_split() {
        assert_eq!(systemd_quote("/srv/my state"), "\"/srv/my state\"");
        assert_eq!(systemd_quote(""), "\"\"");
        assert_eq!(systemd_quote("a\tb"), "\"a\tb\"");
    }

    #[test]
    fn systemd_quote_escapes_what_systemd_reads_as_syntax() {
        // Inside a double-quoted ExecStart token systemd applies C-style
        // escapes, so `\` and `"` have to be escaped or the token ends early.
        assert_eq!(systemd_quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(systemd_quote(r"a\b"), r#""a\\b""#);
        // `%` is a unit specifier and `$` a variable expansion, both expanded
        // whether or not the token is quoted — doubling is the only escape.
        assert_eq!(systemd_quote("/srv/100%cfgd"), "/srv/100%%cfgd");
        assert_eq!(systemd_quote("/srv/$HOME"), "/srv/$$HOME");
    }

    #[test]
    fn generate_systemd_unit_quotes_a_state_dir_containing_a_space() {
        // `cfgd --state-dir '/srv/my state' daemon install` previously wrote an
        // ExecStart systemd split into two arguments; the service then died at
        // clap validation before the daemon ever ran.
        let unit = generate_systemd_unit(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &PathBuf::from("/etc/cfgd/config.yaml"),
            None,
            crate::Scope::User,
            &DaemonDirOverrides {
                state_dir: Some(PathBuf::from("/srv/my state")),
                runtime_dir: Some(PathBuf::from("/run/my cfgd")),
                cache_dir: None,
            },
        );
        assert!(
            unit.contains(
                "ExecStart=/usr/local/bin/cfgd --config /etc/cfgd/config.yaml \
                 --state-dir \"/srv/my state\" --runtime-dir \"/run/my cfgd\" --quiet daemon"
            ),
            "got: {unit}"
        );
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
            &DaemonDirOverrides::default(),
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
            &DaemonDirOverrides::default(),
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
            &DaemonDirOverrides::default(),
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
        // Point PATH at an empty dir so `systemctl_available()` is false,
        // driving the not-found branch without any real systemctl shell-out.
        // The seam is cleared too — it answers before PATH does.
        let _seam = crate::test_helpers::EnvVarGuard::unset(crate::SYSTEMCTL_BIN_ENV);
        let empty = TempDir::new().expect("tempdir");
        let empty_path = empty.path().display().to_string();
        // Holds the spawn-exclusion lock across the empty-PATH window so no
        // concurrent script test resolves its interpreter while PATH is bare.
        let _spawn_excl = crate::test_helpers::path_env_mutation_guard();
        let _path_g = crate::test_helpers::EnvVarGuard::set("PATH", &empty_path);

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let started = start_systemd_service(&printer, crate::Scope::User).expect("ok(false)");
        assert!(!started, "missing systemctl cannot start the service");

        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("systemctl not found — daemon installed but not started"),
            "expected not-found warning: {out}"
        );
        assert!(
            out.contains("Start it later with `systemctl --user enable --now cfgd.service`"),
            "user-scope hint must carry the --user form: {out}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn start_systemd_service_system_scope_hint_omits_user_flag_when_systemctl_missing() {
        let empty = TempDir::new().expect("tempdir");
        let empty_path = empty.path().display().to_string();
        // Holds the spawn-exclusion lock across the empty-PATH window so no
        // concurrent script test resolves its interpreter while PATH is bare.
        let _spawn_excl = crate::test_helpers::path_env_mutation_guard();
        let _path_g = crate::test_helpers::EnvVarGuard::set("PATH", &empty_path);

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let started = start_systemd_service(&printer, crate::Scope::System).expect("ok(false)");
        assert!(!started);

        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("Start it later with `systemctl enable --now cfgd.service`"),
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

        let out = crate::test_helpers::captured_text(&buf);
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
        // Holds the spawn-exclusion lock across the empty-PATH window so no
        // concurrent script test resolves its interpreter while PATH is bare.
        let _spawn_excl = crate::test_helpers::path_env_mutation_guard();
        let _path_g = crate::test_helpers::EnvVarGuard::set("PATH", &empty_path);

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        stop_systemd_service(&printer, crate::Scope::User);

        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("systemctl not found — unit file removed but daemon may still be running"),
            "expected not-found warning: {out}"
        );
        assert!(
            out.contains("Stop it manually with `systemctl --user disable --now cfgd.service`"),
            "user-scope hint must carry the --user form: {out}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn stop_systemd_service_system_scope_hint_omits_user_flag_when_systemctl_missing() {
        let empty = TempDir::new().expect("tempdir");
        let empty_path = empty.path().display().to_string();
        // Holds the spawn-exclusion lock across the empty-PATH window so no
        // concurrent script test resolves its interpreter while PATH is bare.
        let _spawn_excl = crate::test_helpers::path_env_mutation_guard();
        let _path_g = crate::test_helpers::EnvVarGuard::set("PATH", &empty_path);

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        stop_systemd_service(&printer, crate::Scope::System);

        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("Stop it manually with `systemctl disable --now cfgd.service`"),
            "system-scope hint must be the bare (no --user) form: {out}"
        );
        assert!(
            !out.contains("--user"),
            "system-scope hint must not contain --user: {out}"
        );
    }

    /// The installed unit is a fresh process: whatever `--state-dir` /
    /// `--runtime-dir` the install ran under has to be baked into ExecStart, or
    /// the daemon and the operator's CLI resolve different directories and
    /// their apply locks stop excluding each other.
    #[test]
    fn the_unit_carries_the_state_and_runtime_dir_flags() {
        let dirs = DaemonDirOverrides {
            state_dir: Some(PathBuf::from("/srv/cfgd/state")),
            runtime_dir: Some(PathBuf::from("/srv/cfgd/run")),
            ..Default::default()
        };
        let unit = generate_systemd_unit(
            Path::new("/usr/local/bin/cfgd"),
            Path::new("/etc/cfgd/cfgd.yaml"),
            Some("node"),
            crate::Scope::System,
            &dirs,
        );
        assert!(
            unit.contains("--state-dir /srv/cfgd/state"),
            "unit dropped --state-dir: {unit}"
        );
        assert!(
            unit.contains("--runtime-dir /srv/cfgd/run"),
            "unit dropped --runtime-dir: {unit}"
        );
        // The flags are global (pre-subcommand) on the real CLI, so they must
        // land before `daemon`.
        let exec = unit
            .lines()
            .find(|l| l.starts_with("ExecStart="))
            .expect("ExecStart line");
        assert!(
            exec.ends_with("--quiet daemon"),
            "the subcommand must stay last: {exec}"
        );
    }

    #[test]
    fn the_unit_omits_the_dir_flags_that_were_not_set() {
        let dirs = DaemonDirOverrides {
            state_dir: Some(PathBuf::from("/srv/cfgd/state")),
            runtime_dir: None,
            ..Default::default()
        };
        let unit = generate_systemd_unit(
            Path::new("/usr/local/bin/cfgd"),
            Path::new("/etc/cfgd/cfgd.yaml"),
            None,
            crate::Scope::User,
            &dirs,
        );
        assert!(unit.contains("--state-dir /srv/cfgd/state"));
        assert!(
            !unit.contains("--runtime-dir"),
            "an unset dir must fall through to the env/scope default, not be pinned: {unit}"
        );
    }

    #[test]
    fn an_install_without_dir_overrides_bakes_no_dir_flags() {
        let unit = generate_systemd_unit(
            Path::new("/usr/local/bin/cfgd"),
            Path::new("/etc/cfgd/cfgd.yaml"),
            None,
            crate::Scope::User,
            &DaemonDirOverrides::default(),
        );
        assert!(!unit.contains("--state-dir"));
        assert!(!unit.contains("--runtime-dir"));
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn the_installed_unit_file_carries_the_dir_flags() {
        let home_dir = TempDir::new().expect("tempdir");
        let _home_g = crate::with_test_home_guard(home_dir.path());
        let config = home_dir.path().join("cfgd.yaml");
        std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\n").expect("write config");
        let state = home_dir.path().join("state");
        let runtime = home_dir.path().join("run");

        install_systemd_service(
            &PathBuf::from("/usr/local/bin/cfgd"),
            &config,
            None,
            crate::Scope::User,
            &DaemonDirOverrides {
                state_dir: Some(state.clone()),
                runtime_dir: Some(runtime.clone()),
                ..Default::default()
            },
        )
        .expect("install");

        let unit =
            std::fs::read_to_string(home_dir.path().join(SYSTEMD_USER_DIR).join("cfgd.service"))
                .expect("read unit");
        assert!(unit.contains(&format!("--state-dir {}", state.display())));
        assert!(unit.contains(&format!("--runtime-dir {}", runtime.display())));
    }
}

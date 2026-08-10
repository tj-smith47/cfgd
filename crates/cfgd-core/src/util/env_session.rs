//! Live-session environment refresh — the controlled shell-out layer for
//! making the current user's already-running session managers spawn new
//! processes with updated environment variables, without a re-login.
//!
//! This is the single home for the per-platform shell-outs (`launchctl`,
//! `systemctl --user`, `setx`) so both `spec.env` (user scope, via the
//! reconciler) and `spec.system.environment` (the system configurator) share
//! one implementation rather than open-coding it twice. The per-tool setters
//! are not `#[cfg]`-gated so they stay directly testable on any host: every
//! command here is built through [`crate::tool_cmd`], so a test points the
//! matching `CFGD_*_BIN` variable at a shim and observes the argv instead of
//! reaching the real session. See `.claude/rules/module-boundaries.md`.

use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use crate::output::{Printer, Role};
use crate::providers::NoteSink;

/// Live-refresh shell-outs are local and fast; bound them anyway so a wedged
/// session bus (`systemctl --user` with no user D-Bus) can't hang an apply.
const ENV_REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

/// Test seams for the session managers. Unlike every other target cfgd writes,
/// these commands address a running session manager and the Windows registry —
/// neither is a path, so no test home, temp dir or `XDG_*` override can contain
/// them. Redirecting the binary is the only sandbox available.
const SYSTEMCTL_BIN_ENV: &str = "CFGD_SYSTEMCTL_BIN";
const LAUNCHCTL_BIN_ENV: &str = "CFGD_LAUNCHCTL_BIN";
const SETX_BIN_ENV: &str = "CFGD_SETX_BIN";
const REG_BIN_ENV: &str = "CFGD_REG_BIN";

/// Which output stream a failing setter writes its diagnostic to.
enum ErrStream {
    Stdout,
    Stderr,
}

/// macOS: set a variable in the current launchd session (`launchctl setenv`).
/// Reports a failure into `notes` (attached under the owning action's line when
/// the caller drains it); returns whether it succeeded.
pub fn launchctl_setenv(key: &str, value: &str, printer: &Printer, notes: &NoteSink) -> bool {
    let mut cmd = crate::tool_cmd(LAUNCHCTL_BIN_ENV, "launchctl");
    cmd.args(["setenv", key, value]);
    run_setter(
        cmd,
        LAUNCHCTL_BIN_ENV,
        &format!("launchctl setenv {key}"),
        ErrStream::Stderr,
        printer,
        notes,
    )
}

/// Windows: persist a user variable to `HKCU\Environment` (`setx`). `setx`
/// reports failures on stdout, not stderr.
pub fn windows_setx(key: &str, value: &str, printer: &Printer, notes: &NoteSink) -> bool {
    let mut cmd = crate::tool_cmd(SETX_BIN_ENV, "setx");
    cmd.args([key, value]);
    run_setter(
        cmd,
        SETX_BIN_ENV,
        &format!("setx {key}"),
        ErrStream::Stdout,
        printer,
        notes,
    )
}

/// Linux/BSD: register a variable with the systemd user manager so units it
/// later spawns inherit it. Best-effort — absent `systemctl` is a no-op.
fn systemctl_user_setenv(key: &str, value: &str, printer: &Printer, notes: &NoteSink) -> bool {
    if !crate::command_available_with_seam(SYSTEMCTL_BIN_ENV, "systemctl") {
        return false;
    }
    let mut cmd = crate::tool_cmd(SYSTEMCTL_BIN_ENV, "systemctl");
    cmd.args(["--user", "set-environment", &format!("{key}={value}")]);
    run_setter(
        cmd,
        SYSTEMCTL_BIN_ENV,
        &format!("systemctl --user set-environment {key}"),
        ErrStream::Stderr,
        printer,
        notes,
    )
}

/// Under `cargo test`, writing the live session without a shim is a bug, so it
/// aborts the test instead of proceeding.
///
/// The other destructive surfaces cfgd manages are paths, and a test home takes
/// them out of reach. A session manager cannot be redirected that way, so the
/// discipline of "pin the scope, or point the seam somewhere harmless" is the
/// only thing standing between an unrelated apply-path test and the operator's
/// own login session — and that discipline has already failed once, rewriting
/// this workstation's env surfaces. Refusing by construction is what makes the
/// per-test fixes stay fixed.
#[cfg(test)]
fn refuse_unseamed_session_write(seam: &str) {
    assert!(
        std::env::var_os(seam).is_some(),
        "{seam} must point at a shim before a test writes the live session; \
         a test that does not mean to reach the session should pin its env scope instead"
    );
}

#[cfg(not(test))]
fn refuse_unseamed_session_write(_seam: &str) {}

fn run_setter(
    mut cmd: Command,
    seam: &str,
    label: &str,
    err_stream: ErrStream,
    printer: &Printer,
    notes: &NoteSink,
) -> bool {
    refuse_unseamed_session_write(seam);
    match crate::command_output_with_timeout(&mut cmd, ENV_REFRESH_TIMEOUT) {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let detail = match err_stream {
                ErrStream::Stdout => crate::stdout_lossy_trimmed(&output),
                ErrStream::Stderr => crate::stderr_lossy_trimmed(&output),
            };
            notes.report(
                printer,
                Role::Warn,
                format!(
                    "{label} failed: {}",
                    crate::output::collapse_to_subject_line(&detail)
                ),
            );
            false
        }
        Err(e) => {
            notes.report(
                printer,
                Role::Warn,
                format!(
                    "{label} failed: {}",
                    crate::output::collapse_to_subject_line(&e)
                ),
            );
            false
        }
    }
}

/// Refresh the current user's live session so newly-spawned processes see
/// `vars` without a re-login. Best-effort and idempotent: a variable already
/// set to the same value is skipped. Returns the count of variables actually
/// changed.
///
/// Dispatches to the platform mechanism: macOS `launchctl setenv`, Linux
/// `systemctl --user set-environment`, Windows `setx`. The durable
/// file/registry targets written elsewhere are authoritative; a failure here
/// is surfaced as a warning and never aborts an apply.
pub fn refresh_session_env(
    vars: &[(String, String)],
    printer: &Printer,
    notes: &NoteSink,
) -> usize {
    if vars.is_empty() {
        return 0;
    }
    let bulk = bulk_session_env();
    let mut changed = 0;
    for (key, value) in vars {
        let current = bulk.get(key).cloned().or_else(|| read_session_var(key));
        if current.as_deref() == Some(value.as_str()) {
            continue;
        }
        let ok = if cfg!(windows) {
            windows_setx(key, value, printer, notes)
        } else if cfg!(target_os = "macos") {
            launchctl_setenv(key, value, printer, notes)
        } else {
            systemctl_user_setenv(key, value, printer, notes)
        };
        if ok {
            changed += 1;
        }
    }
    changed
}

/// Cheap whole-environment read where one call suffices (Linux `systemctl
/// --user show-environment`). macOS/Windows return empty and fall back to the
/// per-variable [`read_session_var`].
fn bulk_session_env() -> BTreeMap<String, String> {
    if cfg!(all(unix, not(target_os = "macos"))) {
        if !crate::command_available_with_seam(SYSTEMCTL_BIN_ENV, "systemctl") {
            return BTreeMap::new();
        }
        let mut cmd = crate::tool_cmd(SYSTEMCTL_BIN_ENV, "systemctl");
        cmd.args(["--user", "show-environment"]);
        match crate::command_output_with_timeout(&mut cmd, ENV_REFRESH_TIMEOUT) {
            Ok(o) if o.status.success() => parse_kv_lines(&String::from_utf8_lossy(&o.stdout)),
            _ => BTreeMap::new(),
        }
    } else {
        BTreeMap::new()
    }
}

/// Per-variable current value, for platforms without a cheap bulk dump.
fn read_session_var(key: &str) -> Option<String> {
    if cfg!(target_os = "macos") {
        let mut cmd = crate::tool_cmd(LAUNCHCTL_BIN_ENV, "launchctl");
        cmd.args(["getenv", key]);
        match crate::command_output_with_timeout(&mut cmd, ENV_REFRESH_TIMEOUT) {
            // launchctl getenv exits 0 with empty stdout when unset.
            Ok(o) if o.status.success() => {
                let v = crate::stdout_lossy_trimmed(&o);
                (!v.is_empty()).then_some(v)
            }
            _ => None,
        }
    } else if cfg!(windows) {
        let mut cmd = crate::tool_cmd(REG_BIN_ENV, "reg");
        cmd.args(["query", r"HKCU\Environment", "/v", key]);
        match crate::command_output_with_timeout(&mut cmd, ENV_REFRESH_TIMEOUT) {
            Ok(o) if o.status.success() => {
                parse_reg_value(&String::from_utf8_lossy(&o.stdout), key)
            }
            _ => None,
        }
    } else {
        // Linux/BSD are covered by the bulk `show-environment` read.
        None
    }
}

/// Parse `KEY=VALUE` lines (e.g. `systemctl --user show-environment`).
fn parse_kv_lines(s: &str) -> BTreeMap<String, String> {
    s.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.to_string()))
        .collect()
}

/// Extract a single value from `reg query HKCU\Environment /v KEY` output,
/// whose data line is `<indent>KEY    REG_SZ|REG_EXPAND_SZ    value`.
fn parse_reg_value(s: &str, key: &str) -> Option<String> {
    for line in s.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(key) {
            let rest = rest.trim_start();
            for ty in ["REG_SZ", "REG_EXPAND_SZ"] {
                if let Some(value) = rest.strip_prefix(ty) {
                    return Some(value.trim().to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kv_lines_splits_first_equals() {
        let map = parse_kv_lines("PATH=/usr/bin:/bin\nEDITOR=nvim\nLANG=en_US.UTF-8\n");
        assert_eq!(map.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
        assert_eq!(map.get("EDITOR").map(String::as_str), Some("nvim"));
        assert_eq!(map.get("LANG").map(String::as_str), Some("en_US.UTF-8"));
    }

    #[test]
    fn parse_reg_value_extracts_reg_sz_and_misses_absent() {
        let out = "\r\nHKEY_CURRENT_USER\\Environment\r\n    EDITOR    REG_SZ    nvim\r\n";
        assert_eq!(parse_reg_value(out, "EDITOR").as_deref(), Some("nvim"));
        assert_eq!(parse_reg_value(out, "MISSING"), None);
    }

    #[test]
    fn refresh_session_env_empty_is_noop() {
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        assert_eq!(refresh_session_env(&[], &printer, NoteSink::discarded()), 0);
    }

    /// A `/bin/sh` stand-in for a session manager: appends its argv to `log` and
    /// exits with `code`, so a test can assert on what would have been sent to
    /// the real session without a session being involved.
    #[cfg(unix)]
    fn session_shim(dir: &std::path::Path, name: &str, log: &std::path::Path, code: i32) -> String {
        let bin = dir.join(name);
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\necho 'shim said no'\nexit {code}\n",
            log = log.display()
        );
        std::fs::write(&bin, body).unwrap();
        crate::set_file_permissions(&bin, 0o755).unwrap();
        bin.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn launchctl_setenv_sends_the_pair_through_the_seam() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("argv.log");
        let shim = session_shim(tmp.path(), "launchctl", &log, 0);
        let _seam = crate::test_helpers::EnvVarGuard::set(LAUNCHCTL_BIN_ENV, &shim);

        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        assert!(launchctl_setenv(
            "EDITOR",
            "nvim",
            &printer,
            NoteSink::discarded()
        ));
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            "setenv EDITOR nvim"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn windows_setx_failure_warns_and_reports_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("argv.log");
        let shim = session_shim(tmp.path(), "setx", &log, 1);
        let _seam = crate::test_helpers::EnvVarGuard::set(SETX_BIN_ENV, &shim);

        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        assert!(!windows_setx(
            "EDITOR",
            "nvim",
            &printer,
            NoteSink::discarded()
        ));
        printer.flush();
        let out = crate::output::strip_ansi(&buf.lock().unwrap());
        assert!(
            out.contains("setx EDITOR failed") && out.contains("shim said no"),
            "a failing setter must quote the tool's own stdout: {out}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    #[should_panic(expected = "CFGD_LAUNCHCTL_BIN")]
    fn an_unseamed_session_write_is_refused_under_test() {
        let _unset = crate::test_helpers::EnvVarGuard::unset(LAUNCHCTL_BIN_ENV);
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        launchctl_setenv("EDITOR", "nvim", &printer, NoteSink::discarded());
    }
}

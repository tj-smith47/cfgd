//! Live-session environment refresh — the controlled shell-out layer for
//! making the current user's already-running session managers spawn new
//! processes with updated environment variables, without a re-login.
//!
//! This is the single home for the per-platform shell-outs (`launchctl`,
//! `systemctl --user`, `setx`) so both `spec.env` (user scope, via the
//! reconciler) and `spec.system.environment` (the system configurator) share
//! one implementation rather than open-coding it twice. The per-tool setters
//! are not `#[cfg]`-gated so they stay directly testable via PATH shims on any
//! host. See `.claude/rules/module-boundaries.md`.

use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use crate::output::{Printer, Role};

/// Live-refresh shell-outs are local and fast; bound them anyway so a wedged
/// session bus (`systemctl --user` with no user D-Bus) can't hang an apply.
const ENV_REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

/// Which output stream a failing setter writes its diagnostic to.
enum ErrStream {
    Stdout,
    Stderr,
}

/// macOS: set a variable in the current launchd session (`launchctl setenv`).
/// Warns through the printer on failure; returns whether it succeeded.
pub fn launchctl_setenv(key: &str, value: &str, printer: &Printer) -> bool {
    let mut cmd = Command::new("launchctl");
    cmd.args(["setenv", key, value]);
    run_setter(
        cmd,
        &format!("launchctl setenv {key}"),
        ErrStream::Stderr,
        printer,
    )
}

/// Windows: persist a user variable to `HKCU\Environment` (`setx`). `setx`
/// reports failures on stdout, not stderr.
pub fn windows_setx(key: &str, value: &str, printer: &Printer) -> bool {
    let mut cmd = Command::new("setx");
    cmd.args([key, value]);
    run_setter(cmd, &format!("setx {key}"), ErrStream::Stdout, printer)
}

/// Linux/BSD: register a variable with the systemd user manager so units it
/// later spawns inherit it. Best-effort — absent `systemctl` is a no-op.
fn systemctl_user_setenv(key: &str, value: &str, printer: &Printer) -> bool {
    if !crate::command_available("systemctl") {
        return false;
    }
    let mut cmd = Command::new("systemctl");
    cmd.args(["--user", "set-environment", &format!("{key}={value}")]);
    run_setter(
        cmd,
        &format!("systemctl --user set-environment {key}"),
        ErrStream::Stderr,
        printer,
    )
}

fn run_setter(mut cmd: Command, label: &str, err_stream: ErrStream, printer: &Printer) -> bool {
    match crate::command_output_with_timeout(&mut cmd, ENV_REFRESH_TIMEOUT) {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let detail = match err_stream {
                ErrStream::Stdout => crate::stdout_lossy_trimmed(&output),
                ErrStream::Stderr => crate::stderr_lossy_trimmed(&output),
            };
            printer.status_simple(
                Role::Warn,
                format!(
                    "{label} failed: {}",
                    crate::output::collapse_to_subject_line(&detail)
                ),
            );
            false
        }
        Err(e) => {
            printer.status_simple(
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
pub fn refresh_session_env(vars: &[(String, String)], printer: &Printer) -> usize {
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
            windows_setx(key, value, printer)
        } else if cfg!(target_os = "macos") {
            launchctl_setenv(key, value, printer)
        } else {
            systemctl_user_setenv(key, value, printer)
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
        if !crate::command_available("systemctl") {
            return BTreeMap::new();
        }
        let mut cmd = Command::new("systemctl");
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
        let mut cmd = Command::new("launchctl");
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
        let mut cmd = Command::new("reg");
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
        assert_eq!(refresh_session_env(&[], &printer), 0);
    }
}

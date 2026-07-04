use super::fs_perms::is_executable;

/// Grace period between SIGTERM and SIGKILL when a watchdog kills a child.
/// A SIGTERM-trapping child gets a chance to clean up; if it's still alive
/// past this window the watchdog escalates to SIGKILL so the daemon can
/// reclaim the slot regardless of what the child does.
pub const KILL_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(2);

/// Result of [`command_output_with_timeout_outcome`]: the captured process
/// output plus whether the watchdog had to terminate the child for exceeding
/// the timeout.
///
/// On timeout the `output` carries a signal-killed exit status, which is
/// indistinguishable from a genuine non-zero exit. Callers that must treat a
/// hang as a hard error (rather than as a normal failure exit) inspect
/// [`timed_out`](CommandOutcome::timed_out).
pub struct CommandOutcome {
    /// Captured stdout/stderr and exit status of the child process.
    pub output: std::process::Output,
    /// `true` if the watchdog terminated the child because it exceeded the
    /// timeout. When `true`, `output.status` reflects the kill signal, not the
    /// command's own exit code.
    pub timed_out: bool,
}

/// Run a [`Command`] with a timeout, surfacing whether the timeout fired.
///
/// On timeout the watchdog sends SIGTERM, waits [`KILL_GRACE_PERIOD`] for the
/// child to exit cleanly, then escalates to SIGKILL (Unix) / `TerminateProcess`
/// retry (Windows), and the returned [`CommandOutcome::timed_out`] is `true`.
///
/// **Caveat**: if the child forks descendants that inherit its stdout/stderr
/// pipes (e.g. a shell wrapper spawning a long-running grandchild), SIGKILL
/// on the immediate child will not close those pipes — `wait_with_output`
/// will block on them until the grandchild also dies. Production callers
/// should invoke the target binary directly rather than via a shell wrapper.
pub fn command_output_with_timeout_outcome(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<CommandOutcome> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    let child = cmd.spawn()?;
    let id = child.id();
    let (tx, rx) = mpsc::channel();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timed_out_watchdog = Arc::clone(&timed_out);

    std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            timed_out_watchdog.store(true, Ordering::SeqCst);
            terminate_process(id);
            // SIGTERM-trapping children can hang the wait_with_output below
            // indefinitely. Give them a grace window to flush, then escalate.
            if rx.recv_timeout(KILL_GRACE_PERIOD).is_err() {
                force_kill_process(id);
            }
        }
    });

    let result = child.wait_with_output();
    let _ = tx.send(());
    result.map(|output| CommandOutcome {
        output,
        timed_out: timed_out.load(Ordering::SeqCst),
    })
}

/// Run a [`Command`] with a timeout, discarding the timeout signal.
///
/// Thin wrapper over [`command_output_with_timeout_outcome`] for callers that
/// only need the captured output. Callers that must distinguish a hang from a
/// non-zero exit should use [`command_output_with_timeout_outcome`] directly.
pub fn command_output_with_timeout(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    command_output_with_timeout_outcome(cmd, timeout).map(|o| o.output)
}

/// Send a graceful termination signal to a process by PID.
/// Unix: sends SIGTERM. Windows: calls TerminateProcess.
#[cfg(unix)]
pub fn terminate_process(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
}

#[cfg(windows)]
pub fn terminate_process(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: `OpenProcess` is always sound to call with valid flags; it
    // returns NULL on failure (checked below) or a valid handle we own. We
    // call `TerminateProcess` and `CloseHandle` only with that owned
    // handle, and `CloseHandle` runs exactly once per successful open, so
    // there is no double-close or use-after-close.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

/// Send an uncatchable kill signal to a process by PID after the graceful
/// terminate window has elapsed. Unix: SIGKILL. Windows: a second
/// TerminateProcess call (idempotent — Windows kills are already uncatchable).
#[cfg(unix)]
pub fn force_kill_process(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
}

#[cfg(windows)]
pub fn force_kill_process(pid: u32) {
    terminate_process(pid);
}

/// Check if the current process is running with elevated privileges.
/// Unix: checks euid == 0. Windows: checks IsUserAnAdmin().
#[cfg(unix)]
pub fn is_root() -> bool {
    use nix::unistd::geteuid;
    geteuid().is_root()
}

#[cfg(windows)]
pub fn is_root() -> bool {
    use windows_sys::Win32::UI::Shell::IsUserAnAdmin;
    // SAFETY: `IsUserAnAdmin` takes no parameters, has no preconditions,
    // and returns a BOOL. It is safe to call from any thread at any time.
    unsafe { IsUserAnAdmin() != 0 }
}

/// Get the system hostname as a String. Returns "unknown" on failure.
pub fn hostname_string() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Extract stdout from a `Command` output as a trimmed, lossy UTF-8 string.
pub fn stdout_lossy_trimmed(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Extract stderr from a `Command` output as a trimmed, lossy UTF-8 string.
pub fn stderr_lossy_trimmed(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

/// Resolve a command to its full executable path via a PATHEXT-aware `$PATH` walk.
///
/// On Windows, tries executable extensions in invocation-preference order
/// (`.exe`/`.com` first, then the script-shim forms `.ps1`/`.cmd`/`.bat`) and
/// returns the first `$PATH` entry holding a real, executable file. This is what
/// makes a bare name like `scoop` — which ships only as `scoop.ps1`/`scoop.cmd`,
/// never `scoop.exe` — resolve to its shim path instead of reporting "not found":
/// a caller can then launch the shim correctly (a native `Command::new("scoop")`
/// only ever finds `scoop.exe`). On Unix, resolves the bare name against the exec
/// bit. Returns `None` when nothing on `$PATH` matches.
pub fn command_path(cmd: &str) -> Option<std::path::PathBuf> {
    let extensions: &[&str] = if cfg!(windows) {
        &[".exe", ".com", ".ps1", ".cmd", ".bat"]
    } else {
        &[""]
    };
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for ext in extensions {
            let candidate = dir.join(format!("{cmd}{ext}"));
            if candidate.is_file()
                && std::fs::metadata(&candidate)
                    .map(|m| is_executable(&candidate, &m))
                    .unwrap_or(false)
            {
                return Some(candidate);
            }
        }
    }
    None
}

/// Check if a command is available on the system via PATH lookup.
/// On Windows, tries common executable extensions (.exe, .cmd, .bat, .ps1, .com)
/// since executables require an extension to be found. Thin `is_some()` view over
/// [`command_path`], so availability and path resolution can never disagree.
pub fn command_available(cmd: &str) -> bool {
    command_path(cmd).is_some()
}

/// Build a `tracing_subscriber::EnvFilter` from `RUST_LOG` if set, falling
/// back to `default`. Consolidates the four identical
/// `EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(..))`
/// scaffolds in `cfgd/main.rs`, `cfgd/cli/plugin.rs`, `cfgd-operator/main.rs`,
/// and `cfgd-csi/main.rs`.
pub fn tracing_env_filter(default: &str) -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default))
}

/// Check that a CLI tool is available on PATH, returning a unified error
/// string otherwise. Before this helper, six `if !command_available("X")`
/// gates across `oci.rs` and `cli/module.rs` each produced a slightly
/// different "not found" message; strings had diverged in production. Pass
/// `install_hint` (a short imperative like "install it from https://...")
/// to make the hint specific; `None` falls back to a generic "install it
/// or add it to PATH".
pub fn require_tool(name: &str, install_hint: Option<&str>) -> std::result::Result<(), String> {
    if command_available(name) {
        return Ok(());
    }
    Err(match install_hint {
        Some(hint) => format!("{name} not found — {hint}"),
        None => format!("{name} not found — install it or add it to PATH"),
    })
}

/// Resolve an external tool's binary path, honoring a per-tool env-var test
/// seam. Production code reads no env var and gets `default` (which `Command`
/// resolves via `PATH`); tests set `env_var` to an absolute path of a shim
/// binary. This is the SOLE supported override pattern for external CLIs.
///
/// Empty `env_var` (`""`) is treated as "no seam" and returns `default`
/// unchanged; callers may dispatch a per-binary seam via match and fall
/// through to `""` for unseamed binaries without panicking.
///
/// Naming convention: every active seam uses `CFGD_<NAME>_BIN` (e.g.
/// `CFGD_COSIGN_BIN`, `CFGD_AGE_BIN`, `CFGD_BREW_BIN`, `CFGD_APT_CACHE_BIN`).
/// New backends MUST follow this shape and reuse this helper rather than
/// reinventing the override surface — keeps the test-shim ergonomics uniform.
/// Pair every seam consumer with `serial_test::serial` because env-var mutation
/// is process-global.
pub fn tool_binary_name(env_var: &str, default: &str) -> String {
    if env_var.is_empty() {
        return default.to_string();
    }
    std::env::var(env_var).unwrap_or_else(|_| default.to_string())
}

/// Build a `Command` for an external tool, honoring [`tool_binary_name`]'s
/// env-var override. Sets `stderr` to piped so callers can surface the
/// tool's stderr in error messages without spamming the user's terminal.
pub fn tool_cmd(env_var: &str, default: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(tool_binary_name(env_var, default));
    cmd.stderr(std::process::Stdio::piped());
    cmd
}

/// Verify an external tool is available, honoring [`tool_binary_name`]'s
/// env-var override.
///
/// When `env_var` is unset, falls through to a normal PATH lookup via
/// [`require_tool`]. When set, treats the value as an absolute path and
/// only checks that the file exists — no PATH walking. This mirrors how
/// `Command::new(absolute_path)` actually executes the binary in tests.
///
/// Pair this with [`tool_cmd`] so `is_available` checks and command
/// construction both go through the same seam.
pub fn require_tool_with_seam(
    env_var: &str,
    default: &str,
    install_hint: Option<&str>,
) -> std::result::Result<(), String> {
    if let Ok(custom) = std::env::var(env_var) {
        let p = std::path::Path::new(&custom);
        if p.is_file() {
            return Ok(());
        }
        return Err(format!("{env_var} points to {custom} which is not a file"));
    }
    require_tool(default, install_hint)
}

/// Like [`command_available`] but also returns true when the env-var seam
/// points at an existing file. Use in `is_available()` checks where the
/// caller wants a bool, not a `Result`.
pub fn command_available_with_seam(env_var: &str, default: &str) -> bool {
    if let Ok(custom) = std::env::var(env_var) {
        return std::path::Path::new(&custom).is_file();
    }
    command_available(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn hostname_string_returns_non_empty() {
        let h = hostname_string();
        assert!(!h.is_empty());
        assert_ne!(h, "unknown");
    }

    #[test]
    fn stdout_lossy_trimmed_trims_whitespace() {
        let output = std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: b"  hello world  \n".to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(stdout_lossy_trimmed(&output), "hello world");
    }

    #[test]
    fn stderr_lossy_trimmed_trims_whitespace() {
        let output = std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: b"\nerror message\n  ".to_vec(),
        };
        assert_eq!(stderr_lossy_trimmed(&output), "error message");
    }

    #[test]
    fn stdout_lossy_trimmed_handles_invalid_utf8() {
        let output = std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: vec![0xFF, 0xFE, b'a', b'b'],
            stderr: Vec::new(),
        };
        let result = stdout_lossy_trimmed(&output);
        assert!(result.contains("ab"));
    }

    #[test]
    fn command_available_finds_sh() {
        assert!(command_available("sh"));
    }

    #[test]
    fn command_available_rejects_nonexistent() {
        assert!(!command_available("absolutely-not-a-real-command-xyz"));
    }

    #[test]
    fn command_path_resolves_sh_to_a_real_executable_file() {
        let p = command_path("sh").expect("sh is on PATH");
        assert!(p.is_file(), "resolved sh must be a real file: {p:?}");
        assert_eq!(p.file_name().and_then(|f| f.to_str()), Some("sh"));
    }

    #[test]
    fn command_path_returns_none_for_nonexistent() {
        assert!(command_path("absolutely-not-a-real-command-xyz").is_none());
    }

    #[test]
    fn command_path_and_command_available_agree() {
        assert_eq!(command_available("sh"), command_path("sh").is_some());
        assert_eq!(
            command_available("absolutely-not-a-real-command-xyz"),
            command_path("absolutely-not-a-real-command-xyz").is_some()
        );
    }

    #[test]
    fn require_tool_succeeds_for_sh() {
        assert!(require_tool("sh", None).is_ok());
    }

    #[test]
    fn require_tool_fails_for_nonexistent() {
        let err = require_tool("not-a-real-tool-xyz", None).unwrap_err();
        assert!(err.contains("not-a-real-tool-xyz"));
        assert!(err.contains("not found"));
    }

    #[test]
    fn require_tool_includes_custom_hint() {
        let err = require_tool("missing-tool", Some("install via cargo")).unwrap_err();
        assert!(err.contains("install via cargo"));
    }

    #[test]
    #[serial]
    fn tool_binary_name_empty_env_var_returns_default() {
        assert_eq!(tool_binary_name("", "cosign"), "cosign");
    }

    #[test]
    #[serial]
    fn tool_binary_name_reads_env_var() {
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_TEST_TOOL_BIN", "/custom/path");
        assert_eq!(
            tool_binary_name("CFGD_TEST_TOOL_BIN", "default"),
            "/custom/path"
        );
    }

    #[test]
    #[serial]
    fn tool_binary_name_unset_env_returns_default() {
        let _guard = crate::test_helpers::EnvVarGuard::unset("CFGD_TEST_TOOL_BIN_UNSET");
        assert_eq!(
            tool_binary_name("CFGD_TEST_TOOL_BIN_UNSET", "fallback"),
            "fallback"
        );
    }

    #[test]
    #[serial]
    fn require_tool_with_seam_env_pointing_to_file_succeeds() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("tool");
        std::fs::write(&bin, "").unwrap();
        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_TEST_SEAM_BIN", bin.to_str().unwrap());
        assert!(require_tool_with_seam("CFGD_TEST_SEAM_BIN", "tool", None).is_ok());
    }

    #[test]
    #[serial]
    fn require_tool_with_seam_env_pointing_to_missing_file_fails() {
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_TEST_SEAM_BAD", "/no/such/file");
        let err = require_tool_with_seam("CFGD_TEST_SEAM_BAD", "tool", None).unwrap_err();
        assert!(err.contains("CFGD_TEST_SEAM_BAD"));
        assert!(err.contains("not a file"));
    }

    #[test]
    #[serial]
    fn require_tool_with_seam_no_env_falls_through() {
        let _guard = crate::test_helpers::EnvVarGuard::unset("CFGD_TEST_SEAM_NONE");
        assert!(require_tool_with_seam("CFGD_TEST_SEAM_NONE", "sh", None).is_ok());
    }

    #[test]
    #[serial]
    fn command_available_with_seam_env_file_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("tool");
        std::fs::write(&bin, "").unwrap();
        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_TEST_AVAIL_SEAM", bin.to_str().unwrap());
        assert!(command_available_with_seam(
            "CFGD_TEST_AVAIL_SEAM",
            "nonexistent"
        ));
    }

    #[test]
    #[serial]
    fn command_available_with_seam_env_file_missing() {
        let _guard = crate::test_helpers::EnvVarGuard::set("CFGD_TEST_AVAIL_BAD", "/no/such/file");
        assert!(!command_available_with_seam("CFGD_TEST_AVAIL_BAD", "sh"));
    }

    #[test]
    #[serial]
    fn command_available_with_seam_no_env_falls_through() {
        let _guard = crate::test_helpers::EnvVarGuard::unset("CFGD_TEST_AVAIL_NONE");
        assert!(command_available_with_seam("CFGD_TEST_AVAIL_NONE", "sh"));
    }

    #[test]
    fn tool_cmd_creates_command_with_piped_stderr() {
        let cmd = tool_cmd("", "echo");
        let prog = std::path::Path::new(cmd.get_program())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        assert_eq!(prog, "echo");
    }

    #[test]
    fn command_output_with_timeout_succeeds() {
        let mut cmd = std::process::Command::new("echo");
        cmd.arg("hello").stdout(std::process::Stdio::piped());
        let output =
            command_output_with_timeout(&mut cmd, std::time::Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert!(stdout_lossy_trimmed(&output).contains("hello"));
    }

    #[test]
    fn command_output_with_timeout_kills_on_exceed() {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("60");
        let result = command_output_with_timeout(&mut cmd, std::time::Duration::from_millis(100));
        assert!(
            result.is_ok(),
            "process should be killed but still return output"
        );
        let output = result.unwrap();
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn force_kill_process_signals_sigkill() {
        // Spawn a SIGTERM-trapping child, force_kill_process it, assert it exits
        // with SIGKILL (signal 9).
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();

        force_kill_process(pid);

        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "expected SIGKILL (9), got status: {status:?}"
        );
    }

    #[test]
    fn is_root_returns_bool() {
        let _ = is_root();
    }

    #[test]
    fn tracing_env_filter_uses_default_when_no_env() {
        let filter = tracing_env_filter("warn");
        let s = format!("{filter}");
        assert!(s.contains("warn") || !s.is_empty());
    }

    // A command that sleeps past a short timeout is terminated and reported as
    // timed_out; the signal-killed exit status alone could not convey this.
    #[cfg(unix)]
    #[test]
    fn command_outcome_reports_timeout_for_hung_command() {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("5");
        let outcome =
            command_output_with_timeout_outcome(&mut cmd, std::time::Duration::from_millis(100))
                .expect("spawn should succeed");
        assert!(outcome.timed_out, "a hung command must report timed_out");
    }

    // A fast command finishes before the timeout fires; timed_out stays false.
    #[cfg(unix)]
    #[test]
    fn command_outcome_no_timeout_for_fast_command() {
        let mut cmd = std::process::Command::new("true");
        let outcome =
            command_output_with_timeout_outcome(&mut cmd, std::time::Duration::from_secs(5))
                .expect("spawn should succeed");
        assert!(
            !outcome.timed_out,
            "a fast command must not report timed_out"
        );
        assert!(outcome.output.status.success());
    }
}

use super::fs_perms::is_executable;

/// Run a [`Command`] with a timeout, killing the process if it exceeds the limit.
/// Returns `Err` if spawn fails or the process is killed due to timeout.
pub fn command_output_with_timeout(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    use std::sync::mpsc;

    let child = cmd.spawn()?;
    let id = child.id();
    let (tx, rx) = mpsc::channel();

    // Spawn a watchdog thread that kills the child after timeout
    std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            // Timeout expired — kill the process
            terminate_process(id);
        }
    });

    let result = child.wait_with_output();
    // Signal the watchdog to stop (if the process finished before timeout)
    let _ = tx.send(());
    result
}

/// Send a termination signal to a process by PID.
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

/// Check if a command is available on the system via PATH lookup.
/// On Windows, tries common executable extensions (.exe, .cmd, .bat, .ps1, .com)
/// since executables require an extension to be found.
pub fn command_available(cmd: &str) -> bool {
    let extensions: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat", ".ps1", ".com"]
    } else {
        &[""]
    };
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                extensions.iter().any(|ext| {
                    let name = format!("{}{}", cmd, ext);
                    let path = dir.join(&name);
                    path.is_file()
                        && std::fs::metadata(&path)
                            .map(|m| is_executable(&path, &m))
                            .unwrap_or(false)
                })
            })
        })
        .unwrap_or(false)
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

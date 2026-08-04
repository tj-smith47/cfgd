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
/// Stdio is configured here, not by callers: stdout and stderr are piped and
/// stdin is null, matching [`std::process::Command::output`]. `spawn` alone
/// defaults every stream to *inherit*, which fails twice over — the child's
/// output bypasses the `output` module straight onto the terminal, and the
/// captured buffers come back empty, so the text every caller here parses or
/// reports is silently blank.
///
/// The pipes are drained by reader threads and the exit status is collected
/// with [`Child::wait`](std::process::Child::wait), never `wait_with_output`.
/// A killed child whose descendants inherited its pipe write ends leaves those
/// pipes open, so `wait_with_output` would block past the timeout it exists to
/// enforce — a shell-wrapped command (`run_guard_command`, a user `run:` body)
/// backgrounding a daemon is enough to trigger it. Readers get
/// [`PIPE_DRAIN_GRACE`] after child exit to reach EOF; past that they are
/// abandoned and whatever they captured so far is returned.
pub fn command_output_with_timeout_outcome(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<CommandOutcome> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    let id = child.id();

    let abandoned = Arc::new(AtomicBool::new(false));
    let (drained_tx, drained_rx) = mpsc::channel();
    let stdout_buf = spawn_pipe_reader(child.stdout.take(), &abandoned, drained_tx.clone());
    let stderr_buf = spawn_pipe_reader(child.stderr.take(), &abandoned, drained_tx);

    let (tx, rx) = mpsc::channel();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timed_out_watchdog = Arc::clone(&timed_out);

    std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            timed_out_watchdog.store(true, Ordering::SeqCst);
            terminate_process(id);
            // SIGTERM-trapping children can hang the wait below indefinitely.
            // Give them a grace window to flush, then escalate.
            if rx.recv_timeout(KILL_GRACE_PERIOD).is_err() {
                force_kill_process(id);
            }
        }
    });

    let status = child.wait();
    let _ = tx.send(());
    let status = status?;

    let deadline = std::time::Instant::now() + PIPE_DRAIN_GRACE;
    for _ in 0..2 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if drained_rx.recv_timeout(remaining).is_err() {
            break;
        }
    }
    abandoned.store(true, Ordering::SeqCst);

    Ok(CommandOutcome {
        output: std::process::Output {
            status,
            stdout: take_pipe_buffer(&stdout_buf),
            stderr: take_pipe_buffer(&stderr_buf),
        },
        timed_out: timed_out.load(Ordering::SeqCst),
    })
}

/// How long the pipe readers get to reach EOF after the child has exited,
/// before they are abandoned and their partial capture is returned. Only ever
/// elapses when a surviving descendant still holds the child's pipe write end.
const PIPE_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Drain one child pipe on its own thread into a shared buffer, signalling
/// `drained` at EOF.
///
/// The buffer is shared rather than returned through the channel so that an
/// abandoned reader's partial capture is still readable; the reader checks
/// `abandoned` each chunk so it stops growing the buffer once nobody is left
/// to read it.
fn spawn_pipe_reader<R: std::io::Read + Send + 'static>(
    source: Option<R>,
    abandoned: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    drained: std::sync::mpsc::Sender<()>,
) -> std::sync::Arc<std::sync::Mutex<Vec<u8>>> {
    use std::sync::atomic::Ordering;

    let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let Some(mut source) = source else {
        let _ = drained.send(());
        return buffer;
    };
    let sink = std::sync::Arc::clone(&buffer);
    let abandoned = std::sync::Arc::clone(abandoned);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match source.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if abandoned.load(Ordering::SeqCst) {
                        break;
                    }
                    sink.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(&chunk[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = drained.send(());
    });
    buffer
}

fn take_pipe_buffer(buffer: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> Vec<u8> {
    std::mem::take(&mut *buffer.lock().unwrap_or_else(|e| e.into_inner()))
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
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if let Some(hit) = probe_dir_for_command(&dir, cmd, extensions) {
                return Some(hit);
            }
        }
    }
    for dir in bootstrapped_path_dirs() {
        if let Some(hit) = probe_dir_for_command(&dir, cmd, extensions) {
            return Some(hit);
        }
    }
    None
}

/// First `dir/cmd{ext}` that is a real, executable file, in `extensions` order.
fn probe_dir_for_command(
    dir: &std::path::Path,
    cmd: &str,
    extensions: &[&str],
) -> Option<std::path::PathBuf> {
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
    None
}

/// PATH directories contributed by a package manager cfgd bootstrapped during
/// this process's lifetime.
///
/// A bootstrap installs into a prefix that did not exist when cfgd started, so
/// the inherited `PATH` cannot name it — and the next action in the same apply
/// is routinely the install that needs it, as when brew lands `pipx` and the
/// following action is `pipx install pynvim`. Rewriting the process's own
/// `PATH` would be the obvious fix and is not available: `std::env::set_var` is
/// unsound once any thread is live, and the daemon runs several. Holding the
/// directories beside `PATH` and searching them after it gives the same
/// resolution with none of that exposure.
static BOOTSTRAPPED_PATH_DIRS: std::sync::RwLock<Vec<std::path::PathBuf>> =
    std::sync::RwLock::new(Vec::new());

/// Make `dirs` visible to every later [`command_path`] / [`command_available`]
/// call in this process. Idempotent — a directory already registered is not
/// re-added, so a manager bootstrapped twice in one run resolves the same way.
pub fn register_bootstrapped_path_dirs(dirs: &[String]) {
    if dirs.is_empty() {
        return;
    }
    // A poisoned lock still holds a usable directory list: a panic in another
    // thread is no reason to stop resolving binaries that exist on disk.
    let mut guard = BOOTSTRAPPED_PATH_DIRS
        .write()
        .unwrap_or_else(|e| e.into_inner());
    for dir in dirs {
        let path = std::path::PathBuf::from(dir);
        if !guard.contains(&path) {
            guard.push(path);
        }
    }
}

/// Snapshot of the directories registered by [`register_bootstrapped_path_dirs`].
pub fn bootstrapped_path_dirs() -> Vec<std::path::PathBuf> {
    BOOTSTRAPPED_PATH_DIRS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Replace the registry with `dirs`, discarding everything registered since it
/// was snapshotted. Test-only: a bootstrap that happened cannot un-happen, so
/// production never rewinds this list. Reach for it through
/// [`crate::test_helpers::BootstrappedPathDirsGuard`] rather than calling it
/// directly.
#[cfg(any(test, feature = "test-helpers"))]
pub fn restore_bootstrapped_path_dirs(dirs: Vec<std::path::PathBuf>) {
    *BOOTSTRAPPED_PATH_DIRS
        .write()
        .unwrap_or_else(|e| e.into_inner()) = dirs;
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
        let _path = crate::test_helpers::path_env_read_guard();
        assert!(command_available("sh"));
    }

    /// Write an executable file named so `command_path(stem)` can resolve it.
    fn write_probe_tool(dir: &std::path::Path, stem: &str) -> std::path::PathBuf {
        let name = if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        };
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write probe tool");
        crate::set_file_permissions(&path, 0o755).expect("chmod probe tool");
        path
    }

    #[test]
    #[serial]
    fn command_path_resolves_a_tool_only_a_registered_dir_holds() {
        let _path = crate::test_helpers::path_env_read_guard();
        let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
        let dir = tempfile::tempdir().expect("tempdir");
        let stem = "cfgd-probe-registered-tool";
        let expected = write_probe_tool(dir.path(), stem);

        assert!(
            command_path(stem).is_none(),
            "probe tool must not resolve before its directory is registered"
        );

        register_bootstrapped_path_dirs(&[dir.path().to_string_lossy().into_owned()]);

        assert_eq!(command_path(stem).as_deref(), Some(expected.as_path()));
        assert!(command_available(stem));
    }

    #[test]
    #[serial]
    fn path_still_wins_over_a_registered_dir() {
        // Declared before the `EnvVarGuard` below so it drops last, bracketing
        // the whole window in which `PATH` holds this test's tempdir.
        let _path_excl = crate::test_helpers::path_env_mutation_guard();
        let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
        let on_path = tempfile::tempdir().expect("tempdir");
        let registered = tempfile::tempdir().expect("tempdir");
        let stem = "cfgd-probe-shadowed-tool";
        let preferred = write_probe_tool(on_path.path(), stem);
        write_probe_tool(registered.path(), stem);

        register_bootstrapped_path_dirs(&[registered.path().to_string_lossy().into_owned()]);
        let _path =
            crate::test_helpers::EnvVarGuard::set("PATH", &on_path.path().to_string_lossy());

        assert_eq!(command_path(stem).as_deref(), Some(preferred.as_path()));
    }

    #[test]
    #[serial]
    fn registering_the_same_dir_twice_records_it_once() {
        let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = dir.path().to_string_lossy().into_owned();

        register_bootstrapped_path_dirs(std::slice::from_ref(&entry));
        register_bootstrapped_path_dirs(&[entry]);

        let hits = bootstrapped_path_dirs()
            .into_iter()
            .filter(|p| p == dir.path())
            .count();
        assert_eq!(hits, 1);
    }

    #[test]
    #[serial]
    fn the_test_guard_unregisters_dirs_registered_inside_its_scope() {
        // Production never rewinds this registry, so a fixture that registers a
        // real host directory would otherwise change what every later test in
        // the binary can resolve — the shape that made an empty-PATH "git is
        // missing" test pass on Linux and fail on macOS.
        let before = bootstrapped_path_dirs();
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture();
            register_bootstrapped_path_dirs(&[dir.path().to_string_lossy().into_owned()]);
            assert!(
                bootstrapped_path_dirs().iter().any(|p| p == dir.path()),
                "registration must take effect inside the guard's scope"
            );
        }
        assert_eq!(
            bootstrapped_path_dirs(),
            before,
            "the guard must leave the registry exactly as it found it"
        );
    }

    #[test]
    #[serial]
    fn registering_nothing_leaves_the_list_alone() {
        let before = bootstrapped_path_dirs();
        register_bootstrapped_path_dirs(&[]);
        assert_eq!(bootstrapped_path_dirs(), before);
    }

    #[test]
    fn command_available_rejects_nonexistent() {
        assert!(!command_available("absolutely-not-a-real-command-xyz"));
    }

    #[test]
    fn command_path_resolves_sh_to_a_real_executable_file() {
        let _path = crate::test_helpers::path_env_read_guard();
        let p = command_path("sh").expect("sh is on PATH");
        assert!(p.is_file(), "resolved sh must be a real file: {p:?}");
        // Stem, not file_name: on Windows the resolved binary is `sh.exe`, so its
        // file_name is `sh.exe` while its stem is `sh` on every platform.
        assert_eq!(p.file_stem().and_then(|f| f.to_str()), Some("sh"));
    }

    #[test]
    fn command_path_returns_none_for_nonexistent() {
        assert!(command_path("absolutely-not-a-real-command-xyz").is_none());
    }

    #[test]
    fn command_path_and_command_available_agree() {
        let _path = crate::test_helpers::path_env_read_guard();
        assert_eq!(command_available("sh"), command_path("sh").is_some());
        assert_eq!(
            command_available("absolutely-not-a-real-command-xyz"),
            command_path("absolutely-not-a-real-command-xyz").is_some()
        );
    }

    #[test]
    fn require_tool_succeeds_for_sh() {
        let _path = crate::test_helpers::path_env_read_guard();
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
        cmd.arg("hello");
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

    // Callers pass a bare Command and configure no stdio. Spawning without
    // piping inherits the parent's terminal, which both leaks the child's
    // output past the `output` module and hands back empty capture buffers —
    // every caller that parses this text would silently read "".
    #[cfg(unix)]
    #[test]
    fn command_output_captures_both_streams_without_caller_piping() {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("echo to-stdout; echo to-stderr >&2");
        let output =
            command_output_with_timeout(&mut cmd, std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(stdout_lossy_trimmed(&output), "to-stdout");
        assert_eq!(stderr_lossy_trimmed(&output), "to-stderr");
    }

    // A killed child's descendants keep the pipe write ends open, so waiting
    // for pipe EOF would outlast the timeout by however long the descendant
    // lives — here 30s against a 200ms timeout.
    #[cfg(unix)]
    #[test]
    fn command_output_returns_when_descendant_holds_pipe_open() {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("sleep 30 & echo $!; sleep 30");

        let started = std::time::Instant::now();
        let outcome =
            command_output_with_timeout_outcome(&mut cmd, std::time::Duration::from_millis(200))
                .expect("spawn should succeed");
        let elapsed = started.elapsed();

        let orphan = stdout_lossy_trimmed(&outcome.output);
        if let Ok(pid) = orphan.parse::<u32>() {
            force_kill_process(pid);
        }

        assert!(outcome.timed_out, "the watchdog must report the timeout");
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "returned in {elapsed:?}; a surviving descendant must not extend the wait"
        );
        assert!(
            !orphan.is_empty(),
            "output written before the kill must still be captured"
        );
    }
}

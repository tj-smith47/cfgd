use std::io::IsTerminal;

use crate::PathDisplayExt;
use crate::config::{ScriptEntry, ScriptShell};
use crate::errors::{CfgdError, ConfigError, Result};
use crate::output::{Printer, Role};

use super::types::{ReconcileContext, ScriptPhase};

// ---------------------------------------------------------------------------
// Unified script executor
// ---------------------------------------------------------------------------

/// Default timeout for module-level scripts.
pub(crate) const MODULE_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How `execute_script` should treat a script given its `interactive` flag and
/// whether stdin is a TTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveDisposition {
    /// Interactive and a TTY is present: run attached to the terminal.
    Run,
    /// Interactive but no TTY (CI, piped stdin, daemon): skip with a warning.
    SkipNoTty,
    /// Not an interactive script: run via the normal piped/spinner path.
    NotInteractive,
}

/// Decide how to run a script from its `interactive` flag and stdin TTY state.
///
/// Pure so both terminal-attached and skip-with-warn paths are unit-testable
/// without a PTY.
fn interactive_disposition(interactive: bool, stdin_is_tty: bool) -> InteractiveDisposition {
    match (interactive, stdin_is_tty) {
        (false, _) => InteractiveDisposition::NotInteractive,
        (true, true) => InteractiveDisposition::Run,
        (true, false) => InteractiveDisposition::SkipNoTty,
    }
}

/// Build environment variables injected into every script invocation.
pub(crate) fn build_script_env(
    config_dir: &std::path::Path,
    profile_name: &str,
    context: ReconcileContext,
    phase: &ScriptPhase,
    module_name: Option<&str>,
    module_dir: Option<&std::path::Path>,
) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "CFGD_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        ),
        ("CFGD_PROFILE".to_string(), profile_name.to_string()),
        (
            "CFGD_CONTEXT".to_string(),
            match context {
                ReconcileContext::Apply => "apply".to_string(),
                ReconcileContext::Reconcile => "reconcile".to_string(),
            },
        ),
        ("CFGD_PHASE".to_string(), phase.display_name().to_string()),
    ];
    if let Some(name) = module_name {
        env.push(("CFGD_MODULE_NAME".to_string(), name.to_string()));
    }
    if let Some(dir) = module_dir {
        env.push(("CFGD_MODULE_DIR".to_string(), dir.display().to_string()));
    }
    env
}

/// Build environment variables for a module lifecycle script.
///
/// Extends `build_script_env` with the module's declared `spec.env` vars.
/// CFGD_* names in `spec.env` are silently dropped: runtime-injected metadata
/// must not be shadowed by user-supplied values.
pub(crate) fn build_module_script_env(
    config_dir: &std::path::Path,
    profile_name: &str,
    context: ReconcileContext,
    phase: &ScriptPhase,
    module_name: Option<&str>,
    module_dir: Option<&std::path::Path>,
    module_env: &[crate::config::EnvVar],
) -> Vec<(String, String)> {
    let mut env = build_script_env(
        config_dir,
        profile_name,
        context,
        phase,
        module_name,
        module_dir,
    );
    // Expand `$VAR`/`${VAR}` in each declared value before injecting it into the
    // process environment: no shell is present here to do it, so a literal
    // `PATH: ...:$PATH` would overwrite PATH with garbage and the interpreter
    // itself would fail to spawn (os error 2). Resolve against the current
    // process env plus the metadata + already-expanded module vars, so a later
    // var can reference an earlier one (fold-left, as a shell would).
    let mut resolved: std::collections::HashMap<String, String> = std::env::vars().collect();
    for (k, v) in &env {
        resolved.insert(k.clone(), v.clone());
    }
    for ev in module_env {
        // Expand a leading `~` to home BEFORE `$VAR`/`${VAR}`: like the managed
        // env files, a literal `~/.local/bin` injected straight into the child
        // process environment would never be expanded (no shell is present).
        let tilded = crate::expand_env_value_tilde(&ev.value);
        let value = crate::expand_env_vars(&tilded, &|name| resolved.get(name).cloned());
        resolved.insert(ev.name.clone(), value.clone());
        env.push((ev.name.clone(), value));
    }
    env
}

/// Default working directory for a lifecycle script: the user's home directory.
///
/// Scripts reach module-bundled assets and the config tree through the
/// always-injected `$CFGD_MODULE_DIR` / `$CFGD_CONFIG_DIR` env vars, so the
/// config *source* tree is never the implicit CWD — a relative write from a
/// script lands in `$HOME`, not the user's version-controlled GitOps repo. A
/// per-script `workdir:` overrides this (see `resolve_script_workdir`). Falls
/// back to `config_dir` only when the home directory cannot be resolved.
pub(crate) fn script_default_workdir(config_dir: &std::path::Path) -> std::path::PathBuf {
    crate::home_dir_var()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config_dir.to_path_buf())
}

/// Resolve a `workdir:` value: expand `$VAR`/`${VAR}` against the script
/// environment, then a leading `~` to the user's home directory.
fn resolve_script_workdir(raw: &str, env_vars: &[(String, String)]) -> std::path::PathBuf {
    let expanded = crate::expand_env_vars(raw, &|name| {
        env_vars
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    });
    crate::expand_tilde(std::path::Path::new(&expanded))
}

/// Unified script executor for all hook types at both profile and module level.
///
/// `shell_override` forces every inline command to run under the supplied
/// interpreter, ignoring any `shell:` field on the entry. Set by
/// `cfgd apply --shell <shell>` for debugging. File/shebang scripts ignore the
/// override (the shebang owns the interpreter choice) and emit a debug log.
///
/// Returns (description, changed, captured_output). All scripts set changed=true.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_script(
    entry: &ScriptEntry,
    script_dir: &std::path::Path,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    default_timeout: std::time::Duration,
    printer: &Printer,
    shell_override: Option<ScriptShell>,
    abort: Option<&crate::AbortFlag>,
) -> Result<(String, bool, Option<String>)> {
    let run_str = entry.run_str();

    // Hold the PATH read-lock across interpreter resolution + spawn: a
    // concurrent test emptying `PATH` (command-not-found paths) is a data race
    // on `environ` that surfaces here as a spurious ENOENT. Compiled out of
    // release builds.
    #[cfg(any(test, feature = "test-helpers"))]
    let _path_guard = crate::test_helpers::script_spawn_path_guard();

    // `script_dir` is where the script's bundled files live (the module / config
    // source tree); a relative file-path `run:` resolves against it. `working_dir`
    // is the directory the script *runs in* — the home default by default, so a
    // relative write can't pollute the source tree. A per-script `workdir:`
    // overrides the run directory only (not file resolution): authors target the
    // deploy dir (`workdir: ~/.local/share/app`), the module source
    // (`workdir: $CFGD_MODULE_DIR`), or any absolute path.
    let workdir_override = match entry {
        ScriptEntry::Full {
            workdir: Some(w), ..
        } => Some(resolve_script_workdir(w, env_vars)),
        _ => None,
    };
    let working_dir = workdir_override.as_deref().unwrap_or(working_dir);

    // A stale `working_dir` (e.g. a tempdir from a prior `cfgd init` test that was
    // cleaned up off tmpfs) would otherwise surface as a cryptic `io error: No such
    // file or directory (os error 2)` from `cmd.spawn()` — naming neither the path
    // nor the script. Probe with a single metadata syscall so the failure mode
    // points at the actual offender.
    match std::fs::metadata(working_dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(meta) => {
            let kind = if meta.is_file() { "file" } else { "other" };
            return Err(CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "script '{}' cannot run: working directory is not a directory ({}): {}",
                    run_str,
                    kind,
                    working_dir.posix()
                ),
            }));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "script '{}' cannot run: working directory does not exist: {}",
                    run_str,
                    working_dir.posix()
                ),
            }));
        }
        Err(e) => {
            return Err(CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "script '{}' cannot run: working directory inaccessible ({}): {}",
                    run_str,
                    e,
                    working_dir.posix()
                ),
            }));
        }
    }

    let label = format!("Running script: {}", run_str);

    // Idempotency guards run BEFORE the body: `creates` (path existence),
    // then `onlyIf` (run only on zero exit), then `unless` (run only on
    // non-zero exit). Any guard that says "skip" short-circuits with
    // changed=false. A skip is a clean no-op, not a failure.
    if let ScriptEntry::Full {
        only_if,
        unless,
        creates,
        shell,
        ..
    } = entry
    {
        let guard_shell = shell_override.unwrap_or(*shell);

        if let Some(path) = creates {
            let resolved_creates = resolve_creates_path(path, working_dir);
            if resolved_creates.exists() {
                printer.status_simple(
                    Role::Skipped,
                    format!(
                        "{run_str} — creates path already exists: {}",
                        resolved_creates.posix()
                    ),
                );
                return Ok((label, false, None));
            }
        }

        if let Some(cmd) = only_if {
            let success =
                run_guard_command(cmd, guard_shell, working_dir, env_vars, default_timeout)?;
            if !success {
                printer.status_simple(
                    Role::Skipped,
                    format!("{run_str} — onlyIf condition not met: {cmd}"),
                );
                return Ok((label, false, None));
            }
        }

        if let Some(cmd) = unless {
            let success =
                run_guard_command(cmd, guard_shell, working_dir, env_vars, default_timeout)?;
            if success {
                printer.status_simple(
                    Role::Skipped,
                    format!("{run_str} — unless condition already holds: {cmd}"),
                );
                return Ok((label, false, None));
            }
        }
    }

    tracing::debug!(
        run = %run_str,
        working_dir = %working_dir.display(),
        "executing script"
    );

    let effective_timeout = match entry {
        ScriptEntry::Full {
            timeout: Some(t), ..
        } => crate::parse_duration_str(t)
            .map_err(|e| CfgdError::Config(ConfigError::Invalid { message: e }))?,
        _ => default_timeout,
    };
    let idle_timeout = match entry {
        ScriptEntry::Full {
            idle_timeout: Some(t),
            ..
        } => Some(
            crate::parse_duration_str(t)
                .map_err(|e| CfgdError::Config(ConfigError::Invalid { message: e }))?,
        ),
        _ => None,
    };

    let entry_shell = match entry {
        ScriptEntry::Full { shell, .. } => *shell,
        ScriptEntry::Simple(_) => ScriptShell::Auto,
    };
    let shell = shell_override.unwrap_or(entry_shell);

    let resolved = if std::path::Path::new(run_str).is_relative() {
        script_dir.join(run_str)
    } else {
        std::path::PathBuf::from(run_str)
    };

    let cfgd_env_path = cfgd_env_path_for(shell);

    let mut cmd = if resolved.exists() {
        // File path — check executable bit, run directly (OS handles shebang).
        // The override silently drops out on file scripts: the shebang owns
        // interpreter choice, so wrapping the file in `bash -c` would either
        // double-interpret it or break exec semantics. The entry's own
        // `shell:` field on a file is still a config bug.
        if entry_shell != ScriptShell::Auto {
            return Err(CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "shell field cannot be set on file-shebang scripts — set the shebang line inside '{}' itself",
                    resolved.posix(),
                ),
            }));
        }
        if shell_override.is_some() {
            tracing::debug!(
                script = %resolved.posix(),
                shell_override = ?shell_override,
                "--shell override ignored on file-shebang script"
            );
        }
        let meta = std::fs::metadata(&resolved)?;
        if !crate::is_executable(&resolved, &meta) {
            #[cfg(unix)]
            let hint = "chmod +x";
            #[cfg(windows)]
            let hint = "use a .exe, .cmd, .bat, or .ps1 extension";
            return Err(CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "script '{}' exists but is not executable ({})",
                    resolved.posix(),
                    hint,
                ),
            }));
        }
        let mut c = std::process::Command::new(&resolved);
        c.current_dir(working_dir);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        c
    } else {
        // Inline command — interpreter selected by shell field
        build_inline_command(shell, run_str, working_dir, cfgd_env_path.as_deref())
    };

    // Inject environment variables
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let interactive = matches!(
        entry,
        ScriptEntry::Full {
            interactive: true,
            ..
        }
    );
    match interactive_disposition(interactive, std::io::stdin().is_terminal()) {
        InteractiveDisposition::Run => {
            // Attach to the controlling terminal so the script can prompt the user
            // (e.g. `read`). No spinner and no capture — the user drives the pace,
            // and no idle timeout applies because an interactive step is attended
            // by definition.
            cmd.stdin(std::process::Stdio::inherit());
            cmd.stdout(std::process::Stdio::inherit());
            cmd.stderr(std::process::Stdio::inherit());
            let status = cmd.status()?;
            if !status.success() {
                let exit_code = status.code().unwrap_or(-1);
                return Err(CfgdError::Config(ConfigError::Invalid {
                    message: format!("script '{}' failed (exit {})", run_str, exit_code),
                }));
            }
            return Ok((label, true, None));
        }
        InteractiveDisposition::SkipNoTty => {
            // No TTY (CI, piped stdin, or any daemon-run phase): skip rather than
            // hang on instant EOF. changed=false records this as a clean no-op.
            printer.status_simple(
                Role::Warn,
                format!("{run_str} — interactive script skipped: no TTY available"),
            );
            return Ok((label, false, None));
        }
        InteractiveDisposition::NotInteractive => {}
    }

    // Execute with timeout
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // A spawn ENOENT here almost never means "the script is missing" — the
    // interpreter couldn't be resolved, typically because a `spec.env` PATH
    // entry overwrote PATH. Name the real cause instead of a bare os error 2.
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "could not spawn the script interpreter ({e}) — \
                     a spec.env PATH that drops the system bin dirs is the usual cause"
                ),
            })
        } else {
            CfgdError::from(e)
        }
    })?;

    // Spinner with live output display (same pattern as Printer::run_with_progress)
    let pb = printer.spinner(&label);

    // Channel for live display + Arc buffers for final capture.
    // Reader threads feed both so we get live scrolling output AND full capture.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let last_output = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let stdout_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

    let stdout_handle = spawn_pipe_reader(
        child.stdout.take(),
        std::sync::Arc::clone(&stdout_buf),
        std::sync::Arc::clone(&last_output),
        tx.clone(),
    );
    let stderr_handle = spawn_pipe_reader(
        child.stderr.take(),
        std::sync::Arc::clone(&stderr_buf),
        std::sync::Arc::clone(&last_output),
        tx.clone(),
    );
    drop(tx);

    const VISIBLE_LINES: usize = 5;
    let mut ring: std::collections::VecDeque<String> =
        std::collections::VecDeque::with_capacity(VISIBLE_LINES);

    let start = std::time::Instant::now();
    loop {
        // Drain any pending output lines and update the spinner display
        while let Ok(line) = rx.try_recv() {
            if ring.len() >= VISIBLE_LINES {
                ring.pop_front();
            }
            ring.push_back(line);
        }
        if !ring.is_empty() {
            let mut msg = label.clone();
            for l in &ring {
                let display = if l.len() > 120 {
                    l.get(..120).unwrap_or(l)
                } else {
                    l.as_str()
                };
                msg.push_str(&format!("\n  {}", display));
            }
            pb.set_message(msg);
        }

        match child.try_wait()? {
            Some(status) => {
                // Wait for reader threads to finish draining
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();

                let stdout_str = std::sync::Arc::try_unwrap(stdout_buf)
                    .ok()
                    .and_then(|m| m.into_inner().ok())
                    .unwrap_or_default();
                let stderr_str = std::sync::Arc::try_unwrap(stderr_buf)
                    .ok()
                    .and_then(|m| m.into_inner().ok())
                    .unwrap_or_default();

                let captured = combine_script_output(&stdout_str, &stderr_str);

                if !status.success() {
                    let exit_code = status.code().unwrap_or(-1);
                    pb.finish_fail(format!("{} (exit {})", run_str, exit_code));
                    let base = format!("script '{}' failed (exit {})", run_str, exit_code);
                    let message = match captured.as_deref().filter(|s| !s.is_empty()) {
                        Some(c) => format!("{base}\n{c}"),
                        None => base,
                    };
                    return Err(CfgdError::Config(ConfigError::Invalid { message }));
                }

                let elapsed = start.elapsed();
                pb.finish_ok(format!("{} ({}s)", run_str, elapsed.as_secs()));
                return Ok((label, true, captured));
            }
            None => {
                let elapsed = start.elapsed();
                let mut kill_reason = None;
                // Check absolute timeout
                if elapsed > effective_timeout {
                    kill_reason = Some(("timed out", effective_timeout));
                }
                // Check idle timeout (no output for N seconds)
                if kill_reason.is_none()
                    && let Some(idle_dur) = idle_timeout
                {
                    let last = *last_output.lock().unwrap_or_else(|e| e.into_inner());
                    if last.elapsed() > idle_dur {
                        kill_reason = Some(("idle (no output)", idle_dur));
                    }
                }
                // Cooperative abort: abort flag was set while the script was running.
                // Kill immediately (no grace period — we're already in an interrupt path).
                if kill_reason.is_none()
                    && let Some(a) = abort
                    && a.aborted().is_some()
                {
                    pb.finish_fail(format!("{} interrupted", run_str));
                    kill_script_child(&mut child, false);
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    return Err(CfgdError::Config(ConfigError::Invalid {
                        message: format!("script '{}' interrupted by signal", run_str),
                    }));
                }
                if let Some((reason, duration)) = kill_reason {
                    pb.finish_fail(format!(
                        "{} {} after {}s",
                        run_str,
                        reason,
                        duration.as_secs()
                    ));
                    kill_script_child(&mut child, true);
                    // Join reader threads so we capture partial output
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    let stdout_str = std::sync::Arc::try_unwrap(stdout_buf)
                        .ok()
                        .and_then(|m| m.into_inner().ok())
                        .unwrap_or_default();
                    let stderr_str = std::sync::Arc::try_unwrap(stderr_buf)
                        .ok()
                        .and_then(|m| m.into_inner().ok())
                        .unwrap_or_default();
                    let captured = combine_script_output(&stdout_str, &stderr_str);
                    let base = format!(
                        "script '{}' {} after {}s",
                        run_str,
                        reason,
                        duration.as_secs()
                    );
                    let message = match captured.as_deref().filter(|s| !s.is_empty()) {
                        Some(c) => format!("{base}\n{c}"),
                        None => base,
                    };
                    return Err(CfgdError::Config(ConfigError::Invalid { message }));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

/// Build the `Command` for an inline script based on the chosen shell interpreter.
///
/// When `cfgd_env_path` is `Some`, bash and zsh commands are prepended with a
/// preamble that sources `~/.cfgd.env` (with alias expansion enabled) so that
/// profile-level env vars and aliases are available to lifecycle scripts.
fn build_inline_command(
    shell: ScriptShell,
    run_str: &str,
    working_dir: &std::path::Path,
    cfgd_env_path: Option<&std::path::Path>,
) -> std::process::Command {
    let mut c = match shell {
        ScriptShell::Auto => {
            #[cfg(unix)]
            {
                let mut c = std::process::Command::new("sh");
                c.arg("-c").arg(run_str);
                c
            }
            #[cfg(windows)]
            {
                let mut c = std::process::Command::new("cmd.exe");
                c.arg("/C").arg(run_str);
                c
            }
        }
        ScriptShell::Sh => {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(run_str);
            c
        }
        ScriptShell::Bash => {
            let cmd_str = match cfgd_env_path {
                Some(p) => format!(
                    "shopt -s expand_aliases; source \"{}\" 2>/dev/null; {}",
                    p.display(),
                    run_str,
                ),
                None => run_str.to_string(),
            };
            let mut c = std::process::Command::new("bash");
            c.arg("-c").arg(cmd_str);
            c
        }
        ScriptShell::Zsh => {
            let cmd_str = match cfgd_env_path {
                Some(p) => format!(
                    "setopt aliases; source \"{}\" 2>/dev/null; {}",
                    p.display(),
                    run_str,
                ),
                None => run_str.to_string(),
            };
            let mut c = std::process::Command::new("zsh");
            c.arg("-c").arg(cmd_str);
            c
        }
        ScriptShell::Pwsh => {
            let mut c = std::process::Command::new("pwsh");
            c.arg("-NoProfile").arg("-Command").arg(run_str);
            c
        }
        ScriptShell::Cmd => {
            let mut c = std::process::Command::new("cmd.exe");
            c.arg("/C").arg(run_str);
            c
        }
    };
    c.current_dir(working_dir);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c
}

/// Resolve the `~/.cfgd.env` preamble path for bash/zsh inline commands.
/// Returns the expanded path only when it exists; `None` for other shells or
/// when the env file is absent.
fn cfgd_env_path_for(shell: ScriptShell) -> Option<std::path::PathBuf> {
    match shell {
        ScriptShell::Bash | ScriptShell::Zsh => {
            let p = crate::expand_tilde(std::path::Path::new("~/.cfgd.env"));
            if p.exists() { Some(p) } else { None }
        }
        _ => None,
    }
}

/// Resolve a `creates:` guard path.
///
/// A leading `~` expands to the home directory; a relative path resolves
/// against the script's `working_dir`. Absolute paths are used verbatim.
fn resolve_creates_path(path: &str, working_dir: &std::path::Path) -> std::path::PathBuf {
    let expanded = crate::expand_tilde(std::path::Path::new(path));
    if expanded.is_relative() {
        working_dir.join(expanded)
    } else {
        expanded
    }
}

/// Run an `onlyIf` / `unless` guard command and report whether it succeeded
/// (exited zero). Uses the same interpreter, working directory, and environment
/// as the script body, bounded by `timeout` so a guard can never hang the
/// reconcile.
///
/// Two failure modes are real errors, distinct from a non-zero exit (the normal
/// condition signal): a spawn failure (e.g. a missing interpreter, surfaced via
/// `?`), and a timeout (a hung guard is an environment fault, not a "skip" or
/// "run" decision — the watchdog's signal-kill exit status would otherwise be
/// silently read as a non-zero condition).
fn run_guard_command(
    cmd_str: &str,
    shell: ScriptShell,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    timeout: std::time::Duration,
) -> Result<bool> {
    let cfgd_env_path = cfgd_env_path_for(shell);
    let mut cmd = build_inline_command(shell, cmd_str, working_dir, cfgd_env_path.as_deref());
    for (key, value) in env_vars {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::null());
    let outcome = crate::command_output_with_timeout_outcome(&mut cmd, timeout)?;
    if outcome.timed_out {
        return Err(CfgdError::Config(ConfigError::Invalid {
            message: format!("guard command timed out after {timeout:?}: {cmd_str}"),
        }));
    }
    Ok(outcome.output.status.success())
}

/// Kill a script's process group.
///
/// `graceful=true` sends SIGTERM then waits 5 s before SIGKILL (for timeout/idle
/// kill paths). `graceful=false` sends SIGKILL immediately — used on the
/// abort path where cfgd itself received a signal and must exit quickly.
pub(super) fn kill_script_child(child: &mut std::process::Child, graceful: bool) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let signal = if graceful {
            Signal::SIGTERM
        } else {
            Signal::SIGKILL
        };
        // Negative PID targets the entire process group
        let _ = kill(Pid::from_raw(-(child.id() as i32)), signal);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    if graceful {
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Default `continue_on_error` behavior per script phase.
/// Pre-hooks abort on failure; post-hooks, onChange, onDrift continue.
pub(super) fn default_continue_on_error(phase: &ScriptPhase) -> bool {
    match phase {
        ScriptPhase::PreApply | ScriptPhase::PreReconcile => false,
        ScriptPhase::PostApply
        | ScriptPhase::PostReconcile
        | ScriptPhase::OnChange
        | ScriptPhase::OnDrift => true,
    }
}

/// Resolve the effective `continue_on_error` for a script entry in a given phase.
pub(super) fn effective_continue_on_error(entry: &ScriptEntry, phase: &ScriptPhase) -> bool {
    match entry {
        ScriptEntry::Full {
            continue_on_error: Some(v),
            ..
        } => *v,
        _ => default_continue_on_error(phase),
    }
}

/// Combine stdout and stderr into a single captured output string.
/// Returns `None` if both are empty.
pub(super) fn combine_script_output(stdout: &str, stderr: &str) -> Option<String> {
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    if stdout.is_empty() && stderr.is_empty() {
        return None;
    }
    let mut out = String::new();
    if !stdout.is_empty() {
        out.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !out.is_empty() {
            out.push_str("\n--- stderr ---\n");
        }
        out.push_str(stderr);
    }
    Some(out)
}

fn spawn_pipe_reader<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
    buf: std::sync::Arc<std::sync::Mutex<String>>,
    ts: std::sync::Arc<std::sync::Mutex<std::time::Instant>>,
    tx: std::sync::mpsc::Sender<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Some(pipe) = pipe else { return };
        let reader = std::io::BufReader::new(pipe);
        for line in std::io::BufRead::lines(reader) {
            match line {
                Ok(l) => {
                    *ts.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
                    let mut b = buf.lock().unwrap_or_else(|e| e.into_inner());
                    if !b.is_empty() {
                        b.push('\n');
                    }
                    b.push_str(&l);
                    let _ = tx.send(l);
                }
                Err(_) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EnvVar;

    fn fake_env_var(name: &str, value: &str) -> EnvVar {
        EnvVar {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    fn fake_config_dir() -> std::path::PathBuf {
        std::path::PathBuf::from("/config")
    }

    // build_module_script_env: module spec.env vars appear in the output.
    #[test]
    fn module_env_vars_propagated_to_script_env() {
        let module_env = vec![
            fake_env_var("PATH", "/custom/bin"),
            fake_env_var("GOPATH", "/foo"),
        ];
        let env = build_module_script_env(
            &fake_config_dir(),
            "workstation",
            ReconcileContext::Apply,
            &ScriptPhase::PostApply,
            Some("nvim"),
            None,
            &module_env,
        );

        let lookup = |key: &str| -> Option<&str> {
            env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
        };

        assert_eq!(lookup("PATH"), Some("/custom/bin"));
        assert_eq!(lookup("GOPATH"), Some("/foo"));
        // Runtime metadata is still present.
        assert_eq!(lookup("CFGD_MODULE_NAME"), Some("nvim"));
        assert_eq!(lookup("CFGD_PROFILE"), Some("workstation"));
        assert_eq!(lookup("CFGD_PHASE"), Some("postApply"));
    }

    // build_module_script_env: `$VAR`/`${VAR}` in declared values are expanded
    // against the process env + earlier vars, so `PATH: ...:$PATH` resolves to a
    // real PATH instead of landing literally and breaking interpreter spawn.
    #[test]
    #[serial_test::serial]
    fn module_env_values_expand_dollar_refs() {
        let _g = crate::test_helpers::EnvVarGuard::set("S84_EXPAND_BASE", "/opt/base");
        let module_env = vec![
            fake_env_var("PATH", "/custom/bin:$S84_EXPAND_BASE"),
            // fold-left: a later var resolves one declared earlier in spec.env.
            fake_env_var("FOO", "/x"),
            fake_env_var("BAR", "$FOO/y"),
            // an unset reference expands to empty, like a shell.
            fake_env_var("BAZ", "a${S84_DEFINITELY_UNSET}b"),
        ];
        let env = build_module_script_env(
            &fake_config_dir(),
            "workstation",
            ReconcileContext::Apply,
            &ScriptPhase::PostApply,
            Some("nvim"),
            None,
            &module_env,
        );
        let lookup = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        assert_eq!(lookup("PATH"), Some("/custom/bin:/opt/base"));
        assert_eq!(lookup("BAR"), Some("/x/y"));
        assert_eq!(lookup("BAZ"), Some("ab"));
    }

    // build_module_script_env: a leading `~` in a declared value expands to the
    // user's home BEFORE `$VAR` expansion. Without this, `CLIFT_DIR=~/.local/...`
    // would be injected literally and every consumer of the path would break.
    #[test]
    #[serial_test::serial]
    fn module_env_values_expand_leading_tilde() {
        let home = tempfile::tempdir().unwrap();
        crate::with_test_home(home.path(), || {
            let module_env = vec![
                fake_env_var("CLIFT_DIR", "~/.local/share/clift"),
                // tilde after a `:` (PATH-style) also expands, leading segment too.
                fake_env_var("MIXED", "~/bin:/usr/bin:~/x"),
            ];
            let env = build_module_script_env(
                &fake_config_dir(),
                "workstation",
                ReconcileContext::Apply,
                &ScriptPhase::PostApply,
                Some("clift"),
                None,
                &module_env,
            );
            let lookup = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
            // Env-file/injected values fold home to posix (the production contract):
            // build the expectation through the same central API so Windows matches.
            let h = home.path().posix().to_string();
            assert_eq!(
                lookup("CLIFT_DIR"),
                Some(format!("{h}/.local/share/clift").as_str())
            );
            assert_eq!(
                lookup("MIXED"),
                Some(format!("{h}/bin:/usr/bin:{h}/x").as_str())
            );
        });
    }

    // script_default_workdir: the home directory is the default CWD for every
    // lifecycle script — NOT the config source tree — so a relative write can't
    // pollute the user's GitOps repo (finding E).
    #[test]
    #[serial_test::serial]
    fn script_default_workdir_is_home_not_config() {
        let home = tempfile::tempdir().unwrap();
        crate::with_test_home(home.path(), || {
            let wd = script_default_workdir(&fake_config_dir());
            assert_eq!(wd, home.path());
            assert_ne!(wd, fake_config_dir());
        });
    }

    // execute_script: a per-script `workdir:` overrides the caller-provided CWD;
    // a relative write lands in the override dir, not the default.
    #[cfg(unix)]
    #[test]
    fn execute_script_workdir_override_absolute() {
        let printer = crate::test_helpers::test_printer();
        let default_dir = tempfile::tempdir().unwrap();
        let override_dir = tempfile::tempdir().unwrap();
        let entry = ScriptEntry::Full {
            run: "touch ran.marker".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
            workdir: Some(override_dir.path().display().to_string()),
        };
        execute_script(
            &entry,
            default_dir.path(),
            default_dir.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect("script with workdir override must run");
        assert!(
            override_dir.path().join("ran.marker").exists(),
            "relative write must land in the workdir override"
        );
        assert!(
            !default_dir.path().join("ran.marker").exists(),
            "relative write must NOT land in the caller-provided default dir"
        );
    }

    // execute_script: `workdir:` expands a leading `~` and `$VAR`/`${VAR}` against
    // the script environment (here `$CFGD_MODULE_DIR`).
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn execute_script_workdir_override_expands_tilde_and_vars() {
        let printer = crate::test_helpers::test_printer();
        let home = tempfile::tempdir().unwrap();
        let module_dir = tempfile::tempdir().unwrap();
        crate::with_test_home(home.path(), || {
            // `~` form → home.
            let entry_home = ScriptEntry::Full {
                run: "touch from_tilde".into(),
                timeout: None,
                idle_timeout: None,
                continue_on_error: None,
                shell: ScriptShell::Auto,
                only_if: None,
                unless: None,
                creates: None,
                interactive: false,
                workdir: Some("~".into()),
            };
            execute_script(
                &entry_home,
                module_dir.path(),
                module_dir.path(),
                &[],
                std::time::Duration::from_secs(5),
                &printer,
                None,
                None,
            )
            .expect("workdir ~ must run");
            assert!(home.path().join("from_tilde").exists());

            // `$CFGD_MODULE_DIR` form → the module dir from the script env.
            let entry_var = ScriptEntry::Full {
                run: "touch from_var".into(),
                timeout: None,
                idle_timeout: None,
                continue_on_error: None,
                shell: ScriptShell::Auto,
                only_if: None,
                unless: None,
                creates: None,
                interactive: false,
                workdir: Some("$CFGD_MODULE_DIR".into()),
            };
            let env = vec![(
                "CFGD_MODULE_DIR".to_string(),
                module_dir.path().display().to_string(),
            )];
            execute_script(
                &entry_var,
                home.path(),
                home.path(),
                &env,
                std::time::Duration::from_secs(5),
                &printer,
                None,
                None,
            )
            .expect("workdir $CFGD_MODULE_DIR must run");
            assert!(module_dir.path().join("from_var").exists());
        });
    }

    // CFGD_* env var names are rejected at parse time (EnvVar deserialization),
    // so they never reach build_module_script_env. This test verifies the
    // parse-time guard works via the validate_env_var_user_name function.
    #[test]
    fn cfgd_prefix_rejected_at_parse_time() {
        let yaml = r#"
- name: CFGD_MODULE_NAME
  value: spoofed
"#;
        let err = serde_yaml::from_str::<Vec<crate::config::EnvVar>>(yaml)
            .expect_err("CFGD_* names must be rejected during deserialization");
        let msg = format!("{err}");
        assert!(
            msg.contains("reserved"),
            "error should mention 'reserved': {msg}"
        );
        assert!(
            msg.contains("CFGD_"),
            "error should mention the CFGD_ prefix: {msg}"
        );
    }

    // execute_script: a missing working_dir surfaces a structured error naming
    // both the script and the path, instead of the cryptic `io error: No such
    // file or directory (os error 2)` from `cmd.spawn()`.
    #[test]
    fn execute_script_rejects_missing_working_dir() {
        let printer = crate::test_helpers::test_printer();
        let entry = ScriptEntry::Simple("true".into());
        let missing = std::path::PathBuf::from("/nonexistent/path/does/not/exist");

        let err = execute_script(
            &entry,
            &missing,
            &missing,
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect_err("missing working_dir must error");

        match err {
            CfgdError::Config(ConfigError::Invalid { message }) => {
                assert!(
                    message.contains("working directory does not exist"),
                    "message should describe the missing-dir failure mode: {message}"
                );
                assert!(
                    message.contains("/nonexistent/path/does/not/exist"),
                    "message should name the offending path: {message}"
                );
                assert!(
                    message.contains("'true'"),
                    "message should name the script (run_str): {message}"
                );
            }
            other => panic!("expected ConfigError::Invalid, got: {other:?}"),
        }
    }

    // execute_script: an existing path that is NOT a directory (a regular file)
    // surfaces a distinct error mentioning what was found and the path.
    #[test]
    fn execute_script_rejects_non_directory_working_dir() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("not-a-dir");
        std::fs::write(&file_path, b"hello").unwrap();
        let entry = ScriptEntry::Simple("true".into());

        let err = execute_script(
            &entry,
            &file_path,
            &file_path,
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect_err("non-directory working_dir must error");

        match err {
            CfgdError::Config(ConfigError::Invalid { message }) => {
                assert!(
                    message.contains("not a directory"),
                    "message should distinguish the non-dir failure mode: {message}"
                );
                assert!(
                    message.contains(&crate::to_posix_string(&file_path)),
                    "message should name the offending path: {message}"
                );
            }
            other => panic!("expected ConfigError::Invalid, got: {other:?}"),
        }
    }

    // execute_script: validation does not regress the happy path — a valid
    // working_dir with a trivial inline command still succeeds.
    #[test]
    fn execute_script_runs_with_valid_working_dir() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        let entry = ScriptEntry::Simple("true".into());

        let (label, changed, _captured) = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect("valid working_dir + `true` must succeed");

        assert!(changed, "scripts always report changed=true");
        assert!(
            label.contains("true"),
            "label should reference the script's run_str: {label}"
        );
    }

    // build_module_script_env: empty module env produces the same output as
    // build_script_env (no regressions for modules without spec.env).
    #[test]
    fn empty_module_env_matches_base_build_script_env() {
        let base = build_script_env(
            &fake_config_dir(),
            "workstation",
            ReconcileContext::Apply,
            &ScriptPhase::PreApply,
            Some("mymod"),
            None,
        );
        let with_empty = build_module_script_env(
            &fake_config_dir(),
            "workstation",
            ReconcileContext::Apply,
            &ScriptPhase::PreApply,
            Some("mymod"),
            None,
            &[],
        );
        assert_eq!(base, with_empty);
    }

    // bash on windows GHA resolves to WSL bash, which errors with "Windows
    // Subsystem for Linux has no installed distributions". Gate to unix.
    #[cfg(unix)]
    #[test]
    fn shell_bash_runs_inline_with_bash() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        let entry = ScriptEntry::Full {
            workdir: None,
            run: "echo hello".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Bash,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        };

        let (label, changed, captured) = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect("bash inline script must succeed");

        assert!(changed);
        assert!(label.contains("echo hello"));
        assert_eq!(captured.as_deref(), Some("hello"));
    }

    #[test]
    fn shell_field_rejected_on_file_scripts() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("myscript.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let entry = ScriptEntry::Full {
            workdir: None,
            run: "myscript.sh".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Bash,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        };

        let err = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect_err("shell field on file script must be rejected");

        match err {
            CfgdError::Config(ConfigError::Invalid { message }) => {
                assert!(
                    message.contains("shell field cannot be set on file-shebang scripts"),
                    "unexpected message: {message}"
                );
                assert!(
                    message.contains("myscript.sh"),
                    "message should name the script file: {message}"
                );
            }
            other => panic!("expected ConfigError::Invalid, got: {other:?}"),
        }
    }

    #[test]
    fn bash_inline_prepends_env_source() {
        let tmp = tempfile::tempdir().unwrap();
        let env_file = tmp.path().join(".cfgd.env");
        std::fs::write(&env_file, "export TEST_VAR=hello\n").unwrap();

        let cmd = build_inline_command(
            ScriptShell::Bash,
            "echo $TEST_VAR",
            tmp.path(),
            Some(&env_file),
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let combined = args.join(" ");
        assert!(
            combined.contains("shopt -s expand_aliases"),
            "bash preamble should enable alias expansion: {combined}"
        );
        assert!(
            combined.contains("source"),
            "bash preamble should source the env file: {combined}"
        );
        assert!(
            combined.contains("2>/dev/null"),
            "source should suppress errors: {combined}"
        );
        assert!(
            combined.contains("echo $TEST_VAR"),
            "original command must be preserved: {combined}"
        );
    }

    #[test]
    fn zsh_inline_prepends_env_source() {
        let tmp = tempfile::tempdir().unwrap();
        let env_file = tmp.path().join(".cfgd.env");
        std::fs::write(&env_file, "export TEST_VAR=hello\n").unwrap();

        let cmd = build_inline_command(
            ScriptShell::Zsh,
            "echo $TEST_VAR",
            tmp.path(),
            Some(&env_file),
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let combined = args.join(" ");
        assert!(
            combined.contains("setopt aliases"),
            "zsh preamble should enable alias expansion: {combined}"
        );
        assert!(
            combined.contains("source"),
            "zsh preamble should source the env file: {combined}"
        );
        assert!(
            combined.contains("2>/dev/null"),
            "source should suppress errors: {combined}"
        );
        assert!(
            combined.contains("echo $TEST_VAR"),
            "original command must be preserved: {combined}"
        );
    }

    #[test]
    fn sh_inline_ignores_cfgd_env_path() {
        let tmp = tempfile::tempdir().unwrap();
        let env_file = tmp.path().join(".cfgd.env");
        std::fs::write(&env_file, "export TEST_VAR=hello\n").unwrap();

        let cmd = build_inline_command(ScriptShell::Sh, "echo hello", tmp.path(), Some(&env_file));
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let combined = args.join(" ");
        assert!(
            !combined.contains("source"),
            "sh should not source the env file: {combined}"
        );
        assert_eq!(
            args,
            vec!["-c", "echo hello"],
            "sh command should be unchanged"
        );
    }

    #[test]
    fn bash_inline_no_env_file_skips_preamble() {
        let tmp = tempfile::tempdir().unwrap();

        let cmd = build_inline_command(ScriptShell::Bash, "echo hello", tmp.path(), None);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["-c", "echo hello"], "no env file → no preamble");
    }

    // Auto-detection picks the file's shebang-implied interpreter (`sh`),
    // unavailable on Windows GHA. Gate to unix.
    #[cfg(unix)]
    #[test]
    fn shell_auto_on_file_scripts_allowed() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("ok.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho ok\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let entry = ScriptEntry::Full {
            workdir: None,
            run: "ok.sh".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        };

        let result = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        );
        assert!(result.is_ok(), "Auto shell on file scripts must be allowed");
    }

    // --shell override: passing Some(Bash) on a Simple inline script wraps the
    // command in bash, independent of the entry's own shell field (Auto here).
    #[cfg(unix)]
    #[test]
    fn execute_script_uses_shell_override_for_inline_command() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        // BASH_VERSION is exported by bash but not by sh/dash; if the override
        // wired through, the variable resolves and gets echoed. If not, the
        // empty expansion shows the override was dropped on the floor.
        let entry = ScriptEntry::Simple("echo \"${BASH_VERSION:-no-bash}\"".into());

        let (_, _, captured) = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            Some(ScriptShell::Bash),
            None,
        )
        .expect("inline script with bash override must succeed");

        let out = captured.expect("bash should echo a version string");
        assert!(
            out != "no-bash" && !out.is_empty(),
            "override did not route through bash (got {out:?})"
        );
    }

    // --shell override: passing Some(Bash) on a file-shebang script is silently
    // ignored. The shebang owns interpreter choice — wrapping the file in
    // `bash -c "/path/to/file"` would either double-interpret it or break exec
    // semantics. The script runs directly; no error surfaces.
    #[cfg(unix)]
    #[test]
    fn execute_script_override_ignored_on_file_shebang() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("ok.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho file-shebang\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let entry = ScriptEntry::Simple("ok.sh".into());

        let (_, _, captured) = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            Some(ScriptShell::Bash),
            None,
        )
        .expect("override on file-shebang script must not error");

        assert_eq!(
            captured.as_deref(),
            Some("file-shebang"),
            "file script must run via its own shebang, not via bash wrapper"
        );
    }

    // --shell override: an entry that explicitly sets `shell:` on a file path
    // still errors (that's a user config bug, independent of the override).
    #[test]
    fn execute_script_entry_shell_on_file_script_still_errors_with_override() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("buggy.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let entry = ScriptEntry::Full {
            workdir: None,
            run: "buggy.sh".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Zsh,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        };

        let err = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            Some(ScriptShell::Bash),
            None,
        )
        .expect_err("entry-level shell on a file script must still be rejected");

        match err {
            CfgdError::Config(ConfigError::Invalid { message }) => {
                assert!(
                    message.contains("shell field cannot be set on file-shebang scripts"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected ConfigError::Invalid, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Idempotency guards: creates / onlyIf / unless
    // -----------------------------------------------------------------------

    // A guarded entry whose body writes a sentinel marker into the tempdir.
    // After execution, marker presence proves the body ran; absence proves a
    // skip. The marker name is fixed; the guard fields are the variable.
    #[cfg(unix)]
    fn guarded_entry(
        only_if: Option<&str>,
        unless: Option<&str>,
        creates: Option<&str>,
    ) -> ScriptEntry {
        ScriptEntry::Full {
            workdir: None,
            run: "touch ran.marker".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: only_if.map(String::from),
            unless: unless.map(String::from),
            creates: creates.map(String::from),
            interactive: false,
        }
    }

    #[cfg(unix)]
    fn run_guarded(entry: &ScriptEntry, working_dir: &std::path::Path) -> (bool, bool) {
        let printer = crate::test_helpers::test_printer();
        let (_label, changed, _captured) = execute_script(
            entry,
            working_dir,
            working_dir,
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect("guarded script must not error");
        let ran = working_dir.join("ran.marker").exists();
        (changed, ran)
    }

    #[cfg(unix)]
    #[test]
    fn guard_creates_existing_skips() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("present"), b"x").unwrap();
        let entry = guarded_entry(None, None, Some("present"));
        let (changed, ran) = run_guarded(&entry, tmp.path());
        assert!(!changed, "existing creates path must report changed=false");
        assert!(!ran, "body must not run when creates path exists");
    }

    #[cfg(unix)]
    #[test]
    fn guard_creates_missing_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = guarded_entry(None, None, Some("absent"));
        let (changed, ran) = run_guarded(&entry, tmp.path());
        assert!(changed, "missing creates path must report changed=true");
        assert!(ran, "body must run when creates path is absent");
    }

    #[cfg(unix)]
    #[test]
    fn guard_only_if_true_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = guarded_entry(Some("true"), None, None);
        let (changed, ran) = run_guarded(&entry, tmp.path());
        assert!(changed, "onlyIf zero-exit must permit running");
        assert!(ran, "body must run when onlyIf succeeds");
    }

    #[cfg(unix)]
    #[test]
    fn guard_only_if_false_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = guarded_entry(Some("false"), None, None);
        let (changed, ran) = run_guarded(&entry, tmp.path());
        assert!(!changed, "onlyIf non-zero-exit must skip");
        assert!(!ran, "body must not run when onlyIf fails");
    }

    #[cfg(unix)]
    #[test]
    fn guard_unless_true_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = guarded_entry(None, Some("true"), None);
        let (changed, ran) = run_guarded(&entry, tmp.path());
        assert!(!changed, "unless zero-exit must skip");
        assert!(!ran, "body must not run when unless succeeds");
    }

    #[cfg(unix)]
    #[test]
    fn guard_unless_false_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = guarded_entry(None, Some("false"), None);
        let (changed, ran) = run_guarded(&entry, tmp.path());
        assert!(changed, "unless non-zero-exit must permit running");
        assert!(ran, "body must run when unless fails");
    }

    // All guards permit running only when every one says "run".
    #[cfg(unix)]
    #[test]
    fn guard_combined_all_pass_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = guarded_entry(Some("true"), Some("false"), Some("absent"));
        let (changed, ran) = run_guarded(&entry, tmp.path());
        assert!(changed);
        assert!(ran, "body must run when all guards permit");
    }

    // A single skipping guard short-circuits even when others would permit.
    #[cfg(unix)]
    #[test]
    fn guard_combined_one_blocks_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = guarded_entry(Some("true"), Some("true"), Some("absent"));
        let (changed, ran) = run_guarded(&entry, tmp.path());
        assert!(!changed, "any blocking guard must skip");
        assert!(!ran, "body must not run when unless already holds");
    }

    // creates resolves a leading `~` via expand_tilde; a relative path resolves
    // against working_dir.
    #[cfg(unix)]
    #[test]
    fn creates_path_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let rel = resolve_creates_path("sub/file", tmp.path());
        assert_eq!(rel, tmp.path().join("sub/file"));

        let abs = resolve_creates_path("/etc/hosts", tmp.path());
        assert_eq!(abs, std::path::PathBuf::from("/etc/hosts"));

        let home = tempfile::tempdir().unwrap();
        crate::with_test_home(home.path(), || {
            let tilde = resolve_creates_path("~/thing", tmp.path());
            assert_eq!(tilde, home.path().join("thing"));
        });
    }

    // A guard command whose interpreter cannot spawn is a real error, distinct
    // from a non-zero exit (the normal condition signal). Skip when pwsh is
    // actually installed (the spawn would then succeed).
    #[test]
    fn guard_spawn_failure_errors() {
        if crate::command_available("pwsh") {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let result = run_guard_command(
            "irrelevant",
            ScriptShell::Pwsh,
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
        );
        assert!(
            result.is_err(),
            "a guard whose interpreter cannot spawn must error, not skip silently"
        );
    }

    // A guard command that hangs past its timeout is a hard error — not a
    // silently coerced "skip"/"run" condition signal. Uses the parameterized
    // timeout seam so the test stays fast.
    #[cfg(unix)]
    #[test]
    fn guard_timeout_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run_guard_command(
            "sleep 5",
            ScriptShell::Auto,
            tmp.path(),
            &[],
            std::time::Duration::from_millis(100),
        );
        match result {
            Err(CfgdError::Config(ConfigError::Invalid { message })) => {
                assert!(
                    message.contains("timed out"),
                    "timeout error should say so: {message}"
                );
                assert!(
                    message.contains("sleep 5"),
                    "timeout error should name the guard command: {message}"
                );
            }
            other => panic!("a hung guard must error with a timeout message, got: {other:?}"),
        }
    }

    // End-to-end: a guarded script whose onlyIf/unless command hangs past the
    // (parameterized) default timeout must make execute_script return Err, not
    // silently skip or run the body.
    #[cfg(unix)]
    #[test]
    fn execute_script_guard_timeout_returns_err() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("body-ran");
        let entry = ScriptEntry::Full {
            workdir: None,
            run: format!("touch {}", sentinel.display()),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: Some("sleep 5".to_string()),
            unless: None,
            creates: None,
            interactive: false,
        };

        let result = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_millis(100),
            &printer,
            None,
            None,
        );

        match result {
            Err(CfgdError::Config(ConfigError::Invalid { message })) => {
                assert!(
                    message.contains("timed out"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("a hung guard must propagate as Err, got: {other:?}"),
        }
        assert!(
            !sentinel.exists(),
            "body must not run when a guard timed out"
        );
    }

    // -----------------------------------------------------------------------
    // Interactive scripts (TTY-or-skip-with-warn)
    // -----------------------------------------------------------------------

    #[test]
    fn interactive_disposition_branches() {
        assert_eq!(
            interactive_disposition(false, true),
            InteractiveDisposition::NotInteractive
        );
        assert_eq!(
            interactive_disposition(false, false),
            InteractiveDisposition::NotInteractive
        );
        assert_eq!(
            interactive_disposition(true, true),
            InteractiveDisposition::Run
        );
        assert_eq!(
            interactive_disposition(true, false),
            InteractiveDisposition::SkipNoTty
        );
    }

    // The test process has no TTY, so an interactive script must be SKIPPED:
    // changed=false, the body does not run (sentinel absent), and a Warn line
    // names the script and the missing-TTY reason.
    #[cfg(all(unix, feature = "test-helpers"))]
    #[test]
    fn interactive_script_without_tty_skips_with_warn() {
        let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("body-ran");
        let entry = ScriptEntry::Full {
            workdir: None,
            run: format!("touch {}", sentinel.display()),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: true,
        };

        let (_label, changed, captured) = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect("interactive skip must not error");
        printer.flush();

        assert!(
            !changed,
            "no-TTY interactive script must report changed=false"
        );
        assert!(captured.is_none(), "skip captures no output");
        assert!(
            !sentinel.exists(),
            "body must not run when an interactive script is skipped"
        );
        let out = crate::output::strip_ansi(&buf.lock().unwrap());
        assert!(
            out.contains("interactive script skipped") && out.contains("no TTY"),
            "skip line should name the missing-TTY reason: {out:?}"
        );
    }

    // A skip emits a Role::Skipped status line naming the guard and reason.
    #[cfg(all(unix, feature = "test-helpers"))]
    #[test]
    fn guard_skip_emits_skipped_status_line() {
        let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
        let tmp = tempfile::tempdir().unwrap();
        let entry = guarded_entry(None, Some("true"), None);
        let (_label, changed, _captured) = execute_script(
            &entry,
            tmp.path(),
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect("skip must not error");
        printer.flush();
        assert!(!changed);
        let out = crate::output::strip_ansi(&buf.lock().unwrap());
        assert!(
            out.contains("unless condition already holds"),
            "skip line should name the unless guard and reason: {out:?}"
        );
    }

    // -----------------------------------------------------------------------
    // run_str resolution, spawn-failure mapping, exit-code error path
    // -----------------------------------------------------------------------

    // An absolute `run:` path that exists is executed directly as a file
    // (scripts.rs:306-307, the non-relative branch), NOT joined against
    // script_dir. `/bin/true` exits zero, so the script reports changed=true.
    #[cfg(unix)]
    #[test]
    fn execute_script_absolute_run_path_runs_as_file() {
        let printer = crate::test_helpers::test_printer();
        let work = tempfile::tempdir().unwrap();
        let script_dir = tempfile::tempdir().unwrap();
        let true_bin = if std::path::Path::new("/usr/bin/true").exists() {
            "/usr/bin/true"
        } else {
            "/bin/true"
        };
        let entry = ScriptEntry::Simple(true_bin.into());

        let (label, changed, _captured) = execute_script(
            &entry,
            // A script_dir that does NOT contain `true` — proves the absolute
            // path is used verbatim, never joined onto script_dir.
            script_dir.path(),
            work.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect("absolute executable run path must run directly");
        assert!(changed, "a zero-exit file script reports changed=true");
        assert!(
            label.contains(true_bin),
            "label should reference the absolute run path: {label}"
        );
    }

    // A non-zero exit from the body surfaces a structured error naming the
    // script and the exit code (scripts.rs:490-498). `/bin/false` exits 1.
    #[cfg(unix)]
    #[test]
    fn execute_script_nonzero_exit_errors_with_exit_code() {
        let printer = crate::test_helpers::test_printer();
        let work = tempfile::tempdir().unwrap();
        // Inline command that prints to stdout then exits non-zero, so the
        // error message also folds in the captured output.
        let entry = ScriptEntry::Simple("echo boom; exit 7".into());

        let err = execute_script(
            &entry,
            work.path(),
            work.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect_err("a non-zero exit must surface as Err");

        match err {
            CfgdError::Config(ConfigError::Invalid { message }) => {
                assert!(
                    message.contains("failed (exit 7)"),
                    "message should name the real exit code: {message}"
                );
                assert!(
                    message.contains("echo boom; exit 7"),
                    "message should name the failing script: {message}"
                );
                assert!(
                    message.contains("boom"),
                    "message should fold in the script's captured output: {message}"
                );
            }
            other => panic!("expected ConfigError::Invalid, got: {other:?}"),
        }
    }

    // A spawn ENOENT (the interpreter binary cannot be resolved) is remapped to
    // a targeted message pointing at a PATH-dropping spec.env as the usual
    // cause, instead of a bare `os error 2` (scripts.rs:410-421). Exercised via
    // the Pwsh interpreter when pwsh is not installed; skipped otherwise (the
    // spawn would then succeed).
    #[cfg(unix)]
    #[test]
    fn execute_script_spawn_enoent_maps_to_interpreter_hint() {
        if crate::command_available("pwsh") {
            return;
        }
        let printer = crate::test_helpers::test_printer();
        let work = tempfile::tempdir().unwrap();
        let entry = ScriptEntry::Full {
            workdir: None,
            run: "Write-Output hi".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Pwsh,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        };

        let err = execute_script(
            &entry,
            work.path(),
            work.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
            None,
            None,
        )
        .expect_err("a missing interpreter must surface as Err, not hang");

        match err {
            CfgdError::Config(ConfigError::Invalid { message }) => {
                assert!(
                    message.contains("could not spawn the script interpreter"),
                    "ENOENT spawn must be remapped to the interpreter hint: {message}"
                );
                assert!(
                    message.contains("spec.env PATH"),
                    "hint should point at a PATH-dropping spec.env: {message}"
                );
            }
            other => panic!("expected ConfigError::Invalid, got: {other:?}"),
        }
    }

    // build_inline_command(Pwsh, ...): pwsh is invoked with -NoProfile -Command
    // and the raw command, with no env-file preamble (cfgd_env_path_for returns
    // None for non-bash/zsh shells). Asserts the exact argv shape
    // (scripts.rs:630-633).
    #[test]
    fn pwsh_inline_command_argv_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let cmd = build_inline_command(ScriptShell::Pwsh, "Get-Date", tmp.path(), None);
        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "pwsh",
            "Pwsh shell must invoke the pwsh interpreter"
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            args,
            vec!["-NoProfile", "-Command", "Get-Date"],
            "pwsh argv must be -NoProfile -Command <cmd> with no preamble"
        );
    }

    // cfgd_env_path_for returns None for every non-bash/zsh shell even when a
    // `~/.cfgd.env` exists — only bash/zsh get the source preamble
    // (scripts.rs:653-660).
    #[test]
    #[serial_test::serial]
    fn cfgd_env_path_for_only_bash_and_zsh() {
        let home = tempfile::tempdir().unwrap();
        crate::with_test_home(home.path(), || {
            // Create a real ~/.cfgd.env so the `exists()` check would pass.
            std::fs::write(home.path().join(".cfgd.env"), "export X=1\n").unwrap();

            assert!(
                cfgd_env_path_for(ScriptShell::Bash).is_some(),
                "bash must pick up an existing ~/.cfgd.env"
            );
            assert!(
                cfgd_env_path_for(ScriptShell::Zsh).is_some(),
                "zsh must pick up an existing ~/.cfgd.env"
            );
            assert!(
                cfgd_env_path_for(ScriptShell::Sh).is_none(),
                "sh must never source ~/.cfgd.env"
            );
            assert!(
                cfgd_env_path_for(ScriptShell::Auto).is_none(),
                "auto must never source ~/.cfgd.env"
            );
            assert!(
                cfgd_env_path_for(ScriptShell::Pwsh).is_none(),
                "pwsh must never source ~/.cfgd.env"
            );
        });
    }

    // resolve_script_workdir: a `$VAR` reference present in the script env is
    // expanded; an unset reference expands to empty; a leading `~` expands to
    // home AFTER var expansion (scripts.rs:134-142).
    #[test]
    #[serial_test::serial]
    fn resolve_script_workdir_expands_vars_then_tilde() {
        let home = tempfile::tempdir().unwrap();
        crate::with_test_home(home.path(), || {
            let env = vec![("DEPLOY".to_string(), "/srv/app".to_string())];

            // $VAR present in env → expands to its value.
            assert_eq!(
                resolve_script_workdir("$DEPLOY/cfg", &env),
                std::path::PathBuf::from("/srv/app/cfg")
            );
            // ${VAR} braced form too.
            assert_eq!(
                resolve_script_workdir("${DEPLOY}", &env),
                std::path::PathBuf::from("/srv/app")
            );
            // Leading ~ expands to home.
            assert_eq!(
                resolve_script_workdir("~/sub", &env),
                home.path().join("sub")
            );
            // Unset reference expands to empty (shell-like).
            assert_eq!(
                resolve_script_workdir("/a/${NOPE}/b", &env),
                std::path::PathBuf::from("/a//b")
            );
        });
    }

    // build_script_env: the Reconcile context maps to the "reconcile" CFGD_CONTEXT
    // value (the Apply arm is covered elsewhere), and module_dir is injected when
    // provided. Asserts exact names+values and that module_name is omitted when
    // None (scripts.rs:57-70).
    #[test]
    fn build_script_env_reconcile_context_and_module_dir() {
        let env = build_script_env(
            std::path::Path::new("/cfg"),
            "node",
            ReconcileContext::Reconcile,
            &ScriptPhase::OnDrift,
            None,
            Some(std::path::Path::new("/mods/x")),
        );
        let lookup = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());

        assert_eq!(lookup("CFGD_CONTEXT"), Some("reconcile"));
        assert_eq!(lookup("CFGD_PROFILE"), Some("node"));
        assert_eq!(
            lookup("CFGD_PHASE"),
            Some(ScriptPhase::OnDrift.display_name())
        );
        assert_eq!(lookup("CFGD_CONFIG_DIR"), Some("/cfg"));
        assert_eq!(lookup("CFGD_MODULE_DIR"), Some("/mods/x"));
        assert_eq!(
            lookup("CFGD_MODULE_NAME"),
            None,
            "module name must be omitted when None"
        );
    }
}

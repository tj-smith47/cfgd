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
    // interpreter couldn't be resolved, either because it is not installed
    // (e.g. `shell: bash` on a FreeBSD base that ships only POSIX sh) or
    // because a `spec.env` PATH entry overwrote PATH. Name the real causes
    // instead of a bare os error 2.
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "could not spawn the script interpreter ({e}) — the interpreter \
                     may not be installed, or a spec.env PATH dropped the system bin dirs"
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
mod tests;

use crate::config::{ScriptEntry, ScriptShell};
use crate::errors::{CfgdError, ConfigError, Result};
use crate::output::Printer;

use super::types::{ReconcileContext, ScriptPhase};

// ---------------------------------------------------------------------------
// Unified script executor
// ---------------------------------------------------------------------------

/// Default timeout for module-level scripts.
pub(super) const MODULE_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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
    for ev in module_env {
        env.push((ev.name.clone(), ev.value.clone()));
    }
    env
}

/// Unified script executor for all hook types at both profile and module level.
///
/// Returns (description, changed, captured_output). All scripts set changed=true.
pub(crate) fn execute_script(
    entry: &ScriptEntry,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    default_timeout: std::time::Duration,
    printer: &Printer,
) -> Result<(String, bool, Option<String>)> {
    let run_str = entry.run_str();

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
                    working_dir.display()
                ),
            }));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "script '{}' cannot run: working directory does not exist: {}",
                    run_str,
                    working_dir.display()
                ),
            }));
        }
        Err(e) => {
            return Err(CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "script '{}' cannot run: working directory inaccessible ({}): {}",
                    run_str,
                    e,
                    working_dir.display()
                ),
            }));
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

    let shell = match entry {
        ScriptEntry::Full { shell, .. } => *shell,
        ScriptEntry::Simple(_) => ScriptShell::Auto,
    };

    let resolved = if std::path::Path::new(run_str).is_relative() {
        working_dir.join(run_str)
    } else {
        std::path::PathBuf::from(run_str)
    };

    let cfgd_env_path = match shell {
        ScriptShell::Bash | ScriptShell::Zsh => {
            let p = crate::expand_tilde(std::path::Path::new("~/.cfgd.env"));
            if p.exists() { Some(p) } else { None }
        }
        _ => None,
    };

    let mut cmd = if resolved.exists() {
        // File path — check executable bit, run directly (OS handles shebang)
        if shell != ScriptShell::Auto {
            return Err(CfgdError::Config(ConfigError::Invalid {
                message: format!(
                    "shell field cannot be set on file-shebang scripts — set the shebang line inside '{}' itself",
                    resolved.display(),
                ),
            }));
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
                    resolved.display(),
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

    let label = format!("Running script: {}", run_str);

    // Execute with timeout
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;

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
                if let Some((reason, duration)) = kill_reason {
                    pb.finish_fail(format!(
                        "{} {} after {}s",
                        run_str,
                        reason,
                        duration.as_secs()
                    ));
                    kill_script_child(&mut child);
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

/// Kill a script's process group (SIGTERM + grace period + SIGKILL).
pub(super) fn kill_script_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        // Negative PID targets the entire process group
        let _ = kill(Pid::from_raw(-(child.id() as i32)), Signal::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        crate::terminate_process(child.id());
    }
    std::thread::sleep(std::time::Duration::from_secs(5));
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
            &[],
            std::time::Duration::from_secs(5),
            &printer,
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
            &[],
            std::time::Duration::from_secs(5),
            &printer,
        )
        .expect_err("non-directory working_dir must error");

        match err {
            CfgdError::Config(ConfigError::Invalid { message }) => {
                assert!(
                    message.contains("not a directory"),
                    "message should distinguish the non-dir failure mode: {message}"
                );
                assert!(
                    message.contains(&file_path.display().to_string()),
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
            &[],
            std::time::Duration::from_secs(5),
            &printer,
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

    #[test]
    fn shell_bash_runs_inline_with_bash() {
        let printer = crate::test_helpers::test_printer();
        let tmp = tempfile::tempdir().unwrap();
        let entry = ScriptEntry::Full {
            run: "echo hello".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Bash,
        };

        let (label, changed, captured) = execute_script(
            &entry,
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
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
            run: "myscript.sh".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Bash,
        };

        let err = execute_script(
            &entry,
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
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
            run: "ok.sh".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
        };

        let result = execute_script(
            &entry,
            tmp.path(),
            &[],
            std::time::Duration::from_secs(5),
            &printer,
        );
        assert!(result.is_ok(), "Auto shell on file scripts must be allowed");
    }
}

use crate::config::ScriptEntry;
use crate::errors::{ConfigError, Result};
use crate::output_v2::Printer;

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
    let effective_timeout = match entry {
        ScriptEntry::Full {
            timeout: Some(t), ..
        } => crate::parse_duration_str(t)
            .map_err(|e| crate::errors::CfgdError::Config(ConfigError::Invalid { message: e }))?,
        _ => default_timeout,
    };
    let idle_timeout =
        match entry {
            ScriptEntry::Full {
                idle_timeout: Some(t),
                ..
            } => Some(crate::parse_duration_str(t).map_err(|e| {
                crate::errors::CfgdError::Config(ConfigError::Invalid { message: e })
            })?),
            _ => None,
        };

    let resolved = if std::path::Path::new(run_str).is_relative() {
        working_dir.join(run_str)
    } else {
        std::path::PathBuf::from(run_str)
    };

    let mut cmd = if resolved.exists() {
        // File path — check executable bit, run directly (OS handles shebang)
        let meta = std::fs::metadata(&resolved)?;
        if !crate::is_executable(&resolved, &meta) {
            #[cfg(unix)]
            let hint = "chmod +x";
            #[cfg(windows)]
            let hint = "use a .exe, .cmd, .bat, or .ps1 extension";
            return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
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
        // Inline command — pass through sh -c on Unix, cmd.exe /C on Windows
        #[cfg(unix)]
        let c = {
            use std::os::unix::process::CommandExt;
            let mut c = std::process::Command::new("sh");
            c.arg("-c")
                .arg(run_str)
                .current_dir(working_dir)
                .process_group(0); // New process group so we can kill all children
            c
        };
        #[cfg(windows)]
        let c = {
            let mut c = std::process::Command::new("cmd.exe");
            c.arg("/C").arg(run_str).current_dir(working_dir);
            c
        };
        c
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

    let stdout_handle = {
        let pipe = child.stdout.take();
        let buf = std::sync::Arc::clone(&stdout_buf);
        let ts = std::sync::Arc::clone(&last_output);
        let tx = tx.clone();
        std::thread::spawn(move || {
            if let Some(pipe) = pipe {
                let reader = std::io::BufReader::new(pipe);
                for line in std::io::BufRead::lines(reader) {
                    match line {
                        Ok(l) => {
                            *ts.lock().unwrap_or_else(|e| e.into_inner()) =
                                std::time::Instant::now();
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
            }
        })
    };

    let stderr_handle = {
        let pipe = child.stderr.take();
        let buf = std::sync::Arc::clone(&stderr_buf);
        let ts = std::sync::Arc::clone(&last_output);
        let tx = tx.clone();
        std::thread::spawn(move || {
            if let Some(pipe) = pipe {
                let reader = std::io::BufReader::new(pipe);
                for line in std::io::BufRead::lines(reader) {
                    match line {
                        Ok(l) => {
                            *ts.lock().unwrap_or_else(|e| e.into_inner()) =
                                std::time::Instant::now();
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
            }
        })
    };
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
                    return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                        message: format!(
                            "script '{}' failed (exit {})\n{}",
                            run_str,
                            exit_code,
                            captured.as_deref().unwrap_or("")
                        ),
                    }));
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
                    return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                        message: format!(
                            "script '{}' {} after {}s\n{}",
                            run_str,
                            reason,
                            duration.as_secs(),
                            captured.as_deref().unwrap_or("")
                        ),
                    }));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
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

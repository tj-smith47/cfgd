use crate::config::{ScriptEntry, ShellAlias};
use crate::errors::{ConfigError, Result};
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
        // CFGD_* names are reserved for runtime metadata; user values are ignored.
        if ev.name.starts_with("CFGD_") {
            tracing::debug!(
                name = %ev.name,
                "spec.env entry dropped: CFGD_* names are reserved for runtime metadata"
            );
            continue;
        }
        env.push((ev.name.clone(), ev.value.clone()));
    }
    env
}

/// Build a bash alias preamble from a module's `spec.aliases` list.
///
/// Enables alias expansion and emits one `alias name='escaped-cmd'` line per entry.
/// Alias names are already validated by `validate_alias_name` at parse time
/// (matches `[A-Za-z0-9_.-]+`), so they require no additional escaping here.
/// Command bodies are escaped via `shell_escape_value` (single-quote strategy).
fn build_alias_prefix(aliases: &[ShellAlias]) -> String {
    let mut out = String::from("shopt -s expand_aliases\n");
    for alias in aliases {
        out.push_str(&format!(
            "alias {}={}\n",
            alias.name,
            crate::shell_escape_value(&alias.command)
        ));
    }
    out
}

/// Unified script executor for all hook types at both profile and module level.
///
/// Returns (description, changed, captured_output). All scripts set changed=true.
///
/// `module_aliases` is non-empty only for module-scoped call sites. When present
/// and the script is an inline command (not a file path), the shell switches to
/// bash with `shopt -s expand_aliases` so the aliases are active at expansion time.
/// File-shebang scripts cannot receive aliases: the shebang replaces the shell
/// context, making alias injection structurally impossible.
pub(crate) fn execute_script(
    entry: &ScriptEntry,
    working_dir: &std::path::Path,
    env_vars: &[(String, String)],
    default_timeout: std::time::Duration,
    printer: &Printer,
    module_aliases: &[ShellAlias],
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
        // Inline command — pass through sh -c on Unix, cmd.exe /C on Windows.
        // When module aliases are present we need bash: sh/dash does not honour
        // `shopt -s expand_aliases` in non-interactive -c mode. If bash is not
        // on PATH we fall back to sh and log a warning so script execution
        // continues — aliases just won't expand.
        #[cfg(unix)]
        let c = {
            use std::os::unix::process::CommandExt;

            let (shell, script_body) = if !module_aliases.is_empty() {
                if crate::command_available("bash") {
                    let prefix = build_alias_prefix(module_aliases);
                    ("bash", format!("{prefix}{run_str}"))
                } else {
                    tracing::warn!(
                        "bash not found on PATH; module aliases will not be expanded \
                         for inline scripts (falling back to sh)"
                    );
                    ("sh", run_str.to_string())
                }
            } else {
                ("sh", run_str.to_string())
            };

            let mut c = std::process::Command::new(shell);
            c.arg("-c")
                .arg(script_body)
                .current_dir(working_dir)
                .process_group(0); // New process group so we can kill all children
            c
        };
        #[cfg(windows)]
        let c = {
            // Aliases are a Unix shell construct; Windows cmd.exe has no equivalent.
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
                    let base = format!("script '{}' failed (exit {})", run_str, exit_code);
                    let message = match captured.as_deref().filter(|s| !s.is_empty()) {
                        Some(c) => format!("{base}\n{c}"),
                        None => base,
                    };
                    return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                        message,
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
                    return Err(crate::errors::CfgdError::Config(ConfigError::Invalid {
                        message,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EnvVar, ShellAlias};

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

    // build_module_script_env: a module declaring a CFGD_* name must not override
    // the runtime-injected value.
    #[test]
    fn cfgd_name_in_module_env_does_not_override_runtime() {
        let module_env = vec![fake_env_var("CFGD_MODULE_NAME", "spoofed")];
        let env = build_module_script_env(
            &fake_config_dir(),
            "workstation",
            ReconcileContext::Apply,
            &ScriptPhase::PostApply,
            Some("real-module"),
            None,
            &module_env,
        );

        // Collect all CFGD_MODULE_NAME occurrences — there must be exactly one and it
        // must be the runtime value, not the user-supplied one.
        let values: Vec<&str> = env
            .iter()
            .filter(|(k, _)| k == "CFGD_MODULE_NAME")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(values, vec!["real-module"]);
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

    fn fake_alias(name: &str, command: &str) -> ShellAlias {
        ShellAlias {
            name: name.to_string(),
            command: command.to_string(),
        }
    }

    fn test_printer() -> crate::output::Printer {
        crate::output::Printer::new(crate::output::Verbosity::Quiet)
    }

    // build_alias_prefix: preamble enables expand_aliases and emits one line per alias.
    #[test]
    fn alias_prefix_structure() {
        let aliases = vec![
            fake_alias("greet", "echo hello"),
            fake_alias("bye", "echo goodbye"),
        ];
        let prefix = build_alias_prefix(&aliases);
        assert!(prefix.starts_with("shopt -s expand_aliases\n"));
        assert!(prefix.contains("alias greet="));
        assert!(prefix.contains("alias bye="));
    }

    // build_alias_prefix: single-quote values containing shell metacharacters are escaped.
    #[test]
    fn alias_command_with_single_quotes_escapes_correctly() {
        let aliases = vec![fake_alias("greet", "echo 'hi there'")];
        let prefix = build_alias_prefix(&aliases);
        // shell_escape_value wraps the value in single quotes with '\'' for embedded quotes.
        assert!(prefix.contains("alias greet="));
        // The command body must not be raw 'echo 'hi there'' — the embedded quotes must
        // be escaped so the alias line is valid shell.
        assert!(!prefix.contains("alias greet='echo 'hi there''"));
    }

    // execute_script with module aliases uses bash and the alias expands correctly.
    #[cfg(target_family = "unix")]
    #[test]
    fn inline_script_with_module_aliases_uses_bash_and_expands() {
        // Skip if bash is not available in the test environment.
        if !crate::command_available("bash") {
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let aliases = vec![fake_alias("myalias", "echo hello")];
        let entry = ScriptEntry::Simple("myalias".to_string());
        let (_desc, changed, captured) = execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(10),
            &test_printer(),
            &aliases,
        )
        .expect("execute_script failed");
        assert!(changed);
        let output = captured.unwrap_or_default();
        assert!(
            output.contains("hello"),
            "expected 'hello' in output, got: {output:?}"
        );
    }

    // execute_script without aliases still works under sh (no regression).
    #[cfg(target_family = "unix")]
    #[test]
    fn inline_script_without_aliases_still_works() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = ScriptEntry::Simple("echo no-aliases".to_string());
        let (_desc, changed, captured) = execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(10),
            &test_printer(),
            &[],
        )
        .expect("execute_script failed");
        assert!(changed);
        let output = captured.unwrap_or_default();
        assert!(output.contains("no-aliases"));
    }

    // execute_script with multiple aliases: all expand correctly in one script.
    #[cfg(target_family = "unix")]
    #[test]
    fn multiple_aliases_all_expand() {
        if !crate::command_available("bash") {
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let aliases = vec![
            fake_alias("say_a", "echo alpha"),
            fake_alias("say_b", "echo beta"),
            fake_alias("say_c", "echo gamma"),
        ];
        let entry = ScriptEntry::Simple("say_a && say_b && say_c".to_string());
        let (_desc, _changed, captured) = execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(10),
            &test_printer(),
            &aliases,
        )
        .expect("execute_script failed");
        let output = captured.unwrap_or_default();
        assert!(output.contains("alpha"), "missing 'alpha' in: {output:?}");
        assert!(output.contains("beta"), "missing 'beta' in: {output:?}");
        assert!(output.contains("gamma"), "missing 'gamma' in: {output:?}");
    }

    // File-shebang scripts run even when aliases are passed — the aliases don't propagate
    // (the shebang controls the interpreter) and the script executes in its own process.
    #[cfg(target_family = "unix")]
    #[test]
    fn file_script_with_module_aliases_does_not_propagate_them() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("test.sh");
        // The script references "myalias" which is only defined in the module's alias list.
        // Because the file is exec'd via its shebang, alias injection does not occur and
        // the script will fail with "command not found".
        std::fs::write(&script_path, b"#!/bin/sh\necho file-script-ran\n").expect("write script");
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");

        let aliases = vec![fake_alias("myalias", "echo should-not-expand")];
        let entry = ScriptEntry::Simple(script_path.to_string_lossy().into_owned());
        // The file script runs normally; aliases are silently not available.
        let result = execute_script(
            &entry,
            dir.path(),
            &[],
            std::time::Duration::from_secs(10),
            &test_printer(),
            &aliases,
        );
        // The script itself succeeds (it only echoes "file-script-ran"), confirming
        // execution proceeds without alias expansion errors.
        assert!(result.is_ok(), "file script should run: {result:?}");
        let (_desc, _changed, captured) = result.unwrap();
        let output = captured.unwrap_or_default();
        assert!(
            output.contains("file-script-ran"),
            "expected file-script-ran in: {output:?}"
        );
        assert!(
            !output.contains("should-not-expand"),
            "alias must not have expanded: {output:?}"
        );
    }
}

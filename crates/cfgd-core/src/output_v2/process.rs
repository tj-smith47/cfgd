//! Process execution with live output display.
//!
//! `run_command` is the single entry point. It picks between two strategies
//! based on TTY + verbosity:
//!
//! - **TTY + non-quiet** → `run_with_progress`: a spinner with a bounded
//!   tailing ring (last N lines of stdout/stderr render under the spinner;
//!   muted, indented to `depth + 1`). On exit, the spinner clears and a
//!   single Status line replaces it.
//! - **Non-TTY or quiet** → `run_streaming`: each child line streams to the
//!   sink as it arrives. A leading Status(Running) opens the activity and a
//!   final Status(Ok|Fail) closes it.
//!
//! Either path captures full stdout + stderr into the returned
//! `CommandOutput` so callers can post-process even when output was muted.
//!
//! This is the controlled `std::process::Command` execution layer for
//! `output_v2`; see `module-boundaries.md`.
use std::collections::VecDeque;
use std::io::BufRead;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::renderer::{Renderer, StatusFields, Writer};
use super::spinner::stderr_is_terminal;
use super::{Role, Verbosity};

pub struct CommandOutput {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
}

enum Captured {
    Stdout(String),
    Stderr(String),
}

/// Run `cmd` with live output display. TTY mode: bounded scrolling region with
/// spinner. Non-TTY / quiet: stream lines as they arrive. Either way, captures
/// stdout+stderr for the return value.
pub(crate) fn run_command(
    renderer: &Renderer,
    sink: &dyn Writer,
    multi: &indicatif::MultiProgress,
    depth: usize,
    cmd: &mut std::process::Command,
    label: &str,
) -> std::io::Result<CommandOutput> {
    let start = Instant::now();
    cmd.stdin(std::process::Stdio::null());
    if stderr_is_terminal() && renderer.verbosity != Verbosity::Quiet {
        run_with_progress(renderer, sink, multi, depth, cmd, label, start)
    } else {
        run_streaming(renderer, sink, depth, cmd, label, start)
    }
}

fn spawn_readers(child: &mut std::process::Child) -> mpsc::Receiver<Captured> {
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                let _ = tx.send(Captured::Stdout(line));
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                let _ = tx.send(Captured::Stderr(line));
            }
        });
    }
    drop(tx);
    rx
}

fn run_with_progress(
    renderer: &Renderer,
    sink: &dyn Writer,
    multi: &indicatif::MultiProgress,
    depth: usize,
    cmd: &mut std::process::Command,
    label: &str,
    start: Instant,
) -> std::io::Result<CommandOutput> {
    const VISIBLE_LINES: usize = 5;
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let pb = super::spinner::build_spinner(multi, renderer, label);
    let rx = spawn_readers(&mut child);
    let mut ring: VecDeque<String> = VecDeque::with_capacity(VISIBLE_LINES);
    let mut all_stdout = Vec::new();
    let mut all_stderr = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                let text = match &line {
                    Captured::Stdout(s) => {
                        all_stdout.push(s.clone());
                        s
                    }
                    Captured::Stderr(s) => {
                        all_stderr.push(s.clone());
                        s
                    }
                };
                if ring.len() >= VISIBLE_LINES {
                    ring.pop_front();
                }
                ring.push_back(text.clone());
                let mut msg = label.to_string();
                for l in &ring {
                    let display = if l.len() > 120 {
                        l.get(..120).unwrap_or(l)
                    } else {
                        l
                    };
                    msg.push_str(&format!(
                        "\n{}{}",
                        "  ".repeat(depth + 1),
                        renderer.theme.muted.apply_to(display)
                    ));
                }
                pb.set_message(msg);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait()?;
    let duration = start.elapsed();
    pb.finish_and_clear();
    if status.success() {
        renderer.render_status(
            sink,
            depth,
            &StatusFields {
                role: Role::Ok,
                subject: label,
                detail: None,
                duration: Some(duration),
                target: None,
            },
        );
    } else {
        renderer.render_status(
            sink,
            depth,
            &StatusFields {
                role: Role::Fail,
                subject: label,
                detail: Some("failed"),
                duration: Some(duration),
                target: None,
            },
        );
        for line in &all_stderr {
            let dim = renderer.theme.muted.apply_to(line).to_string();
            renderer.write_line(sink, depth + 1, &dim);
        }
    }
    Ok(CommandOutput {
        status,
        stdout: all_stdout.join("\n"),
        stderr: all_stderr.join("\n"),
        duration,
    })
}

fn run_streaming(
    renderer: &Renderer,
    sink: &dyn Writer,
    depth: usize,
    cmd: &mut std::process::Command,
    label: &str,
    start: Instant,
) -> std::io::Result<CommandOutput> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if renderer.verbosity != Verbosity::Quiet {
        renderer.render_status(
            sink,
            depth,
            &StatusFields {
                role: Role::Running,
                subject: label,
                detail: None,
                duration: None,
                target: None,
            },
        );
    }
    let rx = spawn_readers(&mut child);
    let mut all_stdout = Vec::new();
    let mut all_stderr = Vec::new();
    for line in rx {
        match &line {
            Captured::Stdout(s) => {
                if renderer.verbosity != Verbosity::Quiet {
                    renderer.write_line(sink, depth + 1, s);
                }
                all_stdout.push(s.clone());
            }
            Captured::Stderr(s) => {
                if renderer.verbosity != Verbosity::Quiet {
                    renderer.write_line(sink, depth + 1, s);
                }
                all_stderr.push(s.clone());
            }
        }
    }
    let status = child.wait()?;
    let duration = start.elapsed();
    let role = if status.success() {
        Role::Ok
    } else {
        Role::Fail
    };
    renderer.render_status(
        sink,
        depth,
        &StatusFields {
            role,
            subject: label,
            detail: None,
            duration: Some(duration),
            target: None,
        },
    );
    Ok(CommandOutput {
        status,
        stdout: all_stdout.join("\n"),
        stderr: all_stderr.join("\n"),
        duration,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::super::Theme;
    use super::super::renderer::StringSink;
    use super::*;

    /// Run `f` in a thread with a deadline; panic if it doesn't return in time.
    /// Used to bound this test's worst-case if a child process hangs (CI flake).
    fn with_deadline<F: FnOnce() -> R + Send + 'static, R: Send + 'static>(d: Duration, f: F) -> R {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(d).expect("test exceeded deadline")
    }

    // serial_test::serial because the test mutates the process's stdio inheritance
    // tracking implicitly via `Command::spawn`; running concurrently with another
    // process-spawning test can cause stderr_is_terminal() to flip mid-test.
    #[test]
    #[serial_test::serial]
    fn run_streaming_captures_stdout_and_emits_status() {
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
            let multi = indicatif::MultiProgress::new();
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg("printf 'hello\nworld\n'");
            // Streaming path: in CI, stderr is not a TTY → run_streaming fires.
            // Locally in a terminal you'll hit run_with_progress instead — both
            // paths satisfy this test's assertions, but if you see flakes
            // locally, run with `TERM=dumb cargo test ...`.
            let out = run_command(&renderer, &sink, &multi, 0, &mut cmd, "say hi").unwrap();
            assert!(out.status.success());
            assert!(out.stdout.contains("hello"));
            assert!(out.stdout.contains("world"));
            let s = buf.lock().unwrap();
            assert!(s.contains("say hi"));
        });
    }
}

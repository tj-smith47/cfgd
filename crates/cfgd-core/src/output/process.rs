//! Process execution with live output display.
//!
//! `run_command` is the single entry point, and it has a single strategy: the
//! child's stdout and stderr both feed an [`OutputWindow`], which owns the
//! decision between a bounded repainting tail (TTY, non-quiet) and plain
//! streaming (everything else). On exit the window collapses to one Status
//! line; a failure additionally dumps the captured stderr beneath it, which is
//! the only diagnostic surface a user gets for a spawned command that died.
//!
//! Either way the full stdout + stderr are captured into the returned
//! `CommandOutput`, so callers can post-process even when the display muted or
//! truncated what was on screen.
//!
//! This is the controlled `std::process::Command` execution layer for
//! `output`; see `module-boundaries.md`.
use std::io::BufRead;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::Printer;
use super::window::OutputWindow;

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

fn make_output(
    status: std::process::ExitStatus,
    all_stdout: Vec<String>,
    all_stderr: Vec<String>,
    duration: Duration,
) -> CommandOutput {
    CommandOutput {
        status,
        stdout: all_stdout.join("\n"),
        stderr: all_stderr.join("\n"),
        duration,
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

/// Run `cmd` with live output displayed through an [`OutputWindow`], capturing
/// stdout and stderr for the returned `CommandOutput`.
pub(crate) fn run_command(
    printer: &Printer,
    depth: usize,
    cmd: &mut std::process::Command,
    label: &str,
) -> std::io::Result<CommandOutput> {
    let start = Instant::now();
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let rx = spawn_readers(&mut child);
    let mut window = printer.output_window_at(depth, label);
    let mut all_stdout = Vec::new();
    let mut all_stderr = Vec::new();
    // Blocking recv: the spinner's steady tick redraws independently of message
    // updates, so a poll loop adds no value. Iteration ends when all tx clones
    // drop (reader threads finish).
    for line in rx {
        match line {
            Captured::Stdout(s) => {
                window.push_line(&s);
                all_stdout.push(s);
            }
            Captured::Stderr(s) => {
                window.push_line(&s);
                all_stderr.push(s);
            }
        }
    }

    let status = child.wait()?;
    let duration = start.elapsed();
    if status.success() {
        drop(window.finish_ok(label).duration(duration));
    } else {
        drop(
            window
                .finish_fail(label)
                .detail("failed")
                .duration(duration),
        );
        // The window's tail is cleared by the collapse, so a failure has to
        // re-render what the user needs to diagnose it.
        OutputWindow::dump_below(printer, depth, &all_stderr);
    }
    Ok(make_output(status, all_stdout, all_stderr, duration))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::super::Verbosity;
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

    /// A Printer whose stderr sink is a capture buffer.
    fn capturing_printer(verbosity: Verbosity) -> (Printer, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let mut p = Printer::new(verbosity);
        p.sink_stderr = Arc::new(StringSink(buf.clone()));
        (p, buf)
    }

    fn sh(script: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    #[test]
    fn make_output_joins_captured_lines_with_newlines() {
        let stdout = vec!["a".into(), "b".into(), "c".into()];
        let stderr = vec!["x".into(), "y".into()];
        let status = exit_status_from_code(0);
        let out = make_output(status, stdout, stderr, Duration::from_millis(42));
        assert_eq!(out.stdout, "a\nb\nc");
        assert_eq!(out.stderr, "x\ny");
        assert_eq!(out.duration, Duration::from_millis(42));
        assert!(out.status.success());
    }

    #[test]
    fn make_output_empty_captures_produce_empty_strings() {
        let status = exit_status_from_code(0);
        let out = make_output(status, vec![], vec![], Duration::from_secs(0));
        assert!(out.stdout.is_empty());
        assert!(out.stderr.is_empty());
    }

    // serial_test::serial because the test mutates the process's stdio inheritance
    // tracking implicitly via `Command::spawn`; running concurrently with another
    // process-spawning test can cause the TTY probe to flip mid-test.
    #[test]
    #[serial_test::serial]
    fn captures_stdout_and_surfaces_the_label() {
        with_deadline(Duration::from_secs(10), || {
            let (p, buf) = capturing_printer(Verbosity::Normal);
            let out = run_command(&p, 0, &mut sh("printf 'hello\nworld\n'"), "say hi").unwrap();
            assert!(out.status.success());
            assert_eq!(out.stdout, "hello\nworld");
            let captured = crate::output::strip_ansi(&buf.lock().unwrap());
            assert!(captured.contains("say hi"), "got: {captured:?}");
        });
    }

    #[test]
    #[serial_test::serial]
    fn captures_stderr_separately_from_stdout() {
        with_deadline(Duration::from_secs(10), || {
            let (p, _buf) = capturing_printer(Verbosity::Normal);
            let out = run_command(
                &p,
                0,
                &mut sh("printf 'out\n'; printf 'err\n' 1>&2"),
                "split",
            )
            .unwrap();
            assert!(out.status.success());
            assert_eq!(out.stdout, "out");
            assert_eq!(out.stderr, "err");
        });
    }

    #[test]
    #[serial_test::serial]
    fn failure_emits_fail_status_and_propagates_exit_code() {
        with_deadline(Duration::from_secs(10), || {
            let (p, buf) = capturing_printer(Verbosity::Normal);
            let out =
                run_command(&p, 0, &mut sh("printf 'partial\n'; exit 7"), "fail-job").unwrap();

            assert!(!out.status.success());
            assert_eq!(out.status.code(), Some(7));
            assert_eq!(out.stdout, "partial");

            let captured = crate::output::strip_ansi(&buf.lock().unwrap());
            assert!(
                captured.contains("✗") || captured.contains("fail-job"),
                "fail status must surface in sink; got: {captured:?}"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn failure_dumps_every_captured_stderr_line_below_the_status() {
        // The dump is the only diagnostic surface left once the window
        // collapses, so it must carry the whole of stderr — not the ring's tail.
        with_deadline(Duration::from_secs(10), || {
            let (p, buf) = capturing_printer(Verbosity::Normal);
            let out = run_command(
                &p,
                0,
                &mut sh("printf 'boom-1\n' 1>&2; printf 'boom-2\n' 1>&2; exit 9"),
                "spin-fail",
            )
            .unwrap();

            assert!(!out.status.success());
            assert_eq!(out.status.code(), Some(9));
            assert_eq!(out.stderr, "boom-1\nboom-2");

            let captured = crate::output::strip_ansi(&buf.lock().unwrap());
            assert!(captured.contains("spin-fail"), "got: {captured:?}");
            assert!(captured.contains("boom-1"), "got: {captured:?}");
            assert!(captured.contains("boom-2"), "got: {captured:?}");
        });
    }

    #[test]
    #[serial_test::serial]
    fn quiet_suppresses_display_but_still_captures() {
        with_deadline(Duration::from_secs(10), || {
            let (p, buf) = capturing_printer(Verbosity::Quiet);
            let out = run_command(&p, 0, &mut sh("printf 'q1\nq2\n'"), "quiet-job").unwrap();

            assert!(out.status.success());
            // Capture is independent of verbosity — the caller still sees both lines.
            assert_eq!(out.stdout, "q1\nq2");

            let captured = crate::output::strip_ansi(&buf.lock().unwrap());
            assert!(
                !captured.contains("q1"),
                "quiet leaked stdout: {captured:?}"
            );
            assert!(
                !captured.contains("q2"),
                "quiet leaked stdout: {captured:?}"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn capture_holds_every_line_the_display_trimmed() {
        // The window shows at most five lines; the capture the caller
        // post-processes must still hold all twelve.
        with_deadline(Duration::from_secs(15), || {
            let (p, _buf) = capturing_printer(Verbosity::Normal);
            let out = run_command(
                &p,
                0,
                &mut sh("for i in $(seq 1 12); do printf 'line-%02d\n' $i; done"),
                "many-lines",
            )
            .unwrap();

            assert!(out.status.success());
            let captured_lines: Vec<&str> = out.stdout.split('\n').collect();
            assert_eq!(captured_lines.len(), 12);
            assert_eq!(captured_lines.first().copied(), Some("line-01"));
            assert_eq!(captured_lines.last().copied(), Some("line-12"));
        });
    }

    #[test]
    #[serial_test::serial]
    fn capture_holds_the_full_line_the_display_clamped() {
        with_deadline(Duration::from_secs(10), || {
            let (p, _buf) = capturing_printer(Verbosity::Normal);
            let payload = "x".repeat(250);
            let out = run_command(
                &p,
                0,
                &mut sh(&format!("printf '%s\n' {payload}")),
                "long-line",
            )
            .unwrap();

            assert!(out.status.success());
            assert_eq!(out.stdout, payload);
        });
    }

    #[test]
    #[serial_test::serial]
    fn foreign_ansi_never_reaches_the_sink() {
        // A child's own SGR escapes would otherwise close the renderer's muted
        // styling early and paint past the window.
        with_deadline(Duration::from_secs(10), || {
            let (p, buf) = capturing_printer(Verbosity::Normal);
            let out = run_command(
                &p,
                0,
                &mut sh(r"printf 'tool: \033[31mred\033[0m text\n'"),
                "ansi-job",
            )
            .unwrap();
            assert!(out.status.success());
            let raw = buf.lock().unwrap();
            assert!(
                !raw.contains("\x1b[31m"),
                "foreign red SGR reached the sink; got: {raw:?}"
            );
        });
    }

    /// Build an `ExitStatus` with the given exit code, portable across Unix
    /// and Windows for the make_output tests above.
    fn exit_status_from_code(code: i32) -> std::process::ExitStatus {
        // Run `sh -c "exit N"` synchronously and capture the resulting status.
        // Cheaper than depending on platform-specific `ExitStatusExt`.
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("exit {code}"))
            .status()
            .expect("sh exit must succeed")
    }
}

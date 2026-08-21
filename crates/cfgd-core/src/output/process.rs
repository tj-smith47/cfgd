//! Process execution with live output display.
//!
//! `run_command` is the single entry point, and it has a single strategy: the
//! child's stdout and stderr both feed an [`OutputWindow`], which owns the
//! decision between a bounded repainting tail (TTY, non-quiet) and plain
//! streaming (everything else). On exit the window collapses to one Status
//! line; a failure additionally dumps the captured stderr beneath it when the
//! tail lived in the repainting window ([`OutputWindow::tail_needs_replay`]) —
//! the only diagnostic surface a user gets for a spawned command that died. In
//! the streaming degradation the lines are already in the scrollback, so no
//! replay is needed.
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

/// Who emits the one status line for the work a [`run_command`] call is part of.
///
/// A command run on its own behalf settles its own line; a command run *inside*
/// an action whose line the reconciler emits must not, or the action renders
/// twice. The window's live tail is unaffected either way — only the collapse
/// differs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusOwner {
    Window,
    Caller,
}

/// What one [`spawn_and_pump`] run yields: the display sink, the exit status,
/// the stdout and stderr captures, and the elapsed clock.
type Pumped<S> = (
    S,
    std::process::ExitStatus,
    Vec<String>,
    Vec<String>,
    Duration,
);

/// Spawn `cmd` with both pipes captured and feed every line to `on_line` as it
/// arrives, returning the display sink, the exit status, the two captures and
/// the elapsed clock.
///
/// The ONE spawn-and-pump in this module, so a live window and a concurrent
/// lane cannot disagree about stdio configuration, about the `PATH` guard's
/// span, or about which stream a captured line came from.
///
/// `sink` is a thunk rather than a value because it is only built once the
/// spawn SUCCEEDED: an `OutputWindow` created ahead of a failing spawn would
/// be abandoned, and an abandoned window leaves a status line behind.
fn spawn_and_pump<S>(
    cmd: &mut std::process::Command,
    sink: impl FnOnce() -> S,
    mut on_line: impl FnMut(&mut S, &str),
) -> std::io::Result<Pumped<S>> {
    // Held for the whole run, not just the spawn: the child resolves its
    // program through `PATH` and reads its inherited working directory after
    // exec, so both must stay stable until it exits. Re-entrant, so a caller
    // that already holds the guard is unaffected. Compiled out of release
    // builds.
    #[cfg(any(test, feature = "test-helpers"))]
    let _spawn_guard = crate::test_helpers::path_env_read_guard();

    let start = Instant::now();
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut sink = sink();
    let rx = spawn_readers(&mut child);
    let mut all_stdout = Vec::new();
    let mut all_stderr = Vec::new();
    // Blocking recv: the spinner's steady tick redraws independently of message
    // updates, so a poll loop adds no value. Iteration ends when all tx clones
    // drop (reader threads finish).
    for line in rx {
        match line {
            Captured::Stdout(s) => {
                on_line(&mut sink, &s);
                all_stdout.push(s);
            }
            Captured::Stderr(s) => {
                on_line(&mut sink, &s);
                all_stderr.push(s);
            }
        }
    }

    let status = child.wait()?;
    Ok((sink, status, all_stdout, all_stderr, start.elapsed()))
}

/// Run `cmd` inside a concurrent lane: the lane owns the rendering (a bounded
/// window on a TTY, a capture off one) and the coordinator owns the status
/// line, so nothing is settled here.
pub(crate) fn run_command_in_lane(
    cmd: &mut std::process::Command,
    lane: &(impl super::lane::LaneOutput + ?Sized),
) -> std::io::Result<CommandOutput> {
    let ((), status, all_stdout, all_stderr, duration) =
        spawn_and_pump(cmd, || (), |(), line| lane.push_line(line))?;
    Ok(make_output(status, all_stdout, all_stderr, duration))
}

/// Run `cmd` with live output displayed through an [`OutputWindow`], capturing
/// stdout and stderr for the returned `CommandOutput`.
pub(crate) fn run_command(
    printer: &Printer,
    depth: usize,
    cmd: &mut std::process::Command,
    label: &str,
    settle: StatusOwner,
) -> std::io::Result<CommandOutput> {
    let (window, status, all_stdout, all_stderr, duration) = spawn_and_pump(
        cmd,
        || printer.output_window_at(depth, label),
        |window, line| window.push_line(line),
    )?;
    if status.success() {
        match settle {
            StatusOwner::Window => drop(window.finish_ok(label).duration(duration)),
            StatusOwner::Caller => window.finish_silent(),
        }
    } else {
        let needs_replay = window.tail_needs_replay();
        match settle {
            StatusOwner::Window => {
                drop(
                    window
                        .finish_fail(label)
                        .detail(failure_detail(&status))
                        .duration(duration),
                );
                // Streaming already left every line in the scrollback; replaying
                // them here would print the whole of stderr a second time.
                if needs_replay {
                    OutputWindow::dump_below(printer, depth, &all_stderr);
                }
            }
            // No replay: the caller's failure line has not been written yet, so
            // a dump here would land the body ABOVE the status it belongs to.
            // The tail travels back in the returned `CommandOutput` and reaches
            // the user as that line's detail.
            StatusOwner::Caller => window.finish_silent(),
        }
    }
    Ok(make_output(status, all_stdout, all_stderr, duration))
}

/// Render a failed child's exit status as a status-line detail: `exit <code>`,
/// or `signal <n>` when it was killed rather than exiting on its own.
fn failure_detail(status: &std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("signal {sig}");
        }
    }
    "failed".to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::super::Verbosity;
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

    /// A Printer whose stderr sink is a capture buffer, with live_region,
    /// interactive_stdin, and colour all pinned off rather than inherited from
    /// however the suite was invoked — real `sh` children run under these
    /// tests, already guarded by `with_deadline` against a hang; an inherited
    /// stdin-tty would add a second, worse one (an unanswered prompt).
    fn capturing_printer(verbosity: Verbosity) -> (Printer, Arc<Mutex<String>>) {
        Printer::for_test_at(verbosity)
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
            let out = run_command(
                &p,
                0,
                &mut sh("printf 'hello\nworld\n'"),
                "say hi",
                StatusOwner::Window,
            )
            .unwrap();
            assert!(out.status.success());
            assert_eq!(out.stdout, "hello\nworld");
            let captured = crate::test_helpers::captured_text(&buf);
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
                StatusOwner::Window,
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
            let out = run_command(
                &p,
                0,
                &mut sh("printf 'partial\n'; exit 7"),
                "fail-job",
                StatusOwner::Window,
            )
            .unwrap();

            assert!(!out.status.success());
            assert_eq!(out.status.code(), Some(7));
            assert_eq!(out.stdout, "partial");

            let captured = crate::test_helpers::captured_text(&buf);
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
                StatusOwner::Window,
            )
            .unwrap();

            assert!(!out.status.success());
            assert_eq!(out.status.code(), Some(9));
            assert_eq!(out.stderr, "boom-1\nboom-2");

            let captured = crate::test_helpers::captured_text(&buf);
            assert!(captured.contains("spin-fail"), "got: {captured:?}");
            assert!(captured.contains("boom-1"), "got: {captured:?}");
            assert!(captured.contains("boom-2"), "got: {captured:?}");
        });
    }

    #[test]
    #[serial_test::serial]
    fn failure_does_not_duplicate_streamed_stderr() {
        // The test harness has no TTY, so `OutputWindow` takes the streaming
        // branch: every stderr line lands in the scrollback as it arrives.
        // A failure that unconditionally replayed the capture below the
        // status would print each line twice.
        with_deadline(Duration::from_secs(10), || {
            let (p, buf) = capturing_printer(Verbosity::Normal);
            let out = run_command(
                &p,
                0,
                &mut sh("printf 'MARKER-LINE\n' 1>&2; exit 3"),
                "dup-check",
                StatusOwner::Window,
            )
            .unwrap();

            assert!(!out.status.success());
            assert_eq!(out.status.code(), Some(3));

            let captured = crate::test_helpers::captured_text(&buf);
            assert_eq!(
                captured.matches("MARKER-LINE").count(),
                1,
                "stderr line duplicated in streaming mode: {captured:?}"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn failure_detail_carries_the_exit_code() {
        with_deadline(Duration::from_secs(10), || {
            let (p, buf) = capturing_printer(Verbosity::Normal);
            let out = run_command(
                &p,
                0,
                &mut sh("exit 42"),
                "exit-code-job",
                StatusOwner::Window,
            )
            .unwrap();

            assert!(!out.status.success());
            assert_eq!(out.status.code(), Some(42));

            let captured = crate::test_helpers::captured_text(&buf);
            assert!(
                captured.contains("exit 42"),
                "failure detail must carry the exit code; got: {captured:?}"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn quiet_suppresses_display_but_still_captures() {
        with_deadline(Duration::from_secs(10), || {
            let (p, buf) = capturing_printer(Verbosity::Quiet);
            let out = run_command(
                &p,
                0,
                &mut sh("printf 'q1\nq2\n'"),
                "quiet-job",
                StatusOwner::Window,
            )
            .unwrap();

            assert!(out.status.success());
            // Capture is independent of verbosity — the caller still sees both lines.
            assert_eq!(out.stdout, "q1\nq2");

            let captured = crate::test_helpers::captured_text(&buf);
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
                StatusOwner::Window,
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
                StatusOwner::Window,
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
                StatusOwner::Window,
            )
            .unwrap();
            assert!(out.status.success());
            // raw-capture-ok: the claim IS that no escape survives, and captured_text strips exactly what this test looks for
            let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            assert!(
                !raw.contains("\x1b[31m"),
                "foreign red SGR reached the sink; got: {raw:?}"
            );
            assert!(
                raw.contains("red") && raw.contains("text"),
                "the child's own words were dropped, so the assertion above \
                 proves nothing; got: {raw:?}"
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

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
//! `output`; see `module-boundaries.md`.
use std::collections::VecDeque;
use std::io::BufRead;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::renderer::{Renderer, StatusFields, Writer};
use super::spinner::stderr_is_terminal;
use super::{Role, Verbosity, strip_ansi};

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

/// Sanitize a captured external-tool line and wrap it in the renderer's
/// `muted` style. Strips foreign ANSI BEFORE the style is applied so a
/// stray `\x1b[0m` in the tool output cannot prematurely close the muted
/// styling, and foreign color escapes cannot paint past the spinner /
/// post-failure dump.
fn sanitize_and_mute(renderer: &Renderer, line: &str) -> String {
    let clean = strip_ansi(line);
    renderer.theme.muted.apply_to(clean).to_string()
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
    // Blocking recv: the spinner's steady tick redraws independently of message
    // updates, so a poll loop adds no value. Iteration ends when all tx clones
    // drop (reader threads finish).
    for line in rx {
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
                sanitize_and_mute(renderer, display)
            ));
        }
        pb.set_message(msg);
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
            let dim = sanitize_and_mute(renderer, line);
            renderer.write_line(sink, depth + 1, &dim);
        }
    }
    Ok(make_output(status, all_stdout, all_stderr, duration))
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
    Ok(make_output(status, all_stdout, all_stderr, duration))
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

    /// Foreign ANSI carried in a captured external-tool stdout/stderr line
    /// must be stripped BEFORE the renderer's `muted` style wraps it. A stray
    /// `\x1b[0m` in the tool output would otherwise prematurely close the
    /// muted styling on the spinner display line (or the post-failure dump),
    /// and foreign color escapes would paint past the spinner. `Printer::run`
    /// hands captured lines through `sanitize_and_mute` for that reason.
    #[test]
    #[serial_test::serial]
    fn run_spinner_strips_ansi_from_external_tool_output() {
        let _restore_no_color = std::env::var("NO_COLOR").ok();
        // SAFETY: single-threaded under serial_test::serial; restored below.
        unsafe {
            std::env::remove_var("NO_COLOR");
        }
        let _guard = crate::output::test_support::ColorsEnabledGuard::set(true);

        let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
        let foreign = "tool: \x1b[31mred\x1b[0m text \x1b[1mbold\x1b[0m";
        let out = sanitize_and_mute(&renderer, foreign);
        // The visible payload survives sanitation.
        let visible = crate::output::strip_ansi(&out);
        assert!(
            visible.contains("tool: red text bold"),
            "visible payload mismatch; got: {visible:?}"
        );
        // None of the foreign SGRs survive. The renderer's `muted` style is a
        // dim grey foreground; the foreign red foreground `31` would never be
        // emitted by the renderer itself, so its absence proves sanitation.
        assert!(
            !out.contains("\x1b[31m"),
            "foreign red SGR must be stripped before muted wrap; got: {out:?}"
        );
        assert!(
            !out.contains("\x1b[1m"),
            "foreign bold SGR must be stripped before muted wrap; got: {out:?}"
        );

        unsafe {
            match _restore_no_color {
                Some(v) => std::env::set_var("NO_COLOR", v),
                None => std::env::remove_var("NO_COLOR"),
            }
        }
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

    #[test]
    #[serial_test::serial]
    fn run_streaming_emits_running_status_then_ok_on_success() {
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg("printf 'line-one\nline-two\n'");
            let out =
                run_streaming(&renderer, &sink, 0, &mut cmd, "stream-job", Instant::now()).unwrap();

            assert!(out.status.success());
            assert_eq!(out.stdout, "line-one\nline-two");
            assert!(out.stderr.is_empty());

            let captured = crate::output::strip_ansi(&buf.lock().unwrap());
            assert!(
                captured.contains("stream-job"),
                "label must appear in sink output; got: {captured:?}"
            );
            assert!(
                captured.contains("line-one"),
                "stdout line must be streamed to sink; got: {captured:?}"
            );
            assert!(
                captured.contains("line-two"),
                "stdout line must be streamed to sink; got: {captured:?}"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn run_streaming_captures_stderr_separately() {
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg("printf 'out\n'; printf 'err\n' 1>&2");
            let out =
                run_streaming(&renderer, &sink, 0, &mut cmd, "split", Instant::now()).unwrap();
            assert!(out.status.success());
            assert_eq!(out.stdout, "out");
            assert_eq!(out.stderr, "err");
        });
    }

    #[test]
    #[serial_test::serial]
    fn run_streaming_failure_emits_fail_role_and_propagates_exit_code() {
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg("printf 'partial\n'; exit 7");
            let out =
                run_streaming(&renderer, &sink, 0, &mut cmd, "fail-job", Instant::now()).unwrap();

            assert!(!out.status.success());
            assert_eq!(out.status.code(), Some(7));
            assert_eq!(out.stdout, "partial");

            let captured = crate::output::strip_ansi(&buf.lock().unwrap());
            // Failure renders the configured fail icon (✗ by default).
            assert!(
                captured.contains("✗") || captured.contains("fail-job"),
                "fail status must surface in sink; got: {captured:?}"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn run_streaming_quiet_verbosity_suppresses_running_and_per_line_output() {
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Quiet);
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg("printf 'q1\nq2\n'");
            let out =
                run_streaming(&renderer, &sink, 0, &mut cmd, "quiet-job", Instant::now()).unwrap();

            assert!(out.status.success());
            // Capture is independent of verbosity — the caller still sees both lines.
            assert_eq!(out.stdout, "q1\nq2");

            let captured = crate::output::strip_ansi(&buf.lock().unwrap());
            // Quiet verbosity: no Running status, no per-line passthrough. The
            // final Ok status is rendered unconditionally (render_status is
            // routed regardless of verbosity in this path so callers know the
            // process finished).
            assert!(
                !captured.contains("q1"),
                "quiet should not stream stdout lines; got: {captured:?}"
            );
            assert!(
                !captured.contains("q2"),
                "quiet should not stream stdout lines; got: {captured:?}"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn run_with_progress_captures_both_streams_and_renders_label() {
        // Force the spinner path by calling `run_with_progress` directly; the
        // public `run_command` would route to `run_streaming` in this test env
        // because stderr is not a TTY.
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
            let multi = indicatif::MultiProgress::new();
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg("printf 'p-out\n'; printf 'p-err\n' 1>&2");

            let out = run_with_progress(
                &renderer,
                &sink,
                &multi,
                0,
                &mut cmd,
                "spin-ok",
                Instant::now(),
            )
            .unwrap();

            assert!(out.status.success());
            assert_eq!(out.stdout, "p-out");
            assert_eq!(out.stderr, "p-err");

            let captured = crate::output::strip_ansi(&buf.lock().unwrap());
            assert!(
                captured.contains("spin-ok"),
                "success status must surface label; got: {captured:?}"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn run_with_progress_dumps_stderr_under_fail_status() {
        // Failure path emits a Fail status followed by every captured stderr
        // line dumped at depth+1 under the muted style — this is the diagnostic
        // surface a user sees when a spawned build/lint command fails.
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
            let multi = indicatif::MultiProgress::new();
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c")
                .arg("printf 'boom-1\n' 1>&2; printf 'boom-2\n' 1>&2; exit 9");

            let out = run_with_progress(
                &renderer,
                &sink,
                &multi,
                0,
                &mut cmd,
                "spin-fail",
                Instant::now(),
            )
            .unwrap();

            assert!(!out.status.success());
            assert_eq!(out.status.code(), Some(9));
            assert_eq!(out.stderr, "boom-1\nboom-2");

            let captured = crate::output::strip_ansi(&buf.lock().unwrap());
            assert!(
                captured.contains("spin-fail"),
                "fail status must surface label; got: {captured:?}"
            );
            assert!(
                captured.contains("boom-1"),
                "failed run must dump captured stderr; got: {captured:?}"
            );
            assert!(
                captured.contains("boom-2"),
                "failed run must dump every stderr line; got: {captured:?}"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn run_with_progress_caps_ring_to_visible_lines_but_captures_everything() {
        // The spinner ring only shows the last 5 lines, but the captured
        // `stdout` collection must still hold every single line the child
        // emitted — callers post-process this collection.
        with_deadline(Duration::from_secs(15), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
            let multi = indicatif::MultiProgress::new();
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c")
                .arg("for i in $(seq 1 12); do printf 'line-%02d\n' $i; done");

            let out = run_with_progress(
                &renderer,
                &sink,
                &multi,
                0,
                &mut cmd,
                "many-lines",
                Instant::now(),
            )
            .unwrap();

            assert!(out.status.success());
            // Every emitted line is captured (ring trimming is purely visual).
            let captured_lines: Vec<&str> = out.stdout.split('\n').collect();
            assert_eq!(captured_lines.len(), 12);
            assert_eq!(captured_lines.first().copied(), Some("line-01"));
            assert_eq!(captured_lines.last().copied(), Some("line-12"));
        });
    }

    #[test]
    #[serial_test::serial]
    fn run_with_progress_truncates_long_lines_in_ring_display() {
        // Lines longer than 120 chars are truncated on the spinner display,
        // but full content is preserved in the captured `stdout`. We verify
        // capture preserves the full line — the spinner display path is
        // exercised but not directly observable from the StringSink.
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
            let multi = indicatif::MultiProgress::new();
            let payload = "x".repeat(250);
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg(format!("printf '%s\n' {}", payload));

            let out = run_with_progress(
                &renderer,
                &sink,
                &multi,
                0,
                &mut cmd,
                "long-line",
                Instant::now(),
            )
            .unwrap();

            assert!(out.status.success());
            assert_eq!(out.stdout.len(), 250);
            assert_eq!(out.stdout, payload);
        });
    }

    #[test]
    #[serial_test::serial]
    fn run_command_dispatches_to_streaming_when_stderr_not_tty() {
        // In CI / under `cargo test`, stderr is never a TTY, so `run_command`
        // routes to `run_streaming`. Verify the public entry point produces
        // the same CommandOutput shape as a direct `run_streaming` call.
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
            let multi = indicatif::MultiProgress::new();
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg("printf 'dispatch-ok\n'; exit 0");
            let out = run_command(&renderer, &sink, &multi, 0, &mut cmd, "dispatch").unwrap();
            assert!(out.status.success());
            assert_eq!(out.stdout, "dispatch-ok");
        });
    }

    #[test]
    #[serial_test::serial]
    fn run_command_quiet_verbosity_takes_streaming_path() {
        // Even when stderr IS a TTY, Quiet verbosity forces the streaming
        // path. We can't fake a TTY easily, but Quiet should always work
        // and still capture output.
        with_deadline(Duration::from_secs(10), || {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let renderer = Renderer::new(Theme::default(), Verbosity::Quiet);
            let multi = indicatif::MultiProgress::new();
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg("printf 'quiet-cap\n'");
            let out = run_command(&renderer, &sink, &multi, 0, &mut cmd, "qcmd").unwrap();
            assert!(out.status.success());
            assert_eq!(out.stdout, "quiet-cap");
        });
    }

    #[test]
    fn sanitize_and_mute_preserves_text_when_no_foreign_ansi() {
        let renderer = Renderer::new(Theme::default(), Verbosity::Normal);
        let out = sanitize_and_mute(&renderer, "plain text");
        // Without ColorsEnabledGuard the wrapped style may or may not emit
        // escape codes depending on the suite's prior state. Strip ANSI and
        // confirm the payload survives.
        let visible = crate::output::strip_ansi(&out);
        assert_eq!(visible, "plain text");
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

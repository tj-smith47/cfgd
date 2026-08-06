//! `OutputWindow` — the bounded tailing view every long-running step's muted
//! output passes through.
//!
//! A step that shells out produces an unbounded amount of text that is not the
//! answer to anything the user asked. The window is the design system's answer:
//! at most `VISIBLE_LINES` of it are on screen at once, they live under the
//! spinner rather than in the scrollback, and the whole window collapses to a
//! single Status line the moment the step finishes. What remains afterwards is
//! one line per step, at the step's own indent — the same shape a step that
//! shelled out to nothing would leave.
//!
//! Two degradations, both automatic so no caller has to branch:
//!
//! - **Non-TTY or `Verbosity::Quiet`** — there is no window to repaint, so the
//!   lines stream instead and a leading `Status(Running)` opens the step. A CI
//!   log keeps the full output it exists to preserve.
//! - **Over-wide lines** — clamped to the live terminal width at the window's
//!   own indent, so a long line never wraps the window to a second row or
//!   scrolls the terminal sideways.
//!
//! Callers hand over raw child bytes: `push_line` strips ANSI and escapes
//! residual control characters itself, because a child that emits its own
//! screen-reset (`nvim --headless`) would otherwise execute it against the real
//! terminal.
use std::collections::VecDeque;
use std::marker::PhantomData;

use super::renderer::StatusFields;
use super::spinner::Spinner;
use super::status_builder::StatusBuilder;
use super::{Role, Verbosity};

/// Lines of tail kept on screen under the spinner.
const VISIBLE_LINES: usize = 5;

/// Fallback width when stderr has no size (redirected, or a terminal that
/// won't answer). Wide enough to stay readable, narrow enough that a real
/// 80-column terminal is the only thing that wraps.
const FALLBACK_WIDTH: usize = 100;

/// Smallest width worth clamping to; below this the ellipsis is most of the
/// line and truncation stops telling the user anything.
const MIN_WIDTH: usize = 24;

/// Terminal columns available to a line rendered at `depth`, after the
/// spinner's own glyph and separating space.
fn available_width(depth: usize) -> usize {
    let cols = console::Term::stderr()
        .size_checked()
        .map_or(FALLBACK_WIDTH, |(_, cols)| cols as usize);
    cols.saturating_sub(depth * 2 + 2).max(MIN_WIDTH)
}

/// Clamp `text` to `max` display columns, width-aware (a CJK or emoji glyph
/// counts as the two columns it occupies).
fn clamp_line(text: &str, max: usize) -> String {
    console::truncate_str(text, max, "…").into_owned()
}

/// Strip child ANSI, then render any control byte that survived as visible
/// `\xNN`. Order matters: stripping first keeps a legitimate colour escape
/// from surviving as literal `\x1b[32m` text.
fn sanitize(line: &str) -> String {
    crate::escape_control_chars(&super::strip_ansi(line))
}

/// Bounded live view over a child process's output. Build with
/// [`super::Printer::output_window`], feed with [`OutputWindow::push_line`],
/// close with one of the `finish_*` methods.
///
/// Dropping without an explicit finish collapses the window and emits a
/// `Status(Info)`, so an abandoned step still leaves one line behind.
pub struct OutputWindow<'p> {
    spinner: Spinner<'p>,
    ring: VecDeque<String>,
    label: String,
    /// True when the tail repaints in place; false when it streams.
    windowed: bool,
    /// Indent the tail renders at — one level inside the step's own line.
    body_depth: usize,
}

impl<'p> OutputWindow<'p> {
    pub(crate) fn new(spinner: Spinner<'p>, label: String) -> Self {
        let windowed = !spinner.bar.is_hidden();
        let body_depth = spinner.depth + 1;
        if !windowed && spinner.renderer.verbosity != Verbosity::Quiet {
            // No window to hold the label, so the step announces itself the way
            // `run_streaming` does: a Running status, then its lines.
            spinner.renderer.render_status(
                spinner.sink.as_ref(),
                spinner.depth,
                &StatusFields {
                    role: Role::Running,
                    subject: &label,
                    detail: None,
                    duration: None,
                    target: None,
                },
            );
        }
        Self {
            spinner,
            ring: VecDeque::with_capacity(VISIBLE_LINES),
            label,
            windowed,
            body_depth,
        }
    }

    /// Feed one raw line of child output. Blank lines are dropped — they cost a
    /// window row and carry nothing.
    pub fn push_line(&mut self, raw: &str) {
        let clean = sanitize(raw);
        let text = clean.trim_end();
        if text.is_empty() {
            return;
        }
        let clamped = clamp_line(text, available_width(self.body_depth));
        if !self.windowed {
            self.spinner.renderer.render_stream_line(
                self.spinner.sink.as_ref(),
                self.body_depth,
                &clamped,
            );
            return;
        }
        if self.ring.len() >= VISIBLE_LINES {
            self.ring.pop_front();
        }
        self.ring.push_back(clamped);
        self.repaint();
    }

    fn repaint(&self) {
        let indent = "  ".repeat(self.body_depth);
        let mut msg = self.label.clone();
        for line in &self.ring {
            msg.push('\n');
            msg.push_str(&indent);
            msg.push_str(&self.spinner.renderer.theme.muted.apply_to(line).to_string());
        }
        self.spinner.set_message(msg);
    }

    /// Collapse the window and replace it with a single success Status.
    pub fn finish_ok(self, subject: impl Into<String>) -> StatusBuilder<'p> {
        self.spinner.finish_ok(subject)
    }

    /// Collapse the window and replace it with a single warning Status.
    pub fn finish_warn(self, subject: impl Into<String>) -> StatusBuilder<'p> {
        self.spinner.finish_warn(subject)
    }

    /// Collapse the window and replace it with a single failure Status.
    ///
    /// The window's tail is gone once this returns; a caller that needs the
    /// child's output visible after a failure holds the full capture and
    /// renders it itself, below the Status.
    pub fn finish_fail(self, subject: impl Into<String>) -> StatusBuilder<'p> {
        self.spinner.finish_fail(subject)
    }

    /// True when the tail lived in the repainting window and is therefore gone
    /// after the collapse — the only case where a failure has to replay it. In
    /// the streaming degradation the lines are already in the scrollback, and
    /// replaying them prints the whole stderr twice.
    #[must_use]
    pub fn tail_needs_replay(&self) -> bool {
        self.windowed
    }

    /// Render `lines` as muted body text under the closed window's Status —
    /// the post-failure dump. Call only when the closed window's
    /// [`Self::tail_needs_replay`] was `true`; in the streaming degradation the
    /// lines this would render are already in the scrollback.
    pub fn dump_below(
        printer: &super::Printer,
        depth: usize,
        lines: impl IntoIterator<Item = impl AsRef<str>>,
    ) {
        let width = available_width(depth + 1);
        for line in lines {
            let clean = sanitize(line.as_ref());
            let text = clean.trim_end();
            if text.is_empty() {
                continue;
            }
            printer.stream_line_at(depth + 1, &clamp_line(text, width));
        }
    }
}

impl super::Printer {
    /// Open a bounded output window at `depth` for a step labelled `label`.
    ///
    /// Every surface that displays a child process's muted output goes through
    /// this — there is no second tail implementation to drift from it.
    #[must_use]
    pub fn output_window_at(&self, depth: usize, label: impl Into<String>) -> OutputWindow<'_> {
        let label = label.into();
        let bar = super::spinner::make_spinner_bar(
            &self.multi_progress,
            &self.renderer,
            self.verbosity(),
            &label,
        );
        let spinner = Spinner {
            renderer: self.renderer.clone(),
            sink: self.sink_stderr.clone(),
            depth,
            bar,
            message: label.clone(),
            finished: false,
            _phantom: PhantomData,
        };
        OutputWindow::new(spinner, label)
    }

    /// Top-level [`Printer::output_window_at`].
    #[must_use]
    pub fn output_window(&self, label: impl Into<String>) -> OutputWindow<'_> {
        self.output_window_at(0, label)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::renderer::{Renderer, StringSink};
    use super::super::{Theme, Verbosity};
    use super::*;
    use crate::output::strip_ansi;

    fn window(depth: usize, verbosity: Verbosity) -> (OutputWindow<'static>, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let spinner = Spinner {
            renderer: Arc::new(Renderer::new(Theme::default(), verbosity)),
            sink: Arc::new(StringSink(buf.clone())),
            depth,
            // Hidden bar: no TTY under test, which is also the streaming path.
            bar: indicatif::ProgressBar::hidden(),
            message: "step".into(),
            finished: false,
            _phantom: PhantomData,
        };
        (OutputWindow::new(spinner, "step".into()), buf)
    }

    #[test]
    fn clamp_line_truncates_to_width_with_ellipsis() {
        let out = clamp_line(&"x".repeat(200), 40);
        assert_eq!(console::measure_text_width(&out), 40, "got: {out:?}");
        assert!(out.ends_with('…'), "got: {out:?}");
    }

    #[test]
    fn clamp_line_leaves_short_lines_untouched() {
        assert_eq!(clamp_line("npm warn deprecated", 40), "npm warn deprecated");
    }

    #[test]
    fn clamp_line_counts_display_columns_not_bytes() {
        // Multibyte input is where a byte-index truncation silently gave up and
        // let the whole line through.
        let wide = "日".repeat(100);
        let out = clamp_line(&wide, 20);
        assert!(
            console::measure_text_width(&out) <= 20,
            "wide line not clamped: {out:?}"
        );
    }

    #[test]
    fn sanitize_neutralizes_child_screen_resets() {
        let out = sanitize("\x1b[2J\x1b[Hcleared\r");
        assert!(!out.contains('\x1b'), "escape survived: {out:?}");
        assert!(out.contains("cleared"), "text lost: {out:?}");
    }

    #[test]
    fn hidden_bar_announces_then_streams_each_line() {
        let (mut w, buf) = window(0, Verbosity::Normal);
        w.push_line("first");
        w.push_line("second");
        let _ = w.finish_ok("step done");
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("first"), "got: {out:?}");
        assert!(out.contains("second"), "got: {out:?}");
        assert!(out.contains("step done"), "got: {out:?}");
    }

    #[test]
    fn quiet_emits_neither_announcement_nor_lines() {
        let (mut w, buf) = window(0, Verbosity::Quiet);
        w.push_line("noise");
        let _ = w.finish_ok("step done");
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(!out.contains("noise"), "quiet leaked output: {out:?}");
    }

    #[test]
    fn blank_lines_are_dropped() {
        let (mut w, buf) = window(0, Verbosity::Normal);
        w.push_line("   ");
        w.push_line("kept");
        let _ = w.finish_ok("done");
        let out = strip_ansi(&buf.lock().unwrap());
        let body: Vec<&str> = out.lines().filter(|l| l.trim() == "kept").collect();
        assert_eq!(body.len(), 1, "got: {out:?}");
    }

    #[test]
    fn ring_keeps_only_the_last_visible_lines() {
        let buf = Arc::new(Mutex::new(String::new()));
        let spinner = Spinner {
            renderer: Arc::new(Renderer::new(Theme::default(), Verbosity::Normal)),
            sink: Arc::new(StringSink(buf.clone())),
            depth: 0,
            bar: indicatif::ProgressBar::hidden(),
            message: "step".into(),
            finished: false,
            _phantom: PhantomData,
        };
        let mut w = OutputWindow::new(spinner, "step".into());
        // Force the windowed branch: the hidden bar picked streaming, but the
        // ring is what this asserts on.
        w.windowed = true;
        for i in 0..20 {
            w.push_line(&format!("line {i}"));
        }
        assert_eq!(w.ring.len(), VISIBLE_LINES);
        assert_eq!(w.ring.back().map(String::as_str), Some("line 19"));
        assert_eq!(w.ring.front().map(String::as_str), Some("line 15"));
        let _ = w.finish_ok("done");
    }

    #[test]
    fn finish_fail_collapses_to_one_status() {
        let (mut w, buf) = window(0, Verbosity::Normal);
        w.windowed = true;
        w.push_line("some noise nobody needs");
        let _ = w.finish_fail("step failed");
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("step failed"), "got: {out:?}");
        assert!(
            !out.contains("some noise nobody needs"),
            "window tail leaked into scrollback: {out:?}"
        );
    }

    #[test]
    fn tail_needs_replay_is_false_on_the_streaming_path() {
        // The test harness produces a hidden bar (no TTY), which is exactly
        // the streaming degradation whose tail is already in the scrollback —
        // a caller must not replay it after the collapse.
        let (w, _buf) = window(0, Verbosity::Normal);
        assert!(!w.tail_needs_replay(), "expected the streaming path");
    }

    #[test]
    fn available_width_shrinks_with_depth() {
        assert!(available_width(3) < available_width(0));
        assert!(available_width(40) >= MIN_WIDTH);
    }
}

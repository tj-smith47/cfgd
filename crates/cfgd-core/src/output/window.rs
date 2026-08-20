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
//! - **Over-wide lines in the repainting window** — clamped to the live
//!   terminal width, since indicatif rewrites the ring's rows in place and has
//!   no way to reach a second row for one that's too long. A line reaching the
//!   terminal any other way (streamed, or dumped below a collapsed window) is
//!   soft-wrapped by the renderer instead, which can lay out a second row.
//!
//! Callers hand over raw child bytes: `push_line` strips ANSI and escapes
//! residual control characters itself, because a child that emits its own
//! screen-reset (`nvim --headless`) would otherwise execute it against the real
//! terminal.
use std::collections::VecDeque;
use std::marker::PhantomData;

use super::renderer::StatusFields;
use super::renderer::finalize_subject;
use super::renderer::indent_prefix;
use super::renderer::wrap::{available_width, clamp as clamp_line};
use super::spinner::Spinner;
use super::status_builder::StatusBuilder;
use super::{Role, Verbosity};

/// Lines of tail kept on screen under the spinner.
const VISIBLE_LINES: usize = 5;

/// Strip child ANSI, collapse any in-place rewrite to what it settled on, then
/// render a control byte that still survived as visible `\xNN`.
///
/// Order matters twice over. Stripping first keeps a legitimate colour escape
/// from surviving as literal `\x1b[32m` text. Resolving carriage returns
/// before escaping is what keeps a progress meter readable: a child that
/// redraws one line by returning the carriage instead of ending it (`git
/// clone`, `pip`) sends its entire redraw history as one line, and escaping
/// that renders every superseded frame joined by a literal `\x0d`. Only the
/// segment after the final return was ever meant to be on screen.
fn sanitize(line: &str) -> String {
    let stripped = super::strip_ansi(line);
    let settled = stripped.trim_end_matches('\r');
    crate::escape_control_chars(settled.rsplit('\r').next().unwrap_or(settled))
}

/// Bounded live view over a child process's output. Build with
/// [`super::Printer::output_window`], feed with [`OutputWindow::push_line`],
/// close with one of the `finish_*` methods.
///
/// Dropping without an explicit finish collapses the window and settles it
/// via the inner [`Spinner`]'s own Drop — `Role::Skipped` with an
/// "(interrupted)" suffix — so an abandoned step still leaves one line
/// behind, distinct from a real success or failure.
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
        Self::opened(spinner, label, /*announce=*/ true)
    }

    /// A window over a line its CALLER owns and will settle — a
    /// [`super::live_row::LiveRow`]'s.
    ///
    /// It never announces the step. The row above it already names the action
    /// and will carry its outcome, so a `Status(Running)` here would be that
    /// action's second line, and the one the row settles would be its third.
    pub(crate) fn borrowed(spinner: Spinner<'p>, label: String) -> Self {
        Self::opened(spinner, label, /*announce=*/ false)
    }

    fn opened(spinner: Spinner<'p>, label: String, announce: bool) -> Self {
        let windowed = !spinner.bar.is_hidden();
        let body_depth = spinner.depth + 1;
        if announce && !windowed && spinner.renderer.verbosity != Verbosity::Quiet {
            // No window to hold the label, so the step announces itself the way
            // `run_streaming` does: a Running status, then its lines.
            //
            // The label is caller-supplied — a module's own `run:` script body
            // reaches it — and this arm fires exactly when there is no bar to
            // hold it, so an unfolded escape lands in a redirected log the
            // operator reads later. Same fold every other subject slot takes.
            let announced = finalize_subject(&spinner.renderer.theme, &label, None, None, None);
            spinner.renderer.render_status(
                spinner.sink.as_ref(),
                spinner.depth,
                &StatusFields {
                    role: Role::Running,
                    subject: &announced,
                    detail: None,
                    duration: None,
                    target: None,
                    subject_style: None,
                    detail_style: None,
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
        if !self.windowed {
            // Streamed lines append and scroll rather than repainting a fixed
            // row, so `render_stream_line` (via `write_line`) can soft-wrap
            // them at the sink's real width instead of losing the tail.
            self.spinner.renderer.render_stream_line(
                self.spinner.sink.as_ref(),
                self.body_depth,
                text,
            );
            return;
        }
        // The ring's rows are rewritten in place by `repaint` below — indicatif
        // has no way to reach a second row for an over-wide one — so this is
        // the one path that must clamp rather than wrap.
        let clamped = clamp_line(
            text,
            available_width(self.spinner.sink.as_ref(), self.body_depth),
        );
        if self.ring.len() >= VISIBLE_LINES {
            self.ring.pop_front();
        }
        self.ring.push_back(clamped);
        self.repaint();
    }

    fn repaint(&mut self) {
        let indent = indent_prefix(self.body_depth);
        let mut msg = self.label.clone();
        for line in &self.ring {
            msg.push('\n');
            msg.push_str(&indent);
            msg.push_str(&self.spinner.renderer.theme.muted.apply_to(line).to_string());
        }
        self.spinner.set_message(msg);
    }

    /// Collapse the window and replace it with a single Status in `role`.
    ///
    /// The general form: `finish_ok` / `finish_warn` / `finish_fail` are the
    /// three roles with names, and a caller whose role is computed needs the
    /// parameter rather than a fourth name.
    pub fn finish_with(self, role: Role, subject: impl Into<String>) -> StatusBuilder<'p> {
        self.spinner.finish_with(role, subject)
    }

    /// Collapse the window and replace it with a single success Status.
    pub fn finish_ok(self, subject: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Ok, subject)
    }

    /// Collapse the window and replace it with a single warning Status.
    pub fn finish_warn(self, subject: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Warn, subject)
    }

    /// Collapse the window and replace it with a single failure Status.
    ///
    /// The window's tail is gone once this returns; a caller that needs the
    /// child's output visible after a failure holds the full capture and
    /// renders it itself, below the Status.
    pub fn finish_fail(self, subject: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Fail, subject)
    }

    /// Collapse the window leaving NO status line behind.
    ///
    /// For the one case the window is not the action: when a caller further up
    /// emits the action's own status line (the reconciler's tree), a settled
    /// window line would be a second line for one action. The live tail still
    /// renders while the command runs; only the collapse is silent.
    pub fn finish_silent(self) {
        self.spinner.finish_silent();
    }

    /// Give the line back to whoever owns it, printing nothing and leaving it
    /// on screen exactly as the last repaint left it.
    ///
    /// For a window drawn on a [`super::live_row::LiveRow`]: the row settles
    /// its own line, and [`Self::finish_silent`] would retire the row's bar
    /// along with the window, so the settle would paint a line nothing draws.
    pub(crate) fn release(self) {
        self.spinner.release();
    }

    /// Collapse the window into the deepest level of the phase → owner →
    /// action tree: [`Self::finish_with`] with the subject painted
    /// `theme.primary`, so a script's line matches every other action line.
    pub fn finish_action(self, role: Role, subject: impl Into<String>) -> StatusBuilder<'p> {
        let style = self.spinner.renderer.theme.primary.clone();
        self.finish_with(role, subject).with_subject_style(style)
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
    ///
    /// And only from a window that settled a status of its own: the body is
    /// positioned relative to a line already written, so a window closed with
    /// [`Self::finish_silent`] has nothing for it to land under and the dump
    /// would print above the caller's line instead of below it.
    pub fn dump_below(
        printer: &super::Printer,
        depth: usize,
        lines: impl IntoIterator<Item = impl AsRef<str>>,
    ) {
        // A pure append below an already-collapsed status, not a repaint — so
        // `stream_line_at` (via `write_line`) wraps this the same way it wraps
        // any other muted body text, instead of clamping the tail away.
        for line in lines {
            let clean = sanitize(line.as_ref());
            let text = clean.trim_end();
            if text.is_empty() {
                continue;
            }
            printer.stream_line_at(depth + 1, text);
        }
    }
}

impl super::Printer {
    /// Open a bounded output window at `depth` for a step labelled `label`.
    ///
    /// Every surface that displays a child process's muted output goes through
    /// this — there is no second tail implementation to drift from it.
    /// Open a bounded output window at the ambient depth — the innermost open
    /// section while a `DepthInheritGuard` is held, column 0 otherwise. The
    /// same relationship [`Printer::run`] has to `run_command`: a caller
    /// inside a section gets its window indented under the line it belongs to
    /// without naming a depth it would have to keep in sync.
    #[must_use]
    pub fn output_window(&self, label: impl Into<String>) -> OutputWindow<'_> {
        self.output_window_at(self.renderer.inherit_depth(), label)
    }

    #[must_use]
    pub fn output_window_at(&self, depth: usize, label: impl Into<String>) -> OutputWindow<'_> {
        let label = super::spinner::compose_in_flight_subject(label);
        let (bar, live) = super::spinner::make_spinner_bar(
            &self.multi_progress,
            &self.renderer,
            self.live_bars(),
            depth,
            &label,
        );
        let spinner = Spinner {
            renderer: self.renderer.clone(),
            sink: self.sink_stderr.clone(),
            depth,
            bar,
            message: label.clone(),
            finished: false,
            _live: live,
            borrowed: false,
            _phantom: PhantomData,
        };
        OutputWindow::new(spinner, label)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::renderer::{Renderer, StringSink};
    use super::super::{Theme, Verbosity};
    use super::*;

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
            _live: None,
            borrowed: false,
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
    fn sanitize_keeps_only_what_a_rewritten_line_settled_on() {
        // One `git clone` line carrying its whole redraw history. Escaping it
        // wholesale rendered every superseded frame joined by literal `\x0d`.
        let out =
            sanitize("Receiving objects: 46% (4368/9495)\rReceiving objects: 91% (8640/9495)");
        assert_eq!(out, "Receiving objects: 91% (8640/9495)");
    }

    #[test]
    fn sanitize_keeps_a_line_a_trailing_return_left_on_screen() {
        // The return came last, so nothing overwrote the text before it.
        assert_eq!(
            sanitize("Resolving deltas: 100%\r"),
            "Resolving deltas: 100%"
        );
    }

    #[test]
    fn hidden_bar_announces_then_streams_each_line() {
        let (mut w, buf) = window(0, Verbosity::Normal);
        w.push_line("first");
        w.push_line("second");
        let _ = w.finish_ok("step done");
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("first"), "got: {out:?}");
        assert!(out.contains("second"), "got: {out:?}");
        assert!(out.contains("step done"), "got: {out:?}");
    }

    #[test]
    fn streamed_long_line_reaches_the_sink_untruncated() {
        // The non-windowed path appends and scrolls, so the renderer wraps it
        // rather than clamping it — unlike the windowed ring below, which
        // repaints a fixed row and has nowhere to put a second line.
        let (mut w, buf) = window(0, Verbosity::Normal);
        let long = "x".repeat(200);
        w.push_line(&long);
        let _ = w.finish_ok("step done");
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains(&long), "long line was truncated: {out:?}");
    }

    #[test]
    fn quiet_emits_neither_announcement_nor_lines() {
        let (mut w, buf) = window(0, Verbosity::Quiet);
        w.push_line("noise");
        let _ = w.finish_ok("step done");
        let out = crate::test_helpers::captured_text(&buf);
        assert!(!out.contains("noise"), "quiet leaked output: {out:?}");
    }

    #[test]
    fn blank_lines_are_dropped() {
        let (mut w, buf) = window(0, Verbosity::Normal);
        w.push_line("   ");
        w.push_line("kept");
        let _ = w.finish_ok("done");
        let out = crate::test_helpers::captured_text(&buf);
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
            _live: None,
            borrowed: false,
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
    fn windowed_ring_clamps_a_line_too_wide_for_the_repaint_row() {
        // The ring is rewritten in place by `repaint`, so an over-wide line
        // must be clamped rather than wrapped — indicatif cannot reach a
        // second row for it.
        let (mut w, _buf) = window(0, Verbosity::Normal);
        w.windowed = true;
        w.push_line(&"x".repeat(200));
        let stored = w.ring.back().expect("line was pushed");
        assert!(stored.ends_with('…'), "ring line not clamped: {stored:?}");
        assert!(stored.len() < 200, "ring line not clamped: {stored:?}");
        let _ = w.finish_ok("done");
    }

    #[test]
    fn finish_fail_collapses_to_one_status() {
        let (mut w, buf) = window(0, Verbosity::Normal);
        w.windowed = true;
        w.push_line("some noise nobody needs");
        let _ = w.finish_fail("step failed");
        let out = crate::test_helpers::captured_text(&buf);
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
        let sink = StringSink(Arc::new(Mutex::new(String::new())));
        assert!(available_width(&sink, 3) < available_width(&sink, 0));
        assert!(
            available_width(&sink, 40) >= 24,
            "clamped below a usable floor"
        );
    }

    #[test]
    fn window_finish_with_emits_the_given_role() {
        // The Windowed script arm picks its role from `non_fatal` at the call
        // site, so the primitive has to carry an arbitrary role through rather
        // than offering three fixed collapses.
        for (role, glyph) in [(Role::Warn, "⚠"), (Role::Fail, "✗"), (Role::Skipped, "—")] {
            let (w, buf) = window(0, Verbosity::Normal);
            let _ = w.finish_with(role, "script finished");
            let out = crate::test_helpers::captured_text(&buf);
            assert!(
                out.contains(&format!("{glyph} script finished")),
                "role {role:?} did not reach the status line: {out:?}"
            );
        }
    }
}

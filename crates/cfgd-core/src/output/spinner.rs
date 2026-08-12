//! `Spinner` and `ProgressBar` — live progress indicators.
//!
//! `Spinner::finish_ok` / `finish_warn` / `finish_fail` / `finish_skipped`
//! return a `StatusBuilder` so the caller can chain `.detail` / `.duration`
//! / `.target` before the Status commits on Drop.
//!
//! A `Spinner` dropped without an explicit finish emits a `Status(Info)` so
//! the spinner doesn't disappear silently — abandonment leaves a record.
use std::io::IsTerminal;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use indicatif::{ProgressBar as IndProgressBar, ProgressStyle};

use super::Role;
use super::renderer::{LiveBarGuard, Renderer, Writer, wrap};
use super::status_builder::StatusBuilder;

pub(crate) fn stderr_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}

/// Truncate a spinner message's own line to what `sink` can hold at `depth`,
/// leaving any lines below it alone.
///
/// indicatif repaints a spinner in place by rewinding a fixed number of rows.
/// A label the terminal has to hard-wrap occupies one row more than the
/// repaint accounts for, so the overflow is stranded in the scrollback on
/// every tick and again at the collapse — the wrap this can't be handed to
/// `wrap_body`, because there is no second row to wrap onto. The lines that
/// follow are an `OutputWindow` tail, already clamped at their own indent.
fn clamp_label(sink: &dyn Writer, message: &str, depth: usize) -> String {
    let width = wrap::available_width(sink, depth);
    match message.split_once('\n') {
        Some((head, rest)) => format!("{}\n{}", wrap::clamp(head, width), rest),
        None => wrap::clamp(message, width),
    }
}

/// Live spinner. Drop without `finish_*()` emits a `Status(Info)` with the
/// spinner message at the active depth — leaves a record so the spinner
/// doesn't disappear silently.
pub struct Spinner<'p> {
    pub(crate) renderer: Arc<Renderer>,
    pub(crate) sink: Arc<dyn Writer>,
    pub(crate) depth: usize,
    pub(crate) bar: IndProgressBar,
    pub(crate) message: String,
    pub(crate) finished: bool,
    /// Held for the bar's lifetime so the renderer's live-bar count tracks it
    /// with no paired decrement to forget. `None` for a hidden bar, which is
    /// never added to the MultiProgress and so must not be counted.
    pub(crate) _live: Option<LiveBarGuard>,
    pub(crate) _phantom: PhantomData<&'p ()>,
}

impl<'p> Spinner<'p> {
    pub fn set_message(&self, text: impl Into<String>) {
        self.bar
            .set_message(clamp_label(self.sink.as_ref(), &text.into(), self.depth));
    }

    pub fn finish_ok(self, final_text: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Ok, final_text)
    }
    pub fn finish_warn(self, final_text: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Warn, final_text)
    }
    pub fn finish_fail(self, final_text: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Fail, final_text)
    }
    pub fn finish_skipped(self, final_text: impl Into<String>) -> StatusBuilder<'p> {
        self.finish_with(Role::Skipped, final_text)
    }

    /// Retire the bar without printing a status line of its own.
    ///
    /// For a caller that collapses several concurrent spinners into one
    /// combined status line elsewhere (the index-refresh pre-pass) — each
    /// lane's own spinner must vanish silently, or every lane would print its
    /// own line on top of the one summary line describing all of them.
    /// Suppresses `Drop`'s `Status(Info)`, the same way an explicit
    /// `finish_*` does.
    pub(crate) fn finish_silent(mut self) {
        self.bar.finish_and_clear();
        self.finished = true;
    }

    /// The general form the four named finishes delegate to. `pub(crate)` so
    /// `OutputWindow` can offer the same shape without widening a spinner's
    /// finish to the public API, where it would invite a caller to bypass the
    /// window's collapse.
    pub(crate) fn finish_with(
        mut self,
        role: Role,
        subject: impl Into<String>,
    ) -> StatusBuilder<'p> {
        self.bar.finish_and_clear();
        self.finished = true;
        // The Arc clones below give the returned StatusBuilder an
        // independent reference to the renderer and sink. `self` is moved
        // into this fn and dropped at the end of the call, but the
        // StatusBuilder must outlive it (Drop fires when the caller drops
        // the builder).
        StatusBuilder::new(
            self.renderer.clone(),
            self.sink.clone(),
            self.depth,
            role,
            subject,
        )
    }
}

impl Drop for Spinner<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.bar.finish_and_clear();
        // Emit an Info Status so the spinner leaves a record.
        //
        // The `self.renderer.clone()` and `self.sink.clone()` Arc-clones
        // inside `StatusBuilder::new` (passed as arguments below) are
        // LOAD-BEARING. The StatusBuilder needs an independent Arc so that
        // when `self` finishes dropping and its Arc fields are released,
        // the builder (whose own Drop fires at the end of this function via
        // the `drop(sb)` call) still holds a live reference to the
        // renderer and sink.
        let msg = std::mem::take(&mut self.message);
        let sb = StatusBuilder::new(
            self.renderer.clone(),
            self.sink.clone(),
            self.depth,
            Role::Info,
            msg,
        );
        drop(sb);
    }
}

/// Bounded progress bar.
pub struct ProgressBar<'p> {
    pub(crate) bar: IndProgressBar,
    /// See `Spinner::_live`. `finish` consumes the wrapper, so the guard is
    /// released exactly once with no `Drop` impl of its own.
    pub(crate) _live: Option<LiveBarGuard>,
    pub(crate) _phantom: PhantomData<&'p ()>,
}

impl<'p> ProgressBar<'p> {
    pub fn inc(&self, delta: u64) {
        self.bar.inc(delta);
    }
    pub fn set_position(&self, pos: u64) {
        self.bar.set_position(pos);
    }
    pub fn set_message(&self, m: impl Into<String>) {
        self.bar.set_message(m.into());
    }
    pub fn finish(self) {
        self.bar.finish_and_clear();
    }
}

impl super::Printer {
    /// Whether anything can be drawn in this printer's live region at all — the
    /// ONE statement of that gate, so a caller that has to decide BEFORE
    /// building a bar (a lane choosing between a live window and a capture)
    /// cannot answer it differently from the builders below.
    ///
    /// A property of the printer rather than of the process: the terminal a bar
    /// would repaint is the one this printer writes to.
    pub(crate) fn live_bars(&self) -> bool {
        self.verbosity() != super::Verbosity::Quiet && self.live_region
    }
}

/// Return the appropriate spinner bar for the current verbosity/TTY state:
/// a hidden bar under Quiet or non-TTY, otherwise a styled spinner attached
/// to the MultiProgress. Used by both `Printer::spinner` and
/// `SectionGuard::spinner` to avoid duplicating the gate.
pub(crate) fn make_spinner_bar(
    multi: &indicatif::MultiProgress,
    renderer: &Arc<Renderer>,
    live_bars: bool,
    depth: usize,
    message: &str,
) -> (IndProgressBar, Option<LiveBarGuard>) {
    if !live_bars {
        (IndProgressBar::hidden(), None)
    } else {
        // No `Spinner` (and so no sink) exists yet at this point in
        // construction — this measures `console::Term::stderr()` directly,
        // which is exactly the sink `Printer` wires into every `Spinner` in
        // production (see `sink_stderr` in printer.rs). Reachable only when
        // the printer reported a live region, and a captured-output test
        // always trips the `IndProgressBar::hidden()` branch above instead, so
        // this can never influence a `StringSink` capture.
        let (bar, live) = build_spinner(
            multi,
            renderer,
            &clamp_label(&console::Term::stderr(), message, depth),
        );
        (bar, Some(live))
    }
}

/// Same gate as `make_spinner_bar`, for bounded progress bars.
pub(crate) fn make_progress_bar(
    multi: &indicatif::MultiProgress,
    renderer: &Arc<Renderer>,
    total: u64,
    live_bars: bool,
    message: &str,
) -> (IndProgressBar, Option<LiveBarGuard>) {
    if !live_bars {
        (IndProgressBar::hidden(), None)
    } else {
        let (bar, live) = build_progress_bar(multi, renderer, total, message);
        (bar, Some(live))
    }
}

/// Build a styled spinner ProgressBar attached to a MultiProgress.
pub(crate) fn build_spinner(
    multi: &indicatif::MultiProgress,
    renderer: &Arc<Renderer>,
    message: &str,
) -> (IndProgressBar, LiveBarGuard) {
    let pb = multi.add(IndProgressBar::new_spinner());
    let live = LiveBarGuard::acquire(renderer);
    let frames_raw = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let styled: Vec<String> = frames_raw
        .iter()
        .map(|f| renderer.theme.info.apply_to(f).to_string())
        .collect();
    let mut tick_refs: Vec<&str> = styled.iter().map(|s| s.as_str()).collect();
    tick_refs.push(" ");
    pb.set_style(
        ProgressStyle::with_template("{spinner} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner())
            .tick_strings(&tick_refs),
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    (pb, live)
}

pub(crate) fn build_progress_bar(
    multi: &indicatif::MultiProgress,
    renderer: &Arc<Renderer>,
    total: u64,
    message: &str,
) -> (IndProgressBar, LiveBarGuard) {
    let pb = multi.add(IndProgressBar::new(total));
    let live = LiveBarGuard::acquire(renderer);
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} [{bar:30.cyan/dim}] {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("━╸─"),
    );
    pb.set_message(message.to_string());
    (pb, live)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::renderer::{Renderer, StringSink};
    use super::super::{Theme, Verbosity};
    use super::*;
    use crate::output::strip_ansi;

    fn renderer() -> Arc<Renderer> {
        Arc::new(Renderer::new(Theme::default(), Verbosity::Normal))
    }

    fn sink_for(buf: &Arc<Mutex<String>>) -> Arc<dyn Writer> {
        Arc::new(StringSink(buf.clone()))
    }

    #[test]
    fn clamp_label_keeps_the_spinner_on_one_row() {
        let sink = sink_for(&Arc::new(Mutex::new(String::new())));
        let long = "sudo apt-get install -y ".repeat(20);
        let out = clamp_label(sink.as_ref(), &long, 0);
        assert!(!out.contains('\n'), "label gained a row: {out:?}");
        assert!(out.len() < long.len(), "label was not clamped");
        assert!(out.ends_with('…'));
    }

    #[test]
    fn clamp_label_leaves_the_window_tail_below_it_alone() {
        // Only the spinner's own row is unwrappable; the tail beneath it was
        // already clamped at its own indent and must survive byte for byte.
        let sink = sink_for(&Arc::new(Mutex::new(String::new())));
        let tail = "  first tail line\n  second tail line";
        let out = clamp_label(sink.as_ref(), &format!("short label\n{tail}"), 0);
        assert_eq!(out, format!("short label\n{tail}"));
    }

    #[test]
    fn finish_ok_emits_status_at_section_depth() {
        let r = renderer();
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = sink_for(&buf);
        // Hidden bar (no TTY in test); finish_ok still emits the Status line.
        let sp = Spinner {
            renderer: r.clone(),
            sink: sink.clone(),
            depth: 1,
            bar: indicatif::ProgressBar::hidden(),
            message: "doing work".into(),
            finished: false,
            _live: None,
            _phantom: std::marker::PhantomData,
        };
        let _ = sp.finish_ok("done");
        // _ drops here → Status committed
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("  ✓ done"), "got: {out:?}");
    }

    #[test]
    fn drop_without_finish_emits_info_record() {
        let r = renderer();
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = sink_for(&buf);
        {
            let _sp = Spinner {
                renderer: r.clone(),
                sink: sink.clone(),
                depth: 0,
                bar: indicatif::ProgressBar::hidden(),
                message: "abandoned".into(),
                finished: false,
                _live: None,
                _phantom: std::marker::PhantomData,
            };
        }
        let out = strip_ansi(&buf.lock().unwrap());
        // Info role has no icon; subject text appears.
        assert!(out.contains("abandoned"), "got: {out:?}");
    }

    #[test]
    fn quiet_printer_returns_hidden_spinner() {
        use super::super::printer::Printer;
        let p = Printer::with_format(
            super::super::Verbosity::Quiet,
            None,
            super::super::OutputFormat::Table,
        );
        let sp = p.spinner("x");
        assert!(sp.bar.is_hidden(), "Quiet should yield a hidden bar");
    }
}

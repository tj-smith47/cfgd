//! `Spinner` and `ProgressBar` — live progress indicators.
//!
//! `Spinner::finish_ok` / `finish_warn` / `finish_fail` / `finish_skipped`
//! return a `StatusBuilder` so the caller can chain `.detail` / `.duration`
//! / `.target` before the Status commits on Drop.
//!
//! A `Spinner` dropped without an explicit finish emits a `Status(Info)` so
//! the spinner doesn't disappear silently — abandonment leaves a record. The
//! one exception is a spinner whose bar it BORROWED from a
//! [`super::live_row::LiveRow`]: that line has an owner who will settle or
//! retire it, so an abandoned one leaves it alone rather than clearing it and
//! recording a second line for the action the row is about to describe.
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
/// The message NEVER carries the indent — [`set_bar_depth`] puts it in the
/// bar's `{prefix}`, ahead of the animated frame. Indenting here instead would
/// leave the frame at column 0 with the text pushed away from it, which is
/// where a spinner inside a section drifts out of the glyph column its own
/// settled line lands in.
pub(super) fn clamp_label(sink: &dyn Writer, message: &str, depth: usize) -> String {
    // Still measured at `depth`: the prefix occupies those columns whether or
    // not this string contains them.
    let width = wrap::available_width(sink, depth);
    match message.split_once('\n') {
        Some((head, rest)) => format!("{}\n{}", wrap::clamp(head, width), rest),
        None => wrap::clamp(message, width),
    }
}

/// Indent a live bar by putting `depth`'s indent in its `{prefix}` field — the
/// ONE way any bar in this module is indented, and the reason a spinner, a
/// progress bar and a `LiveRow` cannot disagree about where the indent goes.
///
/// indicatif draws a bar's line at column 0 whatever else is on screen, so the
/// indent has to be part of what the bar paints. Every template below leads
/// with `{prefix}`, so the indent always lands ahead of the frame: a running
/// line sits in the same column its settled line will, rather than jumping into
/// the tree the moment it stops moving.
pub(super) fn set_bar_depth(bar: &IndProgressBar, depth: usize) {
    bar.set_prefix("  ".repeat(depth));
}

/// Live spinner. Drop without `finish_*()` emits a `Status(Info)` with the
/// spinner message at the active depth — leaves a record so the spinner
/// doesn't disappear silently. A `borrowed` spinner is the exception and ends
/// silently, because the line is not its to end (see the field's own doc).
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
    /// The bar belongs to a [`super::live_row::LiveRow`], which owns it for
    /// longer than the spinner does. Two consequences, both from that one fact:
    /// the row's style carries the indent in a `{prefix}` field, so a message
    /// must not repeat it; and the spinner never ends the bar — not on
    /// `Drop` either, which for an owned bar clears the line and leaves a
    /// `Status(Info)` record. On a borrowed bar that would retire the row its
    /// owner is still going to settle, and print a line for an action whose
    /// outcome is about to be written by the row itself.
    pub(crate) borrowed: bool,
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
    /// combined status line elsewhere — each lane's own spinner must vanish
    /// silently, or every lane would print its own line on top of the one
    /// summary line describing all of them.
    /// Suppresses `Drop`'s `Status(Info)`, the same way an explicit
    /// `finish_*` does.
    pub(crate) fn finish_silent(mut self) {
        self.bar.finish_and_clear();
        self.finished = true;
    }

    /// Give the bar back to whoever owns it, printing nothing.
    ///
    /// For a spinner drawn on a line it does not own — a
    /// [`super::live_row::LiveRow`]'s, which the caller goes on to settle in
    /// place. `finish_silent` would clear that line and retire the row's bar
    /// with it, leaving the row unable to say anything ever again.
    ///
    /// The named end of the same thing `Drop` does for a `borrowed` spinner:
    /// this is the call site saying it, `Drop` is the abandoned worker that
    /// never reached one.
    pub(crate) fn release(mut self) {
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
        // A borrowed bar is its owner's to end: the row settles the action's
        // one line, or the tree retires the row unsettled. Clearing it here
        // would leave the row unable to speak, and the record below would be a
        // second line for an action that already has one — which is what an
        // abandoned lane (a worker that panicked before `LaneHandle::finish`)
        // left in the scrollback.
        if self.finished || self.borrowed {
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
            depth,
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
    depth: usize,
    message: &str,
) -> (IndProgressBar, Option<LiveBarGuard>) {
    if !live_bars {
        (IndProgressBar::hidden(), None)
    } else {
        let (bar, live) = build_progress_bar(multi, renderer, total, depth, message);
        (bar, Some(live))
    }
}

/// Compose a bar template, with the depth `{prefix}` supplied here rather than
/// by the caller.
///
/// A caller names only what its bar draws AFTER the indent, so no template in
/// this module can put the indent anywhere but first, or omit it. That freedom
/// is what let a `LiveRow` indent ahead of its frame while a section spinner
/// indented behind one — two bars on the same tree, in two different columns,
/// with nothing in the types to say they disagreed.
fn indented_template(body: &str) -> String {
    format!("{{prefix}}{body}")
}

/// The animated frames a spinner cycles, painted by the theme. Shared with
/// [`super::live_row::LiveRow`], whose running state is the same animation on a
/// line it owns for longer than one step.
///
/// `body` is the template WITHOUT its leading `{prefix}` — see
/// [`indented_template`].
pub(super) fn spinner_style(renderer: &Renderer, body: &str) -> ProgressStyle {
    let frames_raw = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let styled: Vec<String> = frames_raw
        .iter()
        .map(|f| renderer.theme.info.apply_to(f).to_string())
        .collect();
    let mut tick_refs: Vec<&str> = styled.iter().map(|s| s.as_str()).collect();
    tick_refs.push(" ");
    ProgressStyle::with_template(&indented_template(body))
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_strings(&tick_refs)
}

/// The unanimated counterpart of [`spinner_style`], on the same indent
/// contract: a settled row, and any bar whose line is a static one.
pub(super) fn plain_style(body: &str) -> ProgressStyle {
    ProgressStyle::with_template(&indented_template(body))
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
}

/// How often a spinner redraws its animation.
pub(super) const SPINNER_TICK: Duration = Duration::from_millis(80);

/// Build a styled spinner ProgressBar attached to a MultiProgress.
pub(crate) fn build_spinner(
    multi: &indicatif::MultiProgress,
    renderer: &Arc<Renderer>,
    depth: usize,
    message: &str,
) -> (IndProgressBar, LiveBarGuard) {
    let pb = multi.add(IndProgressBar::new_spinner());
    let live = LiveBarGuard::acquire(renderer);
    pb.set_style(spinner_style(renderer, "{spinner} {msg}"));
    set_bar_depth(&pb, depth);
    pb.set_message(message.to_string());
    pb.enable_steady_tick(SPINNER_TICK);
    (pb, live)
}

pub(crate) fn build_progress_bar(
    multi: &indicatif::MultiProgress,
    renderer: &Arc<Renderer>,
    total: u64,
    depth: usize,
    message: &str,
) -> (IndProgressBar, LiveBarGuard) {
    let pb = multi.add(IndProgressBar::new(total));
    let live = LiveBarGuard::acquire(renderer);
    // indicatif resolves a `.cyan` template field against `console`'s own colour
    // flags, which no longer track the printer's decision — nothing writes them.
    // The template therefore has to carry that decision itself, or `--no-color`
    // renders unstyled text beside a still-green bar. The spinner frames above
    // need no such branch: they are styled through the theme, which already
    // holds it.
    let template = if renderer.theme.colors() {
        "{spinner:.cyan} [{bar:30.cyan/dim}] {pos}/{len} {msg}"
    } else {
        // The empty half keeps `dim` — that is an ATTRIBUTE, not a colour, and
        // `NO_COLOR` governs colour only, so dropping it made a colourless bar
        // lose the contrast between filled and unfilled that is the only thing
        // left telling them apart. Empty fill colour before the `/` is what
        // spends no colour on the filled half.
        "{spinner} [{bar:30./dim}] {pos}/{len} {msg}"
    };
    pb.set_style(
        ProgressStyle::with_template(&indented_template(template))
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("━╸─"),
    );
    set_bar_depth(&pb, depth);
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
            borrowed: false,
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
                borrowed: false,
                _phantom: std::marker::PhantomData,
            };
        }
        let out = strip_ansi(&buf.lock().unwrap());
        // Info role has no icon; subject text appears.
        assert!(out.contains("abandoned"), "got: {out:?}");
    }

    /// A `--no-color` run must not draw a green bar beside unstyled text.
    /// indicatif resolves a `.cyan` template field against `console`'s colour
    /// flags, which no printer writes any more, so the only thing that can keep
    /// the bar honest is the template carrying the printer's own decision.
    ///
    /// Both draws happen with `console`'s flags ON, because that IS the
    /// reported condition: `--no-color` on a colour terminal. indicatif
    /// resolves the template field against those flags, so with them off the
    /// negative assertion would hold for the wrong reason and prove nothing.
    ///
    /// The colourless bar is checked for COLOUR escapes, not for escapes: it
    /// keeps `dim` on its unfilled half, which is an attribute, and `NO_COLOR`
    /// governs colour only. Dropping the attribute too would leave a
    /// colourless bar with nothing at all separating filled from unfilled.
    #[cfg(feature = "test-helpers")]
    #[test]
    #[serial_test::serial]
    fn a_colourless_printer_draws_a_colourless_progress_bar() {
        use std::sync::{Arc, Mutex};

        let _terminal = crate::output::printer::ColorGlobalOn::set();

        fn draw(colors: bool) -> String {
            let drawn = Arc::new(Mutex::new(String::new()));
            let multi = indicatif::MultiProgress::with_draw_target(
                indicatif::ProgressDrawTarget::term_like(Box::new(
                    crate::output::test_capture::RecordingTerm {
                        drawn: drawn.clone(),
                    },
                )),
            );
            let renderer = Arc::new(Renderer::new(
                Theme::from_preset("dracula").with_colors(colors),
                Verbosity::Normal,
            ));
            let (bar, _live) = build_progress_bar(&multi, &renderer, 4, 0, "installing");
            bar.set_position(2);
            bar.tick();
            drawn.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        /// Whether `drawn` carries an SGR sequence that sets a colour, as
        /// opposed to one that sets an attribute like `dim` (`\x1b[2m`).
        fn has_color_sgr(drawn: &str) -> bool {
            drawn.split("\u{1b}[").skip(1).any(|seq| {
                let Some((params, 'm')) = seq
                    .find('m')
                    .map(|i| (&seq[..i], seq[i..].chars().next().unwrap_or('\0')))
                else {
                    return false;
                };
                params.split(';').filter_map(|p| p.parse::<u16>().ok()).any(
                    // 30-37/90-97 foreground, 40-47/100-107 background,
                    // 38/48 the 256-colour and truecolor selectors.
                    |n| {
                        (30..=38).contains(&n)
                            || (40..=48).contains(&n)
                            || (90..=97).contains(&n)
                            || (100..=107).contains(&n)
                    },
                )
            })
        }

        let off = draw(false);
        assert!(
            !has_color_sgr(&off),
            "a colourless printer drew colour: {off:?}"
        );
        assert!(off.contains("2/4"), "bar did not draw at all: {off:?}");
        assert!(
            off.contains("\u{1b}[2m"),
            "the colourless bar dropped `dim`, so its unfilled half is \
             indistinguishable from its filled half: {off:?}"
        );

        let on = draw(true);
        assert!(
            has_color_sgr(&on),
            "a colour printer drew no colour, so the assertion above proves \
             nothing: {on:?}"
        );
    }

    #[test]
    fn quiet_printer_returns_hidden_spinner() {
        use super::super::printer::Printer;
        let p = Printer::with_format(
            super::super::Verbosity::Quiet,
            None,
            super::super::OutputFormat::Table,
            super::super::ColorChoice::Auto,
        );
        let sp = p.spinner("x");
        assert!(sp.bar.is_hidden(), "Quiet should yield a hidden bar");
    }

    /// Every live bar a section opens paints its glyph in the SAME column, and
    /// in the same column the section's settled lines put theirs.
    ///
    /// The bug this pins: a spinner's indent used to live in its message,
    /// behind the animated frame, so a running step drew its frame at column 0
    /// and its text two columns right of where its own settled line would land
    /// — while a `LiveRow` on the same tree, indenting through its `{prefix}`,
    /// drew the frame in the right column all along. A progress bar had no
    /// indent at all. Three builders, three answers, on one tree.
    #[test]
    fn every_live_bar_in_a_section_paints_its_glyph_in_the_settled_glyph_column() {
        let (printer, screen) = super::super::Printer::for_test_live_terminal(24, 100);
        let section = printer.section("Packages");
        let sp = section.spinner("brew install fd");
        let pb = section.progress_bar(4, "downloading");
        pb.set_position(2);
        // A tick each, so both have painted a frame rather than only a message.
        std::thread::sleep(std::time::Duration::from_millis(120));
        let held = screen.contents();
        sp.finish_silent();
        pb.finish();
        drop(section);

        let indent = "  ";
        let rows: Vec<&str> = held
            .lines()
            .filter(|l| l.contains("brew install fd") || l.contains("downloading"))
            .collect();
        assert_eq!(rows.len(), 2, "expected both bars on screen: {held:?}");
        for row in rows {
            assert!(
                row.starts_with(indent) && !row.starts_with("   "),
                "bar is not in the section's glyph column: {row:?}"
            );
            let glyph = row.trim_start().chars().next().unwrap_or(' ');
            assert!(
                !glyph.is_ascii_alphanumeric(),
                "expected a glyph before the text, got {row:?}"
            );
        }
    }
}

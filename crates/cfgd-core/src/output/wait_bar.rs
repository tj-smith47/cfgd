//! `WaitBar` — the live-region line that says why something has not started.
//!
//! A concurrent phase writes its tree at phase close, so between the phase
//! heading and that tree there is nothing on screen describing work the
//! scheduler is holding back. A `WaitBar` fills exactly that gap: one
//! [`Role::Pending`] line per blocked owner group or per blocked action,
//! carrying the subject the scheduler computes.
//!
//! It exists in the live region and nowhere else. It never settles a status,
//! it emits nothing on drop, and it has no non-TTY form — a log line saying
//! "waiting" that nothing ever supersedes would be worse than silence. That is
//! also why its subject is never persisted and appears in no golden.
use std::marker::PhantomData;
use std::sync::Arc;

use indicatif::{ProgressBar as IndProgressBar, ProgressStyle};

use super::Role;
use super::renderer::{LiveBarGuard, Renderer, role_glyph};

/// One live-region wait line. Replace its subject with [`WaitBar::set_subject`]
/// as the thing being waited on changes; drop it when the wait ends.
pub(crate) struct WaitBar<'p> {
    bar: IndProgressBar,
    renderer: Arc<Renderer>,
    /// Depth the line sits at, as an indent it carries itself — indicatif
    /// draws every bar's message at column 0, and a wait line belongs in the
    /// same column as the action line that will replace it.
    indent: String,
    /// Held for the bar's lifetime so the renderer's live-bar count tracks it.
    /// `None` for a hidden bar, which is never added to the `MultiProgress`.
    _live: Option<LiveBarGuard>,
    _phantom: PhantomData<&'p ()>,
    /// A monotonic creation order, visible only to tests. `indicatif::ProgressBar::index`
    /// (the `MultiProgress` draw position this bar was actually assigned) is
    /// crate-private to indicatif and unreachable from here, so a determinism
    /// test asserting the TOP-TO-BOTTOM order new bars are drawn in has no
    /// other observable proxy for "the order `printer.wait_bar` was called
    /// in" — which is the exact thing F1 fixes.
    #[cfg(test)]
    seq: u64,
}

impl WaitBar<'_> {
    /// Replace what the line says it is waiting on.
    ///
    /// Replacement rather than a second bar: a group waits on one thing at a
    /// time, and stacking the chain would leave superseded claims on screen.
    pub(crate) fn set_subject(&self, subject: &str) {
        self.bar.set_message(self.compose(subject));
    }

    /// What the line currently says, without styling — the live region's own
    /// state, so a scheduler test can assert on the bars it produced rather
    /// than on the inputs it fed them.
    #[cfg(test)]
    pub(crate) fn subject(&self) -> String {
        super::strip_ansi(&self.bar.message())
    }

    /// This bar's creation order relative to every other `WaitBar` in the
    /// same test binary — see the field doc on `seq`.
    #[cfg(test)]
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    /// `<glyph> <subject>`, both from the theme. The glyph is never written at
    /// a call site: `minimal` renders it as a space and an ASCII preset renders
    /// it differently again, and a literal here would defeat both.
    fn compose(&self, subject: &str) -> String {
        let theme = &self.renderer.theme;
        let (icon, style) = role_glyph(theme, Role::Pending);
        let body = theme.muted.apply_to(subject).to_string();
        match icon {
            Some(icon) => format!("{}{} {}", self.indent, style.apply_to(icon), body),
            None => format!("{}{}", self.indent, body),
        }
    }
}

impl Drop for WaitBar<'_> {
    /// Clears, never settles. A wait line describes a state that has ended by
    /// the time the bar goes away, so leaving a record of it — the way an
    /// abandoned `Spinner` deliberately does — would put a stale claim in the
    /// scrollback that nothing below it contradicts.
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}

impl super::Printer {
    /// Open a wait line in the live region.
    ///
    /// The subject is the scheduler's whole sentence (`profile:work · waiting
    /// on modules`); this owns only the glyph, the indent and the styling. Off
    /// a TTY, or under `Verbosity::Quiet`, the returned bar is hidden and every
    /// call on it is inert.
    ///
    /// `depth` is the depth the action line this wait line stands in for will
    /// render at, so the two occupy the same column.
    #[must_use]
    pub(crate) fn wait_bar(&self, depth: usize, subject: &str) -> WaitBar<'_> {
        let (bar, live) = if self.live_bars() {
            let bar = self.multi_progress.add(IndProgressBar::new_spinner());
            // No `{spinner}`: a wait line is not progress, and an animated
            // frame beside a static "waiting on" reads as work happening.
            bar.set_style(
                ProgressStyle::with_template("{msg}")
                    .unwrap_or_else(|_| ProgressStyle::default_spinner()),
            );
            (bar, Some(LiveBarGuard::acquire(&self.renderer)))
        } else {
            (IndProgressBar::hidden(), None)
        };
        let wait = WaitBar {
            bar,
            renderer: self.renderer.clone(),
            indent: "  ".repeat(depth),
            _live: live,
            _phantom: PhantomData,
            #[cfg(test)]
            seq: {
                use std::sync::atomic::{AtomicU64, Ordering};
                static WAIT_BAR_SEQ: AtomicU64 = AtomicU64::new(0);
                WAIT_BAR_SEQ.fetch_add(1, Ordering::SeqCst)
            },
        };
        wait.set_subject(subject);
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::super::{OutputFormat, Printer, Verbosity};

    #[test]
    fn off_a_tty_the_wait_bar_is_hidden_and_writes_nothing() {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        let bar = printer.wait_bar(0, "profile:work · waiting on modules");
        bar.set_subject("profile:work · waiting on bootstraps");
        assert!(bar.bar.is_hidden(), "a wait line has no non-TTY form");
        drop(bar);
        assert!(
            buf.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
            "a hidden wait line writes nothing, on any call and on drop"
        );
    }

    #[test]
    fn the_line_carries_the_indent_of_the_action_it_stands_in_for() {
        // indicatif draws a bar's message at column 0, so a wait line that did
        // not carry its own indent would sit outside the phase tree it
        // describes and jump into it the moment the action started.
        let (printer, _buf) = Printer::for_test_at(Verbosity::Normal);
        let bar = printer.wait_bar(2, "cfgd:managers · waiting on brew");
        assert!(
            bar.subject().starts_with("    "),
            "got: {:?}",
            bar.subject()
        );
        assert!(bar.subject().ends_with("cfgd:managers · waiting on brew"));
    }

    #[test]
    fn the_glyph_comes_from_the_theme_not_the_call_site() {
        // `minimal` renders the pending icon as a space, so a call site that
        // hardcoded `○` would show one here.
        let printer = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Table,
            crate::output::ColorChoice::Auto,
        );
        let bar = printer.wait_bar(0, "x");
        let composed = bar.compose("profile:work · waiting on modules");
        let icon = super::super::Theme::default().icon_pending;
        assert!(
            crate::output::strip_ansi(&composed).starts_with(&format!("{icon} ")),
            "got: {composed:?}"
        );
    }
}

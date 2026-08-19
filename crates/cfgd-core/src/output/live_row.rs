//! `LiveRow` — one line of the live region, owned for its whole life by the
//! caller that put it there.
//!
//! Every other live-region primitive owns its line for one purpose and then
//! retires it: a [`super::Spinner`] collapses into the status it settles, an
//! [`OutputWindow`] into the line its caller writes. A row inverts that
//! arrangement — the CALLER owns the line and rewrites what it says, so one row
//! can read pending, then running with its child's output beneath it, then
//! settled, without the line ever moving.
//!
//! That is the whole point. A concurrent phase draws its tree as rows appended
//! in the order the scheduler reaches them; because a finishing action rewrites
//! the row it already occupies instead of retiring it and printing somewhere
//! else, an action that finishes early cannot appear above one that started
//! before it and is still running.
//!
//! Two details make a row's three states line up in the same column:
//!
//! - The indent lives in the bar's `{prefix}` field rather than in its message,
//!   so the running state's animated frame occupies the same column the settled
//!   state's `✓` does. indicatif draws every bar's message at column 0, and a
//!   message that carried the indent itself would push the glyph two columns
//!   right of where the committed line puts it — the row would jump sideways at
//!   the moment it settled.
//! - The settled text is composed through [`super::renderer::Renderer::compose_status`]
//!   against the same live column the section will pad to, so the row and the
//!   permanent line that replaces it are the same bytes.
use std::marker::PhantomData;
use std::sync::Arc;

use indicatif::{ProgressBar as IndProgressBar, ProgressStyle};

use super::renderer::{
    LiveBarGuard, Renderer, StatusFields, Writer, finalize_subject, indent_prefix, wrap,
};
use super::spinner::Spinner;
use super::window::OutputWindow;

/// How close to the bottom of the terminal a live region may grow before
/// indicatif stops drawing the rows at its foot. Two lines for the shell's own
/// prompt and one for the line the region is drawn under.
const LIVE_REGION_HEADROOM: usize = 3;

/// One action line of the phase tree, as the caller that owns the row holds
/// it: the same four fields [`super::SectionGuard::action_status`] takes,
/// because the row and the line that eventually replaces it are one status
/// rendered twice.
pub(crate) struct RowStatus<'a> {
    pub(crate) role: super::Role,
    pub(crate) subject: &'a str,
    pub(crate) detail: Option<&'a str>,
    /// The detail states what the plan expected rather than what happened, so
    /// it renders muted — the same distinction `detail_muted_opt` draws.
    pub(crate) detail_muted: bool,
    pub(crate) duration: Option<std::time::Duration>,
}

/// One line of the live region. Build with [`super::Printer::live_row_at`],
/// rewrite with [`LiveRow::set_status`] / [`LiveRow::window`], remove with
/// [`LiveRow::retire`].
///
/// Off a TTY (or under `Verbosity::Quiet`) the bar is hidden and every call is
/// inert, exactly as a `Spinner`'s is — a caller that must produce output there
/// writes it through the printer instead, and does not branch here.
pub(crate) struct LiveRow<'p> {
    bar: IndProgressBar,
    multi: indicatif::MultiProgress,
    renderer: Arc<Renderer>,
    sink: Arc<dyn Writer>,
    /// The depth the row's own line renders at — its indent, and the width its
    /// text is clamped to.
    depth: usize,
    /// Held for the bar's lifetime so the renderer's live-bar count tracks it.
    /// `None` for a hidden bar, which is never added to the `MultiProgress`.
    _live: Option<LiveBarGuard>,
    _phantom: PhantomData<&'p ()>,
}

impl<'p> LiveRow<'p> {
    /// Draw this row as a settled — or pending — status line.
    ///
    /// `column` is the phase's alignment column, applied through the same
    /// derivation the section applies when it writes the line permanently, so a
    /// row cannot settle into one shape and commit as another.
    pub(crate) fn set_status(&self, fields: &StatusFields<'_>, column: usize) {
        if self.bar.is_hidden() {
            return;
        }
        let padded =
            self.renderer
                .padded_for_column(self.sink.as_ref(), self.depth, fields, column);
        let (line, tails) = match &padded {
            Some(subject) => self.renderer.compose_status(&StatusFields {
                role: fields.role,
                subject,
                detail: fields.detail,
                duration: fields.duration,
                target: fields.target,
                subject_style: fields.subject_style.clone(),
                detail_style: fields.detail_style.clone(),
            }),
            None => self.renderer.compose_status(fields),
        };
        // The row is repainted in place, so its own line cannot reach a second
        // row and is clamped — at the COMPLETE-line budget the alignment
        // ceiling padded it to, not at the narrower wrapped-body width. The
        // continuation lines below it are separate rows of the same repaint
        // and are clamped at their own indent.
        let width = wrap::line_width(self.sink.as_ref(), self.depth);
        let indent = indent_prefix(self.depth + 1);
        let mut message = wrap::clamp(&line, width);
        for tail in &tails {
            message.push('\n');
            message.push_str(&indent);
            message.push_str(&wrap::clamp(
                tail,
                wrap::line_width(self.sink.as_ref(), self.depth + 1),
            ));
        }
        self.bar.disable_steady_tick();
        self.bar.set_style(plain_style());
        self.bar.set_message(message);
    }

    /// Draw this row as running, and hand back the window its child's output
    /// tails into — the same bounded view a sequential step gets, on the line
    /// this row already occupies.
    ///
    /// The returned window does NOT retire the bar when it closes: the row
    /// outlives it and settles the line itself.
    pub(crate) fn window(&self, subject: impl Into<String>) -> OutputWindow<'p> {
        let subject = subject.into();
        if !self.bar.is_hidden() {
            self.bar.set_style(super::spinner::spinner_style(
                &self.renderer,
                "{spinner} {msg}",
            ));
            self.bar.enable_steady_tick(super::spinner::SPINNER_TICK);
        }
        let mut spinner = Spinner {
            renderer: self.renderer.clone(),
            sink: self.sink.clone(),
            depth: self.depth,
            bar: self.bar.clone(),
            message: subject.clone(),
            finished: false,
            // The row's own guard already counts this bar; a second one would
            // leave the renderer routing lines through a MultiProgress that no
            // longer has anything in it.
            _live: None,
            borrowed: true,
            _phantom: PhantomData,
        };
        spinner.set_message(subject.clone());
        OutputWindow::borrowed(spinner, subject)
    }

    /// Draw this row as one action line of the phase tree — the subject
    /// painted `theme.primary`, exactly as
    /// [`super::SectionGuard::action_status`] paints it.
    pub(crate) fn set_action_status(&self, status: &RowStatus<'_>, column: usize) {
        if self.bar.is_hidden() {
            return;
        }
        let theme = &self.renderer.theme;
        let subject = finalize_subject(theme, status.subject, None, None);
        self.set_status(
            &StatusFields {
                role: status.role,
                subject: &subject,
                detail: status.detail,
                duration: status.duration,
                target: None,
                subject_style: theme.primary.clone(),
                detail_style: (status.detail_muted && status.detail.is_some())
                    .then(|| theme.muted.clone()),
            },
            column,
        );
    }

    /// Draw this row as an owner group's heading — the same styled token the
    /// section writes when the group's first permanent line commits, so the
    /// heading does not change shape when the live region hands it over.
    pub(crate) fn set_owner_label(&self, label: &super::OwnerLabel) {
        if self.bar.is_hidden() {
            return;
        }
        let width = wrap::available_width(self.sink.as_ref(), self.depth);
        self.bar.disable_steady_tick();
        self.bar.set_style(plain_style());
        self.bar
            .set_message(wrap::clamp(&label.styled(&self.renderer.theme), width));
    }

    /// Draw this row as a NOTE about the region rather than a step of it: a
    /// leading ellipsis and muted text.
    ///
    /// The one line a live region writes about itself — what it is holding back
    /// because the terminal has no room for it. It carries no status glyph on
    /// purpose: a `✓` or `○` would read as an action of the phase, and this
    /// line stands for several of them.
    pub(crate) fn set_note(&self, text: &str) {
        if self.bar.is_hidden() {
            return;
        }
        let width = wrap::available_width(self.sink.as_ref(), self.depth);
        let body = self
            .renderer
            .theme
            .muted
            .apply_to(format!("… {text}"))
            .to_string();
        self.bar.disable_steady_tick();
        self.bar.set_style(plain_style());
        self.bar.set_message(wrap::clamp(&body, width));
    }

    /// Take this row out of the live region. Called once the line has been
    /// committed to the scrollback permanently, or once the work it described
    /// will never happen.
    ///
    /// The erase itself is [`Drop`]'s, so a row that is dropped instead of
    /// retired takes its line with it too.
    pub(crate) fn retire(self) {
        drop(self);
    }

    /// What the row currently says, unstyled — the live region's own state, so
    /// a scheduler test can assert on the rows it produced rather than on the
    /// inputs it fed them.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        super::strip_ansi(&format!("{}{}", self.bar.prefix(), self.bar.message()))
    }
}

/// Erasing the row's line is the DROP, not the `retire` call, so a row cannot
/// leave a line behind by ending any other way.
///
/// The order of the two statements is the whole of it. `MultiProgress::remove`
/// points the bar's draw target at nowhere, so a `finish_and_clear` issued
/// after it paints nothing at all: the line the region last drew for this row
/// stays on the terminal until some LATER draw of the region happens to clear
/// it. Every row but one gets that later draw — the next committed line
/// repaints the region around itself — and the last row of a phase does not.
/// The region empties, `emit_block` stops routing through a multi with no bars
/// left in it, and everything after is written straight to the terminal BELOW
/// the row's final paint, which is how one action came to stand in the
/// scrollback twice: once as the committed line, once as a live paint,
/// truncated to the terminal's width the way only a live paint is.
///
/// Cleared first and removed second, the clear draws while the bar is still a
/// member, the region redraws one row shorter, and nothing survives the row.
impl Drop for LiveRow<'_> {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
        self.multi.remove(&self.bar);
    }
}

/// The style of a row that is not running: its indent, then its line. No
/// `{spinner}` field — a static line beside an animated frame reads as work
/// still happening.
fn plain_style() -> ProgressStyle {
    super::spinner::plain_style("{msg}")
}

/// Where a new row joins the live region.
enum RowSlot<'a> {
    /// The bottom, under everything already there.
    Foot,
    /// The top, above everything already there.
    First,
    /// Directly below an existing row.
    After(&'a LiveRow<'a>),
}

impl super::Printer {
    /// Append a row to the bottom of the live region, at `depth`.
    #[must_use]
    pub(crate) fn live_row_at(&self, depth: usize) -> LiveRow<'_> {
        self.build_live_row(depth, RowSlot::Foot)
    }

    /// Insert a row at the TOP of the live region, at `depth`.
    ///
    /// For a line that is about the region rather than about one step of it:
    /// the top is the only position no later append pushes down, and an
    /// over-full region truncates from the FOOT, so it is also the only
    /// position that cannot be the line the terminal drops.
    #[must_use]
    pub(crate) fn live_row_first(&self, depth: usize) -> LiveRow<'_> {
        self.build_live_row(depth, RowSlot::First)
    }

    /// Insert a row directly below `after`, at `depth`.
    ///
    /// For a tree whose groups are not contiguous in dispatch order: an action
    /// joining a group that already has rows belongs under that group, not at
    /// the foot of the region under someone else's heading.
    #[must_use]
    pub(crate) fn live_row_after(&self, depth: usize, after: &LiveRow<'_>) -> LiveRow<'_> {
        self.build_live_row(depth, RowSlot::After(after))
    }

    /// How many rows the live region can hold before indicatif stops drawing
    /// the ones at its foot.
    ///
    /// indicatif draws a `MultiProgress` top down and stops at the terminal's
    /// last row, so an over-full region hides the work that started most
    /// recently — the opposite of what a reader needs. A caller that can choose
    /// NOT to draw a row (one describing work that has not started) asks here
    /// first. `0` when there is no live region at all.
    pub(crate) fn live_row_budget(&self) -> usize {
        if !self.live_bars() {
            return 0;
        }
        let (rows, _) = console::Term::stderr().size();
        usize::from(rows).saturating_sub(LIVE_REGION_HEADROOM)
    }

    fn build_live_row(&self, depth: usize, slot: RowSlot<'_>) -> LiveRow<'_> {
        let (bar, live) = if self.live_bars() {
            let fresh = IndProgressBar::new_spinner();
            let bar = match slot {
                RowSlot::After(row) => self.multi_progress.insert_after(&row.bar, fresh),
                RowSlot::First => self.multi_progress.insert(0, fresh),
                RowSlot::Foot => self.multi_progress.add(fresh),
            };
            super::spinner::set_bar_depth(&bar, depth);
            bar.set_style(plain_style());
            (bar, Some(LiveBarGuard::acquire(&self.renderer)))
        } else {
            (IndProgressBar::hidden(), None)
        };
        LiveRow {
            bar,
            multi: self.multi_progress.clone(),
            renderer: self.renderer.clone(),
            sink: self.sink_stderr.clone(),
            depth,
            _live: live,
            _phantom: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Printer, Role, Verbosity};
    use super::*;

    fn fields<'a>(role: Role, subject: &'a str) -> StatusFields<'a> {
        StatusFields {
            role,
            subject,
            detail: None,
            duration: None,
            target: None,
            subject_style: None,
            detail_style: None,
        }
    }

    #[test]
    fn off_a_tty_a_row_is_hidden_and_writes_nothing() {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        let row = printer.live_row_at(2);
        row.set_status(&fields(Role::Ok, "install ripgrep"), 30);
        assert!(row.bar.is_hidden(), "a row has no non-TTY form");
        row.retire();
        assert!(
            crate::test_helpers::captured_text(&buf).is_empty(),
            "a hidden row writes nothing, on any call and on retire"
        );
    }

    #[test]
    fn a_row_keeps_its_indent_in_the_prefix_not_its_message() {
        // indicatif draws a bar's message at column 0. The indent has to reach
        // the terminal some other way, or the running row's glyph sits two
        // columns left of the settled line's and the row shifts sideways the
        // moment it settles.
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let row = printer.live_row_at(2);
        row.set_status(&fields(Role::Ok, "install ripgrep"), 0);
        assert_eq!(row.bar.prefix(), "    ");
        assert!(
            !super::super::strip_ansi(&row.bar.message()).starts_with(' '),
            "the message carries a second copy of the indent: {:?}",
            row.bar.message()
        );
        assert_eq!(row.text(), "    ✓ install ripgrep");
    }

    #[test]
    fn a_row_settles_in_place_over_its_own_running_state() {
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let row = printer.live_row_at(2);
        let window = row.window("install ripgrep");
        assert!(
            row.text().contains("install ripgrep"),
            "got: {:?}",
            row.text()
        );
        window.release();
        row.set_status(&fields(Role::Ok, "install ripgrep"), 0);
        assert_eq!(
            row.text(),
            "    ✓ install ripgrep",
            "the settled line must replace the running one on the same row"
        );
    }

    #[test]
    fn a_released_window_leaves_the_row_able_to_speak() {
        // `finish_silent` would clear the line and retire the bar with it, so
        // the settle that follows would paint a row nothing draws.
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let row = printer.live_row_at(1);
        let mut window = row.window("apt-get install tmux");
        window.push_line("Reading package lists...");
        window.release();
        row.set_status(&fields(Role::Ok, "apt-get install tmux"), 0);
        assert!(!row.bar.is_finished(), "the row's bar was retired early");
        assert_eq!(row.text(), "  ✓ apt-get install tmux");
    }

    #[test]
    fn an_abandoned_window_leaves_the_row_able_to_speak_and_records_nothing() {
        // A lane worker that panics drops its handle without releasing the
        // window. The line is the ROW's, and an owned spinner's Drop would
        // clear it and settle a `Role::Skipped` "(interrupted)" line — retiring a row its owner
        // is still going to settle, and printing a second line for an action
        // whose outcome that settle is about to write.
        let (printer, buf) = Printer::for_test_with_live_bars();
        let row = printer.live_row_at(1);
        {
            let mut window = row.window("pipx install pynvim");
            window.push_line("installed package pynvim");
        }
        row.set_status(&fields(Role::Ok, "pipx install pynvim"), 0);
        assert!(
            !row.bar.is_finished(),
            "the abandoned window retired the row's bar"
        );
        assert_eq!(row.text(), "  ✓ pipx install pynvim");
        let drawn = crate::test_helpers::captured_text(&buf);
        assert!(
            !drawn.contains('⊙'),
            "the abandoned window left a record of its own: {drawn:?}"
        );
    }

    #[test]
    fn a_row_inserted_after_another_lands_between_it_and_the_rest() {
        // The observable is indicatif's own draw order, which is why the
        // recording draw target is the fixture: `MultiProgress` exposes no
        // index, so the only proof an insert landed where it was asked to is
        // what the terminal was told to draw.
        let (printer, buf) = Printer::for_test_with_live_bars();
        let first = printer.live_row_at(1);
        let last = printer.live_row_at(1);
        let middle = printer.live_row_after(1, &first);
        first.set_status(&fields(Role::Ok, "first"), 0);
        middle.set_status(&fields(Role::Ok, "middle"), 0);
        last.set_status(&fields(Role::Ok, "last"), 0);
        let drawn = crate::test_helpers::captured_text(&buf);
        // The last paint of each row, since every redraw appends the whole
        // region to the recording again.
        let positions: Vec<usize> = ["first", "middle", "last"]
            .into_iter()
            .map(|word| {
                drawn
                    .rfind(word)
                    .unwrap_or_else(|| panic!("{word} was never drawn: {drawn:?}"))
            })
            .collect();
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "rows drawn out of order: {drawn:?}"
        );
    }

    #[test]
    fn the_budget_is_zero_without_a_live_region() {
        let (printer, _buf) = Printer::for_test_at(Verbosity::Normal);
        assert_eq!(printer.live_row_budget(), 0);
    }
}

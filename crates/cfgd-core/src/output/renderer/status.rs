use std::path::Path;
use std::time::Duration;

use super::{Emitting, Renderer, Writer, role_glyph};
use crate::PathDisplayExt;
use crate::output::theme::ThemedStyle;
use crate::output::{Role, Theme, Verbosity, cursor_safe};

/// Inputs to a single Status line. Builders convert to this for rendering.
pub struct StatusFields<'a> {
    pub role: Role,
    pub subject: &'a str,
    pub detail: Option<&'a str>,
    pub duration: Option<Elapsed>,
    pub target: Option<&'a Path>,
    /// Style for the SUBJECT only; the glyph always keeps the role's style.
    /// `None` = the subject is painted with the role style.
    pub subject_style: Option<ThemedStyle>,
    /// Style for the DETAIL only. `None` = the detail is written unstyled, in
    /// the terminal's default foreground.
    pub detail_style: Option<ThemedStyle>,
}

/// Where a status emission lands: into the innermost section's close-time
/// buffer, straight out against a pre-computed column, or straight out at the
/// caller's depth.
enum StatusRoute {
    Buffered,
    Live(usize),
    Immediate,
}

/// What a status line's duration slot measures, beside how long it took.
///
/// A ROW's span is its own: the time that one action, spinner or command
/// window ran. A run's closing total is WALL time — what the clock said —
/// and the two are not the same quantity once anything ran in parallel:
/// package lanes dispatched per manager sum to more than the run took, and a
/// reader who adds a column of row spans against an unlabelled total reads
/// the arithmetic as the numbers not being trustworthy. The total says which
/// it is (` (278.2s wall)`); a row never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed {
    pub span: Duration,
    /// Wall-clock, over work that may have overlapped.
    pub wall: bool,
}

impl Elapsed {
    /// One row's own span.
    pub fn row(span: Duration) -> Self {
        Self { span, wall: false }
    }

    /// A run's wall-clock total.
    pub fn wall(span: Duration) -> Self {
        Self { span, wall: true }
    }
}

/// True when a status carries content after its subject, which is the only
/// case either alignment path pads for. THE rule: `StatusFields` and
/// `BufferedStatus` both answer through this, so the live column and the
/// buffered close can never pad different sets of lines.
pub(crate) fn has_trailing(
    detail: Option<&str>,
    duration: Option<Elapsed>,
    target: Option<&Path>,
) -> bool {
    detail.is_some() || duration.is_some() || target.is_some()
}

impl StatusFields<'_> {
    pub(crate) fn has_trailing(&self) -> bool {
        has_trailing(self.detail, self.duration, self.target)
    }
}

/// Right-pad `subject` to `width`, or `None` when this status is one the
/// buffered path would have left alone. Shared by both alignment paths so a
/// live column pads exactly the lines a section close would have.
pub(crate) fn pad_subject(subject: &str, width: usize, has_trailing: bool) -> Option<String> {
    if !has_trailing || width == 0 {
        return None;
    }
    let cur = console::measure_text_width(subject);
    (cur < width).then(|| format!("{}{}", subject, " ".repeat(width - cur)))
}

/// Columns a line rendered at `depth` may occupy before a sink that wraps at
/// `wrap_cols` hard-wraps it. `None` for a sink that never does — a capture
/// buffer or a redirected stream keeps the physical lines the renderer
/// emitted, so no amount of padding can strand anything there.
fn wrap_budget(wrap_cols: Option<usize>, depth: usize) -> Option<usize> {
    wrap_cols.map(|cols| super::wrap::line_budget(cols, depth))
}

/// Build a status line and its continuation tails. Reads the theme only, so
/// every emission route composes the same bytes and none of them needs the
/// state lock to do it.
pub(crate) fn compose_status(theme: &Theme, f: &StatusFields<'_>) -> (String, Vec<String>) {
    let (icon_opt, style) = role_glyph(theme, f.role);
    let mut line = String::new();
    if let Some(icon) = icon_opt {
        line.push_str(&style.apply_to(icon).to_string());
        line.push(' ');
    }
    // The glyph keeps the role's style whatever the subject takes.
    let subject_style = f.subject_style.as_ref().unwrap_or(&style);
    line.push_str(&subject_style.apply_to(f.subject).to_string());

    // Detail may carry multi-line external tool stderr (e.g. a failed
    // `cargo install` dumps a whole error chain). Pre-split it like
    // render_note: the first physical line glues to the subject with the
    // em-dash; any further lines render as indented continuation lines at
    // the same depth. write_line forbids embedded newlines, so passing an
    // unsplit multi-line detail would panic in debug builds.
    let mut detail_tail: Vec<String> = Vec::new();
    if let Some(detail) = f.detail {
        // Sanitize at the renderer boundary: detail may carry embedded ANSI
        // escapes and bare control bytes. A stray `\x1b[0m` would
        // prematurely terminate the role styling above; foreign color
        // escapes would paint subsequent terminal output until the next
        // reset; a `\r` would repaint the line it lands on.
        let clean = cursor_safe(detail);
        let mut lines = clean.lines();
        line.push_str(" — ");
        // Continuation lines take the same style, so a wrapped detail
        // cannot change colour halfway down.
        let paint = |text: &str| match &f.detail_style {
            Some(style) => style.apply_to(text).to_string(),
            None => text.to_string(),
        };
        if let Some(first) = lines.next() {
            line.push_str(&paint(first));
        }
        detail_tail.extend(lines.map(paint));
    }
    if let Some(target) = f.target {
        // A path is as caller-supplied as the subject beside it, and the
        // parentheses are the renderer's — fold before they wrap it.
        let dim = theme
            .muted
            .apply_to(format!(" ({})", cursor_safe(&target.posix().to_string())));
        line.push_str(&dim.to_string());
    }
    if let Some(d) = f.duration {
        line.push_str(&duration_trailer(theme, d));
    }
    (line, detail_tail)
}

/// The styled ` (12.1s)` suffix [`compose_status`] appends when `f.duration`
/// is `Some` — the ONE formatting of it, so the full single-string
/// composition (read by [`affordable_column`] and `live_row.rs`'s
/// single-line live-paint clamp, which never wraps) and the split
/// composition below (read by the wrapped multi-line commit path) can never
/// render different bytes for the same duration.
///
/// Every duration slot on screen composes here — an action row's suffix, a
/// spinner's settled line, the run rollup's total — so the floor below is the
/// one place any of them can acquire it.
fn duration_trailer(theme: &Theme, e: Elapsed) -> String {
    theme.muted.apply_to(duration_text(e)).to_string()
}

/// ` (12.1s)`, and ` (<0.1s)` for anything the one-decimal form would round to
/// zero; a wall-clock total closes on ` wall` (` (278.2s wall)`), the one word
/// that tells a reader adding up the rows above it why they do not sum to it.
///
/// "Took zero seconds" is the one thing the column can say that is certainly
/// false, and it says it beside real measurements — a reader comparing rows
/// reads it as a missing measurement rather than a fast one. The floor keeps
/// the slot true at every magnitude: below the display resolution the column
/// reports the resolution, not the value.
fn duration_text(e: Elapsed) -> String {
    let unit = if e.wall { "s wall" } else { "s" };
    let secs = e.span.as_secs_f64();
    if secs < 0.05 {
        return format!(" (<0.1{unit})");
    }
    format!(" ({secs:.1}{unit})")
}

/// Same composition as [`compose_status`], with the duration trailer held out
/// of the returned line instead of appended to it.
///
/// The permanent-commit path needs the duration separated from the rest of
/// the line so a wrapped subject can anchor it to the shared duration column
/// on its LAST physical line (`wrap::wrap_body_with_trailer`) instead of
/// letting it fall wherever the word-wrap of the full composed string happens
/// to land it.
pub(crate) fn compose_status_split(
    theme: &Theme,
    f: &StatusFields<'_>,
) -> (String, Option<String>, Vec<String>) {
    let (line, tail) = compose_status(
        theme,
        &StatusFields {
            role: f.role,
            subject: f.subject,
            detail: f.detail,
            duration: None,
            target: f.target,
            subject_style: f.subject_style.clone(),
            detail_style: f.detail_style.clone(),
        },
    );
    let trailer = f.duration.map(|d| duration_trailer(theme, d));
    (line, trailer, tail)
}

/// The glyph and the space after it, which open every row a group renders.
/// Counted rather than measured off a composed line: the decision below is the
/// GROUP's, taken once, and a per-row measurement is what made it a per-row
/// decision.
const GLYPH_PREFIX_WIDTH: usize = 2;

/// The column every row of one group pads to at `depth` — the requested
/// column, or none at all.
///
/// Alignment is a property of the SET, not of a line: a row that could not
/// afford the shared column used to render unpadded beside siblings that
/// could, so three rows under one owner settled at three different detail
/// columns and the block read as a broken column rather than as no column.
/// Judged on the padded subject alone, which is the one quantity every row in
/// a group shares — a per-row cap read off each line's own detail length is
/// exactly the measurement that split the group.
pub(crate) fn group_column(
    wrap_cols: Option<usize>,
    depth: usize,
    column: usize,
    trailing: usize,
) -> usize {
    let Some(budget) = wrap_budget(wrap_cols, depth) else {
        return column;
    };
    if column + trailing <= budget {
        column
    } else {
        0
    }
}

/// What a group has to leave beside the padded subject, measured over the
/// whole set: the widest of its rows' non-subject content — the glyph, the
/// first line of the detail, the target, the duration.
///
/// Measured off each composed line rather than re-derived, so no format string
/// here can disagree with the one that builds it, and taken as a MAXIMUM
/// because the answer is the group's: a set whose widest row cannot fit beside
/// the column is a set with no column.
pub(crate) fn group_trailing_allowance<'f>(
    theme: &Theme,
    rows: impl Iterator<Item = StatusFields<'f>>,
) -> usize {
    rows.map(|f| {
        let (line, _) = compose_status(theme, &f);
        console::measure_text_width(&line).saturating_sub(console::measure_text_width(f.subject))
    })
    .max()
    .unwrap_or(GLYPH_PREFIX_WIDTH)
}

/// `f`'s subject padded to the column it renders against, or `None` when this
/// status is one neither alignment path pads.
///
/// The ONE derivation of that decision, because a live-region row draws the
/// same status line twice: once as a bar the caller repaints in place, and
/// once as the permanent line committed when the row leaves the region. A row
/// padded differently from the line that replaces it shifts sideways at the
/// moment it settles.
pub(crate) fn padded_for_column(
    wrap_cols: Option<usize>,
    depth: usize,
    f: &StatusFields<'_>,
    column: usize,
) -> Option<String> {
    let width = live_group_column(wrap_cols, depth, column);
    pad_subject(f.subject, width, f.has_trailing())
}

/// The column a LIVE group's rows settle against — the requested one, or none.
///
/// A live group's rows arrive one at a time, so the only allowance knowable
/// when its column is set is the glyph every row opens with. Reading a row's
/// own trailing content instead would hand each row a different answer, which
/// is the split [`padded_for_column`] exists to prevent; the commit route
/// reads the same answer so a wrapped row's trailer lands where the padded
/// subjects do.
pub(crate) fn live_group_column(wrap_cols: Option<usize>, depth: usize, column: usize) -> usize {
    group_column(wrap_cols, depth, column, GLYPH_PREFIX_WIDTH)
}

impl Emitting<'_> {
    /// Route one status emission: into the innermost open section's
    /// pending-statuses buffer (so subjects can be right-padded to a common
    /// column once the set is known), out now against a pre-computed live
    /// column, or out now at the caller's depth.
    pub(crate) fn route_status(&mut self, depth: usize, f: &StatusFields<'_>) {
        // Buffer when a section is open AND this status's depth is inside
        // (not equal to) the section's header_depth. The depth==header_depth
        // case happens for re-routed top-level emits via
        // `enforce_structural_top_level`; those should render immediately so
        // the warning shape stays inline.
        let route = match self.state.section_stack.last() {
            Some(top) if depth > top.header_depth => match top.live_column {
                // A live frame renders now and pads against a column that was
                // computed before the run, since there is no close to buffer
                // until.
                Some(width) => StatusRoute::Live(width),
                None => StatusRoute::Buffered,
            },
            _ => StatusRoute::Immediate,
        };
        match route {
            StatusRoute::Buffered => {
                // Headers first, and before the status is buffered: the header
                // emission goes through `push_line`, which drains, so a status
                // already in the buffer would be drained out above its own
                // header — and the drain the header triggers is also what
                // keeps a kv block written before the section from rendering
                // under the section's heading.
                self.flush_section_headers();
                // Rows still buffered were written BEFORE this status. The two
                // buffers drain independently, so leaving both loaded is the
                // one shape in which their relative order is no longer
                // recoverable — emptying here keeps at most one of them live.
                if !self.state.kv_buffer.is_empty() {
                    self.drain_buffers();
                }
                if let Some(top) = self.state.section_stack.last_mut() {
                    top.pending_statuses.push(super::section::BufferedStatus {
                        role: f.role,
                        subject: f.subject.to_string(),
                        detail: f.detail.map(|d| d.to_string()),
                        duration: f.duration,
                        target: f.target.map(|p| p.to_path_buf()),
                        depth,
                        subject_style: f.subject_style.clone(),
                        detail_style: f.detail_style.clone(),
                    });
                }
            }
            StatusRoute::Live(width) => {
                let column = live_group_column(self.wrap_cols, depth, width);
                let padded = pad_subject(f.subject, column, f.has_trailing());
                self.flush_section_headers();
                self.drain_buffers();
                match padded {
                    Some(subject) => self.emit_status_line(
                        depth,
                        &StatusFields {
                            role: f.role,
                            subject: &subject,
                            detail: f.detail,
                            duration: f.duration,
                            target: f.target,
                            subject_style: f.subject_style.clone(),
                            detail_style: f.detail_style.clone(),
                        },
                        column,
                    ),
                    None => self.emit_status_line(depth, f, column),
                }
            }
            StatusRoute::Immediate => {
                self.open_top_group(super::TopGroup::Status);
                self.flush_section_headers();
                self.drain_buffers();
                self.emit_status_line(depth, f, 0);
                self.mark_top_level_group(super::TopGroup::Status);
            }
        }
    }

    /// Collect one composed status line and its continuation tails.
    ///
    /// `column` is the group's settled alignment column — what
    /// [`group_column`] answered for the set this row belongs to, `0` for a
    /// row outside any aligned group — and is where a WRAPPED row lands its
    /// duration. An unwrapped row never reads it: its subject was already
    /// padded to that column by `pad_subject`, and the trailer follows the
    /// detail. Threaded from the caller rather than re-derived here, because a
    /// per-row anchor is what put one wrapped row's duration at the terminal
    /// edge beside siblings that glued theirs.
    ///
    /// Deliberately drains NOTHING: the pending-status drain emits through
    /// here, and a drain re-entered from inside itself would let a kv block
    /// written after these statuses render between them. Every other caller
    /// drains explicitly first.
    pub(crate) fn emit_status_line(&mut self, depth: usize, f: &StatusFields<'_>, column: usize) {
        let (line, trailer, detail_tail) = compose_status_split(self.theme, f);
        let trailer_column = (column > 0).then(|| GLYPH_PREFIX_WIDTH + column);
        self.push_line_undrained(depth, &line, trailer.as_deref(), trailer_column);
        // Continuation lines indent one level past the subject so they read as
        // belonging to this status rather than as new siblings.
        for tail in &detail_tail {
            self.push_line_undrained(depth + 1, tail, None, None);
        }
    }
}

impl Renderer {
    /// Top-level status dispatcher.
    pub fn render_status(&self, w: &dyn Writer, depth: usize, f: &StatusFields<'_>) {
        // Status(Fail) is shown even at Quiet.
        if self.verbosity == Verbosity::Quiet && f.role != Role::Fail {
            return;
        }
        // The routing, the group bookkeeping and the lines all run inside the
        // SAME lock acquisition: a concurrent emission landing between the
        // route decision and the block would take this status's blank-line
        // decision with it.
        self.emit_with(w, |e| e.route_status(depth, f));
    }

    /// [`padded_for_column`] against this renderer's theme and `w`'s wrap
    /// width — the form `live_row.rs` reaches, holding a `Renderer` and a sink
    /// rather than an [`Emitting`].
    pub(crate) fn padded_for_column(
        &self,
        w: &dyn Writer,
        depth: usize,
        f: &StatusFields<'_>,
        column: usize,
    ) -> Option<String> {
        padded_for_column(w.wrap_columns(), depth, f, column)
    }

    /// Emit a Status line with no group bookkeeping: the live-column route
    /// reaches the same shape through [`Emitting::route_status`]. Inside a
    /// section, where `open_top_group` / `mark_top_level_group` are no-ops.
    pub(crate) fn render_status_immediate(
        &self,
        w: &dyn Writer,
        depth: usize,
        f: &StatusFields<'_>,
    ) {
        if self.verbosity == Verbosity::Quiet && f.role != Role::Fail {
            return;
        }
        self.emit_with(w, |e| {
            e.flush_section_headers();
            e.drain_buffers();
            e.emit_status_line(depth, f, 0);
        });
    }

    /// [`compose_status`] against this renderer's theme.
    pub(crate) fn compose_status(&self, f: &StatusFields<'_>) -> (String, Vec<String>) {
        compose_status(&self.theme, f)
    }

    /// Emit a Warn-styled diagnostic line that is shown regardless of verbosity
    /// — including under structured output, where the Printer is forced to
    /// `Verbosity::Quiet` and `render_status` would drop every non-`Fail` role.
    /// Reserved for intentional always-visible diagnostics that belong on
    /// stderr but must never reach the stdout data channel — `Printer`
    /// exposes it as `deprecation` and `alert`, which differ in what they
    /// mean, not in how they render. `depth` comes from the caller's
    /// `enforce_structural_top_level(0)` (0 in normal use); subject must not
    /// contain `\n`.
    pub fn render_advisory(&self, w: &dyn Writer, depth: usize, subject: &str) {
        let (icon_opt, style) = role_glyph(&self.theme, Role::Warn);
        let mut line = String::new();
        if let Some(icon) = icon_opt {
            line.push_str(&style.apply_to(icon).to_string());
            line.push(' ');
        }
        line.push_str(&style.apply_to(cursor_safe(subject)).to_string());
        self.write_line(w, depth, &line);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::StringSink;
    use super::*;
    use crate::output::Theme;
    use crate::output::strip_ansi;

    fn capture() -> (Renderer, StringSink, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        (r, sink, buf)
    }

    use super::super::NarrowSink;

    fn narrow(cols: usize) -> (Renderer, NarrowSink, Arc<Mutex<String>>) {
        let (r, sink, buf) = capture();
        (r, NarrowSink(sink, cols), buf)
    }

    fn timed(subject: &str) -> StatusFields<'_> {
        StatusFields {
            role: Role::Ok,
            subject,
            detail: None,
            duration: Some(Elapsed::row(Duration::from_millis(12_100))),
            target: None,
            subject_style: None,
            detail_style: None,
        }
    }

    #[test]
    fn a_column_the_window_cannot_hold_is_dropped_for_the_whole_group() {
        // The plan-wide column is computed from the widest subject in the
        // phase, which says nothing about the window. Padded to it, every row
        // in the group would run past the edge — so the group renders with no
        // column at all rather than with the column some of its rows can
        // afford and others cannot.
        let (_r, sink, _buf) = narrow(40);
        assert_eq!(group_column(sink.wrap_columns(), 1, 120, 2), 0);
        assert_eq!(
            pad_subject("install ripgrep", 0, true),
            None,
            "so no row in the group is padded"
        );
    }

    #[test]
    fn a_column_the_window_holds_is_the_column_every_row_pads_to() {
        // One decision for the set: the widest row sits at the column and the
        // narrow one is padded out to it, whatever each row trails.
        let (_r, sink, _buf) = narrow(40);
        let column = group_column(sink.wrap_columns(), 1, 20, 2);
        assert_eq!(column, 20);
        assert_eq!(
            console::measure_text_width(&pad_subject("short", column, true).unwrap_or_default()),
            20
        );
    }

    #[test]
    fn a_sink_that_never_wraps_keeps_the_column_it_was_given() {
        // A capture buffer or a redirected stream keeps the physical lines the
        // renderer emitted, so padding can strand nothing there — and a golden
        // recorded on one window replays identically on another.
        let (_r, sink, _buf) = capture();
        assert_eq!(group_column(sink.wrap_columns(), 1, 120, 2), 120);
    }

    #[test]
    fn ok_status_renders_check_glyph() {
        let (r, sink, buf) = capture();
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Ok,
                subject: "done",
                detail: None,
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("✓ done"), "got: {out:?}");
    }

    #[test]
    fn info_role_uses_its_theme_icon() {
        let (r, sink, buf) = capture();
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Info,
                subject: "note",
                detail: None,
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert_eq!(out.trim_end(), "◉ note");
    }

    #[test]
    fn detail_appended_with_em_dash() {
        let (r, sink, buf) = capture();
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Fail,
                subject: "/tmp/foo",
                detail: Some("permission denied"),
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("✗ /tmp/foo — permission denied"),
            "got: {out:?}"
        );
    }

    #[test]
    fn duration_trailed_in_parens() {
        let (r, sink, buf) = capture();
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Ok,
                subject: "done",
                detail: None,
                duration: Some(Elapsed::row(std::time::Duration::from_millis(1234))),
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("(1.2s)"), "got: {out:?}");
    }

    #[test]
    fn fail_shown_even_at_quiet() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Fail,
                subject: "boom",
                detail: None,
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("boom"),
            "Fail must render at Quiet; got: {out:?}"
        );
    }

    #[test]
    fn deprecation_shown_even_at_quiet() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_advisory(&sink, 0, "--jsonpath is deprecated");
        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("--jsonpath is deprecated"),
            "deprecation must render at Quiet; got: {out:?}"
        );
    }

    #[test]
    fn ok_suppressed_at_quiet() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Ok,
                subject: "done",
                detail: None,
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }

    #[test]
    fn multiline_detail_renders_without_panic_and_splits_lines() {
        // A failed subprocess (e.g. `cargo install`) yields a multi-line error
        // chain in `detail`. The renderer must pre-split it instead of handing
        // an embedded newline to write_line (which debug_asserts against it).
        let (r, sink, buf) = capture();
        let detail = "cargo install failed: exit code 101: \
                      \x1b[31merror\x1b[0m: download of windows-sys failed\n\
                      Caused by:\n  curl failed\n  [16] HTTP2 framing layer";
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Fail,
                subject: "cargo install bat",
                detail: Some(detail),
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        let out = crate::test_helpers::captured_text(&buf);
        // First physical line glues subject to the first detail line.
        assert!(
            out.lines().next().unwrap().contains(
                "✗ cargo install bat — cargo install failed: exit code 101: error: download of windows-sys failed"
            ),
            "first line must glue subject + first detail line; got: {out:?}"
        );
        // Continuation lines render as indented siblings, never collapsed.
        assert!(out.contains("Caused by:"), "got: {out:?}");
        assert!(out.contains("curl failed"), "got: {out:?}");
        assert!(out.contains("[16] HTTP2 framing layer"), "got: {out:?}");
        // No embedded newline escaped into a single sink write (the panic case):
        // the body is split into >= 4 physical lines.
        assert!(
            out.lines().filter(|l| !l.trim().is_empty()).count() >= 4,
            "multi-line detail must produce multiple physical lines; got: {out:?}"
        );
    }

    #[test]
    fn detail_strips_ansi_to_prevent_terminal_paint() {
        let (r, sink, buf) = capture();
        let detail = "upstream: \x1b[31mred\x1b[0m text \x1b[1mbold\x1b[0m";
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Fail,
                subject: "sync failed",
                detail: Some(detail),
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        let raw = crate::test_helpers::captured_text(&buf);
        let visible = strip_ansi(&raw);
        assert!(
            visible.contains("sync failed — upstream: red text bold"),
            "visible composition mismatch; got: {visible:?}"
        );
        // Scope the escape check to the segment the detail was pushed into. The
        // renderer's own styling of the glyph and subject is process-global
        // state — whether it emits colour, bold, or nothing at all depends on
        // terminal detection and on whatever else in the binary last touched
        // the colour flags — so asserting over the whole buffer measures that
        // instead of the sanitization. After the em-dash there is only the
        // detail, and it must carry no escape at all: a surviving `\x1b[0m`
        // would close the subject styling early, and a colour code would paint
        // every later line the terminal prints.
        let detail_segment = raw.rsplit(" — ").next().unwrap_or("");
        assert_eq!(
            detail_segment.trim_end_matches('\n'),
            "upstream: red text bold",
            "detail must reach the buffer fully sanitized; got raw: {raw:?}"
        );
        assert!(
            !detail_segment.contains('\u{1b}'),
            "detail segment must contain no ANSI escapes; got: {detail_segment:?}"
        );
    }

    #[test]
    fn an_elapsed_time_never_occupies_its_own_line() {
        // render_status_immediate makes its one push_line_with_trailer call
        // for the subject and its duration together — there is no second
        // push for the "(Ns)" suffix. A duration stranded on a line of its
        // own would read as disconnected from whatever it timed.
        let (r, sink, buf) = capture();
        r.render_status_immediate(&sink, 0, &timed("provision brew"));
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "the duration must not wrap onto a line of its own: {lines:?}"
        );
        assert!(
            lines[0].contains("provision brew") && lines[0].contains("(12.1s)"),
            "subject and duration must share the one line: {lines:?}"
        );
    }

    /// The apt-install shape: a subject so long the whole composed line
    /// cannot fit even unpadded, so it wraps and its duration has to land
    /// somewhere on the last physical line.
    const WRAPPING_SUBJECT: &str = "apt install build-essential, make, unzip, git, curl, ripgrep, xclip, \
                        wl-clipboard, xdg-utils, npm, python3, python3-pip, python3-venv, \
                        rustc, ruby-full, libyaml-dev";

    fn wrapping_row() -> StatusFields<'static> {
        StatusFields {
            role: Role::Ok,
            subject: WRAPPING_SUBJECT,
            detail: None,
            duration: Some(Elapsed::row(std::time::Duration::from_millis(23_600))),
            target: None,
            subject_style: None,
            detail_style: None,
        }
    }

    fn wrapped_lines(out: &str) -> Vec<String> {
        let lines: Vec<String> = out
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("Phase"))
            .map(str::to_string)
            .collect();
        assert!(lines.len() > 1, "the subject needed to wrap: {lines:?}");
        let last = lines.last().map(String::as_str).unwrap_or("");
        assert!(
            last.trim_end().ends_with("(23.6s)"),
            "the duration lands on the last wrapped line: {last:?}"
        );
        for line in &lines[..lines.len() - 1] {
            assert!(
                !line.contains("23.6s"),
                "the duration must not land mid-wrap: {lines:?}"
            );
        }
        lines
    }

    #[test]
    fn a_wrapped_row_outside_any_aligned_group_glues_its_duration_inline() {
        // No group, no column: the duration follows the last word one space
        // on, exactly as it does on a row short enough not to wrap. Anchoring
        // it to the terminal edge instead was the one duration on the page in
        // a column nothing else occupied.
        let (r, sink, buf) = narrow(118);
        r.render_status_immediate(&sink, 1, &wrapping_row());
        let out = crate::test_helpers::captured_text(&buf);
        let lines = wrapped_lines(&out);
        let last = lines.last().map(String::as_str).unwrap_or("");
        assert!(
            last.ends_with("libyaml-dev (23.6s)"),
            "the duration glues one space after the last word: {last:?}"
        );
    }

    #[test]
    fn a_wrapped_row_in_a_live_group_lands_its_duration_at_the_groups_column() {
        // The report claimed a column its short rows pad their subjects to.
        // The wrapped row cannot pad its subject — it is wider than the
        // column — but its LAST line is short of it, so the duration opens at
        // the same x every sibling's trailing content opens at.
        let (r, sink, buf) = narrow(118);
        r.render_section_open("Phase: Packages", /*keep_when_empty=*/ true);
        r.render_section_live_column(70);
        r.render_status(&sink, 1, &timed("brew install neovim"));
        r.render_status(&sink, 1, &wrapping_row());
        r.render_section_close(&sink);
        let out = crate::test_helpers::captured_text(&buf);
        // Columns, not byte offsets: the glyph is three bytes wide and one
        // column, and the continuation line carries no glyph.
        fn column_of(line: &str, needle: &str) -> Option<usize> {
            line.find(needle)
                .map(|at| console::measure_text_width(&line[..at]))
        }
        let short = out
            .lines()
            .find(|l| l.contains("brew install neovim"))
            .unwrap_or_default();
        let short_column = column_of(short, "(12.1s)");
        assert!(
            short_column.is_some(),
            "the short row carries its duration: {out}"
        );
        let lines = wrapped_lines(&out);
        let last = lines.last().map(String::as_str).unwrap_or("");
        assert_eq!(
            column_of(last, "(23.6s)"),
            short_column,
            "the wrapped row's duration opens where its sibling's does:\n{out}"
        );
    }

    /// The duration column always says something true. `{:.1}` renders any
    /// sub-tick measurement as a literal zero, and "took zero seconds" beside
    /// `(1.6s)` and `(10.6s)` reads as a missing measurement rather than a fast
    /// action — so everything under the display resolution reports the
    /// resolution instead.
    #[test]
    fn a_sub_tick_duration_renders_the_floor_and_never_a_zero() {
        for millis in [0, 1, 20, 49] {
            let text = duration_text(Elapsed::row(Duration::from_millis(millis)));
            assert_eq!(text, " (<0.1s)", "{millis}ms must render the floor");
        }
        // The first measurement the one-decimal form can state truthfully is
        // the first one above the floor, so the two meet with no gap.
        assert_eq!(
            duration_text(Elapsed::row(Duration::from_millis(50))),
            " (0.1s)"
        );
        assert_eq!(
            duration_text(Elapsed::row(Duration::from_millis(1600))),
            " (1.6s)"
        );
        assert_eq!(
            duration_text(Elapsed::row(Duration::from_secs(64))),
            " (64.0s)"
        );
    }

    /// A wall-clock total names itself, at every magnitude the floor included,
    /// and a row's own span never does: the word is what lets a reader add
    /// the rows above a total and see why they do not sum to it.
    #[test]
    fn a_wall_clock_total_says_wall_and_a_row_span_does_not() {
        assert_eq!(
            duration_text(Elapsed::wall(Duration::from_millis(278_200))),
            " (278.2s wall)"
        );
        assert_eq!(
            duration_text(Elapsed::wall(Duration::ZERO)),
            " (<0.1s wall)"
        );
        assert_eq!(
            duration_text(Elapsed::row(Duration::from_millis(278_200))),
            " (278.2s)"
        );
    }

    /// `normalize_snapshot_durations` folds every duration this renderer can
    /// emit. `duration_text` is the ONE producer, so the population is
    /// enumerable: two kinds — a row's own span and a wall-clock total —
    /// each crossing the display floor. A spelling the fold misses is not a
    /// golden that fails: it is two captures of one run comparing a folded
    /// duration against an unfolded one, agreeing on a fast host and
    /// disagreeing on the host slow enough to cross the floor.
    #[test]
    fn the_snapshot_normalizer_folds_every_duration_this_renderer_emits() {
        for span in [
            Duration::ZERO,
            Duration::from_millis(1),
            Duration::from_millis(49),
            Duration::from_millis(50),
            Duration::from_millis(1_600),
            Duration::from_secs(64),
            Duration::from_secs(3_600),
        ] {
            assert_eq!(
                crate::normalize_snapshot_durations(&duration_text(Elapsed::row(span))),
                " (XXs)",
                "a row span of {span:?} must fold to one token"
            );
            assert_eq!(
                crate::normalize_snapshot_durations(&duration_text(Elapsed::wall(span))),
                " (XXs wall)",
                "a wall-clock total of {span:?} must fold to one token"
            );
        }
    }

    /// Every duration slot on screen — an action row, a settled spinner, the
    /// run rollup — composes through `duration_trailer`, so the floor cannot
    /// hold on one of them and not the others.
    #[test]
    fn every_duration_slot_composes_through_the_one_trailer() {
        let theme = Theme::default();
        let d = Elapsed::row(Duration::from_millis(3));
        let (line, _) = compose_status(
            &theme,
            &StatusFields {
                role: Role::Ok,
                subject: "wrote /home/tj/.cfgd.env",
                detail: None,
                duration: Some(d),
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        assert!(
            strip_ansi(&line).ends_with(" (<0.1s)"),
            "the composed line takes the floor: {line:?}"
        );
        let (_, trailer, _) = compose_status_split(
            &theme,
            &StatusFields {
                role: Role::Ok,
                subject: "wrote /home/tj/.cfgd.env",
                detail: None,
                duration: Some(d),
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        assert_eq!(
            trailer.map(|t| strip_ansi(&t).to_string()),
            Some(" (<0.1s)".to_string()),
            "the wrapped-commit path takes the same bytes"
        );
    }
}

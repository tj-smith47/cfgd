use std::path::PathBuf;
use std::time::Duration;

use super::{Renderer, Writer};
use crate::output::theme::ThemedStyle;
use crate::output::{Role, Verbosity};

/// A Status emission deferred until section close so subjects can be right-
/// padded to a common column. Buffered per section frame.
pub(crate) struct BufferedStatus {
    pub role: Role,
    pub subject: String,
    pub detail: Option<String>,
    pub duration: Option<Duration>,
    pub target: Option<PathBuf>,
    /// The depth at which the line should ultimately render (matches the
    /// section's child depth at the time the status was emitted).
    pub depth: usize,
    pub subject_style: Option<ThemedStyle>,
    pub detail_style: Option<ThemedStyle>,
}

impl BufferedStatus {
    /// The buffered view of [`super::status::has_trailing`].
    pub(crate) fn has_trailing(&self) -> bool {
        super::status::has_trailing(
            self.detail.as_deref(),
            self.duration,
            self.target.as_deref(),
        )
    }
}

/// One open section's bookkeeping. Pushed on open, popped on close.
pub(crate) struct SectionFrame {
    pub name: String,
    /// Pre-styled replacement for the header line's body. `None` renders
    /// `name` through `theme.header`; `Some` is a header the caller composed
    /// from more than one theme slot (an owner token).
    pub styled_name: Option<String>,
    pub keep_when_empty: bool,
    pub empty_state: Option<String>,
    /// True when the parent section's depth + this section's contents have
    /// emitted at least one byte through this frame.
    pub children_emitted: bool,
    /// The depth at which this section's header should sit (parent depth).
    pub header_depth: usize,
    /// True if the header has been written. We defer header emit until the first
    /// child renders so that collapsed sections leave no trace.
    pub header_emitted: bool,
    /// Statuses awaiting flush at section close. Buffering lets us right-pad
    /// subjects to a common width when trailing content (detail/duration/
    /// target) is present, so the trailing column aligns.
    pub pending_statuses: Vec<BufferedStatus>,
    /// When set, statuses render as they arrive and pad to this column instead
    /// of buffering to close. A live stream cannot wait for a close to learn
    /// its own width, so the width is computed before the run and handed in.
    pub live_column: Option<usize>,
}

impl Renderer {
    /// Open a section: pushes a frame, increments indent. Header is NOT emitted
    /// yet — first child emit triggers it.
    pub(crate) fn render_section_open(&self, name: &str, keep_when_empty: bool) {
        self.render_section_open_styled(name, None, keep_when_empty);
    }

    /// Open a section whose header line is `styled_name` rather than `name`
    /// painted with `theme.header`. `name` remains the plain form the
    /// colour-disabled and structured paths render.
    pub(crate) fn render_section_open_styled(
        &self,
        name: &str,
        styled_name: Option<String>,
        keep_when_empty: bool,
    ) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let header_depth = s.depth();
        s.section_stack.push(SectionFrame {
            name: name.into(),
            styled_name,
            keep_when_empty,
            empty_state: None,
            children_emitted: false,
            header_depth,
            header_emitted: false,
            pending_statuses: Vec::new(),
            live_column: None,
        });
        s.indent_depth += 1;
    }

    /// Write the open sections' not-yet-written headers now, instead of at
    /// their first child.
    ///
    /// Distinct from [`Renderer::flush_pending_section_headers`] in what it
    /// leaves behind: the flush also marks every frame as having children,
    /// which is right for a caller that is *about* to write one and wrong for a
    /// caller that only wants the heading on screen ahead of a live region. A
    /// section committed here and then left empty still renders its empty
    /// state at close.
    ///
    /// Only a `keep_when_empty` section may commit: a collapse-if-empty one
    /// leaves no trace at close, so a header written ahead of its content
    /// would stand over nothing if the content never arrived.
    pub(crate) fn render_section_commit_header(&self, w: &dyn Writer) {
        self.emit_with(w, |e| {
            let mut headers = Vec::new();
            for f in e.state.section_stack.iter_mut() {
                if !f.header_emitted {
                    debug_assert!(
                        f.keep_when_empty,
                        "committing the header of a collapse-if-empty section: {}",
                        f.name
                    );
                    headers.push((header_line(e.theme, f), f.header_depth));
                    f.header_emitted = true;
                }
            }
            if e.verbosity == Verbosity::Quiet {
                return;
            }
            for (styled, depth) in headers {
                e.push_line(depth, &styled);
            }
        });
    }

    /// Mark the innermost open section live: its statuses render as they
    /// arrive, padded to `width`.
    pub(crate) fn render_section_live_column(&self, width: usize) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(top) = s.section_stack.last_mut() {
            top.live_column = Some(width);
        }
    }

    /// Set the empty_state placeholder for the topmost open section.
    pub(crate) fn render_section_empty_state(&self, text: &str) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(top) = s.section_stack.last_mut() {
            top.empty_state = Some(text.into());
        }
    }

    /// Close the topmost section: pop frame, decrement indent. May emit
    /// the header (if first deferred + non-empty) and/or an `(none)` placeholder.
    pub(crate) fn render_section_close(&self, w: &dyn Writer) {
        let frame = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.indent_depth -= 1;
            s.section_stack.pop()
        };
        let Some(frame) = frame else {
            return;
        };

        // Flush any buffered statuses BEFORE deciding blank-pending. Subject
        // right-pad is computed across the buffered set so trailing content
        // aligns in a column.
        self.flush_pending_statuses(w, &frame.pending_statuses);

        // Only TOP-LEVEL sections (header_depth == 0) mark blank-pending on
        // close. Subsection close must NOT produce a blank between siblings —
        // Primary/Secondary subsections render adjacent.
        let is_top_level = frame.header_depth == 0;
        match (frame.children_emitted, frame.keep_when_empty) {
            (true, _) => {
                // Children rendered — section is done. Mark blank pending so the
                // next sibling at the same depth gets one blank between.
                if is_top_level {
                    self.mark_blank_pending();
                }
            }
            (false, true) => {
                if self.verbosity == Verbosity::Quiet {
                    // Plain `section` with no children produces no output at
                    // Quiet. Don't mark_blank_pending — there's nothing to space.
                    return;
                }
                // Plain `section`: emit header + empty_state placeholder. A
                // header already committed at open time is not written twice.
                if !frame.header_emitted {
                    self.emit_section_header_now(w, &frame);
                }
                let placeholder = frame.empty_state.as_deref().unwrap_or("(none)");
                let dim = self.theme.muted.apply_to(placeholder).to_string();
                self.write_line(w, frame.header_depth + 1, &dim);
                if is_top_level {
                    self.mark_blank_pending();
                }
            }
            (false, false) => {
                // section_or_collapse with no children — leave no trace.
            }
        }
    }

    /// Emit any not-yet-emitted section headers, walking the stack outer-to-inner.
    /// Idempotent in output (repeat calls produce no further header lines), and
    /// always marks every frame in the stack as having children — so the section
    /// stays in the non-collapse branch even if no real child line follows.
    pub(crate) fn flush_pending_section_headers(&self, w: &dyn Writer) {
        self.emit_with(w, |e| e.flush_section_headers());
    }

    fn emit_section_header_now(&self, w: &dyn Writer, frame: &SectionFrame) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let styled = header_line(&self.theme, frame);
        self.write_line(w, frame.header_depth, &styled);
    }

    /// Drain a section's buffered statuses, padding subjects of those that
    /// carry trailing content (detail|duration|target) to the max subject
    /// width across that subset. Statuses without trailing content render
    /// as-is (no padding). Width is measured with `console::measure_text_width`
    /// so multi-byte glyphs in subjects count as one column each.
    pub(crate) fn flush_pending_statuses(&self, w: &dyn Writer, statuses: &[BufferedStatus]) {
        if statuses.is_empty() {
            return;
        }
        let max_subject_width = statuses
            .iter()
            .filter(|s| s.has_trailing())
            .map(|s| console::measure_text_width(&s.subject))
            .max()
            .unwrap_or(0);
        for s in statuses {
            let fields = super::StatusFields {
                role: s.role,
                subject: &s.subject,
                detail: s.detail.as_deref(),
                duration: s.duration,
                target: s.target.as_deref(),
                subject_style: s.subject_style.clone(),
                detail_style: s.detail_style.clone(),
            };
            // Per line, not once for the set: capping the shared column by the
            // tightest line in it would drop every line's alignment because
            // one of them carries a long detail.
            let column = self.affordable_column(w, s.depth, &fields, max_subject_width);
            let padded = super::status::pad_subject(&s.subject, column, s.has_trailing());
            self.render_status_immediate(
                w,
                s.depth,
                &super::StatusFields {
                    subject: padded.as_deref().unwrap_or(&s.subject),
                    ..fields
                },
            );
        }
    }
}

impl super::Emitting<'_> {
    /// Collect any not-yet-emitted section headers, walking the stack
    /// outer-to-inner. Idempotent in output (repeat calls produce no further
    /// header lines), and always marks every frame in the stack as having
    /// children — so the section stays in the non-collapse branch even if no
    /// real child line follows.
    pub(crate) fn flush_section_headers(&mut self) {
        let mut headers = Vec::new();
        for f in self.state.section_stack.iter_mut() {
            if !f.header_emitted {
                headers.push((header_line(self.theme, f), f.header_depth));
                f.header_emitted = true;
            }
            f.children_emitted = true;
        }
        // State mutation runs even under Quiet so that close()'s collapse
        // decision stays consistent; only the emission of header lines is
        // suppressed.
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        for (styled, depth) in headers {
            self.push_line(depth, &styled);
        }
    }
}

/// The body of a section's header line: the caller's pre-styled token when it
/// supplied one, otherwise the name painted with `theme.header` at the top
/// level or `theme.secondary` when nested — a section opened while another
/// is already open (`header_depth > 0`, the same signal that already governs
/// whether the section's close leaves a blank line behind it) is a
/// subsection regardless of whether the caller reached it through
/// `Doc::subsection` or by nesting `SectionGuard::section` calls by hand, and
/// reads visually apart from the section that owns it rather than repeating
/// its exact heading style one indent level deeper.
pub(crate) fn header_line(theme: &crate::output::Theme, frame: &SectionFrame) -> String {
    match &frame.styled_name {
        Some(styled) => styled.clone(),
        None if frame.header_depth == 0 => theme.header.apply_to(&frame.name).to_string(),
        None => theme.secondary.apply_to(&frame.name).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::{Renderer, StringSink};
    use crate::output::{Theme, Verbosity};

    fn capture() -> (Renderer, StringSink, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        (r, sink, buf)
    }

    #[test]
    fn collapse_section_with_no_children_leaves_no_trace() {
        let (r, sink, buf) = capture();
        r.render_section_open("Empty", /*keep_when_empty=*/ false);
        r.render_section_close(&sink);
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }

    #[test]
    fn plain_section_with_no_children_renders_none_placeholder() {
        let (r, sink, buf) = capture();
        r.render_section_open("Files", /*keep_when_empty=*/ true);
        r.render_section_close(&sink);
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("Files"));
        assert!(s.contains("(none)"));
    }

    #[test]
    fn a_committed_header_reaches_the_sink_before_any_child_does() {
        // The live region paints below the last committed line, so a heading
        // still deferred when an action opens an output window is written
        // after the output it introduces.
        let (r, sink, buf) = capture();
        r.render_section_open("Phase: Packages", /*keep_when_empty=*/ true);
        r.render_section_commit_header(&sink);
        assert!(
            crate::test_helpers::captured_text(&buf).contains("Phase: Packages"),
            "the header is on screen before the section has a child"
        );
        r.render_section_close(&sink);
    }

    #[test]
    fn a_committed_header_is_not_written_twice() {
        let (r, sink, buf) = capture();
        r.render_section_open("Phase: Packages", true);
        r.render_section_commit_header(&sink);
        r.render_section_commit_header(&sink);
        r.render_section_close(&sink);
        let out = crate::test_helpers::captured_text(&buf);
        assert_eq!(out.matches("Phase: Packages").count(), 1, "got: {out}");
    }

    #[test]
    fn a_committed_header_still_renders_its_empty_state() {
        // Committing the heading says nothing about whether the section found
        // anything to put under it; a flush that also marked the frame as
        // having children would swallow the `(none)`.
        let (r, sink, buf) = capture();
        r.render_section_open("Files", true);
        r.render_section_commit_header(&sink);
        r.render_section_close(&sink);
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("(none)"), "got: {out}");
    }

    #[test]
    fn committing_an_inner_header_commits_the_ones_above_it() {
        // An owner group inside a phase: the phase's heading cannot be left
        // behind, or the group's would be the first line of the block.
        let (r, sink, buf) = capture();
        r.render_section_open("Phase: Packages", true);
        r.render_section_open("profile:work", true);
        r.render_section_commit_header(&sink);
        let out = crate::test_helpers::captured_text(&buf);
        let phase = out.find("Phase: Packages");
        let group = out.find("profile:work");
        assert!(
            phase.is_some() && group.is_some() && phase < group,
            "got: {out}"
        );
        r.render_section_close(&sink);
        r.render_section_close(&sink);
    }

    #[test]
    fn empty_state_overrides_none() {
        let (r, sink, buf) = capture();
        r.render_section_open("Files", true);
        r.render_section_empty_state("No files yet");
        r.render_section_close(&sink);
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("No files yet"));
        assert!(!s.contains("(none)"));
    }

    /// The buffered close pads exactly the subjects that carry trailing
    /// content. A section with NO live column mixing both kinds is the only
    /// shape that exercises `BufferedStatus::has_trailing` on both answers:
    /// the two trailing lines align on a common column, and the bare subject
    /// is left at its own width rather than padded into it.
    #[test]
    fn section_close_pads_only_buffered_subjects_with_trailing_content() {
        use std::time::Duration;

        use crate::output::{Printer, Role};

        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Packages");
            let _ = s.status(Role::Ok, "ripgrep").detail("installed");
            let _ = s.status(Role::Ok, "fd");
            let _ = s.status(Role::Ok, "already-current");
            let _ = s
                .status(Role::Ok, "bat")
                .duration(Duration::from_millis(400));
        }
        p.flush();
        let out = crate::test_helpers::captured_text(&buf);

        // Width is the widest TRAILING subject (ripgrep, 7). One bare subject
        // is longer than that and one is shorter, so the assertions below fail
        // both if the width stops filtering (ripgrep would pad out to 15) and
        // if the pad stops asking (fd would pad out to 7).
        assert!(
            out.contains("  ✓ ripgrep — installed\n"),
            "widest trailing subject must not gain padding: {out:?}"
        );
        assert!(
            out.contains("  ✓ bat     (0.4s)\n"),
            "shorter trailing subject must pad to the 7-column width: {out:?}"
        );
        assert!(
            out.contains("  ✓ fd\n") && out.contains("  ✓ already-current\n"),
            "a subject with no trailing content must not be padded: {out:?}"
        );

        // The alignment claim itself: both trailing markers start at the same
        // column, which is what the padding exists to produce.
        let column = |needle: &str| {
            let line = out
                .lines()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("line {needle:?} missing from {out:?}"));
            console::measure_text_width(&line[..line.find(needle).unwrap_or(0)])
        };
        assert_eq!(
            column("—"),
            column("("),
            "trailing column misaligned across buffered statuses: {out:?}"
        );
    }

    #[test]
    fn section_with_children_emits_header_then_indents() {
        let (r, sink, buf) = capture();
        r.render_section_open("Files", false);
        r.flush_pending_section_headers(&sink);
        // Simulate a child line at depth 1.
        r.write_line(&sink, 1, "- foo.txt");
        r.render_section_close(&sink);
        let s = crate::test_helpers::captured_text(&buf);
        // Header carries bold SGR; strip ANSI before shape assertions.
        let plain = crate::output::strip_ansi(&s);
        assert!(plain.starts_with("Files\n"), "got: {plain:?}");
        assert!(plain.contains("\n  - foo.txt\n"), "got: {plain:?}");
    }

    #[test]
    fn no_action_in_a_phase_renders_before_its_heading() {
        // `Printer::section_phase` commits the heading synchronously — before
        // handing back the guard the first child status renders through —
        // unlike a plain `section()`, which defers the header until the
        // section is known to have content. A phase heading that lost that
        // guarantee would let a live-region status window paint above a
        // heading that hasn't reached the sink yet.
        use crate::output::{PhaseLabel, Printer, Role};

        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section_phase(&PhaseLabel::new("Packages"));
            let _ = s.status(Role::Ok, "ripgrep").detail("installed");
        }
        p.flush();
        let out = crate::test_helpers::captured_text(&buf);

        let heading = out
            .find("Phase: Packages")
            .unwrap_or_else(|| panic!("phase heading missing from output: {out:?}"));
        let action = out
            .find("ripgrep")
            .unwrap_or_else(|| panic!("action line missing from output: {out:?}"));
        assert!(
            heading < action,
            "the phase heading must render before any action inside it: {out:?}"
        );
    }

    /// A section nested inside another open section (a subsection, whether
    /// reached through `Doc::subsection` or by nesting `SectionGuard::section`
    /// calls) paints `theme.secondary`, not `theme.header` — the two must read
    /// apart from each other, or a reader cannot tell a subsection's heading
    /// from the section that owns it.
    #[test]
    fn nested_section_header_uses_secondary_not_header() {
        use crate::output::Theme;

        let theme = Theme::from_preset("dracula");
        let colored = theme.clone().with_colors(true);
        let expected_secondary = colored.secondary.clone().apply_to("Child").to_string();
        let header_styled = colored.header.apply_to("Child").to_string();
        assert_ne!(
            expected_secondary, header_styled,
            "the fixture theme must actually distinguish the two slots, or this test proves nothing"
        );

        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(colored, Verbosity::Normal);
        r.render_section_open("Parent", false);
        r.render_section_open("Child", false);
        r.flush_pending_section_headers(&sink);
        r.write_line(&sink, 2, "leaf");
        r.render_section_close(&sink);
        r.render_section_close(&sink);

        // raw-capture-ok: asserting the exact styled runs reach the renderer unrestyled — captured_text would strip the ANSI this test exists to check
        let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            raw.contains(&expected_secondary),
            "the nested section's header must be theme.secondary: {raw:?}"
        );
        assert!(
            !raw.contains(&header_styled),
            "the nested section's header must not fall back to theme.header: {raw:?}"
        );
    }

    /// The top-level section (`header_depth == 0`) keeps `theme.header`
    /// regardless of how many subsections it later opens — the depth check
    /// reads the SECTION's own `header_depth`, not some global "have we
    /// opened a subsection yet" flag.
    #[test]
    fn top_level_section_header_stays_theme_header_even_after_a_subsection_closes() {
        use crate::output::Theme;

        let theme = Theme::from_preset("dracula");
        let colored = theme.clone().with_colors(true);
        let expected_header = colored.header.apply_to("Parent").to_string();

        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(colored, Verbosity::Normal);
        r.render_section_open("Parent", false);
        r.render_section_open("Child", false);
        r.flush_pending_section_headers(&sink);
        r.write_line(&sink, 2, "leaf");
        r.render_section_close(&sink);
        r.render_section_close(&sink);

        // raw-capture-ok: asserting the exact styled run reaches the renderer unrestyled — captured_text would strip the ANSI this test exists to check
        let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            raw.contains(&expected_header),
            "the top-level section's header must stay theme.header: {raw:?}"
        );
    }
}

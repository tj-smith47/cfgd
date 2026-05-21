use std::path::PathBuf;
use std::time::Duration;

use super::{Renderer, Writer};
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
}

/// One open section's bookkeeping. Pushed on open, popped on close.
pub(crate) struct SectionFrame {
    pub name: String,
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
}

impl Renderer {
    /// Open a section: pushes a frame, increments indent. Header is NOT emitted
    /// yet — first child emit triggers it.
    pub(crate) fn render_section_open(&self, name: &str, keep_when_empty: bool) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let header_depth = s.depth();
        s.section_stack.push(SectionFrame {
            name: name.into(),
            keep_when_empty,
            empty_state: None,
            children_emitted: false,
            header_depth,
            header_emitted: false,
            pending_statuses: Vec::new(),
        });
        s.indent_depth += 1;
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
                // Plain `section`: emit header + empty_state placeholder.
                self.emit_section_header_now(w, &frame);
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
        let frames_to_emit = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for f in s.section_stack.iter_mut() {
                if !f.header_emitted {
                    out.push((f.name.clone(), f.header_depth));
                    f.header_emitted = true;
                }
                f.children_emitted = true;
            }
            out
        };
        // State mutation runs even under Quiet so that close()'s collapse decision
        // stays consistent; only the emission of header lines is suppressed.
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        for (name, depth) in frames_to_emit {
            // Mirror `write_line`'s blank-pending handling.
            let styled = self.theme.header.apply_to(&name).to_string();
            self.write_line(w, depth, &styled);
        }
    }

    fn emit_section_header_now(&self, w: &dyn Writer, frame: &SectionFrame) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let styled = self.theme.header.apply_to(&frame.name).to_string();
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
            .filter(|s| s.detail.is_some() || s.duration.is_some() || s.target.is_some())
            .map(|s| console::measure_text_width(&s.subject))
            .max()
            .unwrap_or(0);
        for s in statuses {
            let has_trailing = s.detail.is_some() || s.duration.is_some() || s.target.is_some();
            let subject_owned;
            let subject_ref: &str = if has_trailing && max_subject_width > 0 {
                let cur = console::measure_text_width(&s.subject);
                if cur < max_subject_width {
                    subject_owned = format!("{}{}", s.subject, " ".repeat(max_subject_width - cur));
                    subject_owned.as_str()
                } else {
                    s.subject.as_str()
                }
            } else {
                s.subject.as_str()
            };
            self.render_status_immediate(
                w,
                s.depth,
                &super::StatusFields {
                    role: s.role,
                    subject: subject_ref,
                    detail: s.detail.as_deref(),
                    duration: s.duration,
                    target: s.target.as_deref(),
                },
            );
        }
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
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn plain_section_with_no_children_renders_none_placeholder() {
        let (r, sink, buf) = capture();
        r.render_section_open("Files", /*keep_when_empty=*/ true);
        r.render_section_close(&sink);
        let s = buf.lock().unwrap();
        assert!(s.contains("Files"));
        assert!(s.contains("(none)"));
    }

    #[test]
    fn empty_state_overrides_none() {
        let (r, sink, buf) = capture();
        r.render_section_open("Files", true);
        r.render_section_empty_state("No files yet");
        r.render_section_close(&sink);
        let s = buf.lock().unwrap();
        assert!(s.contains("No files yet"));
        assert!(!s.contains("(none)"));
    }

    #[test]
    fn section_with_children_emits_header_then_indents() {
        let (r, sink, buf) = capture();
        r.render_section_open("Files", false);
        r.flush_pending_section_headers(&sink);
        // Simulate a child line at depth 1.
        r.write_line(&sink, 1, "- foo.txt");
        r.render_section_close(&sink);
        let s = buf.lock().unwrap();
        // Header may carry bold SGR escapes (kept under NO_COLOR per attrs
        // policy in theme::StyledText), so strip ANSI before structural
        // assertions about line shape.
        let plain = crate::output::tests::strip_ansi(&s);
        assert!(plain.starts_with("Files\n"), "got: {plain:?}");
        assert!(plain.contains("\n  - foo.txt\n"), "got: {plain:?}");
    }
}

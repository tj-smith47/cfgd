use super::{Renderer, Writer};
use crate::output_v2::Verbosity;

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

        match (frame.children_emitted, frame.keep_when_empty) {
            (true, _) => {
                // Children rendered — section is done. Mark blank pending so the
                // next sibling at the same depth gets one blank between.
                self.mark_blank_pending();
            }
            (false, true) => {
                // Plain `section`: emit header + empty_state placeholder.
                self.emit_section_header_now(w, &frame);
                let placeholder = frame.empty_state.as_deref().unwrap_or("(none)");
                let dim = self.theme.muted.apply_to(placeholder).to_string();
                self.write_line(w, frame.header_depth + 1, &dim);
                self.mark_blank_pending();
            }
            (false, false) => {
                // section_or_collapse with no children — leave no trace.
            }
        }
    }

    /// Emit a deferred section header. Idempotent: no-op if already emitted.
    /// Walks the section stack from outer to inner and emits any deferred
    /// headers before the current line.
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
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        for (name, depth) in frames_to_emit {
            // Mirror `write_line`'s blank-pending handling.
            let styled = console::Style::new().bold().apply_to(&name).to_string();
            self.write_line(w, depth, &styled);
        }
    }

    fn emit_section_header_now(&self, w: &dyn Writer, frame: &SectionFrame) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let styled = console::Style::new()
            .bold()
            .apply_to(&frame.name)
            .to_string();
        self.write_line(w, frame.header_depth, &styled);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::{Renderer, StringSink};
    use crate::output_v2::{Theme, Verbosity};

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
        assert!(s.starts_with("Files\n"), "got: {s:?}");
        assert!(s.contains("\n  - foo.txt\n"), "got: {s:?}");
    }
}

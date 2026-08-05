//! The renderer is the single layout authority. It owns:
//! - indent depth (push/pop per Section)
//! - blank-line state machine (no leading, no trailing, exactly one between siblings)
//! - kv auto-batching (consecutive `kv` calls coalesce into one aligned block)
//! - glyph + style lookup via Theme
//!
//! Every other module routes terminal writes through here.
//!
//! `RenderState::{depth,push,pop}` and `indent_prefix` are reachable only
//! from tests and from inside the renderer module; the narrow `dead_code`
//! allow keeps them addressable without a workspace-wide warning.
#![allow(dead_code)]

use std::sync::Mutex;

use super::{Theme, Verbosity};

mod glyphs;
pub mod kv;
pub mod section;
pub mod status;
pub mod table;
pub(crate) use glyphs::{finalize_subject, role_glyph};
pub use status::StatusFields;
pub use table::Table;

/// The kind of a top-level (outside any section) group emission.
///
/// Blank lines separate GROUPS, not the lines inside one. Three rules follow
/// from that and are enforced in `open_top_group`:
///
///   - consecutive emissions of a kind whose `runs_contiguously` is true
///     (statuses, hints) are one group and render with no blank between them
///   - a heading binds to whatever it introduces, so nothing directly after a
///     top-level heading is preceded by a blank
///   - the streaming → buffered seam always separates: a streamed line and a
///     buffered `Doc`'s line never join into one group
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TopGroup {
    Heading,
    Status,
    Hint,
    Bullet,
    CodeBlock,
    Note,
    KvBlock,
    Table,
}

impl TopGroup {
    /// True for single-line kinds that read as a list when repeated — a run of
    /// `✓ Created …` lines is one block, not seven blocks of one line.
    fn runs_contiguously(self) -> bool {
        matches!(self, TopGroup::Status | TopGroup::Hint | TopGroup::Bullet)
    }
}

/// Per-Printer rendering state. Held inside `Mutex` because multiple
/// `SectionGuard`s may share the same `&Printer` and write concurrently
/// from one thread (drop ordering is single-threaded but borrow-checker
/// can't see that).
pub(crate) struct RenderState {
    /// Current indent depth. Section open = +1, section close = -1.
    indent_depth: usize,
    /// True if the renderer should emit a blank line before the next non-blank
    /// emission (set by section close, cleared by next emit).
    blank_pending: bool,
    /// True until the first emission lands; suppresses leading blank.
    leading: bool,
    /// Buffered kvs awaiting a non-kv emission to flush as one aligned block.
    kv_buffer: Vec<(String, String)>,
    pub(crate) section_stack: Vec<crate::output::renderer::section::SectionFrame>,
    /// True iff the most recent emission was a top-level heading and no other
    /// emission has happened since. Consumed by the next top-level kv_block,
    /// which re-anchors the block at depth+1 so it visually nests under the
    /// heading. Reset by any other emission (status, section header, bullet,
    /// etc.).
    pub(crate) last_was_top_heading: bool,
    /// Kind of the most recent top-level group emission, or `None` when the
    /// last thing written was not one (a section body, a section close).
    /// Consumed by the next top-level emit to decide whether it continues that
    /// group or starts a new one.
    pub(crate) last_top_group: Option<TopGroup>,
    /// Nesting depth of buffered `Doc` rendering; 0 while output is streaming.
    /// A streamed line and a buffered Doc's line are never the same group even
    /// when they are the same kind — the seam between them always separates.
    doc_depth: usize,
    /// Whether the most recent top-level emission came from inside a `Doc`.
    /// Compared against the current side of the seam in `open_top_group`.
    last_top_in_doc: bool,
}

impl RenderState {
    pub(crate) fn new() -> Self {
        Self {
            indent_depth: 0,
            blank_pending: false,
            leading: true,
            kv_buffer: Vec::new(),
            section_stack: Vec::new(),
            last_was_top_heading: false,
            last_top_group: None,
            doc_depth: 0,
            last_top_in_doc: false,
        }
    }

    pub(crate) fn depth(&self) -> usize {
        self.indent_depth
    }

    pub(crate) fn push(&mut self) -> usize {
        self.indent_depth += 1;
        self.indent_depth
    }

    pub(crate) fn pop(&mut self) {
        debug_assert!(self.indent_depth > 0, "renderer pop at depth 0");
        if self.indent_depth > 0 {
            self.indent_depth -= 1;
        }
    }
}

/// Renderer is created per Printer. All state lives in `RenderState` behind a
/// Mutex so the caller doesn't see interior mutability.
pub struct Renderer {
    pub(crate) theme: Theme,
    pub(crate) verbosity: Verbosity,
    pub(crate) state: Mutex<RenderState>,
}

impl Renderer {
    pub fn new(theme: Theme, verbosity: Verbosity) -> Self {
        Self {
            theme,
            verbosity,
            state: Mutex::new(RenderState::new()),
        }
    }

    /// Build the indent prefix for the current depth.
    pub(crate) fn indent_prefix(&self, depth: usize) -> String {
        "  ".repeat(depth)
    }

    /// Called by every top-level emit before writing. Returns the depth at
    /// which the emit should actually render (clamped to current open section).
    ///
    /// A top-level emit (depth 0) reached while a `SectionGuard` is alive is
    /// a programming error. Debug builds `debug_assert!` to flag the call
    /// site loudly; release builds log a `tracing::warn!` once per process
    /// and re-route the emit to the section's current depth so the output
    /// stays readable.
    pub(crate) fn enforce_top_level_emit(&self, expected_depth: usize) -> usize {
        let actual = self.state.lock().unwrap_or_else(|e| e.into_inner()).depth();
        if expected_depth == 0 && actual > 0 {
            // Top-level emit while a section is open.
            debug_assert!(
                false,
                "top-level emit at depth 0 while section open at depth {actual}"
            );
            // Release build: warn once, render at the section's depth.
            // Process-global: test runs observe at most one warning across the entire suite.
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                tracing::warn!(
                    "cfgd output: top-level Printer emit reached while a SectionGuard \
                     was open. The emit was re-routed to the section's depth. Fix the \
                     call site (move it inside or outside the section)."
                );
            });
            actual
        } else {
            expected_depth
        }
    }
}

/// Sink for one rendered line. Production = stderr Term; tests = string buffer.
pub trait Writer: Send + Sync {
    fn write_line(&self, text: &str);
}

impl Writer for console::Term {
    fn write_line(&self, text: &str) {
        let _ = console::Term::write_line(self, text);
    }
}

pub struct StringSink(pub std::sync::Arc<std::sync::Mutex<String>>);
impl Writer for StringSink {
    fn write_line(&self, text: &str) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.push_str(text);
        g.push('\n');
    }
}

impl Renderer {
    /// Emit a single physical line at the given depth, honoring blank-pending.
    ///
    /// Flushes any pending kvs first — otherwise buffered kvs would render
    /// *after* this non-kv line, inverting the call order. kv emission paths
    /// must call `w.write_line(...)` directly (NOT `self.write_line`) to avoid
    /// recursing back into `flush_kv_buffer_internal`.
    pub(crate) fn write_line(&self, w: &dyn Writer, depth: usize, body: &str) {
        self.flush_kv_buffer_internal(w);
        debug_assert!(
            !body.contains('\n'),
            "Renderer::write_line received body with embedded newline: {body:?}. \
             Callers must pre-split multi-line content (see render_note for the canonical pattern)."
        );
        // Callers must pre-split multi-line content; we normalize embedded \n
        // defensively to keep blank-line accounting honest if they don't. The
        // sink appends its own trailing newline per call; any newlines
        // already in `body` would smuggle physical line breaks past the
        // blank-line accounting (e.g. a Status subject ending with `\n` would
        // produce a stray blank between this emission and the next, breaking
        // the one-blank-between-siblings invariant). Strip trailing newlines
        // and split internal ones into separate sink writes at the same
        // depth — `render_note` is the only intentional multi-line path and
        // pre-splits before calling here.
        let trimmed = body.trim_end_matches(['\n', '\r']);
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.leading {
            s.leading = false;
            s.blank_pending = false;
        } else if s.blank_pending {
            w.write_line("");
            s.blank_pending = false;
        }
        // Any emission resets the heading-just-emitted flag. Heading itself
        // sets the flag back true after this call returns.
        s.last_was_top_heading = false;
        let prefix = "  ".repeat(depth);
        for line in trimmed.split('\n') {
            w.write_line(&format!("{}{}", prefix, line));
        }
    }

    /// Inner kv-buffer flush invoked from `write_line`. Does NOT recurse — it
    /// calls `render_kv_block_no_flush` directly, which uses `w.write_line` for
    /// every emission rather than `self.write_line`.
    fn flush_kv_buffer_internal(&self, w: &dyn Writer) {
        let (pairs, depth) = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if s.kv_buffer.is_empty() {
                return;
            }
            (std::mem::take(&mut s.kv_buffer), s.indent_depth)
        };
        self.render_kv_block_no_flush(w, depth, &pairs);
    }

    /// Mark that the next non-blank emission should be preceded by exactly
    /// one blank line. Called by Section close.
    pub(crate) fn mark_blank_pending(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.blank_pending = true;
        // A section boundary always separates: whatever follows starts a new
        // group even if it is the same kind as what preceded the section.
        s.last_top_group = None;
    }

    /// Set blank-pending iff we're at the root group level (no open section).
    /// Called at the end of every top-level group emission (heading, kv_block,
    /// status, hint, note, table) so the next top-level emit gets one blank.
    /// One blank line precedes every top-level GROUP after the first —
    /// `open_top_group` decides what continues a group rather than starting one.
    pub(crate) fn mark_top_level_group(&self, kind: TopGroup) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.section_stack.is_empty() {
            s.blank_pending = true;
            s.last_top_group = Some(kind);
            s.last_top_in_doc = s.doc_depth > 0;
        } else {
            s.last_top_group = None;
        }
    }

    /// Enter buffered `Doc` rendering. Paired with `exit_doc`; nests because a
    /// Doc may render a nested Doc through a component.
    pub(crate) fn enter_doc(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.doc_depth += 1;
    }

    /// Leave buffered `Doc` rendering.
    pub(crate) fn exit_doc(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.doc_depth = s.doc_depth.saturating_sub(1);
    }

    /// Drop the pending blank when this emission continues the previous group
    /// rather than starting a new one. Call before writing, from every
    /// top-level emitter.
    pub(crate) fn open_top_group(&self, kind: TopGroup) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s.section_stack.is_empty() || !s.blank_pending {
            return;
        }
        let continues = match kind {
            // A heading introduces what follows it, so it never binds to the
            // heading above: two consecutive headings are two groups.
            TopGroup::Heading => false,
            _ => {
                s.last_was_top_heading
                    || (kind.runs_contiguously()
                        && s.last_top_group == Some(kind)
                        // Streamed lines and a buffered Doc's lines are
                        // different groups even when the kind matches: the
                        // seam between them keeps its one blank line.
                        && (s.doc_depth > 0) == s.last_top_in_doc)
            }
        };
        if continues {
            s.blank_pending = false;
        }
    }

    /// Heading: bold styled by Theme::header. No `=== ===` decoration. Always depth 0.
    pub fn render_heading(&self, w: &dyn Writer, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let styled = self.theme.header.apply_to(text).to_string();
        self.open_top_group(TopGroup::Heading);
        self.write_line(w, 0, &styled);
        // Set the heading-just-emitted flag AFTER write_line (which clears
        // it). The next top-level kv_block consumes this to re-anchor itself
        // at depth+1 so it visually nests under the heading.
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if s.section_stack.is_empty() {
                s.last_was_top_heading = true;
            }
        }
        self.mark_top_level_group(TopGroup::Heading);
    }

    /// Bullet: glyph `-`, then space, then text. Uncolored. The renderer's only
    /// bullet glyph; `+`/`~`/`>`/`*` are forbidden.
    pub fn render_bullet(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        self.flush_pending_section_headers(w);
        self.open_top_group(TopGroup::Bullet);
        self.write_line(w, depth, &format!("- {}", text));
        self.mark_top_level_group(TopGroup::Bullet);
    }

    /// One line of live output from a child process, rendered dim and indented.
    /// Unlike a spinner message — which repaints a fixed window in place and so
    /// erases and rewrites the lines above it — this appends, letting output
    /// scroll exactly as it would in a bare terminal with the cursor resting on
    /// the last line. Claims no `TopGroup`: interleaving child output must not
    /// insert group-boundary blank lines between consecutive lines.
    pub fn render_stream_line(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        self.flush_pending_section_headers(w);
        self.write_line(w, depth, &self.theme.muted.apply_to(text).to_string());
    }

    /// Hint: arrow glyph + dim text. Shown at Normal+ (NOT Quiet). The
    /// canonical "next step" surface.
    pub fn render_hint(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        self.flush_pending_section_headers(w);
        let arrow = self
            .theme
            .muted
            .apply_to(format!("{} ", self.theme.icon_arrow));
        let body = self.theme.muted.apply_to(text);
        self.open_top_group(TopGroup::Hint);
        self.write_line(w, depth, &format!("{}{}", arrow, body));
        self.mark_top_level_group(TopGroup::Hint);
    }

    /// Code block: a tight run of verbatim lines (e.g. a copy-pasteable YAML
    /// snippet). Shown at Normal+ like `hint` (NOT Verbose-only like `note`).
    /// Unlike `hint`, NO per-line glyph and NO blank line between rows — the
    /// block renders as one contiguous unit, with a single trailing blank set
    /// at the end (modeled on `render_kv_block_no_flush`). Each entry in `lines`
    /// must be newline-free; multi-line content is split by the caller so the
    /// `write_line` debug_assert holds.
    pub fn render_code_block(&self, w: &dyn Writer, depth: usize, lines: &[String]) {
        if self.verbosity == Verbosity::Quiet || lines.is_empty() {
            return;
        }
        self.flush_pending_section_headers(w);
        self.open_top_group(TopGroup::CodeBlock);
        for line in lines {
            self.write_line(w, depth, &self.theme.muted.apply_to(line).to_string());
        }
        self.mark_top_level_group(TopGroup::CodeBlock);
    }

    /// Note: multi-line prose. Suppressed at both Quiet and Normal; only Verbose.
    pub fn render_note(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity != Verbosity::Verbose {
            return;
        }
        self.flush_pending_section_headers(w);
        self.open_top_group(TopGroup::Note);
        for line in text.lines() {
            let dim = self.theme.muted.apply_to(line);
            self.write_line(w, depth, &dim.to_string());
        }
        self.mark_top_level_group(TopGroup::Note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::strip_ansi;

    #[test]
    fn fresh_renderer_at_depth_0() {
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        assert_eq!(r.state.lock().unwrap().depth(), 0);
    }

    #[test]
    fn push_pop_balances() {
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let mut s = r.state.lock().unwrap();
        assert_eq!(s.push(), 1);
        assert_eq!(s.push(), 2);
        s.pop();
        s.pop();
        assert_eq!(s.depth(), 0);
    }

    #[test]
    fn indent_prefix_uses_two_spaces_per_level() {
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        assert_eq!(r.indent_prefix(0), "");
        assert_eq!(r.indent_prefix(1), "  ");
        assert_eq!(r.indent_prefix(3), "      ");
    }

    use std::sync::{Arc, Mutex};

    fn capture() -> (Renderer, StringSink, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        (r, sink, buf)
    }

    #[test]
    fn no_leading_blank() {
        let (r, sink, buf) = capture();
        r.mark_blank_pending(); // even if requested before first emit
        r.write_line(&sink, 0, "first");
        let s = buf.lock().unwrap();
        assert_eq!(*s, "first\n");
    }

    #[test]
    fn one_blank_between_siblings() {
        let (r, sink, buf) = capture();
        r.write_line(&sink, 0, "A");
        r.mark_blank_pending();
        r.mark_blank_pending(); // duplicate marks coalesce
        r.write_line(&sink, 0, "B");
        let s = buf.lock().unwrap();
        assert_eq!(*s, "A\n\nB\n");
    }

    #[test]
    fn indent_two_spaces_per_level() {
        let (r, sink, buf) = capture();
        r.write_line(&sink, 0, "root");
        r.write_line(&sink, 1, "child");
        r.write_line(&sink, 2, "grand");
        let s = buf.lock().unwrap();
        assert_eq!(*s, "root\n  child\n    grand\n");
    }

    #[test]
    fn heading_renders_at_depth_zero() {
        let (r, sink, buf) = capture();
        r.render_heading(&sink, "Status");
        let s = buf.lock().unwrap();
        assert!(s.contains("Status"));
        // No `=== ===` decoration.
        assert!(!s.contains("==="));
    }

    #[test]
    fn heading_suppressed_when_quiet() {
        let (r_default, _, _) = capture();
        drop(r_default);
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_heading(&sink, "Status");
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn bullet_uses_dash_glyph() {
        let (r, sink, buf) = capture();
        r.render_bullet(&sink, 1, "foo");
        let s = buf.lock().unwrap();
        assert!(s.contains("  - foo"), "got: {s:?}");
    }

    #[test]
    fn bullet_quiet_suppressed() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_bullet(&sink, 1, "foo");
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn hint_uses_arrow_glyph() {
        let (r, sink, buf) = capture();
        r.render_hint(&sink, 0, "run cfgd apply");
        let s = buf.lock().unwrap();
        assert!(s.contains("→"), "got: {s:?}");
        assert!(s.contains("run cfgd apply"));
    }

    #[test]
    fn code_block_renders_tight_no_arrow_no_blank_lines() {
        let (r, sink, buf) = capture();
        r.render_code_block(
            &sink,
            0,
            &[
                "spec:".to_string(),
                "  sources:".to_string(),
                "    - name: acme".to_string(),
            ],
        );
        let out = strip_ansi(&buf.lock().unwrap());
        // Every YAML row present, verbatim, no `→` glyph.
        assert!(out.contains("spec:"), "got: {out:?}");
        assert!(out.contains("  sources:"), "got: {out:?}");
        assert!(out.contains("    - name: acme"), "got: {out:?}");
        assert!(
            !out.contains('→'),
            "code block must NOT prefix `→`: {out:?}"
        );
        // Tight: no blank line between rows.
        assert!(
            !out.contains("\n\n"),
            "code block rows must be contiguous: {out:?}"
        );
    }

    #[test]
    fn code_block_shown_at_normal() {
        // Unlike note (Verbose-only), a code block is visible at Normal.
        let (r, sink, buf) = capture();
        r.render_code_block(&sink, 0, &["line".to_string()]);
        assert!(
            !buf.lock().unwrap().is_empty(),
            "code block visible at Normal"
        );
    }

    #[test]
    fn code_block_quiet_suppressed() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_code_block(&sink, 0, &["line".to_string()]);
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn note_suppressed_at_normal() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        r.render_note(&sink, 0, "long prose");
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn note_shown_at_verbose() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Verbose);
        r.render_note(&sink, 0, "line1\nline2");
        let s = buf.lock().unwrap();
        assert!(s.contains("line1"));
        assert!(s.contains("line2"));
    }
}

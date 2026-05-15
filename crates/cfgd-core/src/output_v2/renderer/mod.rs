//! The renderer is the single layout authority. It owns:
//! - indent depth (push/pop per Section)
//! - blank-line state machine (no leading, no trailing, exactly one between siblings)
//! - kv auto-batching (consecutive `kv` calls coalesce into one aligned block)
//! - glyph + style lookup via Theme
//!
//! Every other module routes terminal writes through here.
//!
//! R1 skeleton: the `render_*` emission family is wired via `Printer` (T14)
//! and the `section::*` family is wired via `SectionGuard` (T15). A few
//! internals remain reachable only from tests until later R1 tasks land
//! more dispatchers — `RenderState::{depth,push,pop}` and `indent_prefix`
//! sit behind a narrow allow so the renderer can keep them addressable
//! from inside the renderer module without a workspace-wide warning.
#![allow(dead_code)]

use std::sync::Mutex;

use super::{Theme, Verbosity};

mod glyphs;
pub mod kv;
pub mod section;
pub mod status;
pub mod table;
pub(crate) use glyphs::role_glyph;
pub use status::StatusFields;
pub use table::Table;

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
    pub(crate) section_stack: Vec<crate::output_v2::renderer::section::SectionFrame>,
}

impl RenderState {
    pub(crate) fn new() -> Self {
        Self {
            indent_depth: 0,
            blank_pending: false,
            leading: true,
            kv_buffer: Vec::new(),
            section_stack: Vec::new(),
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
    /// Per spec §6.2: a top-level emit (depth 0) reached while a `SectionGuard`
    /// is alive is a programming error. Debug builds `debug_assert!` to flag
    /// the call site loudly; release builds log a `tracing::warn!` once per
    /// process and re-route the emit to the section's current depth so the
    /// output stays readable.
    pub(crate) fn enforce_top_level_emit(&self, expected_depth: usize) -> usize {
        let actual = self.state.lock().unwrap_or_else(|e| e.into_inner()).depth();
        if expected_depth == 0 && actual > 0 {
            // Top-level emit while a section is open. Spec §6.2.
            debug_assert!(
                false,
                "top-level emit at depth 0 while section open at depth {actual}"
            );
            // Release build: warn once, render at the section's depth.
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                tracing::warn!(
                    "cfgd output_v2: top-level Printer emit reached while a SectionGuard \
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
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.leading {
            s.leading = false;
            s.blank_pending = false;
        } else if s.blank_pending {
            w.write_line("");
            s.blank_pending = false;
        }
        let prefix = "  ".repeat(depth);
        w.write_line(&format!("{}{}", prefix, body));
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
    }

    /// Heading: bold styled by Theme::header. No `=== ===` decoration. Always depth 0.
    pub fn render_heading(&self, w: &dyn Writer, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let styled = self.theme.header.apply_to(text).to_string();
        self.write_line(w, 0, &styled);
    }

    /// Bullet: glyph `-`, then space, then text. Uncolored. The renderer's only
    /// bullet glyph; `+`/`~`/`>`/`*` are forbidden.
    pub fn render_bullet(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        self.flush_pending_section_headers(w);
        self.write_line(w, depth, &format!("- {}", text));
    }

    /// Hint: arrow glyph + dim text. Shown at Normal+ (NOT Quiet). Per §12,
    /// this is the canonical "next step" surface.
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
        self.write_line(w, depth, &format!("{}{}", arrow, body));
    }

    /// Note: multi-line prose. Suppressed at both Quiet and Normal; only Verbose.
    pub fn render_note(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity != Verbosity::Verbose {
            return;
        }
        self.flush_pending_section_headers(w);
        for line in text.lines() {
            let dim = self.theme.muted.apply_to(line);
            self.write_line(w, depth, &dim.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

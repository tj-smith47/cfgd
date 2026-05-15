//! The renderer is the single layout authority. It owns:
//! - indent depth (push/pop per Section)
//! - blank-line state machine (no leading, no trailing, exactly one between siblings)
//! - kv auto-batching (consecutive `kv` calls coalesce into one aligned block)
//! - glyph + style lookup via Theme
//!
//! Every other module routes terminal writes through here.
//!
//! R1 skeleton: a handful of internals are not yet wired into emission paths:
//! `RenderState::{depth,push,pop}` and `kv_buffer` await `SectionGuard` (T15+)
//! and the kv dispatcher; `indent_prefix` is the depth helper used by future
//! dispatchers (T09+); `mark_blank_pending` is invoked by Section close
//! (T15); the `glyphs::role_glyph` re-export is consumed by status dispatchers
//! (T11+). The `dead_code` / `unused_imports` allows drop once those tasks land.
#![allow(dead_code, unused_imports)]

use std::sync::Mutex;

use super::{Theme, Verbosity};

mod glyphs;
pub(crate) use glyphs::role_glyph;

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
}

impl RenderState {
    pub(crate) fn new() -> Self {
        Self {
            indent_depth: 0,
            blank_pending: false,
            leading: true,
            kv_buffer: Vec::new(),
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
    /// Caller is responsible for kv-buffer flush.
    pub(crate) fn write_line(&self, w: &dyn Writer, depth: usize, body: &str) {
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
}

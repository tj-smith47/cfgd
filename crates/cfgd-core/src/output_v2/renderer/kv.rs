//! `render_kv` / `render_kv_block` dispatchers.
//!
//! Consecutive single-pair `render_kv` calls coalesce into one aligned
//! `KvBlock`; the buffer is flushed by the next non-kv emission, by section
//! close, or by an explicit `flush_kv_buffer`.
//!
//! ## Recursion trap
//!
//! `renderer::write_line` flushes the kv buffer at the top of its body (so
//! pending kvs render *before* a following non-kv line, not after). That means
//! a kv-emission path MUST NOT call `self.write_line(...)` — that would recurse
//! into `flush_kv_buffer_internal` → `render_kv_block_no_flush` → `write_line`.
//! Every line emission below uses `w.write_line(...)` directly, with blank-
//! pending handled inline.
use super::{Renderer, Writer};
use crate::output_v2::Verbosity;

const KEY_WIDTH_CAP: usize = 24;
/// Gap inserted between the (padded) key column and the value.
const KEY_VALUE_GAP: &str = "  ";

impl Renderer {
    /// Buffer a single kv pair. Will be aligned with adjacent kvs into one
    /// block and flushed by the next non-kv emission, by section close, or by
    /// `flush_kv_buffer`.
    pub(crate) fn render_kv(&self, key: &str, value: &str) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.kv_buffer.push((key.into(), value.into()));
    }

    /// Render a KvBlock immediately without first flushing any pending kvs.
    /// This is the `write_line`-bypassing variant — every emit uses
    /// `w.write_line(...)` directly to avoid recursing into the buffer flush
    /// performed at the top of `Renderer::write_line`.
    pub(crate) fn render_kv_block_no_flush(
        &self,
        w: &dyn Writer,
        depth: usize,
        pairs: &[(String, String)],
    ) {
        if self.verbosity == Verbosity::Quiet || pairs.is_empty() {
            return;
        }
        // Flush deferred section headers FIRST so this kv block lands under
        // them, not above. (Section header emission goes through `write_line`,
        // but that path is safe — at that moment the kv buffer has already
        // been drained by the caller.)
        self.flush_pending_section_headers(w);

        // Honor blank-pending / leading without recursing through write_line.
        // Also consume the heading-just-emitted flag: when the previous
        // emission was a top-level heading and we're still at root, re-anchor
        // this kv_block one level deeper so it visually nests under the
        // heading (spec §13.1/§13.3/§13.4). When we bump, also SUPPRESS the
        // would-be blank between heading and kv_block — spec shows the
        // heading + kv_block as one bound unit with no blank between them.
        let effective_depth = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let bump = depth == 0 && s.section_stack.is_empty() && s.last_was_top_heading;
            s.last_was_top_heading = false;
            if s.leading {
                s.leading = false;
                s.blank_pending = false;
            } else if s.blank_pending && !bump {
                w.write_line("");
                s.blank_pending = false;
            } else if bump {
                // kv_block consuming heading-flag: drop the would-be blank.
                s.blank_pending = false;
            }
            if bump { depth + 1 } else { depth }
        };

        let prefix = "  ".repeat(effective_depth);
        let key_col = pairs
            .iter()
            .map(|(k, _)| k.len())
            .max()
            .unwrap_or(0)
            .min(KEY_WIDTH_CAP);
        for (k, v) in pairs {
            if k.len() <= KEY_WIDTH_CAP {
                let key = self
                    .theme
                    .header
                    .apply_to(format!("{:<width$}", k, width = key_col));
                w.write_line(&format!("{}{}{}{}", prefix, key, KEY_VALUE_GAP, v));
            } else {
                // Long key: render on its own line, value wrapped to the
                // following line indented one extra level.
                let key = self.theme.header.apply_to(k);
                w.write_line(&format!("{}{}", prefix, key));
                w.write_line(&format!("{}  {}", prefix, v));
            }
        }
        self.mark_top_level_blank_if_at_root();
    }

    /// Render a KvBlock immediately. Public crate entry — thin forwarder to
    /// `render_kv_block_no_flush`. Callers passing a pre-built block (e.g. the
    /// Doc render path) reach the renderer through here.
    pub(crate) fn render_kv_block(&self, w: &dyn Writer, depth: usize, pairs: &[(String, String)]) {
        self.render_kv_block_no_flush(w, depth, pairs);
    }

    /// Flush any buffered kvs as one aligned block at the current depth.
    /// Public crate API — wired through `Printer::flush` (see interfaces.md).
    pub(crate) fn flush_kv_buffer(&self, w: &dyn Writer) {
        let (pairs, depth) = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if s.kv_buffer.is_empty() {
                return;
            }
            (std::mem::take(&mut s.kv_buffer), s.indent_depth)
        };
        self.render_kv_block(w, depth, &pairs);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::{Renderer, StringSink};
    use crate::output_v2::tests::strip_ansi;
    use crate::output_v2::{Theme, Verbosity};

    fn capture() -> (Renderer, StringSink, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        (r, sink, buf)
    }

    #[test]
    fn kv_block_aligns_to_max_key_in_block() {
        let (r, sink, buf) = capture();
        r.render_kv_block(
            &sink,
            0,
            &[("Foo".into(), "1".into()), ("LongerKey".into(), "2".into())],
        );
        let out = strip_ansi(&buf.lock().unwrap());
        // "Foo" padded to LongerKey.len() (= 9) + "  " gap + value.
        assert!(out.contains("Foo        1"), "got: {out:?}");
        assert!(out.contains("LongerKey  2"), "got: {out:?}");
    }

    #[test]
    fn buffered_kvs_coalesce_into_one_block() {
        let (r, sink, buf) = capture();
        r.render_kv("Foo", "1");
        r.render_kv("LongerKey", "2");
        r.flush_kv_buffer(&sink);
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Foo        1"), "got: {out:?}");
        assert!(out.contains("LongerKey  2"), "got: {out:?}");
    }

    #[test]
    fn long_key_wraps_value_to_next_line() {
        let (r, sink, buf) = capture();
        let long = "x".repeat(30);
        r.render_kv_block(&sink, 0, &[(long.clone(), "value".into())]);
        let out = strip_ansi(&buf.lock().unwrap());
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() >= 2, "expected wrapped output, got {out:?}");
        assert_eq!(lines[0], long);
        assert!(lines[1].starts_with("  value"), "got line: {:?}", lines[1]);
    }

    #[test]
    fn kv_quiet_suppressed() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_kv_block(&sink, 0, &[("Foo".into(), "1".into())]);
        assert!(buf.lock().unwrap().is_empty());
    }
}

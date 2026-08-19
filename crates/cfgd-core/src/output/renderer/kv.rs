//! `render_kv` / `render_kv_block` dispatchers.
//!
//! Consecutive single-pair `render_kv` calls coalesce into one aligned
//! `KvBlock`; the buffer is flushed by the next non-kv emission, by section
//! close, or by an explicit `flush_kv_buffer`.
//!
//! ## Recursion, and why it is safe here
//!
//! `Emitting::push_line` drains the kv buffer at the top of its body (so
//! pending kvs render *before* a following non-kv line, not after), and the
//! block below can itself reach `push_line` through a deferred section header.
//! That recursion terminates because `drain_kv_buffer` takes the buffer before
//! rendering it, so the nested drain sees an empty one.
//!
//! The rule that keeps it safe is structural rather than remembered: the block
//! is built by a collector holding `&mut RenderState`, which can reach neither
//! the state lock nor a sink, so it cannot become a second exit.
use super::{Emitting, Renderer, Writer, indent_prefix};
use crate::output::Verbosity;

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

    /// Render a KvBlock immediately. Public crate entry — callers passing a
    /// pre-built block (e.g. the Doc render path) reach the renderer here.
    pub(crate) fn render_kv_block(&self, w: &dyn Writer, depth: usize, pairs: &[(String, String)]) {
        self.emit_with(w, |e| e.render_kv_block(depth, pairs));
    }

    /// Flush any buffered kvs as one aligned block at the current depth.
    /// Public crate API — wired through `Printer::flush` (see interfaces.md).
    pub(crate) fn flush_kv_buffer(&self, w: &dyn Writer) {
        self.emit_with(w, |e| e.drain_kv_buffer());
    }
}

impl Emitting<'_> {
    /// Collect one aligned kv block at `depth`.
    pub(crate) fn render_kv_block(&mut self, depth: usize, pairs: &[(String, String)]) {
        if self.verbosity == Verbosity::Quiet || pairs.is_empty() {
            return;
        }
        // Collect deferred section headers FIRST so this kv block lands under
        // them, not above.
        self.flush_section_headers();

        // Honor blank-pending / leading. Also consume the heading-just-emitted
        // flag: when the previous emission was a top-level heading and we're
        // still at root, re-anchor this kv_block one level deeper so it
        // visually nests under the heading. When we bump, also SUPPRESS the
        // would-be blank between heading and kv_block — heading + kv_block
        // render as one bound unit with no blank between them.
        let bump =
            depth == 0 && self.state.section_stack.is_empty() && self.state.last_was_top_heading;
        self.state.last_was_top_heading = false;
        if self.state.leading {
            self.state.leading = false;
            self.state.blank_pending = false;
        } else if self.state.blank_pending && !bump {
            self.out.push(String::new());
            self.state.blank_pending = false;
        } else if bump {
            // kv_block consuming heading-flag: drop the would-be blank.
            self.state.blank_pending = false;
        }
        let effective_depth = if bump { depth + 1 } else { depth };

        let prefix = indent_prefix(effective_depth);
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
                    .secondary
                    .apply_to(format!("{:<width$}", k, width = key_col));
                self.out
                    .push(format!("{}{}{}{}", prefix, key, KEY_VALUE_GAP, v));
            } else {
                // Long key: render on its own line, value wrapped to the
                // following line indented one extra level.
                let key = self.theme.secondary.apply_to(k);
                self.out.push(format!("{}{}", prefix, key));
                self.out.push(format!("{}  {}", prefix, v));
            }
        }
        self.mark_top_level_group(super::TopGroup::KvBlock);
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
    fn kv_block_aligns_to_max_key_in_block() {
        let (r, sink, buf) = capture();
        r.render_kv_block(
            &sink,
            0,
            &[("Foo".into(), "1".into()), ("LongerKey".into(), "2".into())],
        );
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("Foo        1"), "got: {out:?}");
        assert!(out.contains("LongerKey  2"), "got: {out:?}");
    }

    #[test]
    fn long_key_wraps_value_to_next_line() {
        let (r, sink, buf) = capture();
        let long = "x".repeat(30);
        r.render_kv_block(&sink, 0, &[(long.clone(), "value".into())]);
        let out = crate::test_helpers::captured_text(&buf);
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
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }

    /// Keys are a structural pivot, not a header: they take `theme.secondary`.
    /// A key painted with `theme.header` competes with the section header
    /// above it for the same weight.
    #[test]
    #[serial_test::serial]
    fn kv_keys_take_the_secondary_slot() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(
            Theme::from_preset("dracula").with_colors(true),
            Verbosity::Normal,
        );
        r.render_kv_block(&sink, 0, &[("Profile".into(), "work".into())]);
        // raw-capture-ok: asserting on the raw secondary-slot SGR bytes themselves — captured_text would strip the ANSI this test exists to check
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let theme = Theme::from_preset("dracula").with_colors(true);
        assert!(
            out.contains(&theme.secondary.apply_to("Profile").to_string()),
            "key is not painted with the secondary slot: {out:?}"
        );
        assert!(
            !out.contains(&theme.header.apply_to("Profile").to_string()),
            "key is still painted with the header slot: {out:?}"
        );
    }
}

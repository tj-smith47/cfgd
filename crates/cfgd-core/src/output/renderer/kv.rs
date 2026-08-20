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
use unicode_width::UnicodeWidthStr;

use super::{Emitting, Renderer, Writer, indent_prefix};
use crate::output::{KvPair, Verbosity, cursor_safe};

/// `text` followed by enough spaces to reach `width` TERMINAL COLUMNS.
///
/// `format!("{:<width$}", …)` pads by char count, which over-pads a
/// multi-byte key and under-pads a zero-width one — the key column is
/// measured in columns (the same measure `render_table` pads by), so it has
/// to be filled in columns or every value after a non-ASCII key sits one
/// position off.
fn pad_to_width(text: &str, width: usize) -> String {
    let pad = width.saturating_sub(UnicodeWidthStr::width(text));
    format!("{text}{}", " ".repeat(pad))
}

const KEY_WIDTH_CAP: usize = 24;
/// Gap inserted between the (padded) key column and the value.
const KEY_VALUE_GAP: &str = "  ";

impl Renderer {
    /// Buffer a single kv pair. Will be aligned with adjacent kvs into one
    /// block and flushed by the next non-kv emission, by section close, or by
    /// `flush_kv_buffer`.
    pub(crate) fn render_kv(&self, key: &str, value: &str) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.kv_buffer.push(KvPair::new(key, value));
    }

    /// Render a KvBlock immediately. Public crate entry — callers passing a
    /// pre-built block (e.g. the Doc render path) reach the renderer here.
    pub(crate) fn render_kv_block(&self, w: &dyn Writer, depth: usize, pairs: &[KvPair]) {
        self.emit_with(w, |e| e.render_kv_block(depth, pairs));
    }

    /// Render a CommandList immediately. Public crate entry — the Doc render
    /// path reaches the renderer here.
    pub(crate) fn render_command_list(
        &self,
        w: &dyn Writer,
        depth: usize,
        pairs: &[(String, String)],
    ) {
        self.emit_with(w, |e| e.render_command_list(depth, pairs));
    }

    /// Flush any buffered kvs as one aligned block at the current depth.
    /// Public crate API — wired through `Printer::flush` (see interfaces.md).
    pub(crate) fn flush_kv_buffer(&self, w: &dyn Writer) {
        self.emit_with(w, |e| e.drain_kv_buffer());
    }
}

impl Emitting<'_> {
    /// Preamble shared by every aligned block renderer (`render_kv_block`,
    /// `render_command_list`): flush deferred section headers so the block
    /// lands under them, not above; honor blank-pending/leading; and consume
    /// the heading-just-emitted flag — when the previous emission was a
    /// top-level heading and we're still at root, re-anchor this block one
    /// level deeper so it visually nests under the heading, SUPPRESSING the
    /// would-be blank between them (heading + block render as one bound
    /// unit). Returns the depth-appropriate indent prefix. Column width and
    /// glue are the one thing each caller still owns — those differ per
    /// block kind, which is the whole justification for two callers sharing
    /// one preamble instead of one renderer for both.
    fn open_aligned_block(&mut self, depth: usize) -> String {
        self.flush_section_headers();

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
            self.state.blank_pending = false;
        }
        let effective_depth = if bump { depth + 1 } else { depth };
        indent_prefix(effective_depth)
    }

    /// Collect one aligned kv block at `depth`.
    pub(crate) fn render_kv_block(&mut self, depth: usize, pairs: &[KvPair]) {
        if self.verbosity == Verbosity::Quiet || pairs.is_empty() {
            return;
        }
        let prefix = self.open_aligned_block(depth);
        // Both halves of a row name things cfgd did not author — a gateway's
        // device id, a source manifest's description, a module's own file
        // paths — so both are folded here rather than at the call sites.
        // Sanitizing at the renderer is what makes it uniform: a row cannot be
        // added to this surface in an unfolded state, and the annotation slot
        // below is the only way styling reaches a value.
        let rows: Vec<(String, String)> = pairs
            .iter()
            .map(|p| (cursor_safe(&p.key), self.compose_kv_value(p)))
            .collect();
        let key_col = rows
            .iter()
            .map(|(k, _)| UnicodeWidthStr::width(k.as_str()))
            .max()
            .unwrap_or(0)
            .min(KEY_WIDTH_CAP);
        for (k, v) in &rows {
            if UnicodeWidthStr::width(k.as_str()) <= KEY_WIDTH_CAP {
                let key = self.theme.secondary.apply_to(pad_to_width(k, key_col));
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

    /// The rendered value column of one row: the folded value, plus the
    /// annotation in the renderer's own muted coat when the row carries one.
    ///
    /// An annotation with no value of its own stands alone as the row —
    /// parenthesising it would enclose the whole column and read as an aside
    /// about nothing.
    fn compose_kv_value(&self, pair: &KvPair) -> String {
        let value = cursor_safe(&pair.value);
        let Some(annotation) = pair.annotation.as_deref().filter(|a| !a.is_empty()) else {
            return value;
        };
        let annotation = cursor_safe(annotation);
        if value.is_empty() {
            self.theme.muted.apply_to(annotation).to_string()
        } else {
            let note = self.theme.muted.apply_to(format!("({annotation})"));
            format!("{value} {note}")
        }
    }

    /// Collect one aligned "command — description" block at `depth`.
    ///
    /// `render_kv_block`'s counterpart for a list whose left column is a
    /// shell command rather than a data-carrying key. Two things differ, both
    /// deliberate: the key column carries no `KEY_WIDTH_CAP` (a wrapped
    /// command severs the exact glue that makes the list scannable, and no
    /// command list in the product runs wide enough for an uncapped column to
    /// become the readability problem the cap exists to prevent for arbitrary
    /// key/value pairs); and the glue is `" — "` — the same em-dash a status
    /// subject/detail pair renders with — never the plain whitespace gap
    /// `render_kv_block` uses, because this is a list of DESCRIPTIONS, not a
    /// list of VALUES.
    pub(crate) fn render_command_list(&mut self, depth: usize, pairs: &[(String, String)]) {
        if self.verbosity == Verbosity::Quiet || pairs.is_empty() {
            return;
        }
        let prefix = self.open_aligned_block(depth);
        // Folded before the key column is measured, for the same reason
        // `render_kv_block` folds: the width has to describe the text that
        // actually renders.
        let rows: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (cursor_safe(k), cursor_safe(v)))
            .collect();
        let key_col = rows
            .iter()
            .map(|(k, _)| UnicodeWidthStr::width(k.as_str()))
            .max()
            .unwrap_or(0);
        for (k, v) in &rows {
            let key = self.theme.secondary.apply_to(pad_to_width(k, key_col));
            self.out.push(format!("{}{} — {}", prefix, key, v));
        }
        self.mark_top_level_group(super::TopGroup::KvBlock);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::{Renderer, StringSink};

    use crate::output::{KvPair, Theme, Verbosity};

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
            &[KvPair::new("Foo", "1"), KvPair::new("LongerKey", "2")],
        );
        let out = crate::test_helpers::captured_text(&buf);
        // "Foo" padded to LongerKey.len() (= 9) + "  " gap + value.
        assert!(out.contains("Foo        1"), "got: {out:?}");
        assert!(out.contains("LongerKey  2"), "got: {out:?}");
    }

    /// The key column is measured and filled in TERMINAL COLUMNS, not bytes
    /// or chars. `"Größe"` is 6 bytes and 5 columns, so a byte-measured
    /// column pads it one position too far and pushes every value in the
    /// block out of line with the ones above it.
    #[test]
    fn kv_block_aligns_a_multibyte_key_by_columns_not_bytes() {
        let (r, sink, buf) = capture();
        r.render_kv_block(
            &sink,
            0,
            &[KvPair::new("Größe", "1"), KvPair::new("ID", "2")],
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("Größe  1"), "got: {out:?}");
        assert!(out.contains("ID     2"), "got: {out:?}");
    }

    /// `command_list` pads through the same helper and takes its width from
    /// the same measure.
    #[test]
    fn command_list_aligns_a_multibyte_command_by_columns_not_bytes() {
        let (r, sink, buf) = capture();
        r.render_command_list(
            &sink,
            0,
            &[
                ("größe".to_string(), "one".to_string()),
                ("ls".to_string(), "two".to_string()),
            ],
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("größe — one"), "got: {out:?}");
        assert!(out.contains("ls    — two"), "got: {out:?}");
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
        r.render_kv_block(&sink, 0, &[KvPair::new(long.clone(), "value")]);
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
        r.render_kv_block(&sink, 0, &[KvPair::new("Foo", "1")]);
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
        r.render_kv_block(&sink, 0, &[KvPair::new("Profile", "work")]);
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

    /// The annotation is the renderer's, not the caller's: it lands
    /// parenthesised after the value, in the muted coat, from a slot of its
    /// own — so the value beside it can be folded unconditionally.
    #[test]
    fn an_annotation_renders_parenthesised_after_its_value() {
        let (r, sink, buf) = capture();
        r.render_kv_block(
            &sink,
            0,
            &[KvPair::annotated(
                "Modules",
                "nvim, zsh",
                "tmux skipped: linux",
            )],
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("Modules  nvim, zsh (tmux skipped: linux)"),
            "got: {out:?}"
        );
    }

    /// With no value of its own the annotation stands alone: parentheses
    /// around the whole column would read as an aside about nothing.
    #[test]
    fn an_annotation_with_no_value_stands_alone_unparenthesised() {
        let (r, sink, buf) = capture();
        r.render_kv_block(&sink, 0, &[KvPair::annotated("Modules", "", "all skipped")]);
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("Modules  all skipped"), "got: {out:?}");
        assert!(!out.contains('('), "got: {out:?}");
    }

    /// The annotation slot takes the muted coat wherever the value sits, so a
    /// row cannot be annotated in one weight beside a row annotated in another.
    #[test]
    #[serial_test::serial]
    fn an_annotation_is_painted_muted_by_the_renderer() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let theme = Theme::from_preset("dracula").with_colors(true);
        let r = Renderer::new(theme.clone(), Verbosity::Normal);
        r.render_kv_block(&sink, 0, &[KvPair::annotated("Modules", "nvim", "skipped")]);
        // raw-capture-ok: the claim IS the muted SGR the renderer wraps the annotation in, which captured_text would strip
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            out.contains(&theme.muted.apply_to("(skipped)").to_string()),
            "annotation is not painted with the muted slot: {out:?}"
        );
    }

    #[test]
    fn command_list_glues_with_em_dash_not_a_whitespace_gap() {
        let (r, sink, buf) = capture();
        r.render_command_list(
            &sink,
            0,
            &[("cfgd apply".into(), "apply configuration".into())],
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains("cfgd apply — apply configuration"),
            "got: {out:?}"
        );
    }

    /// A command past `KEY_WIDTH_CAP` (24) stays on ONE line with its
    /// description: wrapping it would sever the `" — "` glue that makes the
    /// list scannable, unlike `render_kv_block`, which wraps a long key onto
    /// its own line ahead of an ordinary value.
    #[test]
    fn command_list_never_wraps_a_key_past_the_kv_width_cap() {
        let (r, sink, buf) = capture();
        let long_command = "cfgd module create <name>"; // 26 chars, > KEY_WIDTH_CAP
        r.render_command_list(&sink, 0, &[(long_command.into(), "create a module".into())]);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "expected one line, got: {out:?}");
        assert!(
            lines[0].contains("cfgd module create <name> — create a module"),
            "got: {:?}",
            lines[0]
        );
    }

    #[test]
    fn command_list_aligns_every_row_to_the_widest_key_uncapped() {
        let (r, sink, buf) = capture();
        r.render_command_list(
            &sink,
            0,
            &[
                ("cfgd apply".into(), "apply configuration".into()),
                ("cfgd module create <name>".into(), "create a module".into()),
            ],
        );
        let out = crate::test_helpers::captured_text(&buf);
        // Both keys pad to the widest (26 chars) before the glue, so the
        // glue column lines up across rows.
        assert!(
            out.contains("cfgd apply                — apply configuration"),
            "got: {out:?}"
        );
        assert!(
            out.contains("cfgd module create <name> — create a module"),
            "got: {out:?}"
        );
    }

    /// A `command_list` at depth 0 immediately under a top-level heading
    /// re-anchors one level deeper and binds to the heading with no blank
    /// line between them — the exact `open_aligned_block` bump behaviour
    /// `render_kv_block` already exercised, now proven for its sibling too
    /// so the shared preamble cannot silently diverge for one caller.
    #[test]
    fn command_list_nests_under_a_top_level_heading_with_no_blank() {
        let (r, sink, buf) = capture();
        r.render_heading(&sink, "Next Steps");
        r.render_command_list(
            &sink,
            0,
            &[("cfgd apply".into(), "apply configuration".into())],
        );
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            vec!["Next Steps", "  cfgd apply — apply configuration"],
            "got: {out:?}"
        );
    }

    #[test]
    fn command_list_quiet_suppressed() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_command_list(&sink, 0, &[("cfgd apply".into(), "apply".into())]);
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }
}

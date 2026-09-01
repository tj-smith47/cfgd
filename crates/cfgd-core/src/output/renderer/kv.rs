//! `render_kv` / `render_kv_block` dispatchers.
//!
//! Consecutive single-pair `render_kv` calls coalesce into one aligned
//! `KvBlock`; the buffer is flushed by the next non-kv emission, by section
//! close, or by an explicit `flush_kv_buffer`.
//!
//! ## Recursion, and why it is safe here
//!
//! `Emitting::push_line` drains both deferred buffers at the top of its body
//! (so pending kvs render *before* a following non-kv line, not after), and
//! `open_aligned_block` below reaches `flush_section_headers`, which drains
//! again between the two halves of its depth-partitioned header push. That
//! recursion terminates twice over: `drain_kv_buffer` takes the buffer before
//! rendering it, so the nested drain sees an empty one, and the outer flush
//! marked every frame `header_emitted` before pushing, so the nested flush
//! collects no header either.
//!
//! Whether a block lands above or below a still-deferred header is decided in
//! `section::Emitting::push_deferred_headers`, not here: the headers are split
//! at the rows' anchor depth and the drain runs between the halves, with each
//! header written through `push_line_undrained` so no header re-enters the
//! drain and re-orders what it just placed.
//!
//! The rule that keeps it safe is structural rather than remembered: the block
//! is built by a collector holding `&mut RenderState`, which can reach neither
//! the state lock nor a sink, so it cannot become a second exit.
use unicode_width::UnicodeWidthStr;

use super::{Emitting, Renderer, Writer, indent_prefix};
use crate::output::{CommandPair, KvPair, Verbosity, cursor_safe};

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
/// How far a [`KvPair::nested`] row's key sits inside the block's key column.
const NESTED_KEY_INDENT: usize = 2;
/// The glue between a `command_list` row's two columns, rendered with one
/// space either side.
const GLUE_DASH: &str = "—";

impl Renderer {
    /// Buffer a single kv pair. Will be aligned with adjacent kvs into one
    /// block and flushed by the next non-kv emission, by section close, or by
    /// `flush_kv_buffer`.
    pub(crate) fn render_kv(&self, key: &str, value: &str) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // The first row anchors the block. Placement is a fact about where
        // the rows were WRITTEN, and the drain can happen after a section has
        // opened — which moves the indent and empties the heading binding out
        // from under rows that belong to the heading.
        if s.kv_buffer.is_empty() {
            s.kv_anchor = Some(s.kv_anchor_here());
        }
        s.kv_buffer.push(KvPair::new(key, value));
    }

    /// Render a KvBlock immediately. Public crate entry — callers passing a
    /// pre-built block (e.g. the Doc render path) reach the renderer here.
    pub(crate) fn render_kv_block(&self, w: &dyn Writer, depth: usize, pairs: &[KvPair]) {
        self.emit_with(w, |e| e.render_kv_block(depth, pairs));
    }

    /// Render a CommandList immediately. Public crate entry — the Doc render
    /// path reaches the renderer here.
    pub(crate) fn render_command_list(&self, w: &dyn Writer, depth: usize, pairs: &[CommandPair]) {
        self.emit_with(w, |e| e.render_command_list(depth, pairs));
    }

    /// Flush any buffered kvs as one aligned block at the current depth.
    /// Public crate API — wired through `Printer::flush` (see interfaces.md).
    pub(crate) fn flush_kv_buffer(&self, w: &dyn Writer) {
        self.emit_with(w, |e| e.drain_buffers());
    }
}

impl Emitting<'_> {
    /// Preamble shared by every aligned block renderer (`render_kv_block`,
    /// `render_command_list`): flush deferred section headers so the block
    /// lands under them, not above; honor blank-pending/leading; and consume
    /// the heading-just-emitted flag — when the previous emission was a
    /// top-level heading and the block is still at root, re-anchor it one
    /// level deeper so it visually nests under the heading, SUPPRESSING the
    /// would-be blank between them (heading + block render as one bound
    /// unit). Returns the depth-appropriate indent prefix. Column width and
    /// glue are the one thing each caller still owns — those differ per
    /// block kind, which is the whole justification for several callers
    /// sharing one preamble instead of one renderer for all of them.
    ///
    /// `render_paragraph` takes it too, though it aligns nothing: what it
    /// wants is the heading binding, which is a fact about placement rather
    /// than about columns — body text written under a top-level heading nests
    /// beneath it exactly as a kv block written there does.
    pub(super) fn open_aligned_block(
        &mut self,
        depth: usize,
        bound_to_heading: Option<bool>,
    ) -> String {
        // The rows below are pushed straight into `out`, never through
        // `push_line`, so this is the block's only chance to let a section's
        // held-back statuses out ahead of it.
        self.drain_pending_statuses();
        self.flush_section_headers();

        let bump = bound_to_heading.unwrap_or(
            depth == 0 && self.state.section_stack.is_empty() && self.state.last_was_top_heading,
        );
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

    /// Collect one aligned kv block at `depth`, judging its placement from the
    /// state as it stands now — for a block handed in whole (the `Doc` render
    /// path), where now IS when its rows were written.
    pub(crate) fn render_kv_block(&mut self, depth: usize, pairs: &[KvPair]) {
        self.render_kv_block_anchored(depth, pairs, None);
    }

    /// Collect one aligned kv block at `depth`, with `anchor` carrying the
    /// placement the rows were written under: whether they bind to a heading
    /// above them, and whether they are a top-level group that owes the next
    /// emission a blank line. `None` judges both from the current state.
    pub(crate) fn render_kv_block_anchored(
        &mut self,
        depth: usize,
        pairs: &[KvPair],
        anchor: Option<super::KvAnchor>,
    ) {
        if self.verbosity == Verbosity::Quiet || pairs.is_empty() {
            return;
        }
        let prefix = self.open_aligned_block(depth, anchor.map(|a| a.bound_to_heading));
        // Both halves of a row name things cfgd did not author — a gateway's
        // device id, a source manifest's description, a module's own file
        // paths — so both are folded here rather than at the call sites.
        // Sanitizing at the renderer is what makes it uniform: a row cannot be
        // added to this surface in an unfolded state, and the annotation slot
        // below is the only way styling reaches a value.
        let rows: Vec<(String, String, usize)> = pairs
            .iter()
            .map(|p| {
                let indent = if p.nested { NESTED_KEY_INDENT } else { 0 };
                (cursor_safe(&p.key), self.compose_kv_value(p), indent)
            })
            .collect();
        // Measured over the INDENTED width, so a nested breakdown's values line
        // up with the values of the rows it sits under rather than starting two
        // columns to their right.
        let measured = rows
            .iter()
            .map(|(k, _, indent)| indent + UnicodeWidthStr::width(k.as_str()))
            .max()
            .unwrap_or(0)
            .min(KEY_WIDTH_CAP);
        // One key column per section, whatever else the section printed in
        // between: the width is carried on the open frame, so a later block of
        // shorter keys pads to the column the reader is already scanning.
        let key_col = match self.state.section_stack.last_mut() {
            Some(frame) => {
                let width = match frame.kv_key_col {
                    Some((at, width)) if at == depth => width.max(measured),
                    _ => measured,
                };
                frame.kv_key_col = Some((depth, width));
                width
            }
            None => measured,
        };
        for (k, v, indent) in &rows {
            let pad = " ".repeat(*indent);
            if indent + UnicodeWidthStr::width(k.as_str()) <= KEY_WIDTH_CAP {
                let key = self
                    .theme
                    .secondary
                    .apply_to(pad_to_width(k, key_col.saturating_sub(*indent)));
                self.out
                    .push(format!("{}{}{}{}{}", prefix, pad, key, KEY_VALUE_GAP, v));
            } else {
                // Long key: render on its own line, value wrapped to the
                // following line indented one extra level.
                let key = self.theme.secondary.apply_to(k);
                self.out.push(format!("{}{}{}", prefix, pad, key));
                self.out.push(format!("{}{}  {}", prefix, pad, v));
            }
        }
        match anchor {
            Some(a) => {
                self.mark_group_written_at_top_level(super::TopGroup::KvBlock, a.at_top_level)
            }
            None => self.mark_top_level_group(super::TopGroup::KvBlock),
        }
    }

    /// The rendered value column of one row: the folded value, tinted with its
    /// role's theme slot when the row carries one, plus the annotation in the
    /// renderer's own muted coat when the row carries that.
    ///
    /// The tint goes on AFTER the fold, never before: `cursor_safe` strips
    /// ANSI, so a coat applied first would be eaten by the very fold that makes
    /// the untrusted half safe. That ordering is why the role travels in a slot
    /// of its own rather than as a pre-painted value.
    ///
    /// An annotation with no value of its own stands alone as the row —
    /// parenthesising it would enclose the whole column and read as an aside
    /// about nothing.
    ///
    /// A row carrying owner tokens takes their three-slot coat in place of the
    /// role tint: the two say different things about the value, and only one
    /// of them can be its colour.
    fn compose_kv_value(&self, pair: &KvPair) -> String {
        // A linked value is the link's TEXT only where the terminal can open
        // it; anywhere else the URL is the value, because a partial path is
        // something no reader can click and no terminal auto-links.
        let shown = match pair.link.as_deref() {
            Some(url) if !self.theme.hyperlinks() => url,
            _ => pair.value.as_str(),
        };
        let value = if pair.owners.is_empty() {
            match pair.value_role {
                Some(role) if !shown.is_empty() => {
                    let (_, style) = super::role_glyph(self.theme, role);
                    style.apply_to(cursor_safe(shown)).to_string()
                }
                _ => cursor_safe(shown),
            }
        } else {
            // Each token paints its own three slots and folds each of them, so
            // the row's value is assembled from already-safe pieces.
            pair.owners
                .iter()
                .map(|owner| owner.styled(self.theme))
                .collect::<Vec<_>>()
                .join(crate::reconciler::Owner::TOKEN_SEPARATOR)
        };
        let value = match pair.link.as_deref() {
            Some(url) if self.theme.hyperlinks() => {
                crate::output::osc8_hyperlink(&cursor_safe(url), &value)
            }
            _ => value,
        };
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

    /// The rendered left column of one `command_list` row: the folded key in
    /// the renderer's own key coat, with the row's `type_span` (when it names
    /// one) painted `theme.type_hint` instead.
    ///
    /// The coat goes on AFTER the fold, exactly as `compose_kv_value`'s role
    /// tint does and for the same reason: `cursor_safe` strips ANSI, so a
    /// caller that painted the span itself would have it eaten by the very
    /// fold that makes the untrusted half safe. Stripped, the three joined
    /// spans are byte-identical to the single-coat row this replaced; with
    /// colour off but styling live the type slot still emits its own
    /// attributes, as every attribute-carrying slot in the product does.
    ///
    /// A span the key does not contain paints nothing — a row cannot half-tint
    /// itself over a key the fold reshaped. The match is anchored at the END of
    /// the key, because every producer composes the span as the key's tail: a
    /// field name that happens to repeat the span's text would otherwise tint
    /// the earlier run and leave the real type bare.
    fn paint_command_key(&self, key: &str, type_span: &Option<String>) -> String {
        let Some(span) = type_span.as_deref().filter(|s| !s.is_empty()) else {
            return self.theme.secondary.apply_to(key).to_string();
        };
        let Some(at) = key.rfind(span) else {
            return self.theme.secondary.apply_to(key).to_string();
        };
        // An empty segment takes no coat at all: a span sitting at either end
        // of the key (the widest row in a list is padded to nothing) would
        // otherwise emit an open/reset pair around zero rendered columns.
        let coat = |s: &str| {
            if s.is_empty() {
                String::new()
            } else {
                self.theme.secondary.apply_to(s).to_string()
            }
        };
        format!(
            "{}{}{}",
            coat(&key[..at]),
            self.theme.type_hint.apply_to(span),
            coat(&key[at + span.len()..]),
        )
    }

    /// Collect one aligned "command — description" block at `depth`.
    ///
    /// `render_kv_block`'s counterpart for a list whose left column names a
    /// thing rather than carrying data — a shell command the user types, a
    /// schema field's `name <type>` — and whose right column DESCRIBES it.
    /// Three things differ from a kv block, all deliberate: the key column
    /// carries no `KEY_WIDTH_CAP` (a wrapped left column severs the exact glue
    /// that makes the list scannable, and no such list in the product runs
    /// wide enough for an uncapped column to become the readability problem
    /// the cap exists to prevent for arbitrary key/value pairs); the glue is
    /// `" — "` — the same em-dash a status subject/detail pair renders with —
    /// never the plain whitespace gap `render_kv_block` uses, because this is
    /// a list of DESCRIPTIONS, not a list of VALUES; and a description too
    /// long for the window hangs at the DESCRIPTION column rather than at the
    /// left one, so its tail reads as the tail of the row above it instead of
    /// as another row whose left column happens to be blank.
    ///
    /// A row with no description at all renders its left column alone: the
    /// em-dash would introduce nothing, and the padding ahead of it would be
    /// trailing whitespace.
    pub(crate) fn render_command_list(&mut self, depth: usize, pairs: &[CommandPair]) {
        if self.verbosity == Verbosity::Quiet || pairs.is_empty() {
            return;
        }
        let prefix = self.open_aligned_block(depth, None);
        // Folded before the key column is measured, for the same reason
        // `render_kv_block` folds: the width has to describe the text that
        // actually renders.
        let rows: Vec<(String, String, Option<String>)> = pairs
            .iter()
            .map(|p| {
                (
                    cursor_safe(&p.key),
                    cursor_safe(&p.value),
                    p.type_span.as_deref().map(cursor_safe),
                )
            })
            .collect();
        let key_col = rows
            .iter()
            .map(|(k, _, _)| UnicodeWidthStr::width(k.as_str()))
            .max()
            .unwrap_or(0);
        for (k, v, type_span) in &rows {
            if v.is_empty() {
                self.out.push(format!(
                    "{}{}",
                    prefix,
                    self.paint_command_key(k, type_span)
                ));
                continue;
            }
            let key = self.paint_command_key(&pad_to_width(k, key_col), type_span);
            let opening = format!("{}{} {} ", prefix, key, GLUE_DASH);
            // Measured from the parts rather than from `opening`, which
            // carries the key's SGR: the prefix is spaces, the padded key is
            // `key_col` columns by construction, and the glue is space +
            // em-dash + space.
            let hang = " ".repeat(UnicodeWidthStr::width(prefix.as_str()) + key_col + 3);
            for physical in super::wrap::wrap_segment(v, &opening, &hang, self.wrap_cols) {
                self.out.push(physical);
            }
        }
        self.mark_top_level_group(super::TopGroup::KvBlock);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::{Renderer, StringSink};

    use crate::output::renderer::StatusFields;
    use crate::output::{KvPair, Role, Theme, Verbosity};

    fn capture() -> (Renderer, StringSink, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        (r, sink, buf)
    }

    use super::super::NarrowSink;

    fn cp(k: &str, v: &str) -> crate::output::CommandPair {
        crate::output::CommandPair::from((k, v))
    }

    fn narrow(cols: usize) -> (Renderer, NarrowSink, Arc<Mutex<String>>) {
        let (r, sink, buf) = capture();
        (r, NarrowSink(sink, cols), buf)
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

    /// One section, one key column — even when the section printed something
    /// else between its two kv emissions. `cfgd module push` writes its header
    /// facts, pushes (a spinner and a status line), then writes `Digest`; the
    /// second emission measured only itself, so a six-character key sat left of
    /// the nine-character ones above it.
    #[test]
    fn a_section_keeps_one_key_column_across_everything_it_prints() {
        let (printer, buf) = crate::output::Printer::for_test_at(Verbosity::Normal);
        {
            let sec = printer.section("Push Module");
            sec.kv_block([("Directory", "./mod"), ("Artifact", "ghcr.io/a/b:v1")]);
            sec.status_simple(Role::Ok, "Pushed module");
            sec.kv("Digest", "sha256:abc");
        }
        drop(printer);
        let out = crate::test_helpers::captured_text(&buf);
        let value_column = |value: &str| {
            out.lines()
                .find_map(|l| l.find(value))
                .unwrap_or_else(|| panic!("{value} must be rendered: {out:?}"))
        };
        assert_eq!(
            value_column("./mod"),
            value_column("sha256:abc"),
            "the result row pads to the header's column: {out:?}"
        );

        // The carrier IS the section frame: the same two emissions with no
        // frame to hold the width measure themselves, which is the jog the
        // frame removes.
        let (r, sink, raw) = capture();
        r.render_kv_block(
            &sink,
            0,
            &[
                KvPair::new("Directory", "./mod"),
                KvPair::new("Artifact", "ghcr.io/a/b:v1"),
            ],
        );
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Ok,
                subject: "Pushed module",
                detail: None,
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
                verdict: None,
            },
        );
        r.render_kv_block(&sink, 0, &[KvPair::new("Digest", "sha256:abc")]);
        let unframed = crate::test_helpers::captured_text(&raw);
        let unframed_column = |value: &str| {
            unframed
                .lines()
                .find_map(|l| l.find(value))
                .unwrap_or_else(|| panic!("{value} must be rendered: {unframed:?}"))
        };
        assert_ne!(
            unframed_column("./mod"),
            unframed_column("sha256:abc"),
            "no frame, no shared column — the section is what carries it: {unframed:?}"
        );
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

    /// A description too long for the window hangs at the DESCRIPTION column,
    /// not at the left one: its tail is the tail of the row above it, and a
    /// continuation starting under the command reads as another row whose
    /// description happens to be missing.
    #[test]
    fn a_long_description_hangs_under_the_description_column() {
        let (r, sink, buf) = narrow(40);
        r.render_command_list(
            &sink,
            0,
            &[cp(
                "cfgd apply",
                "reconcile every declared surface on this machine",
            )],
        );
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines.len() > 1,
            "expected a wrap at 40 columns, got: {out:?}"
        );
        assert!(lines[0].starts_with("cfgd apply — "), "got: {:?}", lines[0]);
        // "cfgd apply" (10) + " — " (3) = the description column.
        let hang = " ".repeat(13);
        for line in &lines[1..] {
            assert!(
                line.starts_with(&hang) && !line[13..].starts_with(' '),
                "continuation is not flush with the description column: {line:?}"
            );
        }
    }

    /// The hang is the width of the padded key column, not of this row's own
    /// key: every description in the block starts in the same column, so
    /// every continuation must too.
    #[test]
    fn the_hang_follows_the_padded_column_not_the_row_key() {
        let (r, sink, buf) = narrow(44);
        r.render_command_list(
            &sink,
            0,
            &[
                cp("ls", "list every declared module by name"),
                cp("cfgd module list", "short"),
            ],
        );
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines.len() > 2,
            "expected the first row to wrap, got: {out:?}"
        );
        // "cfgd module list" (16) + " — " (3) — the column the SECOND row's
        // description starts in, which the first row's continuation shares.
        assert_eq!(
            lines[1].len() - lines[1].trim_start().len(),
            19,
            "continuation column: {:?}",
            lines[1]
        );
    }

    /// Colour is a coat, never a column: the opening prefix carries the key's
    /// SGR, and measuring that as text wraps every styled row ~13 columns
    /// early — the default path on any colour terminal, and invisible to every
    /// other test here because a capture pins colour off.
    #[test]
    fn colour_does_not_move_the_wrap_point() {
        let rows = [cp(
            "cfgd apply",
            "reconcile every declared surface on this machine",
        )];
        let (plain, plain_sink, plain_buf) = narrow(40);
        plain.render_command_list(&plain_sink, 0, &rows);

        let buf = Arc::new(Mutex::new(String::new()));
        let sink = NarrowSink(StringSink(buf.clone()), 40);
        let colored = Renderer::new(Theme::default().with_colors(true), Verbosity::Normal);
        colored.render_command_list(&sink, 0, &rows);

        assert!(
            // raw-capture-ok: the claim IS that SGR was emitted, which a stripping read cannot see
            buf.lock().expect("capture").contains('\u{1b}'),
            "the colour-on render carried no SGR, so this proves nothing"
        );
        assert_eq!(
            crate::test_helpers::captured_text(&buf),
            crate::test_helpers::captured_text(&plain_buf),
            "styling changed the layout"
        );
    }

    /// A hang deep enough to leave no usable column chops words mid-way in a
    /// phantom column; the row is left for the terminal's own hard wrap.
    #[test]
    fn a_hang_with_no_usable_column_leaves_the_row_unwrapped() {
        let (r, sink, buf) = narrow(60);
        r.render_command_list(
            &sink,
            0,
            &[cp(
                "cfgd module registry add https://example",
                "register a module registry and refresh its index",
            )],
        );
        let out = crate::test_helpers::captured_text(&buf);
        assert_eq!(out.lines().count(), 1, "expected no wrap, got: {out:?}");
    }

    /// A row with no description renders its command alone: the em-dash would
    /// introduce nothing, and the padding ahead of it would be trailing
    /// whitespace on the line.
    #[test]
    fn a_row_with_no_description_renders_no_glue() {
        let (r, sink, buf) = capture();
        r.render_command_list(
            &sink,
            0,
            &[cp("object", ""), cp("string", "a bare command string")],
        );
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "object", "got: {out:?}");
        assert_eq!(lines[1], "string — a bare command string", "got: {out:?}");
    }

    /// `command_list` pads through the same helper and takes its width from
    /// the same measure.
    #[test]
    fn command_list_aligns_a_multibyte_command_by_columns_not_bytes() {
        let (r, sink, buf) = capture();
        r.render_command_list(&sink, 0, &[cp("größe", "one"), cp("ls", "two")]);
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

    /// A role-tinted value takes the SAME theme slot the role's status glyph
    /// does — the one `Role` → theme mapping, never a second one — and the
    /// tint covers the value alone: the key keeps `theme.secondary` and the
    /// gap between them stays unpainted.
    #[test]
    #[serial_test::serial]
    fn a_role_valued_row_paints_its_value_with_the_roles_theme_slot() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        for (role, slot) in [
            (Role::Ok, &theme.success),
            (Role::Warn, &theme.warning),
            (Role::Fail, &theme.error),
            (Role::Skipped, &theme.muted),
        ] {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let r = Renderer::new(theme.clone(), Verbosity::Normal);
            r.render_kv_block(&sink, 0, &[KvPair::role_valued("Status", "Drifted", role)]);
            // raw-capture-ok: the claim IS the role's SGR around the value, which captured_text would strip
            let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            assert!(
                out.contains(&format!(
                    "{}  {}",
                    theme.secondary.apply_to("Status"),
                    slot.apply_to("Drifted")
                )),
                "{role:?} value is not painted with its own theme slot: {out:?}"
            );
        }
    }

    /// An owner-valued row's value is the token's own tri-colour coat, byte for
    /// byte what a section heading over the same owner wears — the whole point
    /// of the slot being that a `kind:name` in a kv value cannot come out as a
    /// second spelling of one the tree already renders. Several owners join
    /// with the same separator the plain value carries.
    #[test]
    #[serial_test::serial]
    fn an_owner_valued_row_paints_its_value_through_owner_label() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        let owners = [
            crate::output::OwnerLabel::new("module", "nvim"),
            crate::output::OwnerLabel::new("module", "zsh"),
        ];
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(theme.clone(), Verbosity::Normal);
        r.render_kv_block(
            &sink,
            0,
            &[KvPair::owner_valued("Scope", owners.iter().cloned())],
        );
        // raw-capture-ok: the claim IS OwnerLabel's three-slot SGR, which captured_text would strip
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let expected = owners
            .iter()
            .map(|o| o.styled(&theme))
            .collect::<Vec<_>>()
            .join(crate::reconciler::Owner::TOKEN_SEPARATOR);
        assert!(
            out.contains(&expected),
            "the value is not the owner tokens' own coat: {out:?}"
        );
        assert_eq!(
            KvPair::owner_valued("Scope", owners.iter().cloned()).value,
            "module:nvim, module:zsh",
            "the plain value stays the joined tokens every colourless path reads"
        );
    }

    /// Colour-only: the same row rendered without colour is byte-identical to
    /// the plain row it would otherwise have been, so no golden moves when a
    /// row gains a tint.
    #[test]
    fn a_role_valued_row_renders_identically_to_a_plain_row_without_colour() {
        let (r, sink, buf) = capture();
        r.render_kv_block(
            &sink,
            0,
            &[KvPair::role_valued("Status", "Drifted", Role::Warn)],
        );
        let tinted = crate::test_helpers::captured_text(&buf);

        let (r, sink, buf) = capture();
        r.render_kv_block(&sink, 0, &[KvPair::new("Status", "Drifted")]);
        assert_eq!(tinted, crate::test_helpers::captured_text(&buf));
    }

    /// The tint is composed AFTER the fold. Painted first, `cursor_safe` would
    /// strip the coat off the very value the role exists to colour — so the
    /// proof is both halves at once: the hostile bytes stand escaped INSIDE
    /// the role's own span.
    #[test]
    #[serial_test::serial]
    fn a_role_valued_row_folds_its_value_before_it_tints_it() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(theme.clone(), Verbosity::Normal);
        r.render_kv_block(
            &sink,
            0,
            &[KvPair::role_valued(
                "Status",
                "Drifted\r\u{1b}[2Krepainted",
                Role::Warn,
            )],
        );
        // raw-capture-ok: the claim is the fold's escapes sitting inside the renderer's own SGR
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            out.contains(&theme.warning.apply_to("Drifted\\x0drepainted").to_string()),
            "the value was not folded before the tint went on: {out:?}"
        );
    }

    /// An empty value with a role names nothing to paint, so the annotation
    /// beside it still stands alone as the row rather than trailing an empty
    /// styled span.
    #[test]
    fn a_role_on_an_empty_value_paints_nothing() {
        let (r, sink, buf) = capture();
        let mut pair = KvPair::annotated("Modules", "", "all skipped");
        pair.value_role = Some(Role::Warn);
        r.render_kv_block(&sink, 0, &[pair]);
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("Modules  all skipped"), "got: {out:?}");
    }

    #[test]
    fn command_list_glues_with_em_dash_not_a_whitespace_gap() {
        let (r, sink, buf) = capture();
        r.render_command_list(&sink, 0, &[cp("cfgd apply", "apply configuration")]);
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
        r.render_command_list(&sink, 0, &[cp(long_command, "create a module")]);
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
                cp("cfgd apply", "apply configuration"),
                cp("cfgd module create <name>", "create a module"),
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
        r.render_command_list(&sink, 0, &[cp("cfgd apply", "apply configuration")]);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            vec!["Next Steps", "  cfgd apply — apply configuration"],
            "got: {out:?}"
        );
    }

    /// Rows written under a top-level heading bind to it even when the drain
    /// is not reached until a section has opened. The buffer is flushed by
    /// the NEXT non-kv emission, and when that emission is the first line of
    /// a section, `indent_depth` has already moved and `section_stack` is no
    /// longer empty — so a binding judged at drain time reads the rows as
    /// belonging to nothing, spends the blank the heading suppresses, and
    /// leaves the section header with none of its own.
    ///
    /// `cfgd checkin`'s drift path is the live shape: heading, two kvs, then
    /// `printer.section("Drift")`. The blank ahead of `Drift` is the trailing
    /// half of the same judged-when-written rule — see
    /// `a_top_level_kv_block_separates_from_the_section_that_follows_it`.
    #[test]
    fn kv_rows_bind_to_their_heading_even_when_a_section_opens_before_the_drain() {
        let (r, sink, buf) = capture();
        r.render_heading(&sink, "Checkin");
        r.render_kv("Server Status", "ok");
        r.render_kv("Config Changed", "false");
        r.render_section_open("Drift", /*keep_when_empty=*/ true);
        r.render_status(
            &sink,
            1,
            &StatusFields {
                role: Role::Ok,
                subject: "3 drift items reported",
                detail: None,
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
                verdict: None,
            },
        );
        r.render_section_close(&sink);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            vec![
                "Checkin",
                "  Server Status   ok",
                "  Config Changed  false",
                "",
                "Drift",
                "  ✓ 3 drift items reported",
            ],
            "got: {out:?}"
        );
    }

    /// The trailing half: a kv block written at the TOP LEVEL is a top-level
    /// group, so the section header after it gets the one blank line every
    /// other top-level group gets. Judged at drain time the answer is wrong in
    /// exactly the same way the binding was — the following section has
    /// already pushed its frame, so the block marks no group at all and its
    /// rows run straight into the header (`cfgd checkin`'s drift path against
    /// its no-drift path, which ends in a status line and is spaced).
    ///
    /// No heading here on purpose: the binding under test is the block's own,
    /// not one inherited from a heading above it.
    #[test]
    fn a_top_level_kv_block_separates_from_the_section_that_follows_it() {
        let (r, sink, buf) = capture();
        r.render_kv("Server Status", "ok");
        r.render_kv("Config Changed", "false");
        r.render_section_open("Drift", /*keep_when_empty=*/ true);
        r.render_status(
            &sink,
            1,
            &StatusFields {
                role: Role::Ok,
                subject: "3 drift items reported",
                detail: None,
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
                verdict: None,
            },
        );
        r.render_section_close(&sink);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            vec![
                "Server Status   ok",
                "Config Changed  false",
                "",
                "Drift",
                "  ✓ 3 drift items reported",
            ],
            "got: {out:?}"
        );
    }

    /// Rows written INSIDE a section belong to it, whatever else happens
    /// before they drain. Left to the next emission, the drain lands after the
    /// frame is gone: the rows render above the section's own header and the
    /// section reports itself empty (`cfgd module add`'s review summary put its
    /// `Commit`/`Integrity` rows above `module:<name>`, which then rendered
    /// `(none)`).
    #[test]
    fn kv_rows_written_inside_a_section_render_inside_it() {
        let (r, sink, buf) = capture();
        r.render_section_open("module:mymod", /*keep_when_empty=*/ true);
        r.render_kv("Commit", "abc1234");
        r.render_kv("Integrity", "sha256:beef");
        r.render_section_close(&sink);
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            vec![
                "module:mymod",
                "  Commit     abc1234",
                "  Integrity  sha256:beef",
            ],
            "got: {out:?}"
        );
    }

    #[test]
    fn command_list_quiet_suppressed() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_command_list(&sink, 0, &[cp("cfgd apply", "apply")]);
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }
}

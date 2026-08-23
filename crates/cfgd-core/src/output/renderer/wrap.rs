//! Soft-wrapping with a hanging indent.
//!
//! Left to the terminal, a status line longer than the window breaks at
//! column 0, so its tail reads as a separate unmarked line sitting outside the
//! layout — `⊙ sudo apt-get install … python3-pip py` followed by
//! `thon3-venv rustc ruby-full` hard against the left edge. Wrapping here
//! instead lets the continuation start under the first word of the line above
//! it, which is what makes it read as the same line.
//!
//! Only a sink that will hard-wrap asks to be broken at a column (see
//! `Writer::wrap_columns`), so a redirected stream or a test capture buffer
//! keeps the physical lines the renderer emitted. The hanging indent itself is
//! not conditional on that: a body carrying its own newlines gets the same
//! continuation column either way, because indentation is layout and layout
//! does not change when output is piped.
use unicode_width::UnicodeWidthChar;

use super::Writer;

/// Below this there is no room for both the marker column and a useful amount
/// of text, and wrapping degenerates into one word per line.
const MIN_WRAP_WIDTH: usize = 24;

/// Fallback width when stderr has no size (redirected, or a terminal that
/// won't answer). Wide enough to stay readable, narrow enough that a real
/// 80-column terminal is the only thing that wraps.
const FALLBACK_WIDTH: usize = 100;

/// Columns available to a line rendered at `depth`, after the spinner's own
/// glyph and separating space.
///
/// Measured against the sink the line is actually being written to, not a
/// process-global terminal probe: a captured/redirected sink (`StringSink`,
/// a piped stream) answers `None` from `Writer::wrap_columns` and falls back
/// to `FALLBACK_WIDTH` regardless of what the host's own terminal happens to
/// report, so a golden recorded on one window size never depends on the size
/// of the runner that later replays it. A real terminal sink answers with its
/// own columns, which is what protects an in-place spinner repaint from
/// wrapping onto a row indicatif will not rewind.
pub(crate) fn available_width(sink: &dyn Writer, depth: usize) -> usize {
    let cols = sink.wrap_columns().unwrap_or(FALLBACK_WIDTH);
    line_budget(cols, depth)
        .saturating_sub(2)
        .max(MIN_WRAP_WIDTH)
}

/// Columns a COMPLETE composed line rendered at `depth` may occupy: the
/// terminal minus the `depth * 2` indent, and nothing else — the glyph, the
/// subject, its alignment padding and the duration all live inside it.
///
/// The ONE formula shared by the alignment ceiling
/// (`Renderer::affordable_column`, via `status.rs`'s `wrap_budget`) and the
/// live repaint clamp (`line_width`, below). The ceiling pads a settled line
/// out to exactly this budget, so a clamp read from any tighter formula —
/// `available_width`'s, say, which is two columns narrower because it
/// measures the room left AFTER the glyph — amputates the tail of the
/// duration the padding just right-aligned.
pub(crate) fn line_budget(cols: usize, depth: usize) -> usize {
    cols.saturating_sub(depth * 2)
}

/// `line_budget` against the sink a live repaint is drawn to, with the same
/// fallback and floor `available_width` applies. This is the clamp for a
/// complete composed line that must stay one physical line — a live row's own
/// repaint — not for a wrapped body, which keeps `available_width`.
pub(crate) fn line_width(sink: &dyn Writer, depth: usize) -> usize {
    let cols = sink.wrap_columns().unwrap_or(FALLBACK_WIDTH);
    line_budget(cols, depth).max(MIN_WRAP_WIDTH)
}

/// Display width of `s` in terminal columns, ignoring characters that occupy
/// none.
fn display_width(s: &str) -> usize {
    s.chars().filter_map(UnicodeWidthChar::width).sum()
}

/// Columns a prefix occupies once its styling is discounted. A prefix may
/// carry SGR — a `command_list`'s opening holds its key's colour — and
/// `display_width` drops only the `\x1b` itself, counting `[38;5;212m` as
/// thirteen columns of text and wrapping the row that far early.
fn visible_width(s: &str) -> usize {
    display_width(&super::super::strip_ansi(s))
}

/// Clamp `text` to `max` display columns, marking the cut with `…`.
///
/// For a line that must stay one physical line no matter what — a spinner
/// message, which indicatif repaints in place and cannot reach a second row.
pub(crate) fn clamp(text: &str, max: usize) -> String {
    console::truncate_str(text, max, "…").into_owned()
}

/// Display width of the line's marker column — a leading one-column glyph
/// (`✓`, `⊙`, `-`, …) plus the space after it. Zero when the line does not
/// open with one, so a plain sentence wraps flush rather than hanging off its
/// own first word.
fn marker_width(visible: &str) -> usize {
    let Some(first) = visible.split(' ').next() else {
        return 0;
    };
    if first.is_empty() || visible.len() == first.len() {
        return 0;
    }
    let width: usize = first.chars().filter_map(UnicodeWidthChar::width).sum();
    if width == 1 { 2 } else { 0 }
}

/// Split a whole message body into the physical lines that render it, hanging
/// every continuation under the first word after the leading glyph.
///
/// The hang is computed once, from the first logical line, and governs every
/// line after it — whether that line came from an embedded newline or from
/// soft-wrapping. A caveat's second sentence and the tail of a wrapped
/// sentence are the same thing to a reader, so they belong in the same column;
/// splitting on `\n` alone left the second sentence at column 0, reading as an
/// unrelated unmarked line.
///
/// `cols` is `None` for a sink that never hard-wraps. The hang still applies
/// there: indentation is a layout decision, so a redirected run has the same
/// shape as a terminal one.
pub(crate) fn wrap_body(body: &str, prefix: &str, cols: Option<usize>) -> Vec<String> {
    let mut logical = body.split('\n');
    let Some(first) = logical.next() else {
        return Vec::new();
    };
    let hang = " ".repeat(display_width(prefix) + marker_width(&super::super::strip_ansi(first)));
    let mut out = wrap_segment(first, prefix, &hang, cols);
    for line in logical {
        // A blank line inside the body separates paragraphs; indenting it
        // would leave trailing whitespace with nothing under it.
        if line.trim().is_empty() {
            out.push(String::new());
        } else {
            out.extend(wrap_segment(line, &hang, &hang, cols));
        }
    }
    out
}

/// Lay out one logical line: `first_prefix` on its first physical line,
/// `cont_prefix` on every physical line the wrap produces after it.
///
/// ANSI escapes are carried through without consuming width; a break lands on
/// the last space that fits, or mid-word when a single word is itself longer
/// than the line.
///
/// Reachable from the renderer directly, for the one shape whose hang cannot
/// be derived from the line itself: a two-column list's description hangs at
/// the DESCRIPTION column, which only its caller knows — [`wrap_body`] reads
/// the marker column off the first word instead, which is right for a status
/// line and would wrap a description back under its own left column.
pub(crate) fn wrap_segment(
    body: &str,
    first_prefix: &str,
    cont_prefix: &str,
    cols: Option<usize>,
) -> Vec<String> {
    let prefix_width = visible_width(first_prefix);
    let cont_width = visible_width(cont_prefix);
    let visible_body = super::super::strip_ansi(body);
    let body_width = display_width(&visible_body);
    let Some(cols) = cols else {
        return vec![format!("{first_prefix}{body}")];
    };
    // Two ways a hang is unaffordable, and the row is then left for the
    // terminal's own hard wrap rather than chopped into a phantom column.
    //
    // It takes more than half the terminal, leaving a sliver no prose reads
    // well in. The test is proportional rather than `MIN_WRAP_WIDTH`, which
    // describes a whole line: measured against the remainder, any hang at all
    // would strand a 24-column terminal unwrapped.
    //
    // Or it steals the columns a word needed: a word the terminal could hold
    // WHOLE must never be split just because the continuation column is
    // narrower than the line. A word longer than the terminal itself is a
    // different case and still splits, here as everywhere.
    let longest_word = visible_body
        .split_whitespace()
        .map(display_width)
        .max()
        .unwrap_or(0);
    let hang_splits_a_word = longest_word <= cols && longest_word > cols.saturating_sub(cont_width);
    if cols < MIN_WRAP_WIDTH
        || prefix_width + body_width <= cols
        || cont_width * 2 > cols
        || hang_splits_a_word
    {
        return vec![format!("{first_prefix}{body}")];
    }

    let hang = cont_prefix;
    let mut out = Vec::new();
    // Whether any physical row has been written, which is what decides between
    // the first prefix and the hang. Not `out.is_empty()`: a break landing
    // inside a run of alignment padding produces a row with nothing on it, and
    // such a row is dropped rather than emitted — it reads as a blank line
    // between an action and its own trailing column.
    let mut wrote_row = false;
    let mut current = String::new();
    let mut current_width = 0usize;
    // Byte index into `current` just after the most recent space, so a break
    // can retract to it rather than splitting a word.
    let mut last_break: Option<usize> = None;
    let mut limit = cols.saturating_sub(prefix_width);
    let mut chars = body.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // An escape occupies no columns, so it is copied verbatim and the
            // width accounting skips it entirely. The `[` introducing a CSI is
            // itself inside the final-byte range, so it is consumed before the
            // scan starts — ending on it would leave `32m` to be counted as
            // three columns of text.
            current.push(c);
            if chars.peek() == Some(&'[') {
                if let Some(bracket) = chars.next() {
                    current.push(bracket);
                }
                for esc in chars.by_ref() {
                    current.push(esc);
                    if ('\u{40}'..='\u{7e}').contains(&esc) {
                        break;
                    }
                }
            } else if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if current_width + w > limit && current_width > 0 {
            let (head, tail) = match last_break {
                Some(at) => (
                    current[..at].trim_end().to_string(),
                    current[at..].to_string(),
                ),
                None => (current.clone(), String::new()),
            };
            if super::super::strip_ansi(&head).trim().is_empty() {
                // The break landed inside a run of alignment padding, so this
                // row would carry nothing visible — and an empty row reads as
                // a blank line between an action and its own trailing column.
                // `head` is already trimmed of that padding, so what is left
                // of it is styling, which moves onto the next row rather than
                // leaving a style run open.
                current = format!("{head}{tail}");
            } else {
                out.push(format!(
                    "{}{}",
                    if wrote_row { hang } else { first_prefix },
                    head
                ));
                wrote_row = true;
                current = tail;
            }
            limit = cols.saturating_sub(if wrote_row { cont_width } else { prefix_width });
            current_width = super::super::strip_ansi(&current)
                .chars()
                .filter_map(UnicodeWidthChar::width)
                .sum();
            last_break = None;
        }
        if c == ' ' {
            last_break = Some(current.len() + c.len_utf8());
        }
        current.push(c);
        current_width += w;
    }
    if !super::super::strip_ansi(&current).trim().is_empty() {
        out.push(format!(
            "{}{}",
            if wrote_row { hang } else { first_prefix },
            current
        ));
    } else if !current.is_empty() {
        // Nothing visible is left, so this is styling with no text under it —
        // usually the reset closing the run above. It rides on the last row
        // rather than claiming a blank one of its own, and a message that was
        // nothing BUT styling still gets a row so the reset is written.
        match out.last_mut() {
            Some(last) => last.push_str(&current),
            None => out.push(format!("{first_prefix}{current}")),
        }
    }
    out
}

/// Same layout as [`wrap_body`], but with `trailer` — a status line's
/// duration suffix — anchored flush right on the LAST physical line instead
/// of flowing inline with the rest of the body.
///
/// `body` wraps as if the line were narrower by `trailer`'s own width, on
/// EVERY physical line it produces, not only the last one: the wrap decision
/// has no way to know in advance which line will end up last, so reserving
/// the column throughout is what guarantees room for the trailer once
/// wrapping is done — and `body_width > cols - trailer_width` is the same
/// test as `body_width + trailer_width > cols`, so reserving up front decides
/// wrap-or-not exactly as `wrap_body` already would from the fully composed
/// string.
///
/// A body that still fits on ONE row even with the reservation gets the
/// trailer glued straight on with no padding, exactly as `wrap_body` alone
/// would have rendered the fully composed string — reaching for the `cols`
/// edge only when a real multi-row wrap happened; an ordinary short status
/// line pads to whatever alignment column its own section requested, never
/// to the terminal's edge. Only once wrapping actually split the body does
/// the last row get padded out to the raw `cols` ceiling `wrap_segment`
/// already wraps every row against — the same edge `line_budget`'s content
/// budget measures in from — and the trailer appended there: the shared
/// duration column a section's OTHER rows reach by padding their own subject
/// (`pad_subject`), so a row too long to reach that column by padding still
/// reaches it with its duration.
pub(crate) fn wrap_body_with_trailer(
    body: &str,
    prefix: &str,
    cols: Option<usize>,
    trailer: Option<&str>,
) -> Vec<String> {
    let Some(trailer) = trailer else {
        return wrap_body(body, prefix, cols);
    };
    let trailer_width = display_width(&super::super::strip_ansi(trailer));
    let reserved = cols.map(|c| c.saturating_sub(trailer_width));
    let mut out = wrap_body(body, prefix, reserved);
    let wrapped = out.len() > 1;
    let Some(last) = out.last_mut() else {
        // `wrap_body` never actually returns an empty vec (its own empty-body
        // arm still yields one row carrying the prefix), but nothing here
        // depends on that holding forever — the trailer still gets a row.
        out.push(format!("{prefix}{trailer}"));
        return out;
    };
    match (wrapped, cols) {
        (true, Some(cols)) => {
            let last_width = display_width(&super::super::strip_ansi(last));
            let pad = cols.saturating_sub(last_width + trailer_width);
            last.push_str(&" ".repeat(pad));
            last.push_str(trailer);
        }
        _ => last.push_str(trailer),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_line_is_returned_untouched_with_its_prefix() {
        assert_eq!(wrap_body("✓ done", "  ", Some(80)), vec!["  ✓ done"]);
    }

    #[test]
    fn continuation_hangs_under_the_first_word_after_the_glyph() {
        let out = wrap_body("⊙ alpha bravo charlie delta", "", Some(24));
        assert_eq!(out.len(), 2, "got: {out:?}");
        assert_eq!(out[0], "⊙ alpha bravo charlie");
        assert_eq!(out[1], "  delta");
    }

    #[test]
    fn depth_prefix_is_added_to_the_hanging_indent() {
        let out = wrap_body("- alpha bravo charlie", "    ", Some(24));
        assert_eq!(out[0], "    - alpha bravo");
        assert_eq!(out[1], "      charlie");
    }

    #[test]
    fn a_line_with_no_glyph_wraps_flush_to_the_prefix() {
        let out = wrap_body("alpha bravo charlie delta echo", "", Some(24));
        assert_eq!(out[0], "alpha bravo charlie");
        assert_eq!(out[1], "delta echo");
    }

    #[test]
    fn ansi_escapes_do_not_consume_display_columns() {
        // Same text as the flush case, with the first word colored. The break
        // must land in the same place — an escape counted as width would move
        // it earlier.
        let out = wrap_body(
            "\x1b[32malpha\x1b[0m bravo charlie delta echo",
            "",
            Some(24),
        );
        assert_eq!(out.len(), 2, "got: {out:?}");
        assert_eq!(
            super::super::super::strip_ansi(&out[0]),
            "alpha bravo charlie"
        );
        assert_eq!(out[1], "delta echo");
    }

    /// A hang can be small enough to keep half the terminal and still leave
    /// less room than a word needs. The word fits the line, so the hang is
    /// what would split it, and the row is left alone instead.
    #[test]
    fn a_word_the_terminal_could_hold_whole_is_not_split_by_the_hang() {
        let hang = " ".repeat(12);
        let out = wrap_segment("run now please administrator", "", &hang, Some(24));
        assert_eq!(out, vec!["run now please administrator"], "got: {out:?}");
    }

    #[test]
    fn a_word_longer_than_the_line_is_broken_rather_than_overflowing() {
        let out = wrap_body("abcdefghijklmnopqrstuvwxyz0123456789", "", Some(24));
        assert!(out.len() > 1, "got: {out:?}");
        for line in &out {
            let w: usize = line.chars().filter_map(UnicodeWidthChar::width).sum();
            assert!(w <= 24, "line over budget: {line:?}");
        }
    }

    #[test]
    fn a_terminal_too_narrow_to_wrap_usefully_is_left_alone() {
        let long = "⊙ alpha bravo charlie delta echo foxtrot";
        assert_eq!(wrap_body(long, "", Some(10)), vec![long.to_string()]);
    }

    #[test]
    fn an_embedded_newline_hangs_under_the_marker_like_a_wrap_does() {
        // The brew caveat shape: two sentences, the first short enough that no
        // wrapping is involved at all. Split alone left the second at column 0,
        // reading as an unmarked line belonging to nothing.
        let out = wrap_body("▲ first sentence.\nsecond sentence.", "", Some(80));
        assert_eq!(out, vec!["▲ first sentence.", "  second sentence."]);
    }

    #[test]
    fn the_hang_comes_from_the_first_line_not_each_continuation() {
        // "Temporal" is eight columns wide, so a continuation asked for its own
        // marker width would get zero and fall back to column 0.
        let out = wrap_body("⊙ alpha\nTemporal support is disabled.", "", Some(80));
        assert_eq!(out[1], "  Temporal support is disabled.");
    }

    #[test]
    fn a_non_wrapping_sink_still_indents_continuations() {
        // Indentation is layout, not terminal width: a redirected run has the
        // same shape as a terminal one.
        let out = wrap_body("✓ head\ntail", "  ", None);
        assert_eq!(out, vec!["  ✓ head", "    tail"]);
    }

    #[test]
    fn a_blank_line_inside_a_body_carries_no_indent() {
        let out = wrap_body("- head\n\ntail", "  ", None);
        assert_eq!(out, vec!["  - head", "", "    tail"]);
    }

    #[test]
    fn a_continuation_that_is_itself_too_long_wraps_to_the_same_column() {
        let out = wrap_body("⊙ head\nalpha bravo charlie delta echo", "", Some(24));
        assert_eq!(out[0], "⊙ head");
        assert_eq!(out[1], "  alpha bravo charlie");
        assert_eq!(out[2], "  delta echo");
    }

    #[test]
    fn a_break_inside_alignment_padding_leaves_no_blank_row() {
        // A subject padded to a plan-wide column wider than the window: the
        // break lands inside the run of spaces, and a row carrying only
        // padding reads as a blank line between an action and its own
        // duration.
        let padded = format!("\u{2713} install ripgrep{} (12.1s)", " ".repeat(100));
        let out = wrap_body(&padded, "  ", Some(40));
        assert!(
            out.iter().all(|line| !line.trim().is_empty()),
            "got: {out:?}"
        );
        assert!(
            out.iter().any(|line| line.contains("(12.1s)")),
            "the duration survives the drop: {out:?}"
        );
    }

    #[test]
    fn styling_a_dropped_row_carried_moves_onto_the_row_that_follows_it() {
        // The dropped row is padding plus whatever escapes happened to fall in
        // it. Dropping those with the row would leave the style run opened
        // above it never closed, and the colour would bleed down the screen.
        let padded = format!(
            "\u{2713} subject{}\u{1b}[32m{}\u{1b}[0m (12.1s)",
            " ".repeat(60),
            " ".repeat(60)
        );
        let out = wrap_body(&padded, "", Some(40));
        let escapes: usize = out.iter().map(|line| line.matches('\u{1b}').count()).sum();
        assert_eq!(escapes, 2, "every escape survives the drop: {out:?}");
        assert!(
            out.iter().all(|line| !line.trim().is_empty()),
            "got: {out:?}"
        );
    }

    #[test]
    fn clamp_cuts_to_display_columns_and_marks_the_cut() {
        let out = clamp(&"x".repeat(200), 40);
        assert_eq!(display_width(&out), 40, "got: {out:?}");
        assert!(out.ends_with('…'));
    }

    #[test]
    fn wide_glyphs_count_their_real_columns() {
        // A CJK character is two columns; counting it as one would let the
        // line overflow the terminal and hard-wrap anyway.
        let out = wrap_body("日本語のテキストです", "", Some(24));
        for line in &out {
            let w: usize = line.chars().filter_map(UnicodeWidthChar::width).sum();
            assert!(w <= 24, "line over budget: {line:?} ({w} cols)");
        }
    }

    #[test]
    fn a_wrapped_bodys_trailer_right_aligns_on_the_last_line_only() {
        // The apt-install shape this fix pins: a subject long enough that
        // wrapping is unavoidable, plus a duration that must not simply flow
        // inline wherever the last word happens to land.
        let body = "\u{2713} apt install build-essential, make, unzip, git, curl, ripgrep, xclip, wl-clipboard, xdg-utils, npm, python3, python3-pip, python3-venv, rustc, ruby-full, libyaml-dev";
        let trailer = " (23.6s)";
        let cols = 118;
        let out = wrap_body_with_trailer(body, "    ", Some(cols), Some(trailer));

        assert!(out.len() > 1, "the body needed to wrap: {out:?}");
        for line in &out {
            let w = display_width(&super::super::super::strip_ansi(line));
            assert!(w <= cols, "line over budget: {line:?} ({w} cols)");
        }
        let last = out.last().unwrap_or(&out[0]);
        assert!(
            last.ends_with(trailer),
            "the trailer lands on the last row: {last:?}"
        );
        for line in &out[..out.len() - 1] {
            assert!(
                !line.contains("23.6s"),
                "the trailer never lands on an earlier row: {out:?}"
            );
        }
        let last_width = display_width(&super::super::super::strip_ansi(last));
        assert_eq!(
            last_width, cols,
            "the trailer right-aligns to the shared duration column: {last:?}"
        );
    }

    #[test]
    fn a_body_that_still_fits_with_the_trailer_reserved_is_not_padded_to_the_edge() {
        // Wrapping was never triggered — the trailer glues straight onto the
        // one row exactly as `wrap_body` alone renders the fully composed
        // string, so an ordinary short status line does not stretch out to
        // the terminal's edge just because it carries a duration.
        let out = wrap_body_with_trailer("✓ install ripgrep", "  ", Some(80), Some(" (1.2s)"));
        assert_eq!(out, vec!["  ✓ install ripgrep (1.2s)"]);
    }

    #[test]
    fn a_non_wrapping_sink_appends_the_trailer_with_no_padding() {
        let out = wrap_body_with_trailer("✓ done", "  ", None, Some(" (1.2s)"));
        assert_eq!(out, vec!["  ✓ done (1.2s)"]);
    }

    #[test]
    fn no_trailer_wraps_exactly_like_wrap_body() {
        let body = "alpha bravo charlie delta echo";
        assert_eq!(
            wrap_body_with_trailer(body, "", Some(24), None),
            wrap_body(body, "", Some(24))
        );
    }
}

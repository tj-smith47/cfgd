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

/// Below this there is no room for both the marker column and a useful amount
/// of text, and wrapping degenerates into one word per line.
const MIN_WRAP_WIDTH: usize = 24;

/// Fallback width when stderr has no size (redirected, or a terminal that
/// won't answer). Wide enough to stay readable, narrow enough that a real
/// 80-column terminal is the only thing that wraps.
const FALLBACK_WIDTH: usize = 100;

/// Terminal columns available to a line rendered at `depth`, after the
/// spinner's own glyph and separating space.
///
/// Measured against stderr, where every spinner and window lives — stdout may
/// be redirected while the progress display still owns a terminal.
pub(crate) fn available_width(depth: usize) -> usize {
    let cols = console::Term::stderr()
        .size_checked()
        .map_or(FALLBACK_WIDTH, |(_, cols)| cols as usize);
    cols.saturating_sub(depth * 2 + 2).max(MIN_WRAP_WIDTH)
}

/// Display width of `s` in terminal columns, ignoring characters that occupy
/// none.
fn display_width(s: &str) -> usize {
    s.chars().filter_map(UnicodeWidthChar::width).sum()
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
fn wrap_segment(
    body: &str,
    first_prefix: &str,
    cont_prefix: &str,
    cols: Option<usize>,
) -> Vec<String> {
    let prefix_width = display_width(first_prefix);
    let body_width = display_width(&super::super::strip_ansi(body));
    let Some(cols) = cols else {
        return vec![format!("{first_prefix}{body}")];
    };
    if cols < MIN_WRAP_WIDTH || prefix_width + body_width <= cols {
        return vec![format!("{first_prefix}{body}")];
    }

    let hang = cont_prefix;
    let mut out = Vec::new();
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
            out.push(format!(
                "{}{}",
                if out.is_empty() { first_prefix } else { hang },
                head
            ));
            limit = cols.saturating_sub(display_width(hang));
            current = tail;
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
    if !current.is_empty() {
        out.push(format!(
            "{}{}",
            if out.is_empty() { first_prefix } else { hang },
            current
        ));
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
}

//! Soft-wrapping with a hanging indent.
//!
//! Left to the terminal, a status line longer than the window breaks at
//! column 0, so its tail reads as a separate unmarked line sitting outside the
//! layout — `⊙ sudo apt-get install … python3-pip py` followed by
//! `thon3-venv rustc ruby-full` hard against the left edge. Wrapping here
//! instead lets the continuation start under the first word of the line above
//! it, which is what makes it read as the same line.
//!
//! Only a sink that will hard-wrap asks for this (see `Writer::wrap_columns`),
//! so a redirected stream or a test capture buffer emits exactly the physical
//! lines it always did.
use unicode_width::UnicodeWidthChar;

/// Below this there is no room for both the marker column and a useful amount
/// of text, and wrapping degenerates into one word per line.
const MIN_WRAP_WIDTH: usize = 24;

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

/// Split `body` into physical lines of at most `cols` columns, each after the
/// first indented to `prefix` plus the marker column.
///
/// ANSI escapes are carried through without consuming width; a break lands on
/// the last space that fits, or mid-word when a single word is itself longer
/// than the line.
pub(crate) fn wrap_line(body: &str, prefix: &str, cols: usize) -> Vec<String> {
    let prefix_width: usize = prefix.chars().filter_map(UnicodeWidthChar::width).sum();
    let visible = super::super::strip_ansi(body);
    let body_width: usize = visible.chars().filter_map(UnicodeWidthChar::width).sum();
    if cols < MIN_WRAP_WIDTH || prefix_width + body_width <= cols {
        return vec![format!("{prefix}{body}")];
    }

    let hang = " ".repeat(prefix_width + marker_width(&visible));
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
                if out.is_empty() {
                    prefix
                } else {
                    hang.as_str()
                },
                head
            ));
            limit = cols.saturating_sub(hang.len());
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
            if out.is_empty() {
                prefix
            } else {
                hang.as_str()
            },
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
        assert_eq!(wrap_line("✓ done", "  ", 80), vec!["  ✓ done"]);
    }

    #[test]
    fn continuation_hangs_under_the_first_word_after_the_glyph() {
        let out = wrap_line("⊙ alpha bravo charlie delta", "", 24);
        assert_eq!(out.len(), 2, "got: {out:?}");
        assert_eq!(out[0], "⊙ alpha bravo charlie");
        assert_eq!(out[1], "  delta");
    }

    #[test]
    fn depth_prefix_is_added_to_the_hanging_indent() {
        let out = wrap_line("- alpha bravo charlie", "    ", 24);
        assert_eq!(out[0], "    - alpha bravo");
        assert_eq!(out[1], "      charlie");
    }

    #[test]
    fn a_line_with_no_glyph_wraps_flush_to_the_prefix() {
        let out = wrap_line("alpha bravo charlie delta echo", "", 24);
        assert_eq!(out[0], "alpha bravo charlie");
        assert_eq!(out[1], "delta echo");
    }

    #[test]
    fn ansi_escapes_do_not_consume_display_columns() {
        // Same text as the flush case, with the first word colored. The break
        // must land in the same place — an escape counted as width would move
        // it earlier.
        let out = wrap_line("\x1b[32malpha\x1b[0m bravo charlie delta echo", "", 24);
        assert_eq!(out.len(), 2, "got: {out:?}");
        assert_eq!(
            super::super::super::strip_ansi(&out[0]),
            "alpha bravo charlie"
        );
        assert_eq!(out[1], "delta echo");
    }

    #[test]
    fn a_word_longer_than_the_line_is_broken_rather_than_overflowing() {
        let out = wrap_line("abcdefghijklmnopqrstuvwxyz0123456789", "", 24);
        assert!(out.len() > 1, "got: {out:?}");
        for line in &out {
            let w: usize = line.chars().filter_map(UnicodeWidthChar::width).sum();
            assert!(w <= 24, "line over budget: {line:?}");
        }
    }

    #[test]
    fn a_terminal_too_narrow_to_wrap_usefully_is_left_alone() {
        let long = "⊙ alpha bravo charlie delta echo foxtrot";
        assert_eq!(wrap_line(long, "", 10), vec![long.to_string()]);
    }

    #[test]
    fn wide_glyphs_count_their_real_columns() {
        // A CJK character is two columns; counting it as one would let the
        // line overflow the terminal and hard-wrap anyway.
        let out = wrap_line("日本語のテキストです", "", 24);
        for line in &out {
            let w: usize = line.chars().filter_map(UnicodeWidthChar::width).sum();
            assert!(w <= 24, "line over budget: {line:?} ({w} cols)");
        }
    }
}

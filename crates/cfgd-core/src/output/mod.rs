//! Typed-component output system — the sole interface for terminal output
//! across cfgd. See `.claude/specs/2026-05-14-output-system-redesign-design.md`
//! for the design.

pub mod role;
pub use role::Role;

pub mod verbosity;
pub use verbosity::{OutputFormat, Verbosity};

pub mod theme;
pub use theme::Theme;

pub mod component;
pub use component::{Component, KvPair};

pub mod renderer;

pub mod printer;
pub use printer::{ColorChoice, DocCapture, Printer, PromptAnswer};

pub mod owner_label;
pub use owner_label::OwnerLabel;

pub mod phase_label;
pub use phase_label::PhaseLabel;

pub mod title_label;
pub use title_label::TitleLabel;

pub mod section_guard;
pub use section_guard::SectionGuard;

pub mod status_builder;
pub use status_builder::StatusBuilder;

pub mod spinner;
pub use spinner::{ProgressBar, Spinner};

pub mod window;
pub use window::OutputWindow;

// Every item is `pub(crate)`: a row is a live-region primitive the reconciler
// draws its phase tree with, never a published surface.
pub(crate) mod live_row;

pub mod lane;
pub use lane::LaneOutput;

pub mod process;
pub use process::CommandOutput;

pub mod prompts;

pub mod raw;

pub mod tracing_writer;
pub use tracing_writer::LiveTracingWriter;

pub mod doc;
pub use doc::{Doc, SectionBuilder, StatusFields};

/// Strip ANSI CSI escape sequences (`ESC [ ... m`) from a string.
///
/// Used as a sanitization boundary for any text that originates outside the
/// renderer (e.g. captured stderr from an external tool, error `Display`
/// output, user-supplied detail strings) before it lands in a styled line.
/// A stray foreign `\x1b[0m` mid-detail would otherwise prematurely terminate
/// the role styling of the subject; foreign color escapes would paint
/// subsequent terminal output until the next reset.
///
/// Walks `char`s (escape sequences are all ASCII, so this is safe across
/// multi-byte UTF-8 glyphs like `✓ ✗ — →`). Recognizes the three shapes a
/// child process actually emits:
///
/// - **CSI** — `ESC [`, parameter and intermediate bytes, then a final byte in
///   `0x40..=0x7E`. Ending a CSI on `m` alone is not a partial implementation
///   but a swallowing one: `ESC [ 2 J` (clear screen) and `ESC [ H` (home)
///   carry no `m`, so an SGR-only stripper consumes the whole remainder of the
///   line as if it were part of the escape. `nvim --headless` emits both.
/// - **OSC** — `ESC ]`, a payload, then `BEL` or the two-char ST (`ESC \`).
/// - **Two-byte escapes** — `ESC 7`, `ESC =`, and the charset selectors
///   (`ESC ( B`), which carry one further byte.
///
/// An unterminated escape is swallowed to end-of-string, which is the safer
/// outcome at a sanitization boundary — a malicious unterminated escape
/// shouldn't paint anything.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for inner in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&inner) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(inner) = chars.next() {
                    if inner == '\u{07}' {
                        break;
                    }
                    if inner == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some('(') | Some(')') | Some('*') | Some('+') => {
                chars.next();
            }
            _ => {}
        }
    }
    out
}

/// The rendered column width of `text`, in terminal columns.
///
/// The ONE width measurement available outside the renderer, so a caller that
/// pre-computes an alignment column measures exactly what the renderer pads
/// against: ANSI escapes count as zero columns and a multi-byte glyph (`✓`,
/// `—`) counts as the columns a terminal gives it, neither of which
/// `str::len()` answers.
pub fn measure_width(text: &str) -> usize {
    console::measure_text_width(text)
}

/// Collapse a multi-line error message into a single subject-safe line.
///
/// `Renderer::write_line` debug-asserts on bodies containing `\n`, so any
/// captured error (`io::Error`, `CfgdError`, command stderr) that gets
/// pumped into a `Printer::status[_simple]` subject or detail must be
/// flattened first. The first non-empty line becomes the head; subsequent
/// non-empty lines are joined with ` — ` so trailing systemctl/launchd
/// context (e.g. `"See system logs and 'systemctl status …' for details."`)
/// stays visible on a single physical row.
pub fn collapse_to_subject_line(err: impl std::fmt::Display) -> String {
    let s = err.to_string();
    let mut lines = s.lines().filter(|l| !l.trim().is_empty());
    let first = match lines.next() {
        Some(line) => line.trim().to_string(),
        None => return String::new(),
    };
    let mut out = first;
    for line in lines {
        out.push_str(" — ");
        out.push_str(line.trim());
    }
    out
}

/// The ONE spelling of a planned-vs-actual mismatch: `want: <expected>,
/// have: <actual>`. [`super::status_builder::StatusBuilder::drift`] and
/// `doc::StatusFields::drift` both compose their detail slot through this —
/// two DISPLAY producers that would otherwise drift apart (two different
/// spellings were observed: `want {}, have {}` with no colon, and this
/// canonical form baked straight into a status subject instead of the
/// detail slot). Reach for this only where the string is RENDERED to a
/// human, never where it is stored: `crate::compliance::system_checks_from_diffs`
/// composes its persisted/hashed `ComplianceCheck.detail` with a plain
/// `format!("expected {}, actual {}", …)` on purpose, because that string is
/// serialized into the `-o json` payload and into `compliance_snapshots`'
/// content-hash digest — a display-only spelling change there breaks a
/// consumer parsing `.detail` and re-hashes every stored snapshot on
/// upgrade for no real drift. `cli::compliance` composes this function at
/// the point it prints a check for a human, not at the point the check is
/// built.
pub fn drift_detail(expected: impl std::fmt::Display, actual: impl std::fmt::Display) -> String {
    format!("want: {expected}, have: {actual}")
}

/// Rendered width cap for [`condense_script_label`], in `char`s.
///
/// Eighty columns is the terminal width a status subject can assume without
/// wrapping on a standard, unresized terminal; `render_status_immediate`
/// still appends a role glyph and an optional `(Ns)` duration suffix after
/// the subject, so the cap leaves that trailing room rather than filling the
/// full width with script text alone.
const SCRIPT_LABEL_MAX_CHARS: usize = 80;

/// Condense a `ScriptEntry::run_str()` body into a single-line, width-bounded
/// label for status subjects and error messages.
///
/// An inline multi-line `run:` script handed straight to a status subject
/// (spinner label, `status_simple`, `finish_ok`/`finish_fail`) trips
/// `Renderer::write_line`'s `!body.contains('\n')` debug_assert; a release
/// build instead prints the whole body down the terminal as if it were one
/// status line. Takes the first non-empty, trimmed line and appends an
/// ellipsis marker when either that line was truncated to fit
/// `SCRIPT_LABEL_MAX_CHARS`, or further non-empty content follows it.
///
/// `str::lines()` only recognizes `\n` and `\r\n` as terminators, so a lone
/// `\r` (a classic-Mac line ending, or any other stray carriage return) would
/// otherwise ride along inside the "first line" untouched; it is scrubbed
/// explicitly so the result can never carry a `\r` forward.
pub fn condense_script_label(body: &str) -> String {
    let mut lines = body.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first_raw) = lines.next() else {
        return String::new();
    };
    let first: String = first_raw.chars().filter(|&c| c != '\r').collect();
    let more_lines = lines.next().is_some();

    if first.chars().count() <= SCRIPT_LABEL_MAX_CHARS {
        if more_lines {
            format!("{first} …")
        } else {
            first
        }
    } else {
        let truncated: String = first.chars().take(SCRIPT_LABEL_MAX_CHARS).collect();
        format!("{truncated}…")
    }
}

/// Build a stable-shaped error Doc for `bail!`-on-emit-then-fail sites.
/// Carries an `error` category key + `name` so structured consumers
/// (`-o json`) see a consistent payload on failure. Any extra fields in
/// `extras` (object literal expected) are merged into the payload alongside
/// `error` + `name`.
pub fn error_doc(
    name: &str,
    error_kind: &str,
    message: impl Into<String>,
    extras: serde_json::Value,
) -> Doc {
    let mut payload = serde_json::json!({
        "error": error_kind,
        "name": name,
    });
    if let serde_json::Value::Object(extra_map) = extras
        && let serde_json::Value::Object(payload_map) = &mut payload
    {
        for (k, v) in extra_map {
            payload_map.insert(k, v);
        }
    }
    let mut doc = Doc::new().status(Role::Fail, message).with_data(payload);
    doc.is_error = true;
    doc
}

pub mod render_doc;

pub mod structured;
pub use structured::validate_jsonpath_expr;

#[cfg(feature = "test-helpers")]
pub mod test_capture;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod drift_detail_tests {
    use super::drift_detail;

    #[test]
    fn composes_the_canonical_want_have_spelling() {
        assert_eq!(drift_detail("1.2.3", "1.2.0"), "want: 1.2.3, have: 1.2.0");
    }

    /// `String`, `&str` and `&String` all reach it unchanged (`impl
    /// Display` rather than `impl Into<String>`) — every producer call
    /// site holds a different one of the three.
    #[test]
    fn accepts_string_and_str_and_ref_string_uniformly() {
        let owned = String::from("present");
        assert_eq!(
            drift_detail(owned.clone(), "absent"),
            drift_detail(owned.as_str(), "absent")
        );
        assert_eq!(
            drift_detail(&owned, "absent"),
            drift_detail(owned, "absent")
        );
    }

    #[test]
    fn a_question_mark_placeholder_composes_like_any_other_value() {
        // status.rs's drift event renders `?` when a value was never
        // recorded (`Option::as_deref().unwrap_or("?")`) — the composer
        // does not special-case it.
        assert_eq!(drift_detail("?", "present"), "want: ?, have: present");
    }
}

#[cfg(test)]
mod collapse_tests {
    use super::collapse_to_subject_line;

    #[test]
    fn single_line_passes_through_trimmed() {
        assert_eq!(collapse_to_subject_line("simple error"), "simple error");
        assert_eq!(
            collapse_to_subject_line("  padded  "),
            "padded",
            "outer whitespace must be trimmed"
        );
    }

    #[test]
    fn multi_line_joined_with_em_dash() {
        let input = "Transport endpoint is not connected\n\
                     See system logs and 'systemctl status kubelet.service' for details.";
        assert_eq!(
            collapse_to_subject_line(input),
            "Transport endpoint is not connected — \
             See system logs and 'systemctl status kubelet.service' for details."
        );
    }

    #[test]
    fn leading_and_trailing_blank_lines_skipped() {
        let input = "\n\n   \nfirst real line\nsecond real line\n   \n\n";
        assert_eq!(
            collapse_to_subject_line(input),
            "first real line — second real line"
        );
    }

    #[test]
    fn interior_blank_lines_skipped_and_inner_lines_trimmed() {
        let input = "  head  \n\n   \n\t  body  \t\n";
        assert_eq!(collapse_to_subject_line(input), "head — body");
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(collapse_to_subject_line(""), "");
        assert_eq!(collapse_to_subject_line("   "), "");
        assert_eq!(collapse_to_subject_line("\n\n\n"), "");
    }

    #[test]
    fn display_impl_consumed_not_just_strings() {
        let err = std::io::Error::other("first line\nsecond line");
        assert_eq!(
            collapse_to_subject_line(&err),
            "first line — second line",
            "any Display value (e.g. io::Error) must work"
        );
    }
}

#[cfg(test)]
mod condense_script_label_tests {
    use super::condense_script_label;

    #[test]
    fn multi_line_keeps_only_first_line_plus_ellipsis() {
        let script = "echo start\napt-get update\napt-get install -y neovim\necho done";
        assert_eq!(condense_script_label(script), "echo start …");
    }

    #[test]
    fn leading_blank_lines_skipped() {
        let script = "\n\n   \necho hello\necho world";
        assert_eq!(condense_script_label(script), "echo hello …");
    }

    #[test]
    fn single_line_no_trailing_ellipsis() {
        assert_eq!(condense_script_label("echo hello"), "echo hello");
    }

    #[test]
    fn whitespace_only_input_returns_empty() {
        assert_eq!(condense_script_label("   \n\t\n   "), "");
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(condense_script_label(""), "");
    }

    #[test]
    fn long_single_line_truncated_at_cap() {
        let long = "x".repeat(200);
        let label = condense_script_label(&long);
        assert_eq!(label.chars().count(), super::SCRIPT_LABEL_MAX_CHARS + 1); // +1 for the appended `…`
        assert!(label.ends_with('…'));
        assert!(!label.contains('\n') && !label.contains('\r'));
    }

    #[test]
    fn multibyte_utf8_truncated_without_panic() {
        // Every char is 3 bytes in UTF-8 (☃ U+2603); a byte-index truncation
        // at SCRIPT_LABEL_MAX_CHARS would land mid-character and panic.
        let long = "☃".repeat(200);
        let label = condense_script_label(&long);
        assert_eq!(label.chars().count(), super::SCRIPT_LABEL_MAX_CHARS + 1);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn lone_carriage_return_never_survives() {
        // A bare `\r` (no following `\n`) is not a line terminator to
        // `str::lines()`, so it stays embedded in the "first line" unless
        // scrubbed explicitly.
        let script = "echo hi\rthere\nnext line";
        let label = condense_script_label(script);
        assert!(!label.contains('\r'));
        assert!(!label.contains('\n'));
    }

    #[test]
    fn never_contains_newline_or_carriage_return() {
        for input in ["a\nb\nc", "\r\n\r\n", "line\r\n", "\r", "a", ""] {
            let label = condense_script_label(input);
            assert!(
                !label.contains('\n') && !label.contains('\r'),
                "condense_script_label({input:?}) = {label:?} carried a line terminator"
            );
        }
    }
}

#[cfg(test)]
mod strip_ansi_tests {
    use super::strip_ansi;

    #[test]
    fn plain_text_passes_through_unchanged() {
        assert_eq!(strip_ansi("hello world"), "hello world");
        assert_eq!(strip_ansi(""), "");
        assert_eq!(strip_ansi("✓ ✗ — →"), "✓ ✗ — →");
    }

    #[test]
    fn red_sgr_pair_stripped() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn compound_sgr_stripped() {
        assert_eq!(strip_ansi("\x1b[1;31mbold-red\x1b[0m"), "bold-red");
    }

    #[test]
    fn prefix_and_suffix_preserved_around_stripped_sgr() {
        assert_eq!(
            strip_ansi("prefix\x1b[31mcolored\x1b[0msuffix"),
            "prefixcoloredsuffix"
        );
    }

    #[test]
    fn incomplete_escape_swallowed_to_eos() {
        assert_eq!(strip_ansi("safe\x1b[31"), "safe");
        assert_eq!(strip_ansi("\x1b[31"), "");
    }

    #[test]
    fn two_byte_escape_consumed_leaving_no_raw_escape_byte() {
        // A raw `\x1b` surviving sanitization is the thing this function
        // exists to prevent, so a two-byte escape is consumed, not passed on.
        assert_eq!(strip_ansi("a\x1bXb"), "ab");
        assert_eq!(strip_ansi("a\x1b7b"), "ab");
    }

    #[test]
    fn charset_selector_consumes_its_trailing_byte() {
        assert_eq!(strip_ansi("\x1b(Bplain"), "plain");
    }

    #[test]
    fn non_sgr_csi_does_not_swallow_the_rest_of_the_line() {
        // `nvim --headless` emits clear-screen and cursor-home; neither ends
        // in `m`, and an SGR-only stripper ate everything that followed.
        assert_eq!(strip_ansi("\x1b[2J\x1b[Hcleared"), "cleared");
        assert_eq!(strip_ansi("\x1b[1;1Hhome"), "home");
        assert_eq!(strip_ansi("before\x1b[Kafter"), "beforeafter");
    }

    #[test]
    fn osc_title_sequence_stripped_at_bel_and_at_st() {
        assert_eq!(strip_ansi("\x1b]0;window title\x07kept"), "kept");
        assert_eq!(strip_ansi("\x1b]0;window title\x1b\\kept"), "kept");
    }
}

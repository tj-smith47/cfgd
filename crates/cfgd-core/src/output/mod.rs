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
pub use printer::{DocCapture, Printer, PromptAnswer};

pub mod section_guard;
pub use section_guard::SectionGuard;

pub mod status_builder;
pub use status_builder::StatusBuilder;

pub mod spinner;
pub use spinner::{ProgressBar, Spinner};

pub mod process;
pub use process::CommandOutput;

pub mod prompts;

pub mod raw;

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
/// Walks `char`s (ANSI CSI sequences are all ASCII, so this is safe across
/// multi-byte UTF-8 glyphs like `✓ ✗ — →`). Treats `\x1b[` followed by
/// anything up to the next `m` (inclusive) as a single escape — incomplete
/// escapes that never reach `m` are swallowed to end-of-string, which is the
/// safer outcome at a sanitization boundary (a malicious unterminated escape
/// shouldn't paint anything).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            for inner in chars.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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
    Doc::new().status(Role::Fail, message).with_data(payload)
}

pub mod render_doc;

pub mod structured;

#[cfg(feature = "test-helpers")]
pub mod test_capture;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;

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
    fn bare_escape_without_bracket_passes_through() {
        assert_eq!(strip_ansi("a\x1bX"), "a\x1bX");
    }
}

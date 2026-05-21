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

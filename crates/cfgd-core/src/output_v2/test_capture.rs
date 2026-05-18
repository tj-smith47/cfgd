//! Test-only Printer constructors. Gated behind the `test-helpers` Cargo feature
//! so production builds drop the buffered-capture machinery.

#![cfg(feature = "test-helpers")]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::printer::{DocCapture, Printer, PromptAnswer};
use super::renderer::{Renderer, StringSink, Writer};
use super::{OutputFormat, Theme, Verbosity};

fn build_test_printer(
    buf: Arc<Mutex<String>>,
    theme: Theme,
    verbosity: Verbosity,
    format: OutputFormat,
    test_doc_capture: Option<DocCapture>,
    prompt_queue: Option<Arc<Mutex<VecDeque<PromptAnswer>>>>,
) -> Printer {
    let sink: Arc<dyn Writer> = Arc::new(StringSink(buf));
    Printer {
        renderer: Arc::new(Renderer::new(theme, verbosity)),
        output_format: format,
        sink_stderr: sink.clone(),
        sink_stdout: sink,
        multi_progress: indicatif::MultiProgress::new(),
        syntax_set: syntect::parsing::SyntaxSet::load_defaults_newlines(),
        theme_set: syntect::highlighting::ThemeSet::load_defaults(),
        test_doc_capture,
        prompt_queue,
    }
}

impl Printer {
    /// Legacy capture: returns a flat-string buffer. Defaults to `Verbosity::Quiet`
    /// (matches the production `with_format`-under-structured-output defaults) and
    /// `OutputFormat::Table`.
    pub fn for_test() -> (Self, Arc<Mutex<String>>) {
        Self::for_test_with_format(OutputFormat::Table)
    }

    /// Like `for_test` but lets callers pick the verbosity. Required by tests
    /// that exercise the human render pipeline (sections, bullets, headings),
    /// which is suppressed under `Verbosity::Quiet`.
    pub fn for_test_at(verbosity: Verbosity) -> (Self, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let p = build_test_printer(
            buf.clone(),
            Theme::default(),
            verbosity,
            OutputFormat::Table,
            None,
            None,
        );
        (p, buf)
    }

    /// Like `for_test_at` but with an explicit Theme. Used by bucket (d) to
    /// snapshot per-preset output without the struct-literal Printer anti-pattern.
    pub fn for_test_with_theme(theme: Theme, verbosity: Verbosity) -> (Self, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let p = build_test_printer(
            buf.clone(),
            theme,
            verbosity,
            OutputFormat::Table,
            None,
            None,
        );
        (p, buf)
    }

    pub fn for_test_with_format(format: OutputFormat) -> (Self, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let p = build_test_printer(
            buf.clone(),
            Theme::default(),
            Verbosity::Quiet,
            format,
            None,
            None,
        );
        (p, buf)
    }

    /// Capture for buffered commands: returns a `DocCapture` with both the
    /// human-rendered string and the Doc's JSON form available.
    pub fn for_test_doc() -> (Self, DocCapture) {
        let human = Arc::new(Mutex::new(String::new()));
        let doc_json = Arc::new(Mutex::new(None));
        let cap = DocCapture {
            human: human.clone(),
            doc_json,
        };
        let p = build_test_printer(
            human,
            Theme::default(),
            Verbosity::Normal,
            OutputFormat::Table,
            Some(cap.clone_internal()),
            None,
        );
        (p, cap)
    }

    /// Capture + canned prompt responses.
    pub fn for_test_with_prompt_responses(
        responses: Vec<PromptAnswer>,
    ) -> (Self, Arc<Mutex<String>>) {
        Self::for_test_with_prompt_responses_at(responses, Verbosity::Quiet)
    }

    /// Capture + canned prompt responses at a chosen verbosity. Required by
    /// tests that drive a prompt AND assert on the rendered status the
    /// command emits in response (e.g. apply_plan's "Skipped" notice) —
    /// the Quiet default filters non-Fail statuses, hiding the line under
    /// assertion.
    pub fn for_test_with_prompt_responses_at(
        responses: Vec<PromptAnswer>,
        verbosity: Verbosity,
    ) -> (Self, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let p = build_test_printer(
            buf.clone(),
            Theme::default(),
            verbosity,
            OutputFormat::Table,
            None,
            Some(Arc::new(Mutex::new(VecDeque::from(responses)))),
        );
        (p, buf)
    }

    /// Doc capture with an explicit OutputFormat. Required by snapshot tests
    /// that exercise behaviour gated on `Printer::is_wide()` (e.g.
    /// `source list --wide` table layout): the default `for_test_doc`
    /// captures at `OutputFormat::Table`, leaving the wide branch
    /// untestable. Use `OutputFormat::Wide` to drive the wide-table path.
    pub fn for_test_doc_with_format(format: OutputFormat) -> (Self, DocCapture) {
        let human = Arc::new(Mutex::new(String::new()));
        let doc_json = Arc::new(Mutex::new(None));
        let cap = DocCapture {
            human: human.clone(),
            doc_json,
        };
        let p = build_test_printer(
            human,
            Theme::default(),
            Verbosity::Normal,
            format,
            Some(cap.clone_internal()),
            None,
        );
        (p, cap)
    }

    /// Doc capture combined with canned prompt responses. Required by snapshot
    /// tests that drive `cmd_x` against a tempdir fixture while the command
    /// itself calls `prompt_confirm` / `prompt_text` (e.g. profile create's
    /// interactive mode, profile edit's accept-retry branch).
    pub fn for_test_doc_with_prompt_responses(responses: Vec<PromptAnswer>) -> (Self, DocCapture) {
        let human = Arc::new(Mutex::new(String::new()));
        let doc_json = Arc::new(Mutex::new(None));
        let cap = DocCapture {
            human: human.clone(),
            doc_json,
        };
        let p = build_test_printer(
            human,
            Theme::default(),
            Verbosity::Normal,
            OutputFormat::Table,
            Some(cap.clone_internal()),
            Some(Arc::new(Mutex::new(VecDeque::from(responses)))),
        );
        (p, cap)
    }
}

impl DocCapture {
    pub(super) fn clone_internal(&self) -> Self {
        Self {
            human: self.human.clone(),
            doc_json: self.doc_json.clone(),
        }
    }

    /// Snapshot helper: assert the captured human output matches the contents
    /// of `src/output_v2/tests/snapshots/<name>`. Use `INSTA_UPDATE=always
    /// cargo test` to refresh.
    pub fn assert_human_snapshot(&self, name: &str) {
        self.assert_human_snapshot_in(std::path::Path::new("src/output_v2/tests/snapshots"), name);
    }

    pub fn assert_json_snapshot(&self, name: &str) {
        self.assert_json_snapshot_in(std::path::Path::new("src/output_v2/tests/snapshots"), name);
    }

    /// Like `assert_human_snapshot` but rooted at `base` instead of the
    /// hard-coded `src/output_v2/tests/snapshots`. Use from downstream test
    /// crates that store snapshots elsewhere (e.g. `tests/output_snapshots/`).
    pub fn assert_human_snapshot_in(&self, base: &std::path::Path, name: &str) {
        let actual = strip_ansi(&self.human());
        snapshot_assert(base, name, &actual);
    }

    pub fn assert_json_snapshot_in(&self, base: &std::path::Path, name: &str) {
        let actual = self
            .json()
            .map(|v| serde_json::to_string_pretty(&v).unwrap())
            .unwrap_or_default();
        snapshot_assert(base, name, &actual);
    }
}

fn snapshot_assert(base: &std::path::Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    pretty_assertions::assert_eq!(actual, expected, "snapshot mismatch: {name}");
}

/// ANSI-stripping helper used by `assert_*_snapshot`. Mirrors
/// `crate::output_v2::tests::strip_ansi` but lives here because this file is
/// feature-gated (not test-gated) — `crate::output_v2::tests` is only present
/// under `#[cfg(test)]` and is unreachable from a `cargo build
/// --features test-helpers` compile of this module.
fn strip_ansi(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_test_returns_buffer() {
        let (p, buf) = Printer::for_test();
        p.heading("Hi");
        p.flush();
        // Buffer access compiles; contents depend on verbosity defaults.
        let _contents = buf.lock().unwrap().clone();
    }

    #[test]
    fn for_test_doc_returns_capture() {
        let (_p, cap) = Printer::for_test_doc();
        assert_eq!(cap.human(), "");
    }
}

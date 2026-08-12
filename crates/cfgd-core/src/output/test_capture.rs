//! Test-only Printer constructors. Gated behind the `test-helpers` Cargo feature
//! so production builds drop the buffered-capture machinery.

#![cfg(feature = "test-helpers")]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::printer::{DocCapture, Printer, PromptAnswer};
use super::renderer::{Renderer, StringSink, Writer};
use super::{OutputFormat, Theme, Verbosity};

/// The draw target behind [`Printer::for_test_with_live_bars`]: everything
/// indicatif paints is appended to the same buffer the printer's sink writes
/// to, in the order the two reach it.
#[derive(Debug)]
struct RecordingTerm {
    drawn: Arc<Mutex<String>>,
}

impl RecordingTerm {
    fn record(&self, s: &str) -> std::io::Result<()> {
        self.drawn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_str(s);
        Ok(())
    }
}

impl indicatif::TermLike for RecordingTerm {
    fn width(&self) -> u16 {
        200
    }
    fn height(&self) -> u16 {
        40
    }
    fn move_cursor_up(&self, _n: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn move_cursor_down(&self, _n: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn move_cursor_right(&self, _n: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn move_cursor_left(&self, _n: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_line(&self, s: &str) -> std::io::Result<()> {
        self.record(s)?;
        self.record("\n")
    }
    fn write_str(&self, s: &str) -> std::io::Result<()> {
        self.record(s)
    }
    fn clear_line(&self) -> std::io::Result<()> {
        Ok(())
    }
    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

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
        output_error: std::sync::atomic::AtomicBool::new(false),
        // A capture buffer is not a terminal, whatever the suite was invoked
        // from: pinning it here is what makes the non-TTY rendering reachable
        // under `cargo test` from an interactive shell.
        live_region: false,
        list_envelope: false,
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

    /// Like `for_test_at` but with an explicit Theme. Used by the themes
    /// snapshot tests to capture per-preset output without the struct-literal
    /// Printer anti-pattern.
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

    /// Capture wired the way PRODUCTION wires a printer that has bars: the
    /// renderer knows its `MultiProgress`, so a line emitted while a bar is
    /// live is routed through it instead of straight to the sink.
    ///
    /// Both the bar draws and the routed lines land in ONE buffer, which is the
    /// point — garbling is two writers interleaving inside a single physical
    /// stream, and two separate captures could not show it. Every other test
    /// constructor builds a bar-less renderer, where routing never happens and
    /// the question cannot be asked.
    pub fn for_test_with_live_bars() -> (Self, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let multi =
            indicatif::MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::term_like(
                Box::new(RecordingTerm { drawn: buf.clone() }),
            ));
        let sink: Arc<dyn Writer> = Arc::new(StringSink(buf.clone()));
        let p = Printer {
            renderer: Arc::new(Renderer::with_bars(
                Theme::default(),
                Verbosity::Normal,
                multi.clone(),
            )),
            output_format: OutputFormat::Table,
            sink_stderr: sink.clone(),
            sink_stdout: sink,
            multi_progress: multi,
            syntax_set: syntect::parsing::SyntaxSet::load_defaults_newlines(),
            theme_set: syntect::highlighting::ThemeSet::load_defaults(),
            test_doc_capture: None,
            prompt_queue: None,
            output_error: std::sync::atomic::AtomicBool::new(false),
            // The whole point of this constructor: the repainting path is
            // reachable without a real terminal, so the proof obligation it
            // carries runs in the ordinary suite rather than only under a pty.
            live_region: true,
            list_envelope: false,
        };
        (p, buf)
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
    /// of `src/output/tests/snapshots/<name>`. Use `INSTA_UPDATE=always
    /// cargo test` to refresh.
    pub fn assert_human_snapshot(&self, name: &str) {
        self.assert_human_snapshot_in(std::path::Path::new("src/output/tests/snapshots"), name);
    }

    pub fn assert_json_snapshot(&self, name: &str) {
        self.assert_json_snapshot_in(std::path::Path::new("src/output/tests/snapshots"), name);
    }

    /// Like `assert_human_snapshot` but rooted at `base` instead of the
    /// hard-coded `src/output/tests/snapshots`. Use from downstream test
    /// crates that store snapshots elsewhere (e.g. `tests/output_snapshots/`).
    pub fn assert_human_snapshot_in(&self, base: &std::path::Path, name: &str) {
        let actual = strip_ansi(&self.human());
        assert_snapshot_at(base, name, &actual);
    }

    pub fn assert_json_snapshot_in(&self, base: &std::path::Path, name: &str) {
        let actual = self
            .json()
            .map(|v| serde_json::to_string_pretty(&v).unwrap())
            .unwrap_or_default();
        assert_snapshot_at(base, name, &actual);
    }
}

pub fn assert_snapshot_at(base: &std::path::Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    // Windows: captured `actual` from native println/writeln carries `\r\n`;
    // committed snapshot files use `\n`. Normalize both sides so the byte
    // comparison succeeds without per-test workarounds.
    let actual_norm = actual.replace("\r\n", "\n");
    let expected_norm = expected.replace("\r\n", "\n");
    pretty_assertions::assert_eq!(actual_norm, expected_norm, "snapshot mismatch: {name}");
}

/// ANSI-stripping helper used by `assert_*_snapshot` and by external
/// integration tests that consume the `test-helpers` feature. Re-exported
/// from the canonical location at `crate::output::strip_ansi` so the
/// long-established `crate::output::test_capture::strip_ansi` path keeps
/// resolving from feature-gated callers.
pub use crate::output::strip_ansi;

/// Strip ` (N.Ns)` spinner finish-duration markers so snapshots survive
/// runtime variance. Matches ` (` + digits + `.` + digits + `s)`.
pub fn strip_spinner_duration(s: String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(idx) = rest.find(" (") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 2..];
        let digit_end = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        if digit_end > 0 && after.as_bytes().get(digit_end).copied() == Some(b'.') {
            let frac_start = digit_end + 1;
            let frac_rest = &after[frac_start..];
            let frac_end = frac_rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(frac_rest.len());
            let total = frac_start + frac_end;
            if frac_end > 0
                && after.as_bytes().get(total).copied() == Some(b's')
                && after.as_bytes().get(total + 1).copied() == Some(b')')
            {
                rest = &after[total + 2..];
                continue;
            }
        }
        out.push_str(" (");
        rest = after;
    }
    out.push_str(rest);
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

//! User-facing handle. Holds the Renderer (single layout authority), the
//! active OutputFormat, and the writers for stderr (status output) +
//! stdout (structured/data output). Section/spinner/process/prompt/buffered
//! emission methods land in later R1 tasks.
//!
//! R1 skeleton: several `Printer` fields are constructed now but not yet
//! consumed. `sink_stderr` is wired (T14 emit family, T15 section guard,
//! T16 StatusBuilder). Pending wiring: `sink_stdout` (T22+ `data_line`/
//! structured emit), `multi_progress` (T19 `spinner`/`progress_bar`),
//! `syntax_set`/`theme_set` (T23 `syntax_highlight`), `test_doc_capture`
//! (T20 test-helpers feature), `prompt_queue` (T26 prompts). The
//! `dead_code` allow drops as those land.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use console::Term;

use super::renderer::{Renderer, StatusFields, Table, Writer};
use super::{OutputFormat, Role, Theme, Verbosity};

/// One canned prompt response. Used by tests to drive prompt_* past
/// non-interactive guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAnswer {
    Confirm(bool),
    Text(String),
    Select(String),
}

/// Captured-output handle returned by `Printer::for_test_doc`. Available with
/// the `test-helpers` feature.
pub struct DocCapture {
    pub(crate) human: Arc<Mutex<String>>,
    pub(crate) doc_json: Arc<Mutex<Option<serde_json::Value>>>,
}

impl DocCapture {
    pub fn human(&self) -> String {
        self.human.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn json(&self) -> Option<serde_json::Value> {
        self.doc_json
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

pub struct Printer {
    pub(crate) renderer: Arc<Renderer>,
    pub(crate) output_format: OutputFormat,
    pub(crate) sink_stderr: Arc<dyn Writer>,
    pub(crate) sink_stdout: Arc<dyn Writer>,
    pub(crate) multi_progress: indicatif::MultiProgress,
    pub(crate) syntax_set: syntect::parsing::SyntaxSet,
    pub(crate) theme_set: syntect::highlighting::ThemeSet,
    /// Set under `test-helpers` when `for_test_doc` is used.
    pub(crate) test_doc_capture: Option<DocCapture>,
    /// Set under `test-helpers` when prompt responses are seeded.
    pub(crate) prompt_queue: Option<Arc<Mutex<VecDeque<PromptAnswer>>>>,
}

impl Printer {
    /// Production constructor: stderr/stdout via `console::Term`.
    pub fn new(verbosity: Verbosity) -> Self {
        Self::with_format(verbosity, None, OutputFormat::Table)
    }

    pub fn with_theme_name(verbosity: Verbosity, theme_name: Option<&str>) -> Self {
        Self::with_format(verbosity, theme_name, OutputFormat::Table)
    }

    pub fn with_format(
        verbosity: Verbosity,
        theme_name: Option<&str>,
        output_format: OutputFormat,
    ) -> Self {
        // Honor NO_COLOR / TERM=dumb at construction.
        if std::env::var_os("NO_COLOR").is_some()
            || std::env::var_os("TERM").is_some_and(|t| t == "dumb")
        {
            console::set_colors_enabled(false);
            console::set_colors_enabled_stderr(false);
        }
        // Auto-quiet under structured output.
        let verbosity = if output_format.is_structured() {
            Verbosity::Quiet
        } else {
            verbosity
        };
        let theme = theme_name.map(Theme::from_preset).unwrap_or_default();
        Self {
            renderer: Arc::new(Renderer::new(theme, verbosity)),
            output_format,
            sink_stderr: Arc::new(Term::stderr()),
            sink_stdout: Arc::new(Term::stdout()),
            multi_progress: indicatif::MultiProgress::new(),
            syntax_set: syntect::parsing::SyntaxSet::load_defaults_newlines(),
            theme_set: syntect::highlighting::ThemeSet::load_defaults(),
            test_doc_capture: None,
            prompt_queue: None,
        }
    }

    pub fn verbosity(&self) -> Verbosity {
        self.renderer.verbosity
    }
    pub fn output_format(&self) -> &OutputFormat {
        &self.output_format
    }
    pub fn is_structured(&self) -> bool {
        self.output_format.is_structured()
    }
    pub fn is_wide(&self) -> bool {
        matches!(self.output_format, OutputFormat::Wide)
    }

    /// Disable color globally (today's `disable_colors`).
    pub fn disable_colors() {
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
    }

    // ----- Top-level emit methods (depth 0) -----

    pub fn heading(&self, text: impl Into<String>) {
        let depth = self.renderer.enforce_top_level_emit(0);
        // render_heading is hardcoded to depth 0 today; for the runtime-check
        // re-route path we emit a styled bold line at the section's depth so
        // the output stays readable despite the shape being wrong.
        if depth == 0 {
            self.renderer
                .render_heading(self.sink_stderr.as_ref(), &text.into());
        } else {
            let text = text.into();
            let styled = self.renderer.theme.header.apply_to(&text).to_string();
            self.renderer
                .write_line(self.sink_stderr.as_ref(), depth, &styled);
        }
    }

    pub fn kv(&self, key: impl Into<String>, value: impl Into<String>) {
        // kv buffers; flush will use the renderer's current depth, so the
        // runtime check is informational here — no depth value to thread
        // through, but we still want the warn/assert at the call site.
        let _ = self.renderer.enforce_top_level_emit(0);
        self.renderer.render_kv(&key.into(), &value.into());
    }

    pub fn kv_block<I, K, V>(&self, pairs: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let depth = self.renderer.enforce_top_level_emit(0);
        let pairs: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self.renderer
            .render_kv_block(self.sink_stderr.as_ref(), depth, &pairs);
    }

    pub fn hint(&self, text: impl Into<String>) {
        let depth = self.renderer.enforce_top_level_emit(0);
        self.renderer
            .render_hint(self.sink_stderr.as_ref(), depth, &text.into());
    }

    pub fn note(&self, text: impl Into<String>) {
        let depth = self.renderer.enforce_top_level_emit(0);
        self.renderer
            .render_note(self.sink_stderr.as_ref(), depth, &text.into());
    }

    pub fn table(&self, table: Table) {
        let depth = self.renderer.enforce_top_level_emit(0);
        self.renderer
            .render_table(self.sink_stderr.as_ref(), depth, &table);
    }

    /// Status with no extra fields. For detail/duration/target, use the builder
    /// returned by the binding helper `status` (see status_builder.rs).
    pub fn status_simple(&self, role: Role, subject: impl Into<String>) {
        let depth = self.renderer.enforce_top_level_emit(0);
        let subject = subject.into();
        self.renderer.render_status(
            self.sink_stderr.as_ref(),
            depth,
            &StatusFields {
                role,
                subject: &subject,
                detail: None,
                duration: None,
                target: None,
            },
        );
    }

    /// Status builder at depth 0. Commits on Drop.
    pub fn status(
        &self,
        role: Role,
        subject: impl Into<String>,
    ) -> super::status_builder::StatusBuilder<'_> {
        let depth = self.renderer.enforce_top_level_emit(0);
        super::status_builder::StatusBuilder::new(
            self.renderer.clone(),
            self.sink_stderr.clone(),
            depth,
            role,
            subject,
        )
    }

    /// Final flush — call at the end of a streaming command to ensure any
    /// buffered kvs land. (Drop on Printer would also do this but tests need
    /// explicit control.)
    pub fn flush(&self) {
        self.renderer.flush_kv_buffer(self.sink_stderr.as_ref());
    }

    // ----- Section entry points -----

    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section(&self, name: impl Into<String>) -> super::section_guard::SectionGuard<'_> {
        self.renderer.render_section_open(&name.into(), true);
        super::section_guard::SectionGuard {
            printer: self,
            renderer: self.renderer.clone(),
            sink: self.sink_stderr.clone(),
            depth: 1,
        }
    }

    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section_or_collapse(
        &self,
        name: impl Into<String>,
    ) -> super::section_guard::SectionGuard<'_> {
        self.renderer.render_section_open(&name.into(), false);
        super::section_guard::SectionGuard {
            printer: self,
            renderer: self.renderer.clone(),
            sink: self.sink_stderr.clone(),
            depth: 1,
        }
    }
}

impl Drop for Printer {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn structured_format_auto_quiets() {
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
        assert_eq!(p.verbosity(), Verbosity::Quiet);
    }

    #[test]
    #[serial]
    fn table_format_keeps_verbosity() {
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Table);
        assert_eq!(p.verbosity(), Verbosity::Normal);
    }

    #[test]
    #[serial]
    fn is_structured_classifies() {
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
        assert!(p.is_structured());
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Table);
        assert!(!p.is_structured());
    }

    use std::sync::Mutex;

    use super::super::renderer::StringSink;

    /// Build a Printer whose stderr sink is a captured StringSink. Production
    /// `for_test`/`for_test_doc` come later; this is a per-test helper.
    fn test_printer() -> (Printer, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink: Arc<dyn Writer> = Arc::new(StringSink(buf.clone()));
        let p = Printer {
            renderer: Arc::new(Renderer::new(Theme::default(), Verbosity::Normal)),
            output_format: OutputFormat::Table,
            sink_stderr: sink.clone(),
            sink_stdout: sink,
            multi_progress: indicatif::MultiProgress::new(),
            syntax_set: syntect::parsing::SyntaxSet::load_defaults_newlines(),
            theme_set: syntect::highlighting::ThemeSet::load_defaults(),
            test_doc_capture: None,
            prompt_queue: None,
        };
        (p, buf)
    }

    fn strip_ansi(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                while i < bytes.len() && bytes[i] != b'm' {
                    i += 1;
                }
                i += 1;
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn section_with_bullets_renders_indented() {
        let (p, buf) = test_printer();
        {
            let s = p.section("Files");
            s.bullet("foo.txt");
            s.bullet("bar.txt");
        } // section closes
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Files\n"), "got: {out:?}");
        assert!(out.contains("\n  - foo.txt\n"), "got: {out:?}");
        assert!(out.contains("\n  - bar.txt\n"), "got: {out:?}");
    }

    #[test]
    fn section_or_collapse_with_no_emits_leaves_no_trace() {
        let (p, buf) = test_printer();
        {
            let _s = p.section_or_collapse("Empty");
        }
        p.flush();
        assert!(buf.lock().unwrap().trim().is_empty());
    }

    #[test]
    fn nested_sections_indent_two_levels() {
        let (p, buf) = test_printer();
        {
            let outer = p.section("Outer");
            {
                let inner = outer.section("Inner");
                inner.bullet("deep");
            }
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Outer\n"));
        assert!(out.contains("\n  Inner\n"));
        assert!(out.contains("\n    - deep\n"));
    }

    /// In debug builds, a top-level emit reached while a section is open
    /// trips `debug_assert!` in `Renderer::enforce_top_level_emit`. We catch
    /// the panic to verify the assert fires.
    #[test]
    #[cfg(debug_assertions)]
    fn debug_mode_panics_on_top_level_emit_during_section() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (p, _buf) = test_printer();
            let _s = p.section("Outer");
            p.heading("MidSection"); // debug_assert! fires
        }));
        assert!(result.is_err(), "expected debug_assert! panic");
    }

    /// In release builds, the assert is compiled out; the warn-once fires
    /// and the emit reroutes to the section's depth instead of column 0.
    #[test]
    #[cfg(not(debug_assertions))]
    fn release_mode_reroutes_top_level_emit_during_section() {
        let (p, buf) = test_printer();
        {
            let _s = p.section("Outer");
            p.heading("MidSection"); // would assert in debug; reroutes in release
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        // The heading rendered at depth 1 (inside the section), not column 0.
        assert!(
            out.contains("\n  MidSection\n"),
            "expected indented; got: {out:?}"
        );
    }
}

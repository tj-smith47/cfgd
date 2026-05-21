//! User-facing handle. Holds the Renderer (single layout authority), the
//! active OutputFormat, and the writers for stderr (status output) +
//! stdout (structured/data output). Sinks: `sink_stderr` for status,
//! `sink_stdout` for `data_line`, `multi_progress` for spinners and progress
//! bars, `syntax_set` / `theme_set` for `syntax_highlight`. The
//! `test_doc_capture` and `prompt_queue` fields are populated by test
//! helpers (gated on the `test-helpers` feature).

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

    /// Force color globally regardless of TTY detection. Symmetric to
    /// `disable_colors` so demo / example binaries that pipe their output for
    /// capture can still emit real ANSI escapes. Production CLI dispatch goes
    /// through `with_format`, which honors `NO_COLOR` and structured-output
    /// gating — call this only from non-production entry points.
    pub fn enable_colors() {
        console::set_colors_enabled(true);
        console::set_colors_enabled_stderr(true);
    }

    /// Style a string with the role's theme color, returning the pre-styled
    /// content for embedding inside another `status` / `kv` value. The
    /// terminal-write boundary stays inside `Printer` — callers pass the
    /// returned `String` back into a Printer method to actually render.
    ///
    /// Use sparingly: nested ANSI styling only renders correctly when the
    /// pre-styled segment is at the END of its enclosing subject (because
    /// the inner reset `\x1b[0m` kills the outer color for any trailing
    /// content). Suitable for trailing labels like ` [source-name]`; not
    /// suitable for highlighting a word in the middle of a sentence.
    pub fn style(&self, role: super::Role, text: impl AsRef<str>) -> String {
        let (_icon, style) = super::renderer::role_glyph(&self.renderer.theme, role);
        style.apply_to(text.as_ref()).to_string()
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
        let _depth = self.renderer.enforce_top_level_emit(0);
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

    // ----- Spinners / progress (depth 0) -----

    /// Top-level spinner (depth 0). Required for ~14 lib-side call sites
    /// in cfgd-core that today take `&Printer` and have no section context
    /// (oci/, upgrade/, sources/, modules/git.rs, reconciler/scripts.rs).
    #[must_use]
    pub fn spinner(&self, message: impl Into<String>) -> super::spinner::Spinner<'_> {
        let message = message.into();
        let bar = super::spinner::make_spinner_bar(
            &self.multi_progress,
            &self.renderer,
            self.verbosity(),
            &message,
        );
        super::spinner::Spinner {
            renderer: self.renderer.clone(),
            sink: self.sink_stderr.clone(),
            depth: 0,
            bar,
            message,
            finished: false,
            _phantom: std::marker::PhantomData,
        }
    }

    #[must_use]
    pub fn progress_bar(
        &self,
        total: u64,
        message: impl Into<String>,
    ) -> super::spinner::ProgressBar<'_> {
        let bar = super::spinner::make_progress_bar(
            &self.multi_progress,
            total,
            self.verbosity(),
            &message.into(),
        );
        super::spinner::ProgressBar {
            bar,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Expose the underlying MultiProgress for callers that need fine-grained
    /// control (kept for API parity with the old Printer).
    pub fn multi_progress(&self) -> &indicatif::MultiProgress {
        &self.multi_progress
    }

    /// Run an external command at top-level (depth 0) with live output.
    /// TTY+non-quiet → spinner with tailing ring; otherwise → streaming lines.
    /// Either path captures full stdout/stderr in the returned `CommandOutput`.
    pub fn run(
        &self,
        cmd: &mut std::process::Command,
        label: impl Into<String>,
    ) -> std::io::Result<super::process::CommandOutput> {
        // run is depth-0 only; the clamp would still return 0, so the value is discarded.
        let _ = self.renderer.enforce_top_level_emit(0);
        super::process::run_command(
            &self.renderer,
            self.sink_stderr.as_ref(),
            &self.multi_progress,
            0,
            cmd,
            &label.into(),
        )
    }

    /// Final flush — call at the end of a streaming command to ensure any
    /// buffered kvs land. (Drop on Printer would also do this but tests need
    /// explicit control.)
    pub fn flush(&self) {
        self.renderer.flush_kv_buffer(self.sink_stderr.as_ref());
    }

    /// Force human render of a Doc to stderr, regardless of `output_format`.
    /// Used by tests; production code should call `emit` (T24) which routes by
    /// `OutputFormat` and falls back to this for human formats.
    pub fn render(&self, doc: super::doc::Doc) {
        super::render_doc::render_doc(&self.renderer, self.sink_stderr.as_ref(), &doc);
    }

    /// Routed emit: structured formats go to stdout as JSON/YAML/etc.; Table/Wide
    /// go to stderr as the human render. This is the canonical buffered-output
    /// entry; production callers use this, not `render`.
    pub fn emit(&self, doc: super::doc::Doc) {
        // Capture the Doc's JSON form for tests, regardless of output_format.
        if let Some(cap) = &self.test_doc_capture {
            let json = doc.data_or_self_json();
            *cap.doc_json.lock().unwrap_or_else(|e| e.into_inner()) = Some(json);
        }
        let handled = super::structured::emit_structured(
            self.sink_stdout.as_ref(),
            &doc,
            &self.output_format,
        );
        if !handled {
            self.render(doc);
        }
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
    #[cfg(feature = "test-helpers")]
    use crate::output::tests::strip_ansi;
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

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_with_bullets_renders_indented() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
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

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_or_collapse_with_no_emits_leaves_no_trace() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let _s = p.section_or_collapse("Empty");
        }
        p.flush();
        assert!(buf.lock().unwrap().trim().is_empty());
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn nested_sections_indent_two_levels() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
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

    #[cfg(feature = "test-helpers")]
    #[test]
    fn render_doc_with_section_indents_correctly() {
        use super::super::doc::Doc;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        let doc = Doc::new()
            .heading("Status")
            .kv("Profile", "dev")
            .section("Files", |s| s.bullet("foo.txt").bullet("bar.txt"));
        p.render(doc);
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Status\n"));
        assert!(out.contains("Profile  dev"));
        assert!(out.contains("Files\n"));
        assert!(out.contains("\n  - foo.txt\n"));
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn empty_section_or_collapse_in_doc_leaves_no_trace() {
        use super::super::doc::Doc;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        let doc = Doc::new()
            .heading("Status")
            .section_or_collapse::<_>("Empty", |s| s);
        p.render(doc);
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Status"));
        assert!(!out.contains("Empty"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn emit_json_writes_data_payload_to_stdout() {
        use super::super::doc::Doc;
        #[derive(serde::Serialize)]
        struct P {
            foo: u32,
        }
        let (p, buf) = Printer::for_test_with_format(OutputFormat::Json);
        let doc = Doc::new().heading("S").with_data(P { foo: 7 });
        p.emit(doc);
        let out = buf.lock().unwrap();
        assert!(out.contains("\"foo\": 7"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn emit_table_writes_human_render() {
        use super::super::doc::Doc;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        let doc = Doc::new().heading("Title").kv("k", "v");
        p.emit(doc);
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Title"));
        assert!(out.contains("k  v"));
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn emit_with_doc_capture_records_both_shapes() {
        use super::super::doc::Doc;
        let (p, cap) = Printer::for_test_doc();
        let doc = Doc::new().heading("S").kv("k", "v");
        p.emit(doc);
        p.flush();
        let human = cap.human();
        let json = cap.json().unwrap();
        assert!(human.contains("S"), "got: {human:?}");
        assert!(human.contains("k"));
        assert!(json["heading"].as_str() == Some("S"));
    }

    /// In debug builds, a top-level emit reached while a section is open
    /// trips `debug_assert!` in `Renderer::enforce_top_level_emit`. We catch
    /// the panic to verify the assert fires.
    #[cfg(feature = "test-helpers")]
    #[test]
    #[cfg(debug_assertions)]
    fn debug_mode_panics_on_top_level_emit_during_section() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (p, _buf) = Printer::for_test_at(Verbosity::Normal);
            let _s = p.section("Outer");
            p.heading("MidSection"); // debug_assert! fires
        }));
        assert!(result.is_err(), "expected debug_assert! panic");
    }

    /// In release builds, the assert is compiled out; the warn-once fires
    /// and the emit reroutes to the section's depth instead of column 0.
    #[cfg(feature = "test-helpers")]
    #[test]
    #[cfg(not(debug_assertions))]
    fn release_mode_reroutes_top_level_emit_during_section() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
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
        assert!(
            !out.contains("\nMidSection\n"),
            "unindented form leaked through: {out:?}"
        );
    }
}

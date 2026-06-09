//! User-facing handle. Holds the Renderer (single layout authority), the
//! active OutputFormat, and the writers for stderr (status output) +
//! stdout (structured/data output). Sinks: `sink_stderr` for status,
//! `sink_stdout` for `data_line`, `multi_progress` for spinners and progress
//! bars, `syntax_set` / `theme_set` for `syntax_highlight`. The
//! `test_doc_capture` and `prompt_queue` fields are populated by test
//! helpers (gated on the `test-helpers` feature).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Flipped by `emit` when a data-dependent structured-output failure
    /// (template render/context error, template-file read error) is routed to
    /// stderr. The CLI entrypoint reads it via `had_output_error` after dispatch
    /// to exit non-zero — the failure has already been reported on stderr.
    pub(crate) output_error: AtomicBool,
    /// When set (via `--list-envelope` / `CFGD_LIST_ENVELOPE`), a top-level JSON
    /// array emitted under `-o json`/`-o yaml` is wrapped in a KRM List envelope
    /// (`{apiVersion, kind: List, items}`). Off by default — bare arrays stay
    /// byte-identical. Never affects projecting formats (name/jsonpath/template).
    pub(crate) list_envelope: bool,
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
        // Honor NO_COLOR / TERM=dumb at construction. Also disable colors
        // under structured output (Json / Yaml / Template / Jsonpath / Name)
        // so a future role-styled emission cannot leak ANSI escapes into
        // payload string fields — the contract is enforced at construction,
        // not by every caller remembering to wrap with with_data.
        if std::env::var_os("NO_COLOR").is_some()
            || std::env::var_os("TERM").is_some_and(|t| t == "dumb")
            || output_format.is_structured()
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
            output_error: AtomicBool::new(false),
            list_envelope: false,
        }
    }

    /// Enable or disable the KRM List envelope for top-level JSON arrays under
    /// `-o json`/`-o yaml`. Builder-style; off by default. Wired from the global
    /// `--list-envelope` flag / `CFGD_LIST_ENVELOPE` env var.
    pub fn with_list_envelope(mut self, enabled: bool) -> Self {
        self.list_envelope = enabled;
        self
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

    /// Emit a deprecation notice on stderr, shown regardless of verbosity or
    /// output format. Unlike `status_simple(Role::Warn, …)`, this survives the
    /// structured-output auto-quiet (which drops every non-`Fail` role), so a
    /// deprecation diagnostic reaches the user even under `-o json` / `--jsonpath`.
    /// It writes only to `sink_stderr`, never to `sink_stdout`, keeping the
    /// `-o` data channel pure.
    pub fn deprecation(&self, msg: impl Into<String>) {
        let depth = self.renderer.enforce_top_level_emit(0);
        self.renderer
            .render_deprecation(self.sink_stderr.as_ref(), depth, &msg.into());
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
            self.sink_stderr.as_ref(),
            &self.output_error,
            &doc,
            &self.output_format,
            self.list_envelope,
        );
        if !handled {
            self.render(doc);
        }
    }

    /// True if any `emit` produced a data-dependent structured-output failure
    /// (template render/context error, or a template-file that could not be
    /// read). The error was already reported on stderr; the CLI entrypoint reads
    /// this after dispatch to exit non-zero rather than falsely reporting
    /// success on a polluted/empty data channel.
    pub fn had_output_error(&self) -> bool {
        self.output_error.load(Ordering::Relaxed)
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
    use crate::output::strip_ansi;
    use crate::output::test_support::ColorsEnabledGuard;
    use crate::test_helpers::EnvVarGuard;
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

    #[test]
    #[serial]
    fn structured_output_disables_colors() {
        // Ensure NO_COLOR / TERM=dumb are not the ones triggering the gate.
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _term = EnvVarGuard::set("TERM", "xterm-256color");
        let _guard = ColorsEnabledGuard::set(true);

        for fmt in [
            OutputFormat::Json,
            OutputFormat::Yaml,
            OutputFormat::Name,
            OutputFormat::Jsonpath("{.foo}".into()),
            OutputFormat::Template("{{ . }}".into()),
        ] {
            console::set_colors_enabled(true);
            console::set_colors_enabled_stderr(true);
            let _p = Printer::with_format(Verbosity::Normal, None, fmt.clone());
            assert!(
                !console::colors_enabled(),
                "stdout colors should be disabled for {fmt:?}"
            );
            assert!(
                !console::colors_enabled_stderr(),
                "stderr colors should be disabled for {fmt:?}"
            );
        }
    }

    #[test]
    #[serial]
    fn table_format_does_not_disable_colors_implicitly() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _term = EnvVarGuard::set("TERM", "xterm-256color");
        let _guard = ColorsEnabledGuard::set(true);
        console::set_colors_enabled(true);
        console::set_colors_enabled_stderr(true);

        let _p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Table);
        assert!(
            console::colors_enabled(),
            "Table format must not implicitly disable colors"
        );
        assert!(
            console::colors_enabled_stderr(),
            "Table format must not implicitly disable stderr colors"
        );
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn deprecation_shows_under_structured_quiet() {
        // for_test_with_format(Json) builds the Printer at Verbosity::Quiet,
        // matching production's structured-output auto-quiet. A normal
        // Role::Warn status is dropped there; the deprecation path must not be.
        let (p, buf) = Printer::for_test_with_format(OutputFormat::Json);
        assert_eq!(p.verbosity(), Verbosity::Quiet);

        p.status_simple(Role::Warn, "ordinary warning");
        p.deprecation("--jsonpath is deprecated");
        p.flush();

        let out = strip_ansi(&buf.lock().unwrap_or_else(|e| e.into_inner()));
        assert!(
            !out.contains("ordinary warning"),
            "Role::Warn must stay suppressed under structured/Quiet; got: {out:?}"
        );
        assert!(
            out.contains("--jsonpath is deprecated"),
            "deprecation must be force-shown under structured/Quiet; got: {out:?}"
        );
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn emit_threads_list_envelope_through_to_structured_output() {
        // for_test_with_format(Json) shares one StringSink for stdout, so the
        // emitted payload lands in `buf`. with_list_envelope(true) must reach
        // emit_structured and wrap the top-level array.
        let payload = serde_json::json!([{"name": "alpha"}, {"name": "beta"}]);
        let (p, buf) = Printer::for_test_with_format(OutputFormat::Json);
        let p = p.with_list_envelope(true);
        p.emit(super::super::doc::Doc::new().with_data(payload.clone()));
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["apiVersion"], "cfgd.io/v1alpha1");
        assert_eq!(parsed["kind"], "List");
        assert_eq!(parsed["items"], payload);
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn emit_default_leaves_array_bare() {
        let payload = serde_json::json!([{"name": "alpha"}]);
        let (p, buf) = Printer::for_test_with_format(OutputFormat::Json);
        p.emit(super::super::doc::Doc::new().with_data(payload.clone()));
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, payload, "default emit must keep the bare array");
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
    fn section_kv_renders_key_value() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Details");
            s.kv("Name", "cfgd");
            s.kv("Version", "0.3.5");
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Details\n"), "got: {out:?}");
        assert!(out.contains("Name"), "got: {out:?}");
        assert!(out.contains("cfgd"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_kv_block_renders_pairs() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Config");
            s.kv_block([("Profile", "default"), ("Source", "local")]);
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Config\n"), "got: {out:?}");
        assert!(out.contains("Profile"), "got: {out:?}");
        assert!(out.contains("default"), "got: {out:?}");
        assert!(out.contains("Source"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_hint_renders() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Setup");
            s.hint("Run cfgd init first");
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Setup\n"), "got: {out:?}");
        assert!(out.contains("cfgd init"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_note_renders_at_verbose() {
        let (p, buf) = Printer::for_test_at(Verbosity::Verbose);
        {
            let s = p.section("Status");
            s.note("All modules up to date");
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Status\n"), "got: {out:?}");
        assert!(out.contains("up to date"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_table_renders() {
        use super::super::renderer::Table;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Packages");
            let table = Table::new(["Name", "Version"]).row(["curl", "8.0"]);
            s.table(table);
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Packages\n"), "got: {out:?}");
        assert!(out.contains("curl"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_status_simple_renders() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Apply");
            s.status_simple(Role::Ok, "package installed");
            s.status_simple(Role::Fail, "file copy failed");
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Apply\n"), "got: {out:?}");
        assert!(out.contains("package installed"), "got: {out:?}");
        assert!(out.contains("file copy failed"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_status_builder_with_detail() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Apply");
            s.status(Role::Ok, "brew install curl")
                .detail("already installed");
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("brew install curl"), "got: {out:?}");
        assert!(out.contains("already installed"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_empty_state_overrides_default() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Modules");
            s.empty_state("no modules configured");
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Modules\n"), "got: {out:?}");
        assert!(out.contains("no modules configured"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_or_collapse_with_child_renders() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section_or_collapse("Optional");
            s.bullet("present");
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Optional\n"), "got: {out:?}");
        assert!(out.contains("present"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn section_close_is_idempotent_via_explicit_close() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Closing");
            s.bullet("item");
            s.close();
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Closing\n"), "got: {out:?}");
        assert!(out.contains("item"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn nested_section_or_collapse_renders_child_content() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let outer = p.section("Outer");
            {
                let inner = outer.section_or_collapse("Inner");
                inner.status_simple(Role::Ok, "done");
            }
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Outer\n"), "got: {out:?}");
        assert!(out.contains("Inner\n"), "got: {out:?}");
        assert!(out.contains("done"), "got: {out:?}");
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

    #[cfg(feature = "test-helpers")]
    #[test]
    fn render_doc_with_hint_renders_content() {
        use super::super::doc::Doc;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        let doc = Doc::new()
            .heading("Setup")
            .hint("Run cfgd init to get started");
        p.render(doc);
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Setup"), "got: {out:?}");
        assert!(out.contains("cfgd init"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn render_doc_with_note_renders_at_verbose() {
        use super::super::doc::Doc;
        let (p, buf) = Printer::for_test_at(Verbosity::Verbose);
        let doc = Doc::new().heading("Info").note("This is supplementary");
        p.render(doc);
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Info"), "got: {out:?}");
        assert!(out.contains("supplementary"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn render_doc_with_status_duration_and_target() {
        use super::super::doc::Doc;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        let doc = Doc::new()
            .heading("Apply")
            .status_with(Role::Ok, "brew install curl", |f| {
                f.detail("already installed")
                    .duration(std::time::Duration::from_millis(1500))
                    .target("/usr/local/bin/curl")
            });
        p.render(doc);
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("brew install curl"), "got: {out:?}");
        assert!(out.contains("already installed"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn render_doc_section_with_empty_state() {
        use super::super::doc::Doc;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        let doc = Doc::new()
            .heading("Modules")
            .section("Installed", |s| s.empty_state("no modules found"));
        p.render(doc);
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Modules"), "got: {out:?}");
        assert!(out.contains("Installed"), "got: {out:?}");
        assert!(out.contains("no modules found"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn render_doc_with_kv_block() {
        use super::super::doc::Doc;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        let doc = Doc::new()
            .heading("Config")
            .kv_block([("Profile", "dev"), ("Source", "local")]);
        p.render(doc);
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("Config"), "got: {out:?}");
        assert!(out.contains("Profile"), "got: {out:?}");
        assert!(out.contains("dev"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn status_builder_detail_opt_none() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.status(Role::Ok, "package check").detail_opt(None);
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("package check"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn status_builder_detail_opt_some() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.status(Role::Ok, "installed").detail_opt(Some("v1.2.3"));
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("installed"), "got: {out:?}");
        assert!(out.contains("v1.2.3"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn status_builder_with_target_path() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.status(Role::Ok, "file deployed")
            .target(std::path::Path::new("/home/user/.zshrc"));
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("file deployed"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn status_builder_with_duration() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.status(Role::Ok, "brew install curl")
            .duration(std::time::Duration::from_secs(3));
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("brew install curl"), "got: {out:?}");
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

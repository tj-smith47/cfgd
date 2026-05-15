//! User-facing handle. Holds the Renderer (single layout authority), the
//! active OutputFormat, and the writers for stderr (status output) +
//! stdout (structured/data output). Section/spinner/process/prompt/buffered
//! emission methods land in later R1 tasks.
//!
//! R1 skeleton: several `Printer` fields are constructed now but not yet
//! consumed — `sink_stdout` lands with `data_line`/structured emit (T22+);
//! `multi_progress` with `spinner`/`progress_bar` (T19); `syntax_set` +
//! `theme_set` with `syntax_highlight` (T23); `test_doc_capture` +
//! `prompt_queue` with the test-helpers feature + prompts (T20, T26). The
//! `dead_code` allow drops as those tasks land.
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
        self.renderer
            .render_heading(self.sink_stderr.as_ref(), &text.into());
    }

    pub fn kv(&self, key: impl Into<String>, value: impl Into<String>) {
        self.renderer.render_kv(&key.into(), &value.into());
    }

    pub fn kv_block<I, K, V>(&self, pairs: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let pairs: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self.renderer
            .render_kv_block(self.sink_stderr.as_ref(), 0, &pairs);
    }

    pub fn hint(&self, text: impl Into<String>) {
        self.renderer
            .render_hint(self.sink_stderr.as_ref(), 0, &text.into());
    }

    pub fn note(&self, text: impl Into<String>) {
        self.renderer
            .render_note(self.sink_stderr.as_ref(), 0, &text.into());
    }

    pub fn table(&self, table: Table) {
        self.renderer
            .render_table(self.sink_stderr.as_ref(), 0, &table);
    }

    /// Status with no extra fields. For detail/duration/target, use the builder
    /// returned by the binding helper `status` (see status_builder.rs).
    pub fn status_simple(&self, role: Role, subject: impl Into<String>) {
        let subject = subject.into();
        self.renderer.render_status(
            self.sink_stderr.as_ref(),
            0,
            &StatusFields {
                role,
                subject: &subject,
                detail: None,
                duration: None,
                target: None,
            },
        );
    }

    /// Final flush — call at the end of a streaming command to ensure any
    /// buffered kvs land. (Drop on Printer would also do this but tests need
    /// explicit control.)
    pub fn flush(&self) {
        self.renderer.flush_kv_buffer(self.sink_stderr.as_ref());
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

    #[test]
    fn structured_format_auto_quiets() {
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
        assert_eq!(p.verbosity(), Verbosity::Quiet);
    }

    #[test]
    fn table_format_keeps_verbosity() {
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Table);
        assert_eq!(p.verbosity(), Verbosity::Normal);
    }

    #[test]
    fn is_structured_classifies() {
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
        assert!(p.is_structured());
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Table);
        assert!(!p.is_structured());
    }
}

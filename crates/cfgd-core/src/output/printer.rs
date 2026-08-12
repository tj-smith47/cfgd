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
    /// Whether this printer has a live region — a terminal it can repaint in
    /// place. Decided ONCE, at construction, from the stderr it will actually
    /// write to, rather than re-read from the process at each bar: a printer
    /// whose sink is a capture buffer has no live region no matter what the
    /// process's own stderr is, and the test that must exercise the repainting
    /// path needs one no matter what the suite was invoked from.
    pub(crate) live_region: bool,
    /// Whether a `prompt_*` on this printer can reach a human. Decided ONCE, at
    /// construction, from the process stdin the prompt would actually read —
    /// the same shape as `live_region`, and for the same reason: a printer
    /// whose sink is a capture buffer is driving no one's keyboard, no matter
    /// what the process's own stdin is. Read at prompt time instead, a test
    /// with no seeded answer BLOCKS on a real `inquire` prompt the moment the
    /// suite is started under a pty, and a hang is a worse failure than a
    /// mismatch because nothing ever reports it.
    pub(crate) interactive_stdin: bool,
    /// When set (via `--list-envelope` / `CFGD_LIST_ENVELOPE`), a top-level JSON
    /// array emitted under `-o json`/`-o yaml` is wrapped in a KRM List envelope
    /// (`{apiVersion, kind: List, items}`). Off by default — bare arrays stay
    /// byte-identical. Never affects projecting formats (name/jsonpath/template).
    pub(crate) list_envelope: bool,
}

/// Whether constructing a `Printer` for `output_format` must turn the terminal's
/// colour flags off.
///
/// Honors `NO_COLOR` / `TERM=dumb`, and additionally disables colour under
/// structured output (Json / Yaml / Template / Jsonpath / Name) so a role-styled
/// emission cannot leak ANSI escapes into payload string fields — the contract is
/// enforced at construction, not by every caller remembering to wrap with
/// `with_data`.
///
/// Split out of [`Printer::with_format`] so the decision is testable without
/// reading `console`'s colour flags. Those flags are process-global and every
/// structured-output `Printer` construction in the test binary writes them, so an
/// assertion made against them races the whole non-serial majority of the suite —
/// a race `#[serial]` cannot fence, because the mutators are ordinary production
/// constructions rather than serial tests.
fn colors_must_be_disabled(output_format: &OutputFormat) -> bool {
    std::env::var_os("NO_COLOR").is_some()
        || std::env::var_os("TERM").is_some_and(|t| t == "dumb")
        || output_format.is_structured()
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
        // A test holding a `ColorsEnabledGuard` owns both flags for its
        // duration; clobbering them here would race it from any concurrently
        // constructing test. Compiled out of release builds.
        #[cfg(test)]
        let pinned = crate::output::test_support::colors_are_pinned();
        #[cfg(not(test))]
        let pinned = false;

        if !pinned && colors_must_be_disabled(&output_format) {
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
        // The MultiProgress is built first and a clone handed to the renderer,
        // so the two are wired at construction. This is the ONE constructor
        // whose stderr sink is that MultiProgress's own draw target, which is
        // what makes routing lines through it correct.
        let multi_progress = indicatif::MultiProgress::new();
        Self {
            renderer: Arc::new(Renderer::with_bars(
                theme,
                verbosity,
                multi_progress.clone(),
            )),
            output_format,
            sink_stderr: Arc::new(Term::stderr()),
            sink_stdout: Arc::new(Term::stdout()),
            multi_progress,
            syntax_set: syntect::parsing::SyntaxSet::load_defaults_newlines(),
            theme_set: syntect::highlighting::ThemeSet::load_defaults(),
            test_doc_capture: None,
            prompt_queue: None,
            output_error: AtomicBool::new(false),
            live_region: super::spinner::stderr_is_terminal(),
            interactive_stdin: super::prompts::stdin_is_terminal(),
            list_envelope: false,
        }
    }

    /// A copy of this printer rendering with `theme_name`, preserving verbosity,
    /// output format, and the List-envelope setting.
    ///
    /// The process printer is built from the config that existed at startup, so
    /// on a fresh machine `cfgd init --theme dracula` would write `spec.theme`
    /// and then render its own run in the default theme — the one command whose
    /// output cannot show the theme it just chose. Re-theming after the config
    /// is written closes that gap.
    ///
    /// Carries no test capture or queued prompts: those belong to the printer a
    /// test constructed, and a re-themed copy is only taken on a real run.
    pub fn rethemed(&self, theme_name: &str) -> Self {
        Self::with_format(
            self.verbosity(),
            Some(theme_name),
            self.output_format.clone(),
        )
        .with_list_envelope(self.list_envelope)
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
        let depth = self.renderer.enforce_structural_top_level(0);
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
        let _depth = self.renderer.enforce_structural_top_level(0);
        self.renderer.render_kv(&key.into(), &value.into());
    }

    pub fn kv_block<I, K, V>(&self, pairs: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let depth = self.renderer.enforce_structural_top_level(0);
        let pairs: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self.renderer
            .render_kv_block(self.sink_stderr.as_ref(), depth, &pairs);
    }

    pub fn hint(&self, text: impl Into<String>) {
        let depth = self.renderer.inherit_depth();
        self.renderer
            .render_hint(self.sink_stderr.as_ref(), depth, &text.into());
    }

    pub fn note(&self, text: impl Into<String>) {
        let depth = self.renderer.inherit_depth();
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
        let depth = self.renderer.enforce_structural_top_level(0);
        self.renderer
            .render_advisory(self.sink_stderr.as_ref(), depth, &msg.into());
    }

    /// Emit a persistent advisory on stderr: a diagnostic about *this* run that
    /// the user must see even when they asked for data only, because acting on
    /// the output without it would be acting on a wrong picture (a `--skip` that
    /// silently stranded package installs). Same routing as [`Printer::deprecation`]
    /// — always visible, stderr only, so the `-o` data channel stays pure — and
    /// deliberately a separate method: a deprecation is about the command's
    /// SPELLING and stays true until the surface is removed, while an alert is
    /// about the command's EFFECT this time. Routing both through one name makes
    /// them indistinguishable to a reader and to a grep.
    pub fn alert(&self, msg: impl Into<String>) {
        let depth = self.renderer.enforce_structural_top_level(0);
        self.renderer
            .render_advisory(self.sink_stderr.as_ref(), depth, &msg.into());
    }

    pub fn table(&self, table: Table) {
        let depth = self.renderer.enforce_structural_top_level(0);
        self.renderer
            .render_table(self.sink_stderr.as_ref(), depth, &table);
    }

    /// Enable depth inheritance for status / hint / note / spinner / run for
    /// as long as the guard lives, so library code reached from inside an open
    /// section renders at that section's depth instead of tripping the
    /// top-level structural assert. Structural emits (`heading`, `kv_block`,
    /// `table`, `emit`) keep the assert in every mode — a heading inside a
    /// group is a bug whatever the caller is doing.
    #[must_use = "inheritance ends when the guard drops; bind it"]
    pub fn depth_inheritance(&self) -> super::renderer::DepthInheritGuard<'_> {
        super::renderer::DepthInheritGuard::acquire(&self.renderer)
    }

    /// `theme.muted` applied to `text` — the one way a caller composes a
    /// subordinate fragment into a value the renderer receives as a single
    /// string (a kv row whose tail qualifies its head, and which therefore has
    /// no field of its own to carry a style). A colour-disabled stream answers
    /// the text unchanged, because `ThemedStyle` decides that and not the
    /// caller. Never reach for `console` to do this at a call site.
    pub fn muted(&self, text: &str) -> String {
        self.renderer.theme.muted.apply_to(text).to_string()
    }

    /// Status with no extra fields. For detail/duration/target, use the builder
    /// returned by the binding helper `status` (see status_builder.rs).
    pub fn status_simple(&self, role: Role, subject: impl Into<String>) {
        let depth = self.renderer.inherit_depth();
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
                subject_style: None,
                detail_style: None,
            },
        );
    }

    /// [`Self::status`] with the subject painted `theme.primary` — the same
    /// seam `SectionGuard::action_status` applies, for an action line emitted
    /// without a section guard in hand (a script settling its own status).
    pub fn action_status(
        &self,
        role: Role,
        subject: impl Into<String>,
    ) -> super::status_builder::StatusBuilder<'_> {
        let style = self.renderer.theme.primary.clone();
        self.status(role, subject).with_subject_style(style)
    }

    /// Status builder at the ambient depth (0 unless a `DepthInheritGuard`
    /// is open). Commits on Drop.
    pub fn status(
        &self,
        role: Role,
        subject: impl Into<String>,
    ) -> super::status_builder::StatusBuilder<'_> {
        let depth = self.renderer.inherit_depth();
        super::status_builder::StatusBuilder::new(
            self.renderer.clone(),
            self.sink_stderr.clone(),
            depth,
            role,
            subject,
        )
    }

    // ----- Spinners / progress -----

    /// Spinner at the ambient depth — 0 unless a `DepthInheritGuard` is open,
    /// in which case it renders inside the innermost section. Required for the
    /// lib-side call sites in cfgd-core that take `&Printer` and have no
    /// section context of their own (oci/, upgrade/, sources/, modules/git.rs,
    /// reconciler/scripts.rs).
    #[must_use]
    pub fn spinner(&self, message: impl Into<String>) -> super::spinner::Spinner<'_> {
        let message = message.into();
        let depth = self.renderer.inherit_depth();
        let (bar, live) = super::spinner::make_spinner_bar(
            &self.multi_progress,
            &self.renderer,
            self.live_bars(),
            depth,
            &message,
        );
        super::spinner::Spinner {
            renderer: self.renderer.clone(),
            sink: self.sink_stderr.clone(),
            depth,
            bar,
            message,
            finished: false,
            _live: live,
            _phantom: std::marker::PhantomData,
        }
    }

    #[must_use]
    pub fn progress_bar(
        &self,
        total: u64,
        message: impl Into<String>,
    ) -> super::spinner::ProgressBar<'_> {
        let (bar, live) = super::spinner::make_progress_bar(
            &self.multi_progress,
            &self.renderer,
            total,
            self.live_bars(),
            &message.into(),
        );
        super::spinner::ProgressBar {
            bar,
            _live: live,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Run an external command at the ambient depth, displaying its output
    /// through an `OutputWindow` and capturing the full stdout/stderr in the
    /// returned `CommandOutput`. The window indents under the innermost open
    /// section while a `DepthInheritGuard` is held, and sits at column 0
    /// otherwise.
    pub fn run(
        &self,
        cmd: &mut std::process::Command,
        label: impl Into<String>,
    ) -> std::io::Result<super::process::CommandOutput> {
        let depth = self.renderer.inherit_depth();
        super::process::run_command(
            self,
            depth,
            cmd,
            &label.into(),
            super::process::StatusOwner::Window,
        )
    }

    /// [`Self::run`] for a command that is part of a larger action whose status
    /// line the CALLER emits: the live window renders exactly as it does for
    /// `run`, but collapses without a line of its own.
    ///
    /// Use it wherever a status line already names the work — a package
    /// install inside the reconciler's action tree. Reaching for `run` there
    /// renders the action twice, once with the command's label and once with
    /// the plan's.
    pub fn run_silent(
        &self,
        cmd: &mut std::process::Command,
        label: impl Into<String>,
    ) -> std::io::Result<super::process::CommandOutput> {
        let depth = self.renderer.inherit_depth();
        super::process::run_command(
            self,
            depth,
            cmd,
            &label.into(),
            super::process::StatusOwner::Caller,
        )
    }

    /// Final flush — call at the end of a streaming command to ensure any
    /// buffered kvs land. (Drop on Printer would also do this but tests need
    /// explicit control.)
    pub fn flush(&self) {
        self.renderer.flush_kv_buffer(self.sink_stderr.as_ref());
    }

    /// Force human render of a Doc to stderr, regardless of `output_format`.
    /// Used by tests; production code should call `emit`, which routes by
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

    /// Open a section headed by a styled owner token (`module:nvim`).
    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section_owner(
        &self,
        label: &super::OwnerLabel,
    ) -> super::section_guard::SectionGuard<'_> {
        self.renderer.render_section_open_styled(
            &label.plain(),
            Some(label.styled(&self.renderer.theme)),
            /*keep_when_empty=*/ true,
        );
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

        for fmt in [
            OutputFormat::Json,
            OutputFormat::Yaml,
            OutputFormat::Name,
            OutputFormat::Jsonpath("{.foo}".into()),
            OutputFormat::Template("{{ . }}".into()),
        ] {
            assert!(
                colors_must_be_disabled(&fmt),
                "colors should be disabled for {fmt:?}"
            );
        }
    }

    #[test]
    #[serial]
    fn no_color_and_dumb_terminal_disable_colors() {
        let _term = EnvVarGuard::set("TERM", "xterm-256color");
        {
            let _no_color = EnvVarGuard::set("NO_COLOR", "1");
            assert!(colors_must_be_disabled(&OutputFormat::Table));
        }
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _dumb = EnvVarGuard::set("TERM", "dumb");
        assert!(colors_must_be_disabled(&OutputFormat::Table));
    }

    #[test]
    #[serial]
    fn table_format_does_not_disable_colors_implicitly() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _term = EnvVarGuard::set("TERM", "xterm-256color");

        assert!(
            !colors_must_be_disabled(&OutputFormat::Table),
            "Table format must not implicitly disable colors"
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
    fn alert_shows_under_structured_quiet() {
        // An alert carries the reason the payload below it is incomplete, so
        // it has to survive exactly what a deprecation survives — otherwise a
        // `-o json` consumer acts on a plan whose caveat was dropped.
        let (p, buf) = Printer::for_test_with_format(OutputFormat::Json);
        assert_eq!(p.verbosity(), Verbosity::Quiet);

        p.status_simple(Role::Warn, "ordinary warning");
        p.alert("2 package action(s) will not apply");
        p.flush();

        let out = strip_ansi(&buf.lock().unwrap_or_else(|e| e.into_inner()));
        assert!(
            !out.contains("ordinary warning"),
            "Role::Warn must stay suppressed under structured/Quiet; got: {out:?}"
        );
        assert!(
            out.contains("2 package action(s) will not apply"),
            "alert must be force-shown under structured/Quiet; got: {out:?}"
        );
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn alert_never_reaches_the_data_channel() {
        let (p, cap) = Printer::for_test_doc();
        p.alert("stranded installs");
        p.emit(super::super::doc::Doc::new().with_data(serde_json::json!({"ok": true})));
        p.flush();

        assert!(
            cap.human().contains("stranded installs"),
            "the alert belongs on the human/stderr channel: {}",
            cap.human()
        );
        let payload = cap.json().expect("emit must produce a doc payload");
        assert!(
            !payload.to_string().contains("stranded installs"),
            "the alert must not contaminate the -o data channel: {payload}"
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
    /// trips `debug_assert!` in `Renderer::enforce_structural_top_level`. We catch
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

    /// With a `DepthInheritGuard` open, a library emit that has no section
    /// context of its own renders inside the section its caller opened,
    /// keeping the `&Printer` signature the provider traits already pass.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn status_inherits_section_depth_under_the_guard() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let _phase = p.section("Phase: Files");
            let _owner = _phase.section("module:nvim");
            let _inherit = p.depth_inheritance();
            p.status_simple(Role::Ok, "wrote init.lua");
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(
            out.contains("\n    ✓ wrote init.lua\n"),
            "expected depth 2; got: {out:?}"
        );
    }

    /// The guard is scoped: once it drops, the codebase-wide structural guard
    /// is armed again for the hundreds of call sites that never wanted
    /// inheritance.
    #[cfg(feature = "test-helpers")]
    #[test]
    #[cfg(debug_assertions)]
    fn inheritance_ends_with_the_guard() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (p, _buf) = Printer::for_test_at(Verbosity::Normal);
            let _s = p.section("Outer");
            {
                let _inherit = p.depth_inheritance();
                p.status_simple(Role::Ok, "inside");
            }
            p.status_simple(Role::Ok, "after"); // debug_assert! fires
        }));
        assert!(result.is_err(), "expected debug_assert! panic");
    }

    /// Inheritance covers NON-structural emits only. A heading inside a group
    /// is a bug in every mode, so the structural assert stays armed even with
    /// the guard open.
    #[cfg(feature = "test-helpers")]
    #[test]
    #[cfg(debug_assertions)]
    fn structural_emits_still_assert_under_the_guard() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (p, _buf) = Printer::for_test_at(Verbosity::Normal);
            let _s = p.section("Outer");
            let _inherit = p.depth_inheritance();
            p.heading("MidSection"); // debug_assert! fires
        }));
        assert!(result.is_err(), "expected debug_assert! panic");
    }

    /// A run at ambient depth 0 — every caller before the run tree existed —
    /// renders at column 0 exactly as it did before the split.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn top_level_run_stays_at_column_zero() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        let out = p
            .run(
                std::process::Command::new("echo").arg("hello-from-run"),
                "echo step",
            )
            .expect("echo must succeed");
        assert!(out.status.success());
        p.flush();
        let rendered = strip_ansi(&buf.lock().unwrap());
        let line = rendered
            .lines()
            .find(|l| l.contains("echo step"))
            .expect("label missing");
        assert!(
            !line.starts_with(' '),
            "top-level run gained an indent: {line:?}"
        );
    }

    /// A top-level owner group: the token heads the section at column 0 and
    /// its children indent under it.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn printer_section_owner_heads_a_top_level_group() {
        use crate::output::OwnerLabel;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let owner = p.section_owner(&OwnerLabel::new("profile", "work"));
            owner.bullet("installed ripgrep");
        }
        p.flush();
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.starts_with("profile:work\n"), "got: {out:?}");
        assert!(out.contains("\n  - installed ripgrep\n"), "got: {out:?}");
    }
}

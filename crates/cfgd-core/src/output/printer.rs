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

use super::renderer::{Renderer, StatusFields, Table, Writer, finalize_subject};
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
#[derive(Clone)]
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
    /// Whether this printer's output may carry colour. Decided ONCE, at
    /// construction, and folded into the renderer's theme — the third ambient
    /// terminal input, and the one that used to be re-read from
    /// `console::colors_enabled()` inside every styled render. That global is
    /// mutable by any thread, so a capture buffer could come back styled
    /// because an unrelated test flipped it, which turns a negative assertion
    /// (`!contains("✓ Foo")`) into one that passes vacuously. Held here as well
    /// as in the theme so a re-themed copy keeps the decision this printer made.
    pub(crate) colors: bool,
    /// When set (via `--list-envelope` / `CFGD_LIST_ENVELOPE`), a top-level JSON
    /// array emitted under `-o json`/`-o yaml` is wrapped in a KRM List envelope
    /// (`{apiVersion, kind: List, items}`). Off by default — bare arrays stay
    /// byte-identical. Never affects projecting formats (name/jsonpath/template).
    pub(crate) list_envelope: bool,
}

/// How a `Printer` under construction decides whether it may emit colour.
///
/// The decision is an input rather than a global the caller mutates beforehand:
/// a printer's colour is settled once, at construction, and folded into its
/// theme, so nothing a later thread does can change what this printer renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChoice {
    /// Colour when the terminal and the output format both allow it —
    /// `console`'s own tty/`CLICOLOR` detection, read once, minus the cases
    /// `colors_must_be_disabled` rules out.
    Auto,
    /// Colour whatever the terminal says, short of the one case that would
    /// corrupt data. What `--color always` selects, for a run piped into a
    /// pager that renders escapes (`less -R`) or into a docs capture.
    Always,
    /// Never colour. What `--color never` / `--no-color` selects.
    Never,
}

impl ColorChoice {
    /// Resolve to the concrete decision this printer will hold for its lifetime.
    fn resolve(self, output_format: &OutputFormat) -> bool {
        match self {
            // The colour question is asked of STDERR, because stderr is where
            // every human emission goes: stdout carries structured data only,
            // and colour is already forced off there. Asking `colors_enabled()`
            // (the stdout answer) styles `cfgd apply 2> log` into the log file
            // and strips `cfgd apply | tee log` on a live terminal — both
            // backwards.
            Self::Auto => {
                console::colors_enabled_stderr() && !colors_must_be_disabled(output_format)
            }
            // An explicit request outranks `NO_COLOR` / `TERM=dumb` (the
            // convention is a default, not a veto) but never outranks the
            // structured-output gate: an escape inside a JSON string field is
            // corrupt data, not a styling preference.
            Self::Always => !output_format.is_structured(),
            Self::Never => false,
        }
    }
}

/// Whether a `Printer` for `output_format` must refuse colour outright.
///
/// Honors `NO_COLOR` / `TERM=dumb`, and additionally disables colour under
/// structured output (Json / Yaml / Template / Jsonpath / Name) so a role-styled
/// emission cannot leak ANSI escapes into payload string fields — the contract is
/// enforced at construction, not by every caller remembering to wrap with
/// `with_data`.
///
/// Split out of [`ColorChoice::resolve`] so the decision is testable without
/// reading `console`'s colour flags at all.
pub(crate) fn colors_must_be_disabled(output_format: &OutputFormat) -> bool {
    std::env::var_os("NO_COLOR").is_some()
        || std::env::var_os("TERM").is_some_and(|t| t == "dumb")
        || output_format.is_structured()
}

/// Stamp OSC 8 hyperlinks onto `theme` iff colour resolved on and the terminal
/// is a known emitter. Read by the two PRODUCTION constructors only: a capture
/// never detects, so no golden can pick up an escape from the developer's own
/// terminal. `build` re-applies the same `colors`, and `with_colors` withdraws
/// the stamp with the colour, so the two cannot end up disagreeing.
fn stamp_hyperlinks(theme: Theme, colors: bool) -> Theme {
    // Colour off already settles the answer, so the terminal is never asked:
    // the probe reads several environment variables on every construction, and
    // `with_hyperlinks` would discard what it learned. `-o json`, `NO_COLOR`
    // and a piped stdout are the common case, not the rare one.
    if !colors {
        return theme.with_colors(false);
    }
    theme
        .with_colors(true)
        .with_hyperlinks(super::terminal_supports_hyperlinks())
}

/// The depth an action row renders at in a report: under its phase's section
/// and its owner's group. A sole-lane phase draws a level shallower, which
/// only leaves such a row more room than it was budgeted.
pub const ACTION_ROW_DEPTH: usize = 2;

impl Printer {
    /// Production constructor: stderr/stdout via `console::Term`.
    pub fn new(verbosity: Verbosity) -> Self {
        Self::with_format(verbosity, None, OutputFormat::Table, ColorChoice::Auto)
    }

    /// A non-emitting sink for a process that owns no terminal at all — the
    /// Windows service entry point and the MCP server's JSON-RPC dispatch.
    ///
    /// Quiet AND [`ColorChoice::Never`], because there is no parent printer to
    /// inherit a decision from and `Auto` would answer to whatever the service
    /// host or the MCP client left on stderr. Every other quiet sink in the
    /// workspace derives from a real printer via [`Printer::at_verbosity`];
    /// reach for this one only where no such printer exists.
    pub fn silent() -> Self {
        Self::with_format(
            Verbosity::Quiet,
            None,
            OutputFormat::Table,
            ColorChoice::Never,
        )
    }

    pub fn with_format(
        verbosity: Verbosity,
        theme_name: Option<&str>,
        output_format: OutputFormat,
        colors: ColorChoice,
    ) -> Self {
        let theme = theme_name.map(Theme::from_preset).unwrap_or_default();
        let colors = colors.resolve(&output_format);
        Self::build(
            verbosity,
            stamp_hyperlinks(theme, colors),
            output_format,
            colors,
        )
    }

    /// Production constructor for a printer built from the user's `spec.theme`
    /// block: the preset it names AND the per-slot `overrides` it declares.
    ///
    /// Separate from [`Printer::with_format`] because the override pass has to
    /// run before the colour stamp — `Theme::from_config` fills the optional
    /// `primary` slot with a fresh style when a preset leaves it empty, and a
    /// slot minted after the stamp would carry the default (colour-off)
    /// decision instead of this printer's.
    pub fn with_theme_config(
        verbosity: Verbosity,
        theme: Option<&crate::config::ThemeConfig>,
        output_format: OutputFormat,
        colors: ColorChoice,
    ) -> Self {
        let theme = Theme::from_config(theme);
        let colors = colors.resolve(&output_format);
        Self::build(
            verbosity,
            stamp_hyperlinks(theme, colors),
            output_format,
            colors,
        )
    }

    fn build(
        verbosity: Verbosity,
        theme: Theme,
        output_format: OutputFormat,
        colors: bool,
    ) -> Self {
        // Auto-quiet under structured output.
        let verbosity = if output_format.is_structured() {
            Verbosity::Quiet
        } else {
            verbosity
        };
        let theme = theme.with_colors(colors);
        // The MultiProgress is built first and a clone handed to the renderer,
        // so the two are wired at construction. This is the ONE constructor
        // whose stderr sink is that MultiProgress's own draw target, which is
        // what makes routing lines through it correct.
        let multi_progress = indicatif::MultiProgress::new();
        let sink_stderr: Arc<dyn Writer> = Arc::new(Term::stderr());
        Self {
            renderer: Arc::new(Renderer::with_bars(
                theme,
                verbosity,
                multi_progress.clone(),
                sink_stderr.clone(),
            )),
            output_format,
            sink_stderr,
            sink_stdout: Arc::new(Term::stdout()),
            multi_progress,
            syntax_set: syntect::parsing::SyntaxSet::load_defaults_newlines(),
            theme_set: syntect::highlighting::ThemeSet::load_defaults(),
            test_doc_capture: None,
            prompt_queue: None,
            output_error: AtomicBool::new(false),
            live_region: super::spinner::stderr_is_terminal(),
            interactive_stdin: super::prompts::stdin_is_terminal(),
            colors,
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
    /// Inherits every ambient terminal decision and test channel from `self` —
    /// see `Printer::build_derived`. `cmd_init` re-themes mid-run and keeps
    /// using the derived printer for the apply that follows; a re-probed
    /// `interactive_stdin`/`live_region` there is the pty-hang shape this
    /// closes (a `prompt_queue` reset to `None` would also silently drop a
    /// test's seeded answer).
    pub fn rethemed(&self, theme_name: &str) -> Self {
        self.build_derived(
            self.verbosity(),
            Theme::from_preset(theme_name),
            self.output_format.clone(),
        )
    }

    /// A copy of this printer at `verbosity`, inheriting the theme (preset plus
    /// `spec.theme.overrides`) and every ambient terminal decision and test
    /// channel from `self` — see `Printer::build_derived`.
    ///
    /// The one way to mint the quiet sink a command hands to a library call, and
    /// the daemon's own printer. Deriving `live_region`/`multi_progress` from
    /// `self` rather than re-probing is what keeps the daemon's printer from
    /// standing up a second `MultiProgress` against the same stderr as the
    /// process printer's.
    pub fn at_verbosity(&self, verbosity: Verbosity) -> Self {
        self.build_derived(
            verbosity,
            self.renderer.theme.clone(),
            self.output_format.clone(),
        )
    }

    /// The one way to derive a copy of `self`: only `verbosity`, `theme`, and
    /// `output_format` are recomputed. Every ambient terminal decision this
    /// printer already settled — its stdout/stderr sinks, its `MultiProgress`,
    /// whether it has a live region, whether its stdin is interactive — and
    /// every test-only channel (`test_doc_capture`, `prompt_queue`) carry over
    /// unchanged rather than being re-probed from the real process.
    ///
    /// Re-probing any of those here is the leak F9 closes: a derived quiet
    /// sink (`cli/compliance.rs`, `daemon/sync.rs`, …) would answer
    /// `render_status`'s `Role::Fail` line to the REAL stderr instead of a
    /// test's capture buffer even at `Verbosity::Quiet`, a re-themed printer
    /// would regain a live region and a real stdin-tty mid-run and block on an
    /// unanswered confirmation prompt under a pty, and a second `at_verbosity`
    /// call (the daemon's own printer) would stand up a second
    /// `MultiProgress` doing independent cursor arithmetic on the one real
    /// stderr the process printer already owns.
    ///
    /// The live-bar bookkeeping travels with the `MultiProgress` rather than
    /// being minted fresh — see [`super::renderer::LiveBarState`] for the
    /// stranded-paint bug a per-renderer count causes.
    fn build_derived(
        &self,
        verbosity: Verbosity,
        theme: Theme,
        output_format: OutputFormat,
    ) -> Self {
        // Auto-quiet under structured output, same as `build`.
        let verbosity = if output_format.is_structured() {
            Verbosity::Quiet
        } else {
            verbosity
        };
        // The copy inherits the colour this printer resolved rather than
        // re-resolving: re-reading the terminal would let the two disagree
        // mid-run, which is the whole class of bug the field exists to close.
        let theme = theme.with_colors(self.colors);
        // The derived renderer writes the SAME sink as `self.renderer` (both
        // clone `sink_stderr`/`sink_stdout` below rather than opening new
        // ones), so it has to continue `self`'s blank-line bookkeeping rather
        // than starting fresh — a bare `Renderer::with_bars` here defaults
        // `leading: true`, which reads as "nothing has been written yet" even
        // when `self` just closed a section that owes the next heading a
        // blank line. See `RenderState::continued_from`.
        let seed = self.renderer.continuation_seed();
        Self {
            renderer: Arc::new(Renderer::with_bars_continued(
                theme,
                verbosity,
                self.multi_progress.clone(),
                seed,
                self.renderer.live.clone(),
            )),
            output_format,
            sink_stderr: self.sink_stderr.clone(),
            sink_stdout: self.sink_stdout.clone(),
            multi_progress: self.multi_progress.clone(),
            syntax_set: syntect::parsing::SyntaxSet::load_defaults_newlines(),
            theme_set: syntect::highlighting::ThemeSet::load_defaults(),
            test_doc_capture: self.test_doc_capture.clone(),
            prompt_queue: self.prompt_queue.clone(),
            output_error: AtomicBool::new(false),
            live_region: self.live_region,
            interactive_stdin: self.interactive_stdin,
            colors: self.colors,
            list_envelope: self.list_envelope,
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

    /// Whether this printer's output may carry colour.
    pub fn colors(&self) -> bool {
        self.colors
    }

    /// The ONE arrow glyph for a rendered `old -> new` / `source -> target`
    /// relationship — see [`Theme::arrow`], which this delegates to. A caller
    /// composing such a string interpolates this instead of hardcoding ASCII
    /// `->`, so a preset override applies uniformly rather than leaving some
    /// relationships themed and others not.
    pub fn arrow(&self) -> &str {
        self.renderer.theme.arrow()
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
            let styled = heading_fallback_line(&self.renderer.theme, &text.into());
            self.renderer
                .write_line(self.sink_stderr.as_ref(), depth, &styled);
        }
    }
}

/// The line [`Printer::heading`] writes on its re-route branch — a heading
/// asked for at column 0 while a section is open.
///
/// A free function rather than three inline statements because the branch
/// itself is behind a `debug_assert!(false)` and so cannot be entered from a
/// test build at all: without this seam the fold on it would be provable only
/// by reading it, which is how a slot ends up unfolded in the first place.
pub(super) fn heading_fallback_line(theme: &super::Theme, text: &str) -> String {
    theme.header.apply_to(super::cursor_safe(text)).to_string()
}

impl Printer {
    /// Top-level `Label: value` heading (`Status: dev-tools`), styled through
    /// [`super::TitleLabel`]'s three slots instead of `heading`'s single
    /// `theme.header` coat.
    pub fn heading_title(&self, title: &super::TitleLabel) {
        let depth = self.renderer.enforce_structural_top_level(0);
        let styled = title.styled(&self.renderer.theme);
        // See `heading`'s comment: render_heading_styled is hardcoded to
        // depth 0, so the runtime re-route path writes the same styled line
        // at the section's actual depth instead.
        if depth == 0 {
            self.renderer
                .render_heading_styled(self.sink_stderr.as_ref(), &styled);
        } else {
            self.renderer
                .write_line(self.sink_stderr.as_ref(), depth, &styled);
        }
    }

    /// Top-level `<Verb> <owner>` heading (`Add source:acme`), styled through
    /// [`super::OwnerLabel`]'s three slots for the token instead of folding
    /// the whole line into `heading`'s single `theme.header` coat.
    ///
    /// Named `_prefixed` because the verb is the point: an owner token names
    /// WHOSE the rows below it are, which is a section's job
    /// ([`Printer::section_owner`], [`super::Doc::section_owner`]), so a bare
    /// `kind:name` never occupies a top-level heading slot. There is
    /// deliberately no unprefixed counterpart on either the streaming or the
    /// buffered side.
    pub fn heading_owner_prefixed(&self, prefix: impl Into<String>, owner: &super::OwnerLabel) {
        let depth = self.renderer.enforce_structural_top_level(0);
        let styled = owner.styled_with_prefix(&self.renderer.theme, &prefix.into());
        // See `heading`'s comment: render_heading_styled is hardcoded to
        // depth 0, so the runtime re-route path writes the same styled line
        // at the section's actual depth instead.
        if depth == 0 {
            self.renderer
                .render_heading_styled(self.sink_stderr.as_ref(), &styled);
        } else {
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
        let pairs: Vec<crate::output::KvPair> = pairs
            .into_iter()
            .map(|(k, v)| crate::output::KvPair::new(k, v))
            .collect();
        self.renderer
            .render_kv_block(self.sink_stderr.as_ref(), depth, &pairs);
    }

    /// `kv_block` over hand-built [`KvPair`]s, so a top-level block can reach
    /// the renderer-owned `annotated` / `nested` / `role_valued` slots.
    ///
    /// Without it a command with no section open had to hand-build
    /// `format!("{value} ({note})")`, which paints the note the same weight as
    /// the value and misaligns nothing the renderer can see.
    ///
    /// [`KvPair`]: crate::output::KvPair
    pub fn kv_rows<I>(&self, rows: I)
    where
        I: IntoIterator<Item = crate::output::KvPair>,
    {
        let depth = self.renderer.enforce_structural_top_level(0);
        let rows: Vec<crate::output::KvPair> = rows.into_iter().collect();
        self.renderer
            .render_kv_block(self.sink_stderr.as_ref(), depth, &rows);
    }

    /// A "command — description" list — `kv_block`'s counterpart for a left
    /// column that is a shell command rather than a data-carrying key. See
    /// `Renderer::render_command_list` for why it needs its own layout.
    pub fn command_list<I>(&self, pairs: I)
    where
        I: IntoIterator,
        I::Item: Into<crate::output::CommandPair>,
    {
        let depth = self.renderer.enforce_structural_top_level(0);
        let pairs: Vec<crate::output::CommandPair> = pairs.into_iter().map(Into::into).collect();
        self.renderer
            .render_command_list(self.sink_stderr.as_ref(), depth, &pairs);
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
    ///
    /// Keeps the top-level structural assert that [`Printer::alert`] gives up:
    /// a deprecation is drained at the command boundary that owns the terminal
    /// (`CfgdConfig.deprecations`, drained once per command), so reaching one
    /// from inside an open section means the drain moved, not that the notice
    /// was discovered there.
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
    ///
    /// Correct at ANY depth (see `Renderer::advisory_depth`): unlike a
    /// deprecation, which is drained at the command boundary that owns the
    /// terminal, an alert is emitted where the effect is discovered — mid
    /// composition, inside whatever section the caller opened — and a message
    /// the user must see may not be the thing that panics a debug build for
    /// being nested.
    pub fn alert(&self, msg: impl Into<String>) {
        let depth = self.renderer.advisory_depth();
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

    /// The columns an action row's SUBJECT may occupy on this printer's
    /// terminal before its operand list is cut, or `None` for a sink that
    /// never wraps (a capture, a redirected stream), where the list is cut at
    /// the floor alone.
    ///
    /// Half of what the complete-line budget at [`ACTION_ROW_DEPTH`] leaves
    /// after the glyph and the wait framing
    /// ([`WAIT_FRAMING_WIDTH`](super::renderer::status::WAIT_FRAMING_WIDTH)):
    /// the widest row a report can print is a subject at the budget, ` —
    /// queued behind `, and ANOTHER subject at the budget, so a budget that
    /// halved the whole line let a filled subject and the report's claimed
    /// column contradict each other — every row reached the budget T5
    /// grants, and the claim's retreat then refused every report a column.
    /// Sized this way the two agree by construction: the widest subject the
    /// budget permits is always a column the widest wait reason still fits
    /// beside. Read once per report and threaded to every reader of
    /// `action_display_subject_within`, so the preview, the alignment column,
    /// the apply ledger, the live tree and the wait lines name one action one
    /// way.
    ///
    /// The FLOOR, not always the answer: a report that measured its own
    /// trailing allowance claims a wider budget through
    /// [`Printer::report_column_beside`], and this answers that claim while
    /// it is held — so a subject cut inside the claim (a preview bullet, a
    /// settled row, a wait line) is cut once, to the report's budget.
    pub fn subject_budget(&self) -> Option<usize> {
        self.renderer
            .report_subject_budget()
            .or_else(|| self.subject_budget_floor())
    }

    /// The budget [`Printer::subject_budget`] answers when no report has
    /// claimed a wider one: the constant-reserved half of the line.
    pub fn subject_budget_floor(&self) -> Option<usize> {
        self.action_row_line_budget().map(|line| {
            line.saturating_sub(
                super::renderer::status::GLYPH_PREFIX_WIDTH
                    + super::renderer::status::WAIT_FRAMING_WIDTH,
            ) / 2
        })
    }

    /// The columns an action row has in total on this terminal — the whole
    /// line at [`ACTION_ROW_DEPTH`] — or `None` for a sink that never wraps.
    pub fn action_row_line_budget(&self) -> Option<usize> {
        self.sink_stderr
            .wrap_columns()
            .map(|cols| super::renderer::wrap::line_budget(cols, ACTION_ROW_DEPTH))
    }

    /// Declare the alignment column every action row of THIS report pads to,
    /// for as long as the guard lives.
    ///
    /// The trailing column — an elapsed time, a detail, a target — is the one
    /// a reader's eye scans straight down, so it belongs to the report, not to
    /// whichever phase happens to be drawing. A section that declares a live
    /// column takes this in preference to its own.
    ///
    /// Claimed by whoever can see the whole report: an apply run measures its
    /// plan AND the backup labels it will print after it, and the preview
    /// nested inside that run leaves the wider claim alone.
    #[must_use = "the column is released when the guard drops; bind it"]
    pub fn report_column(&self, width: usize) -> super::renderer::ReportColumnGuard<'_> {
        super::renderer::ReportColumnGuard::acquire(&self.renderer, width, None)
    }

    /// [`Printer::report_column`] for a report that knows what its rows will
    /// carry BESIDE the subject: `trailing` is the widest non-subject content
    /// any row of it may print after the glyph — a wait reason, a produced
    /// count — and the column is claimed only if that content still fits
    /// beside it on this terminal, else the report claims no column at all.
    ///
    /// The live twin of the buffered path's `group_trailing_allowance`. A
    /// live group's rows arrive one at a time, so its column used to be
    /// judged against the glyph alone, and on a 44-column terminal a padded
    /// `brew install gum` pushed its `queued behind provision brew via
    /// homebrew` off the line, where the repaint cut it to `via h…`. Judged
    /// once here, the same answer reaches every reader of the column: the
    /// section's own live column defers to the claim, and a live tree asks
    /// [`Printer::live_column_for`] for it.
    #[must_use = "the column is released when the guard drops; bind it"]
    ///
    /// `budget` is the subject budget the report settled for its rows
    /// (`reconciler::report_subject_budget`), held with the column so every
    /// reader of [`Printer::subject_budget`] inside the claim answers it.
    pub fn report_column_beside(
        &self,
        budget: Option<usize>,
        width: usize,
        trailing: usize,
    ) -> super::renderer::ReportColumnGuard<'_> {
        let column = super::renderer::status::group_column(
            self.sink_stderr.wrap_columns(),
            ACTION_ROW_DEPTH,
            width,
            super::renderer::status::GLYPH_PREFIX_WIDTH + trailing,
        );
        super::renderer::ReportColumnGuard::acquire(&self.renderer, column, budget)
    }

    /// The column a live painter pads a row to: the report's claimed column
    /// when one is held, else `width` — the ONE rule, shared with the
    /// section's own live column, so a row's repaint and the line that
    /// replaces it at commit cannot pad to two different columns.
    pub fn live_column_for(&self, width: usize) -> usize {
        self.renderer.report_column_or(width)
    }

    /// Status with no extra fields. For detail/duration/target, use the builder
    /// returned by the binding helper `status` (see status_builder.rs).
    ///
    /// The subject is finalized exactly as the builder path finalizes its own
    /// — no marker, no qualifier, no label, which is what "simple" means — so
    /// the two produce the same bytes for the same string. That is also the
    /// only sanitation this slot gets: a subject can be a gateway-supplied or
    /// tool-supplied value, and a `\x1b[2K` inside one erases the line it is
    /// being written to, while any foreign escape mis-measures every column
    /// downstream of it.
    pub fn status_simple(&self, role: Role, subject: impl Into<String>) {
        let depth = self.renderer.inherit_depth();
        let subject = finalize_subject(&self.renderer.theme, &subject.into(), None, None, None);
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
        let style = super::renderer::action_subject_style(&self.renderer.theme, role);
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
        let message = super::spinner::compose_in_flight_subject(&self.renderer.theme, message);
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
            borrowed: false,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Run `work` under a spinner that narrates its own steps, settling it on
    /// every exit path.
    ///
    /// `running` labels the first animated frame; `work` receives the live
    /// spinner and renames it per step with [`Spinner::set_message`], so the
    /// line always names the manager being enumerated, the source being
    /// fetched, the probe being run — real state, never decoration.
    ///
    /// A success retires the bar SILENTLY. Narration is live-region-only: the
    /// permanent output of a successful run is byte-identical to the same run
    /// before the spinner existed, which is what lets every golden in the
    /// suite stay a golden. A failure settles `Fail` at whatever step was
    /// running, because that is the one fact the propagated error does not
    /// carry — and it carries no detail of its own, since the error itself is
    /// about to be rendered at the CLI boundary.
    ///
    /// That last clause is a PRECONDITION on the caller, not a description:
    /// only wrap work whose failure is not reported by anybody else. Three
    /// shapes break it, and each would print the same fact twice — once here
    /// and once from whoever owns the outcome. The caller swallows the `Err`
    /// and words it; the caller already emitted a permanent line naming this
    /// same wait; or the site settles its own outcome afterwards. Because
    /// `Fail` is the one role that survives `Verbosity::Quiet`, the duplicate
    /// lands on stderr even under `-o json`. Any of the three reaches for
    /// [`Printer::narrate_silent`] instead.
    ///
    /// The settle discipline is the whole point: a caller that opens a spinner
    /// by hand and hits an early `?` between creation and its matching finish
    /// abandons it to `Drop`, which can only report the generic
    /// `(interrupted)` because it cannot know whether the work succeeded. Here
    /// the match is written once and no call site can forget it.
    ///
    /// Correct at ANY depth: the bar is opened through
    /// `Printer::narration_bar`, which carries the depth-inheritance guard.
    ///
    /// [`Spinner::set_message`]: super::spinner::Spinner::set_message
    /// [`Printer::narrate_silent`]: Printer::narrate_silent
    pub fn narrate<T, E>(
        &self,
        running: impl Into<String>,
        work: impl FnOnce(&mut super::spinner::Spinner<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut sp = self.narration_bar(running);
        match work(&mut sp) {
            Ok(value) => {
                sp.finish_silent();
                Ok(value)
            }
            Err(e) => {
                let step = sp.message.clone();
                let _ = sp.finish_fail(step);
                Err(e)
            }
        }
    }

    /// [`Printer::narrate`] for a wait whose OUTCOME LINE belongs to somebody
    /// else: the bar is retired SILENTLY on both arms, so nothing here can
    /// state a result a second time.
    ///
    /// It exists because the alternative — a `Fail` settle stacked under a
    /// line that already says the same thing — survives `Verbosity::Quiet`
    /// and so lands beside a `-o json` payload carrying the identical fact.
    /// The three shapes that need it are listed on `narrate`; production
    /// currently holds two, both of them waits asked from INSIDE a section
    /// their caller opened: a package manager's enumeration, whose five
    /// callers each render their own row, and a device-gateway round-trip,
    /// whose callers each print a permanent line naming the request first.
    ///
    /// The settle discipline is the reason to reach for this rather than a
    /// hand-built bar: an early `?` between a bar's creation and its matching
    /// finish abandons it to `Drop`, which can only report the generic
    /// `(interrupted)`. Here the finish is written once and no call site can
    /// skip it.
    pub fn narrate_silent<T, E>(
        &self,
        running: impl Into<String>,
        work: impl FnOnce(&mut super::spinner::Spinner<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut sp = self.narration_bar(running);
        let out = work(&mut sp);
        sp.finish_silent();
        out
    }

    /// Open a narrated wait's bar at whatever depth the caller is standing at.
    ///
    /// The guard is what makes a narrated wait correct at any depth: without
    /// it the spinner is a depth-0 non-structural emit, which trips the
    /// top-level structural assert the moment any caller has a section open —
    /// and both `narrate_silent` sites are asked from inside one.
    ///
    /// It covers the construction and nothing else, deliberately. The guard
    /// is a counter on the SHARED renderer, so while it is alive the assert
    /// that catches a misplaced top-level emit is disarmed for every emit on
    /// every thread, and the bodies these wrappers cover are whole commands.
    /// Nothing longer is needed: `spinner` reads the ambient depth once and
    /// stores it, and both `set_message` and every `finish_*` render from
    /// that stored field rather than re-reading the renderer.
    fn narration_bar(&self, running: impl Into<String>) -> super::spinner::Spinner<'_> {
        let _inherit = self.depth_inheritance();
        self.spinner(running)
    }

    #[must_use]
    pub fn progress_bar(
        &self,
        total: u64,
        message: impl Into<String>,
    ) -> super::spinner::ProgressBar<'_> {
        let message = super::spinner::compose_in_flight_subject(&self.renderer.theme, message);
        let depth = self.renderer.inherit_depth();
        let (bar, live) = super::spinner::make_progress_bar(
            &self.multi_progress,
            &self.renderer,
            total,
            self.live_bars(),
            depth,
            &message,
        );
        super::spinner::ProgressBar {
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

    /// Open a run phase's section (`Phase: Packages`).
    ///
    /// The ONE way to open one, so every phase heading in the workspace takes
    /// the same two theme slots — and commits to scrollback before anything
    /// the phase does. A phase's work can open a live region (a lane's output
    /// window, a wait line), and the live region paints below the last
    /// committed line: a heading still deferred to its first status would be
    /// written *after* the output it introduces.
    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section_phase(
        &self,
        label: &super::PhaseLabel,
    ) -> super::section_guard::SectionGuard<'_> {
        self.renderer.render_section_open_styled(
            &label.plain(),
            Some(label.styled(&self.renderer.theme)),
            /*keep_when_empty=*/ true,
        );
        self.renderer
            .render_section_commit_header(&*self.sink_stderr);
        super::section_guard::SectionGuard {
            printer: self,
            renderer: self.renderer.clone(),
            sink: self.sink_stderr.clone(),
            depth: 1,
        }
    }

    /// Open a section headed by a `Label: value` title (`Restore: notes`).
    ///
    /// [`Printer::heading_title`]'s sectioned counterpart, and the only way a
    /// titled heading can carry a block of rows: a heading plus a top-level
    /// `kv_block` puts the rows at the heading's own indent, so they read as
    /// the command's output rather than as facts about the run above them.
    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section_title(
        &self,
        label: &super::TitleLabel,
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

    /// [`Printer::section_owner`] for a caller that cannot know whether the
    /// owner has anything to say until after it has said it — the top-level
    /// counterpart of [`super::section_guard::SectionGuard::section_owner_or_collapse`].
    ///
    /// `cfgd source update` opens one group per source before it knows whether
    /// the fetch, the permission review or the knob writes will render a row,
    /// and an owner heading over nothing reads as a source that was consulted
    /// and had nothing wrong with it.
    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section_owner_or_collapse(
        &self,
        label: &super::OwnerLabel,
    ) -> super::section_guard::SectionGuard<'_> {
        self.renderer.render_section_open_styled(
            &label.plain(),
            Some(label.styled(&self.renderer.theme)),
            /*keep_when_empty=*/ false,
        );
        super::section_guard::SectionGuard {
            printer: self,
            renderer: self.renderer.clone(),
            sink: self.sink_stderr.clone(),
            depth: 1,
        }
    }

    /// Open the run's closing `Caveats` section — provider narration
    /// collected during the run, grouped under the owner that produced it and
    /// rendered once at the very end instead of inline under each action.
    ///
    /// Styled through the same `Role::Accent` slot [`super::PhaseLabel`]
    /// paints its name in: Caveats is a phase-class heading meant to draw the
    /// eye, and accent is the slot that draws attention without alarm, so it
    /// reads apart from every ordinary `theme.header` section title while
    /// still reading as part of the run's phase structure. No `.bold()` —
    /// bold never pairs with colour on a colour-bearing slot (every named
    /// colour preset), and `minimal`'s accent already carries the
    /// distinction as italic rather than as an attribute this heading would
    /// have to add on top.
    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section_caveats(&self) -> super::section_guard::SectionGuard<'_> {
        let label = super::AccentHeading::new("Caveats");
        let styled = label.styled(&self.renderer.theme);
        self.renderer.render_section_open_styled(
            label.plain(),
            Some(styled),
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

/// Turns `console`'s process-global colour flags ON and restores them on drop,
/// including on unwind, so a failed assertion cannot leave the suite's terminal
/// decision flipped under every test that runs after it.
///
/// The ONE writer of those flags in the workspace, and test-only: production
/// never touches them, because a printer's colour is decided at construction
/// (see [`ColorChoice`]). A test reaches for this when the flags being ON is
/// the reported condition it must reproduce — `--no-color` on a colour
/// terminal, or a capture taken while an unrelated thread flipped them. Pair it
/// with `serial_test::serial`; the flags are process-global.
#[cfg(test)]
pub(crate) struct ColorGlobalOn {
    stdout: bool,
    stderr: bool,
}

#[cfg(test)]
impl ColorGlobalOn {
    pub(crate) fn set() -> Self {
        let prior = Self {
            stdout: console::colors_enabled(),
            stderr: console::colors_enabled_stderr(),
        };
        console::set_colors_enabled(true);
        console::set_colors_enabled_stderr(true);
        prior
    }
}

#[cfg(test)]
impl Drop for ColorGlobalOn {
    fn drop(&mut self) {
        console::set_colors_enabled(self.stdout);
        console::set_colors_enabled_stderr(self.stderr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_helpers::EnvVarGuard;
    use serial_test::serial;

    /// The top-level counterpart of the section-guard case: the same subject
    /// slot, the same untrusted sources (a gateway JSON field, a tool's
    /// captured stderr), and the same erasure if an escape survives it.
    #[test]
    fn top_level_status_subject_is_stripped_of_foreign_escapes() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.status_simple(Role::Ok, "Enrolled as user '\x1b[2Kroot\x1b[31m'");
        p.flush();
        // raw-capture-ok: the claim IS that no escape survives, and captured_text strips exactly what this test looks for
        let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            !raw.contains("\x1b[2K") && !raw.contains("\x1b[31m"),
            "a foreign escape reached the terminal: {raw:?}"
        );
        assert!(
            crate::output::strip_ansi(&raw).contains("Enrolled as user 'root'"),
            "stripping must keep the visible text: {raw:?}"
        );
    }

    /// A quiet library sink derived from a printer that has a spinner running
    /// must clear and repaint that spinner around its own line, not write over
    /// the top of it.
    ///
    /// The bug: `cfgd sync` hands `load_source` a `Verbosity::Quiet` printer,
    /// whose `Fail` statuses and `alert()`s are printed anyway. Derived from a
    /// renderer with its own zero live-bar count, each of those took the raw
    /// branch and left the "Syncing sources" frame on the terminal for the rest
    /// of the session — the frozen spinner the user recorded on camera.
    ///
    /// Read from the emulated screen, the only live capture that can see a
    /// paint the region never took back.
    #[test]
    fn a_derived_printers_line_does_not_strand_the_parents_live_bar() {
        let (parent, screen) = Printer::for_test_live_terminal(24, 100);
        let sp = parent.spinner("Syncing sources");
        // Joins the steady-tick thread, so this thread is the only writer and
        // the bar has already painted one frame — no sleep, no race.
        sp.bar.disable_steady_tick();
        let quiet = parent.at_verbosity(Verbosity::Quiet);
        quiet.status_simple(crate::output::Role::Fail, "source acme: fetch failed");
        sp.finish_ok("Synced sources");
        parent.flush();

        let held = screen.contents();
        assert!(
            !held.contains("Syncing sources"),
            "the running spinner's paint was stranded on the terminal: {held:?}"
        );
        assert_eq!(
            held.matches("source acme: fetch failed").count(),
            1,
            "the derived line must land exactly once: {held:?}"
        );
        assert!(
            held.contains("Synced sources"),
            "the settled line went missing: {held:?}"
        );
    }

    #[test]
    fn structured_format_auto_quiets() {
        let p = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Json,
            ColorChoice::Auto,
        );
        assert_eq!(p.verbosity(), Verbosity::Quiet);
    }

    #[test]
    fn table_format_keeps_verbosity() {
        let p = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Table,
            ColorChoice::Auto,
        );
        assert_eq!(p.verbosity(), Verbosity::Normal);
    }

    #[test]
    #[serial]
    fn derived_printers_inherit_the_colour_decision() {
        // The terminal is asked to say YES, so a derived printer that re-reads
        // it would come back coloured and the assertion below would fail. That
        // is the regression: `cfgd daemon --no-color` built its own printer
        // with `Printer::new`, which resolves `Auto`, and drew a fully coloured
        // reconcile tree into journald.
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _term = EnvVarGuard::set("TERM", "xterm-256color");
        let _globals = ColorGlobalOn::set();

        let never = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Table,
            ColorChoice::Never,
        );
        assert!(!never.colors());
        assert!(!never.at_verbosity(Verbosity::Quiet).colors());
        assert!(!never.at_verbosity(Verbosity::Verbose).colors());
        assert!(!never.rethemed("dracula").colors());

        // `silent()` answers the same way with no parent to inherit from.
        assert!(!Printer::silent().colors());

        // And the inheritance is faithful in the other direction: an ON
        // decision survives the derivation too, so this is not a test that
        // would pass with the field hardcoded to false.
        let always = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Table,
            ColorChoice::Always,
        );
        assert!(always.colors());
        assert!(always.at_verbosity(Verbosity::Quiet).colors());
    }

    /// F9: `rethemed`/`at_verbosity` used to rebuild every ambient terminal
    /// decision via `build`, re-probing the REAL stderr/stdin/`MultiProgress`
    /// and dropping the parent's test channels — colour was already proven
    /// above; this extends the same shape to the five ambient inputs that
    /// were not: the sinks, `live_region`, `interactive_stdin`,
    /// `test_doc_capture`, and `prompt_queue`. A re-probe there is the
    /// pty-hang shape (`cmd_init`'s re-theme regaining a live region + a real
    /// stdin-tty mid-run and blocking on an unanswered confirmation prompt),
    /// the vacuous-test-pass shape (a derived quiet sink's `Role::Fail` line
    /// landing on the real terminal instead of the parent test's capture
    /// buffer, at `Verbosity::Quiet`), and the double-`MultiProgress` shape
    /// (the daemon's own `at_verbosity` call painting a second live region
    /// over the process printer's).
    #[cfg(feature = "test-helpers")]
    #[test]
    fn derived_printers_inherit_ambient_terminal_and_test_channels() {
        // Sinks, live_region, interactive_stdin, and the seeded prompt queue.
        let (parent, _buf) =
            Printer::for_test_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
        for derived in [
            parent.at_verbosity(Verbosity::Normal),
            parent.rethemed("dracula"),
        ] {
            assert_eq!(
                derived.live_region, parent.live_region,
                "live_region must be inherited, not re-probed from the real stderr"
            );
            assert_eq!(
                derived.interactive_stdin, parent.interactive_stdin,
                "interactive_stdin must be inherited, not re-probed from the real stdin"
            );
            assert!(
                Arc::ptr_eq(&derived.sink_stderr, &parent.sink_stderr),
                "a derived printer must write into the SAME sink as its parent, not a fresh Term"
            );
            assert!(
                Arc::ptr_eq(&derived.sink_stdout, &parent.sink_stdout),
                "a derived printer must write into the SAME sink as its parent, not a fresh Term"
            );
            let queue = derived
                .prompt_queue
                .as_ref()
                .expect("a derived printer must inherit the parent's seeded prompt answers");
            assert!(
                Arc::ptr_eq(queue, parent.prompt_queue.as_ref().unwrap()),
                "the derived queue must be the SAME queue the parent was seeded with, not None"
            );
        }

        // test_doc_capture: an emit on the DERIVED printer must land in the
        // capture the PARENT test is holding, not on the real stdout.
        let (doc_parent, cap) = Printer::for_test_doc();
        let derived_doc = doc_parent.at_verbosity(Verbosity::Quiet);
        derived_doc.emit(super::super::doc::Doc::new().with_data(serde_json::json!({"ok": true})));
        assert_eq!(
            cap.json(),
            Some(serde_json::json!({"ok": true})),
            "a derived printer must keep writing into the parent's test_doc_capture"
        );

        // multi_progress: a derived printer must draw into the SAME
        // MultiProgress as its parent, not stand up a second one against a
        // fresh (real) draw target.
        let (live_parent, drawn) = Printer::for_test_with_live_bars();
        let derived_live = live_parent.at_verbosity(Verbosity::Normal);
        assert!(derived_live.live_bars());
        let bar = derived_live.progress_bar(1, "deriving");
        bar.inc(1);
        bar.finish();
        let out = drawn.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            !out.is_empty(),
            "a derived printer's progress bar never reached the parent's recording draw \
             target — it stood up a fresh MultiProgress instead of inheriting one"
        );
    }

    /// `spec.theme.overrides` is a documented field, and until the process
    /// printer was built from the whole block it was inert: `main` passed only
    /// `theme.name`, so `Theme::from_config` was reachable from nothing but its
    /// own tests and every declared override was silently dropped.
    #[test]
    #[serial]
    fn theme_overrides_reach_the_rendered_style() {
        use crate::config::{ThemeConfig, ThemeOverrides};

        let _ct = EnvVarGuard::set("COLORTERM", "truecolor");
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _term = EnvVarGuard::set("TERM", "xterm-256color");

        let config = ThemeConfig {
            name: "dracula".to_string(),
            overrides: ThemeOverrides {
                success: Some("#010203".to_string()),
                // The optional slot: a preset answering `None` must be FILLED
                // by an override, and filled before the colour stamp — a slot
                // minted afterwards would carry the default colour-off answer
                // instead of this printer's.
                primary: Some("#040506".to_string()),
                ..Default::default()
            },
        };

        let p = Printer::with_theme_config(
            Verbosity::Normal,
            Some(&config),
            OutputFormat::Table,
            ColorChoice::Always,
        );
        // Asserted on the RENDER rather than on the stored triple: reaching the
        // theme struct is not the claim, reaching the escape a user sees is.
        let theme = &p.renderer.theme;
        assert!(theme.colors(), "the stamp must reach the overridden slots");
        assert_eq!(
            theme.success.apply_to("x").to_string(),
            "\u{1b}[38;2;1;2;3mx\u{1b}[0m"
        );
        let primary = theme
            .primary
            .as_ref()
            .expect("an override must fill an optional slot the preset leaves empty");
        assert_eq!(
            primary.apply_to("x").to_string(),
            "\u{1b}[38;2;4;5;6mx\u{1b}[0m",
            "a slot minted during the override pass must still carry this \
             printer's colour decision, not the default colour-off one"
        );

        // No config at all is the default theme, not a panic or an empty one.
        let bare = Printer::with_theme_config(
            Verbosity::Normal,
            None,
            OutputFormat::Table,
            ColorChoice::Always,
        );
        assert_eq!(
            bare.renderer.theme.success.apply_to("x").to_string(),
            Theme::default()
                .with_colors(true)
                .success
                .apply_to("x")
                .to_string()
        );
    }

    #[test]
    fn is_structured_classifies() {
        let p = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Json,
            ColorChoice::Auto,
        );
        assert!(p.is_structured());
        let p = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Table,
            ColorChoice::Auto,
        );
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

        let out = crate::test_helpers::captured_text(&buf);
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
        p.alert("2 package actions will not apply");
        p.flush();

        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            !out.contains("ordinary warning"),
            "Role::Warn must stay suppressed under structured/Quiet; got: {out:?}"
        );
        assert!(
            out.contains("2 package actions will not apply"),
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

    /// An alert is emitted where the effect is DISCOVERED, and that site cannot
    /// know whether its caller opened a section — a source-constraint bypass is
    /// found while composing, under whichever group the command opened. The
    /// structural assert would make that call a debug-build panic over a message
    /// the user must see, so the advisory channel takes the open depth instead
    /// and renders there.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn an_alert_renders_inside_an_open_section() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        let section = p.section("Sources");
        p.alert("source 'team' bypassed requireSignedCommits");
        drop(section);
        p.flush();

        let out = crate::test_helpers::captured_text(&buf);
        let line = out
            .lines()
            .find(|l| l.contains("bypassed requireSignedCommits"))
            .unwrap_or_else(|| panic!("the alert must still be emitted; got: {out:?}"));
        assert!(
            line.starts_with("  "),
            "the alert renders at the open section's depth, not at column 0: {line:?}"
        );
    }

    /// The indent is half the claim; the other half is that an in-section alert
    /// still goes through the live region. The depth change is what made this
    /// call reachable mid-composition, where a spinner is exactly what is on
    /// screen — a raw write there strands the spinner's last paint for the rest
    /// of the session. Read from the emulated screen, the only capture that can
    /// see a paint the region never took back.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn an_in_section_alert_does_not_strand_the_live_region() {
        let (printer, screen) = Printer::for_test_live_terminal(24, 100);
        let section = printer.section("Sources");
        // A spinner is a non-structural emit and needs the guard to render
        // inside a section; the alert deliberately needs no such arrangement.
        let inherit = printer.depth_inheritance();
        let sp = printer.spinner("Composing sources");
        // Joins the steady-tick thread, so this thread is the only writer and
        // the bar has already painted one frame — no sleep, no race.
        sp.bar.disable_steady_tick();
        printer.alert("source 'team': --allow-unsigned bypassed requireSignedCommits");
        sp.finish_ok("Composed sources");
        drop(inherit);
        drop(section);
        printer.flush();

        let held = screen.contents();
        assert!(
            !held.contains("Composing sources"),
            "the running spinner's paint was stranded on the terminal: {held:?}"
        );
        assert_eq!(
            held.matches("bypassed requireSignedCommits").count(),
            1,
            "the alert must land exactly once: {held:?}"
        );
        assert!(
            held.contains("Composed sources"),
            "the settled line went missing: {held:?}"
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        assert!(crate::test_helpers::captured_text(&buf).trim().is_empty());
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("Status\n"));
        assert!(out.contains("Profile  dev"));
        assert!(out.contains("Files\n"));
        assert!(out.contains("\n  - foo.txt\n"));
    }

    /// `Printer::heading_title` composes the same three slots
    /// [`super::TitleLabel::styled`] tests directly — this proves the
    /// composer's output actually reaches the terminal through the
    /// imperative `Printer` entry point, not only through its own unit tests.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn heading_title_reaches_the_terminal_styled() {
        use super::super::TitleLabel;
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.heading_title(&TitleLabel::new("Status", "dev-tools"));
        p.flush();
        let out = crate::test_helpers::captured_text(&buf);
        assert_eq!(out.trim_end(), "Status: dev-tools");
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("package check"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn status_builder_detail_opt_some() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.status(Role::Ok, "installed").detail_opt(Some("v1.2.3"));
        p.flush();
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("file deployed"), "got: {out:?}");
    }

    #[cfg(feature = "test-helpers")]
    #[test]
    fn status_builder_with_duration() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.status(Role::Ok, "brew install curl")
            .duration(std::time::Duration::from_secs(3));
        p.flush();
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
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
        let rendered = crate::test_helpers::captured_text(&buf);
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
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.starts_with("profile:work\n"), "got: {out:?}");
        assert!(out.contains("\n  - installed ripgrep\n"), "got: {out:?}");
    }

    /// Every narrated wait in the workspace goes through `Printer::narrate`,
    /// so the settle discipline is proven once here rather than once per call
    /// site: the sites differ only in what they narrate, never in how the bar
    /// is opened, renamed or retired.
    ///
    /// A successful wait must leave NOTHING behind. Narration is
    /// live-region-only by contract — that is what lets every golden in the
    /// suite keep describing a run as if no spinner had ever existed — so a
    /// success that committed even one line would move goldens across the
    /// whole product.
    #[test]
    fn narrate_commits_nothing_when_the_work_succeeds() {
        let (printer, buf) = Printer::for_test_live_scrollback();
        let out: Result<u8, std::io::Error> = printer.narrate("Scanning packages", |sp| {
            sp.set_message("Enumerating apt");
            sp.set_message("Enumerating brew");
            Ok(7)
        });
        printer.flush();
        assert_eq!(out.ok(), Some(7));

        let committed = crate::test_helpers::captured_text(&buf);
        assert_eq!(
            committed, "",
            "a successful narrated wait committed a permanent line: {committed:?}"
        );
    }

    /// A bar paints the moment it is opened, so a wait that returns straight
    /// away flashes its label and takes it back. Both halves are pinned here:
    /// the label is on the terminal while the work runs, and the screen holds
    /// nothing once the wait ends.
    ///
    /// The flash is the accepted behaviour rather than an oversight. indicatif
    /// hands cfgd no timer callback — the steady tick drives its own redraw
    /// internally and never calls back — so deferring the first paint until a
    /// bar outlives a threshold needs a trigger cfgd owns, and it owns only
    /// two: `set_message` and `finish_*`. The waits that most need narrating
    /// (one blocking probe, one network round-trip) fire neither for seconds
    /// at a time, so a hook-checked reveal would silence the longest waits to
    /// spare the shortest a flicker. Nothing is stranded and no permanent line
    /// is written either way, which is what this asserts.
    #[test]
    fn narrate_paints_at_once_and_clears_a_wait_that_returns_immediately() {
        let (printer, screen) = Printer::for_test_live_terminal(24, 100);
        let out: Result<(), std::io::Error> = printer.narrate("Enumerating apt packages", |sp| {
            // Joins the steady-tick thread, so this thread is the only writer
            // and whatever is on screen was painted by the construction above.
            sp.bar.disable_steady_tick();
            assert!(
                screen.contents().contains("Enumerating apt packages"),
                "the label must be on the terminal before the work returns: {:?}",
                screen.contents()
            );
            Ok(())
        });
        printer.flush();
        assert!(out.is_ok());

        let held = screen.contents();
        assert_eq!(
            held.trim(),
            "",
            "an instant wait left its flashed label behind: {held:?}"
        );
    }

    /// A failure settles `Fail` at the step that was actually running, and the
    /// live region is left holding nothing — neither the opening label, nor an
    /// earlier step, nor `Drop`'s generic `(interrupted)` record.
    ///
    /// Read from the emulated screen: it is the only capture that can see a
    /// line the region painted and never took back. The hidden target draws
    /// nothing at all, and the recording buffer holds every repaint, where one
    /// paint too many is indistinguishable from one repaint too many.
    #[test]
    fn narrate_settles_fail_at_the_last_step_and_strands_no_running_line() {
        let (printer, screen) = Printer::for_test_live_terminal(24, 100);
        let out: Result<(), std::io::Error> = printer.narrate("Scanning packages", |sp| {
            // Joins the steady-tick thread, so this thread is the only writer
            // and the bar has already painted a frame — no sleep, no race.
            sp.bar.disable_steady_tick();
            sp.set_message("Enumerating apt");
            sp.set_message("Enumerating brew");
            Err(std::io::Error::other("brew exploded"))
        });
        printer.flush();
        assert!(out.is_err(), "the closure's error must propagate");

        let held = screen.contents();
        assert_eq!(
            held.matches("Enumerating brew").count(),
            1,
            "the failing step must be on screen exactly once: {held:?}"
        );
        for gone in ["Scanning packages", "Enumerating apt", "(interrupted)"] {
            assert!(
                !held.contains(gone),
                "{gone:?} was stranded on the terminal: {held:?}"
            );
        }
        let line = held
            .lines()
            .find(|l| l.contains("Enumerating brew"))
            .unwrap_or_default();
        assert!(
            line.contains(Theme::default().icon_fail.as_str()),
            "the settled step is not a Fail line: {line:?}"
        );
        assert!(
            !line.contains("brew exploded"),
            "the settle must carry no detail — the error is rendered at the \
             CLI boundary and would print twice: {line:?}"
        );
    }

    /// The per-step renames reach the terminal in the order the work made
    /// them, so the line always names the step actually running rather than
    /// the one the wait opened with.
    ///
    /// `for_test_with_live_bars` is the constructor that can answer this: it
    /// records every paint in the order it was made, where the emulated screen
    /// holds only the last one and the hidden target holds none.
    #[test]
    fn narrate_paints_each_step_in_the_order_the_work_named_it() {
        let (printer, buf) = Printer::for_test_with_live_bars();
        let steps = ["Checking files", "Checking packages", "Checking system"];
        let out: Result<(), std::io::Error> = printer.narrate("Verifying", |sp| {
            sp.bar.disable_steady_tick();
            for step in steps {
                sp.set_message(step);
            }
            Ok(())
        });
        printer.flush();
        assert!(out.is_ok());

        let painted = crate::test_helpers::captured_text(&buf);
        let mut cursor = 0usize;
        for step in steps {
            let at = painted[cursor..].find(step).unwrap_or_else(|| {
                panic!("{step:?} never painted, or painted out of order: {painted:?}")
            });
            cursor += at + step.len();
        }
    }

    /// `Quiet` — what a `-o json` run derives, and what a command hands its
    /// library work — narrates nothing at all: the bar is hidden, so no step
    /// ever reaches the terminal and the structured channel stays pure.
    ///
    /// Read through the emulated screen at `Quiet`, the ONE capture that can
    /// state this. Every other constructor pins `live_region: false`, so its
    /// bar is hidden for a reason that has nothing to do with the verbosity —
    /// the claim would hold with the `Quiet` gate deleted outright, which is
    /// the definition of a test that cannot go red.
    #[test]
    fn narrate_under_quiet_paints_no_step() {
        let (printer, screen) = Printer::for_test_live_terminal_at(Verbosity::Quiet, 24, 100);
        assert!(
            printer.live_region,
            "the capture must own a real live region, or the claim below is vacuous"
        );
        assert!(
            !printer.live_bars(),
            "Quiet must be what closes the region here, nothing else"
        );
        let out: Result<(), std::io::Error> = printer.narrate("Collecting snapshot", |sp| {
            assert!(sp.bar.is_hidden(), "Quiet must yield a hidden bar");
            sp.set_message("Checking packages");
            Ok(())
        });
        printer.flush();
        assert!(out.is_ok());

        let held = screen.contents();
        assert_eq!(
            held.trim(),
            "",
            "a Quiet narrated wait painted on the terminal: {held:?}"
        );
    }

    /// A narrated wait reached from INSIDE an open section renders at that
    /// section's depth instead of tripping the top-level structural assert.
    ///
    /// This is not a theoretical arrangement, and both wrappers open their bar
    /// through the same guarded constructor: `PackageContext::installed_for`
    /// narrates the per-manager enumeration silently, and `cfgd diff` asks it
    /// from inside its open Packages section while a bare `status` asks it at
    /// top level. A wrapper that opened its bar at a hard depth 0 panicked the
    /// first caller in a debug build.
    #[test]
    fn narrate_renders_inside_a_section_its_caller_opened() {
        let (printer, screen) = Printer::for_test_live_terminal(24, 100);
        let section = printer.section("Packages");
        // Read from INSIDE the wait: `narrate` clears its own bar on the way
        // out, so after it returns there is nothing left on screen to place.
        let mut running = String::new();
        let out: Result<(), std::io::Error> = printer.narrate("Enumerating apt packages", |sp| {
            // Joins the steady-tick thread, so this thread is the only writer
            // and the bar has already painted a frame — no sleep, no race.
            sp.bar.disable_steady_tick();
            running = screen.contents();
            Ok(())
        });
        assert!(out.is_ok());
        drop(section);
        printer.flush();

        let line = running
            .lines()
            .find(|l| l.contains("Enumerating apt packages"))
            .unwrap_or_default();
        assert!(
            line.starts_with("  ") && !line.starts_with("   "),
            "the wait did not paint in the section's glyph column: {line:?}"
        );
    }

    /// The silent wrapper is the one production actually reaches from inside a
    /// section, so it is placed here too rather than being inferred from its
    /// sibling: both open their bar through the same guarded constructor, and
    /// a change that dropped the guard from only one of them would leave this
    /// pair disagreeing instead of both passing.
    #[test]
    fn narrate_silent_renders_inside_a_section_its_caller_opened() {
        let (printer, screen) = Printer::for_test_live_terminal(24, 100);
        let section = printer.section("Packages");
        let mut running = String::new();
        let out: Result<(), std::io::Error> =
            printer.narrate_silent("Enumerating apt packages", |sp| {
                sp.bar.disable_steady_tick();
                running = screen.contents();
                Ok(())
            });
        assert!(out.is_ok());
        drop(section);
        printer.flush();

        let line = running
            .lines()
            .find(|l| l.contains("Enumerating apt packages"))
            .unwrap_or_default();
        assert!(
            line.starts_with("  ") && !line.starts_with("   "),
            "the wait did not paint in the section's glyph column: {line:?}"
        );
    }

    /// A silent wait says nothing on EITHER arm: the failure belongs to
    /// whoever already named the request, and the `Err` still reaches them.
    ///
    /// The failing half is what separates this from `narrate` — a `Fail`
    /// settle survives `Verbosity::Quiet` and would land beside a `-o json`
    /// payload carrying the identical fact — so it is read from the emulated
    /// screen, the only capture that can see a line the region drew and never
    /// took back.
    #[test]
    fn narrate_silent_leaves_no_line_on_either_arm() {
        let (printer, screen) = Printer::for_test_live_terminal(24, 100);
        let out: Result<(), std::io::Error> = printer.narrate_silent("Enumerating apt", |sp| {
            sp.bar.disable_steady_tick();
            sp.set_message("Enumerating brew");
            Err(std::io::Error::other("brew exploded"))
        });
        printer.flush();
        assert!(out.is_err(), "the closure's error must still propagate");

        let held = screen.contents();
        assert_eq!(
            held.trim(),
            "",
            "a silent wait settled a line its caller already owns: {held:?}"
        );
    }

    /// A `Quiet` FAILURE still settles its one `Fail` line, because `Fail` is
    /// the single role the renderer shows at `Quiet` — the step that was
    /// running is the one fact the propagated error does not carry, and a
    /// silent Quiet failure would drop it. It lands on stderr like every other
    /// status, so a `-o json` run's data channel is untouched.
    #[test]
    fn narrate_under_quiet_still_settles_the_failing_step() {
        let (printer, stdout, stderr) = Printer::for_test_split_streams(Verbosity::Quiet);
        let out: Result<(), std::io::Error> = printer.narrate("Collecting snapshot", |sp| {
            sp.set_message("Checking packages");
            Err(std::io::Error::other("nope"))
        });
        printer.flush();
        assert!(out.is_err());

        assert_eq!(
            crate::test_helpers::captured_text(&stdout),
            "",
            "the data channel must stay pure"
        );
        let diagnostics = crate::test_helpers::captured_text(&stderr);
        assert!(
            diagnostics.contains("Checking packages"),
            "the failing step went unreported: {diagnostics:?}"
        );
    }
}

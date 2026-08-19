//! `SectionGuard` is the only path to indented output. Its lifetime is tied
//! to `&Printer`, and Drop closes the section.
use std::sync::Arc;

use super::renderer::{Renderer, StatusFields, Table, Writer};
use super::{Printer, Role};

/// Open section. Holds a reference to Printer and the renderer's depth.
/// Drop closes the section: emits a deferred `(none)` placeholder if no
/// children rendered (and `keep_when_empty` was true), or leaves no trace
/// (if `keep_when_empty` was false).
pub struct SectionGuard<'p> {
    pub(crate) printer: &'p Printer,
    pub(crate) renderer: Arc<Renderer>,
    pub(crate) sink: Arc<dyn Writer>,
    pub(crate) depth: usize,
}

impl<'p> SectionGuard<'p> {
    pub fn bullet(&self, text: impl Into<String>) -> &Self {
        self.renderer
            .render_bullet(self.sink.as_ref(), self.depth, &text.into());
        self
    }

    pub fn kv(&self, key: impl Into<String>, value: impl Into<String>) -> &Self {
        // Defer to the buffer so consecutive kvs at this depth coalesce.
        self.renderer.render_kv(&key.into(), &value.into());
        self
    }

    pub fn kv_block<I, K, V>(&self, pairs: I) -> &Self
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
            .render_kv_block(self.sink.as_ref(), self.depth, &pairs);
        self
    }

    pub fn hint(&self, text: impl Into<String>) -> &Self {
        self.renderer
            .render_hint(self.sink.as_ref(), self.depth, &text.into());
        self
    }

    pub fn note(&self, text: impl Into<String>) -> &Self {
        self.renderer
            .render_note(self.sink.as_ref(), self.depth, &text.into());
        self
    }

    pub fn table(&self, table: Table) -> &Self {
        self.renderer
            .render_table(self.sink.as_ref(), self.depth, &table);
        self
    }

    /// Append a tight, copy-pasteable block of verbatim lines (e.g. the full
    /// body of a security-review script preview, one entry per source line).
    /// Mirrors `Doc::code_block`: each entry must already be one physical
    /// line (`render_code_block`'s `write_line` calls debug_assert on `\n`),
    /// but a stray `\r` — which `str::lines()` upstream wouldn't have
    /// stripped unless paired with `\n` — is scrubbed here regardless, so
    /// unlike `bullet` this stays the correct sink for content whose line
    /// count isn't controlled by the caller.
    pub fn code_block(&self, lines: impl IntoIterator<Item = impl Into<String>>) -> &Self {
        let lines: Vec<String> = lines
            .into_iter()
            .map(|l| l.into().chars().filter(|&c| c != '\r').collect())
            .collect();
        self.renderer
            .render_code_block(self.sink.as_ref(), self.depth, &lines);
        self
    }

    /// Set the empty-state placeholder for this section (overrides the default
    /// "(none)"). Only meaningful for sections opened with `section()` (not
    /// `section_or_collapse()`).
    pub fn empty_state(&self, text: impl Into<String>) -> &Self {
        self.renderer.render_section_empty_state(&text.into());
        self
    }

    /// Status with no extra fields. For chained detail/duration/target, use
    /// `status` for the chainable builder.
    pub fn status_simple(&self, role: Role, subject: impl Into<String>) -> &Self {
        let subject = subject.into();
        self.renderer.render_status(
            self.sink.as_ref(),
            self.depth,
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
        self
    }

    /// Status at this section's depth whose SUBJECT is painted
    /// `theme.primary` — the deepest level of the phase → owner → action tree,
    /// and the only caller of `StatusFields::subject_style`. A preset with no
    /// palette foreground of its own answers `None` and the subject keeps its
    /// role style, which is byte-identical to `status`.
    pub fn action_status(
        &self,
        role: Role,
        subject: impl Into<String>,
    ) -> super::status_builder::StatusBuilder<'_> {
        self.status(role, subject)
            .with_subject_style(self.renderer.theme.primary.clone())
    }

    /// Write this section's header now rather than at its first child.
    ///
    /// For a section whose content can open a live region before it settles a
    /// line — an owner group whose first action runs a command in an output
    /// window. The live region paints below the last committed line, so a
    /// header still deferred at that point is written after the output it
    /// introduces.
    ///
    /// Valid only on a section that keeps its header when empty (`section`,
    /// `section_phase`, `section_owner`): one opened to collapse if empty
    /// leaves no trace at close, so a header committed ahead of content that
    /// never arrives is orphaned. Debug builds assert it.
    pub fn commit_header(&self) -> &Self {
        self.renderer
            .render_section_commit_header(self.sink.as_ref());
        self
    }

    /// Mark this section live: its statuses render as they complete rather
    /// than at close, right-padded to `width` so the trailing muted column
    /// still aligns. `width` is computed from the plan before the run, because
    /// a live stream cannot wait for its own close to learn it. Sections that
    /// never call this keep buffer-and-align semantics exactly.
    pub fn live_column(&self, width: usize) -> &Self {
        self.renderer.render_section_live_column(width);
        self
    }

    /// Open a child section headed by a styled owner token.
    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section_owner(&self, label: &super::OwnerLabel) -> SectionGuard<'_> {
        self.renderer.render_section_open_styled(
            &label.plain(),
            Some(label.styled(&self.renderer.theme)),
            /*keep_when_empty=*/ true,
        );
        SectionGuard {
            printer: self.printer,
            renderer: self.renderer.clone(),
            sink: self.sink.clone(),
            depth: self.depth + 1,
        }
    }

    /// [`SectionGuard::section_owner`] for a caller that cannot know whether
    /// the owner has anything to say until after it has said it.
    ///
    /// A plan walks its groups, so every group it opens is non-empty by
    /// construction ("never empty — an owner with no actions in a phase
    /// produces no group"). A streaming drift surface opens the group around a
    /// renderer that decides per file whether to speak, so the same invariant
    /// has to be enforced at close: the group leaves no trace rather than an
    /// owner heading over `(none)`.
    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section_owner_or_collapse(&self, label: &super::OwnerLabel) -> SectionGuard<'_> {
        self.renderer.render_section_open_styled(
            &label.plain(),
            Some(label.styled(&self.renderer.theme)),
            /*keep_when_empty=*/ false,
        );
        SectionGuard {
            printer: self.printer,
            renderer: self.renderer.clone(),
            sink: self.sink.clone(),
            depth: self.depth + 1,
        }
    }

    /// Status builder at this section's depth. Commits on Drop.
    pub fn status(
        &self,
        role: Role,
        subject: impl Into<String>,
    ) -> super::status_builder::StatusBuilder<'_> {
        super::status_builder::StatusBuilder::new(
            self.renderer.clone(),
            self.sink.clone(),
            self.depth,
            role,
            subject,
        )
    }

    /// Open a child section. Returns a guard that borrows `&self` so the parent
    /// is locked until the child drops.
    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section(&self, name: impl Into<String>) -> SectionGuard<'_> {
        self.renderer
            .render_section_open(&name.into(), /*keep_when_empty=*/ true);
        SectionGuard {
            printer: self.printer,
            renderer: self.renderer.clone(),
            sink: self.sink.clone(),
            depth: self.depth + 1,
        }
    }

    #[must_use = "section closes when SectionGuard is dropped; bind it"]
    pub fn section_or_collapse(&self, name: impl Into<String>) -> SectionGuard<'_> {
        self.renderer
            .render_section_open(&name.into(), /*keep_when_empty=*/ false);
        SectionGuard {
            printer: self.printer,
            renderer: self.renderer.clone(),
            sink: self.sink.clone(),
            depth: self.depth + 1,
        }
    }

    /// Section-scoped spinner. Inherits the section's depth so the eventual
    /// Status emitted by `finish_*` lands at the right indentation.
    #[must_use]
    pub fn spinner(&self, message: impl Into<String>) -> super::spinner::Spinner<'_> {
        let message = message.into();
        let (bar, live) = super::spinner::make_spinner_bar(
            &self.printer.multi_progress,
            &self.renderer,
            self.printer.live_bars(),
            self.depth,
            &message,
        );
        super::spinner::Spinner {
            renderer: self.renderer.clone(),
            sink: self.sink.clone(),
            depth: self.depth,
            bar,
            message,
            finished: false,
            _live: live,
            borrowed: false,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Section-scoped progress bar.
    #[must_use]
    pub fn progress_bar(
        &self,
        total: u64,
        message: impl Into<String>,
    ) -> super::spinner::ProgressBar<'_> {
        let message = message.into();
        let (bar, live) = super::spinner::make_progress_bar(
            &self.printer.multi_progress,
            &self.renderer,
            total,
            self.printer.live_bars(),
            self.depth,
            &message,
        );
        super::spinner::ProgressBar {
            renderer: self.renderer.clone(),
            sink: self.sink.clone(),
            depth: self.depth,
            bar,
            message,
            finished: false,
            _live: live,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Run an external command at this section's depth, displaying its output
    /// through an `OutputWindow` indented under the section and capturing the
    /// full stdout/stderr.
    pub fn run(
        &self,
        cmd: &mut std::process::Command,
        label: impl Into<String>,
    ) -> std::io::Result<super::process::CommandOutput> {
        super::process::run_command(
            self.printer,
            self.depth,
            cmd,
            &label.into(),
            super::process::StatusOwner::Window,
        )
    }

    /// Manually close (alternative to drop). Useful when the caller needs the
    /// section to close before the binding goes out of scope.
    pub fn close(self) { /* drop happens here */
    }
}

impl Drop for SectionGuard<'_> {
    fn drop(&mut self) {
        self.renderer.render_section_close(self.sink.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use crate::output::{Printer, Role, Verbosity, strip_ansi};

    // --- progress_bar (lines 156-171) ---

    /// `SectionGuard::progress_bar` returns a usable `ProgressBar` (non-TTY path
    /// returns a hidden bar; `inc` / `set_message` / `finish` must not panic).
    #[test]
    fn section_progress_bar_returns_usable_bar() {
        let (p, _buf) = Printer::for_test_at(Verbosity::Normal);
        let s = p.section("Work");
        let mut bar = s.progress_bar(10, "loading");
        bar.inc(3);
        bar.set_position(5);
        bar.set_message("half done");
        bar.finish();
        // The section itself must still render normally after the bar is finished.
        s.bullet("done");
        drop(s);
        p.flush();
    }

    /// `progress_bar` on a section opened at depth > 0 (nested section) does
    /// not panic and the outer/inner content renders correctly.
    #[test]
    fn nested_section_progress_bar_does_not_panic() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let outer = p.section("Outer");
            {
                let inner = outer.section("Inner");
                let bar = inner.progress_bar(5, "downloading");
                bar.inc(5);
                bar.finish();
                inner.bullet("complete");
            }
            outer.bullet("all done");
        }
        p.flush();
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("Outer\n"), "outer header missing: {out:?}");
        assert!(out.contains("Inner\n"), "inner header missing: {out:?}");
        assert!(out.contains("complete"), "inner bullet missing: {out:?}");
        assert!(out.contains("all done"), "outer bullet missing: {out:?}");
    }

    // --- run (lines 176-189) ---

    /// `SectionGuard::run` executes an external command and returns its output.
    /// Non-TTY path → streaming; the rendered output must include the label and
    /// the section header must appear before it.
    #[test]
    fn section_run_captures_command_output() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Build");
            let result = s
                .run(
                    std::process::Command::new("echo").arg("hello-from-section-run"),
                    "echo step",
                )
                .expect("echo must succeed");
            assert!(
                result.status.success(),
                "echo should exit 0, got: {:?}",
                result.status
            );
            // The captured stdout must contain what echo printed.
            assert!(
                result.stdout.contains("hello-from-section-run"),
                "stdout missing: {:?}",
                result.stdout
            );
        }
        p.flush();
        let out = crate::test_helpers::captured_text(&buf);
        // Section header must appear in the rendered output.
        assert!(out.contains("Build\n"), "section header missing: {out:?}");
        // The streaming path emits the label as a Status(Running) line.
        assert!(out.contains("echo step"), "run label missing: {out:?}");
    }

    /// `SectionGuard::run` for a failing command returns a non-success exit
    /// status (does NOT propagate as Err; the command itself ran).
    #[test]
    fn section_run_non_zero_exit_is_not_io_error() {
        let (p, _buf) = Printer::for_test_at(Verbosity::Normal);
        let s = p.section("Fail");
        // `false` exits 1 on all POSIX targets.
        let result = s
            .run(&mut std::process::Command::new("false"), "false step")
            .expect("run itself must not return Err for a non-zero exit");
        assert!(
            !result.status.success(),
            "false should exit non-zero, got: {:?}",
            result.status
        );
    }

    /// A section opened via `SectionGuard::section` (child of another guard)
    /// receives its own `progress_bar` call without borrowing from the parent
    /// and closes cleanly.
    #[test]
    fn child_section_progress_bar_depth_is_parent_plus_one() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let parent = p.section("Parent");
            {
                let child = parent.section("Child");
                // progress_bar at depth == 2; must not panic.
                let bar = child.progress_bar(1, "step");
                bar.finish();
                child.status_simple(Role::Ok, "child-status");
            }
            parent.status_simple(Role::Ok, "parent-status");
        }
        p.flush();
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("Parent\n"), "parent header missing: {out:?}");
        assert!(out.contains("Child\n"), "child header missing: {out:?}");
        assert!(
            out.contains("child-status"),
            "child status missing: {out:?}"
        );
        assert!(
            out.contains("parent-status"),
            "parent status missing: {out:?}"
        );
    }

    // --- action_status / live_column / section_owner ---

    /// Render one section body against `theme`, returning the captured bytes
    /// with the section header line dropped.
    fn capture_section(
        theme: crate::output::Theme,
        body: impl FnOnce(&crate::output::section_guard::SectionGuard<'_>),
    ) -> String {
        let (p, buf) = Printer::for_test_with_theme_colored(theme, Verbosity::Normal);
        {
            let s = p.section("Files");
            body(&s);
        }
        p.flush();
        // raw-capture-ok: two callers compare the RAW capture for exact colour equality/inequality (action_subject_keeps_role_style_under_default, action_status_leaves_the_glyph_on_the_role_style) — captured_text would strip the ANSI both exist to check
        let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        raw.lines().skip(1).collect::<Vec<_>>().join("\n")
    }

    /// `theme.primary` is `None` under `default`, so an action subject keeps
    /// its role style and the redesign's deepest line is byte-identical to
    /// what `status` renders today. A preset that DOES carry a primary must
    /// diverge, or the assertion above would pass on a seam that does nothing.
    #[test]
    #[serial_test::serial]
    fn action_subject_keeps_role_style_under_default() {
        use crate::output::Theme;

        let plain_default = capture_section(Theme::from_preset("default"), |s| {
            let _ = s.status(Role::Ok, "wrote /etc/hosts");
        });
        let action_default = capture_section(Theme::from_preset("default"), |s| {
            let _ = s.action_status(Role::Ok, "wrote /etc/hosts");
        });
        assert_eq!(
            action_default, plain_default,
            "default has no primary, so the subject must keep the role style"
        );

        let plain_dracula = capture_section(Theme::from_preset("dracula"), |s| {
            let _ = s.status(Role::Ok, "wrote /etc/hosts");
        });
        let action_dracula = capture_section(Theme::from_preset("dracula"), |s| {
            let _ = s.action_status(Role::Ok, "wrote /etc/hosts");
        });
        assert_ne!(
            action_dracula, plain_dracula,
            "dracula carries a primary, so the subject must take it"
        );
        assert_eq!(
            strip_ansi(&action_dracula),
            strip_ansi(&plain_dracula),
            "the seam is colour-only; no visible text may change"
        );
    }

    /// The glyph is the role's, whatever the subject takes — a green ✓ in
    /// front of a palette-coloured subject is the whole point of the split.
    #[test]
    #[serial_test::serial]
    fn action_status_leaves_the_glyph_on_the_role_style() {
        use crate::output::Theme;
        let theme = Theme::from_preset("dracula").with_colors(true);
        let glyph_run = theme.success.apply_to(&theme.icon_ok).to_string();
        let out = capture_section(Theme::from_preset("dracula"), |s| {
            let _ = s.action_status(Role::Ok, "wrote /etc/hosts");
        });
        assert!(
            out.contains(&glyph_run),
            "glyph lost the role style: {out:?}"
        );
    }

    /// A live section emits each status as it arrives; a buffered one holds
    /// every status until close. The ordering against an interleaved bullet is
    /// what separates the two.
    #[test]
    fn live_column_emits_statuses_before_close() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Live");
            s.live_column(20);
            let _ = s.status(Role::Ok, "first");
            s.bullet("after");
        }
        p.flush();
        let live = crate::test_helpers::captured_text(&buf);
        let first = live.find("first").expect("status missing");
        let after = live.find("after").expect("bullet missing");
        assert!(
            first < after,
            "live status did not emit on arrival: {live:?}"
        );

        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Buffered");
            let _ = s.status(Role::Ok, "first");
            s.bullet("after");
        }
        p.flush();
        let buffered = crate::test_helpers::captured_text(&buf);
        let first = buffered.find("first").expect("status missing");
        let after = buffered.find("after").expect("bullet missing");
        assert!(
            after < first,
            "a section with no live column must still buffer to close: {buffered:?}"
        );
    }

    /// The live column pads exactly the lines a section close would have
    /// padded: a subject with trailing content, and no other.
    #[test]
    fn live_column_pads_only_subjects_with_trailing_content() {
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let s = p.section("Live");
            s.live_column(20);
            let _ = s.status(Role::Ok, "short").detail("done");
            let _ = s.status(Role::Ok, "bare");
        }
        p.flush();
        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.contains(&format!("short{} — done", " ".repeat(15))),
            "subject was not padded to the column: {out:?}"
        );
        assert!(
            out.contains("✓ bare\n"),
            "a subject with no trailing content must not be padded: {out:?}"
        );
    }

    /// A child section headed by an owner token renders the token, not the
    /// header slot's own styling of it.
    #[test]
    #[serial_test::serial]
    fn section_owner_heads_the_group_with_the_owner_token() {
        use crate::output::{OwnerLabel, Theme};
        let label = OwnerLabel::new("module", "nvim");
        let expected = label.styled(&Theme::from_preset("dracula").with_colors(true));

        let (p, buf) =
            Printer::for_test_with_theme_colored(Theme::from_preset("dracula"), Verbosity::Normal);
        {
            let phase = p.section("Phase: Files");
            let owner = phase.section_owner(&label);
            owner.bullet("wrote init.lua");
        }
        p.flush();
        // raw-capture-ok: asserting the owner token's exact styled run reaches the renderer unrestyled — captured_text would strip the ANSI this test exists to check
        let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            raw.contains(&expected),
            "owner token missing or restyled: {raw:?}"
        );
        let plain = strip_ansi(&raw);
        assert!(
            plain.contains("\n  module:nvim\n"),
            "owner group must sit one level under its phase: {plain:?}"
        );
    }

    /// `commit_header` writes an owner group's header immediately instead of
    /// leaving it deferred to the group's first committed child line — for
    /// exactly the shape `cli/sync.rs` hits: the group's first action opens a
    /// live spinner, and a DERIVED Quiet printer sharing the same live region
    /// (the `silent_printer` a library call runs under) can still land a
    /// `Role::Fail` line while that spinner is running, since `Fail` survives
    /// `Quiet`. Reached only through `for_test_with_live_bars`, the one
    /// capture that records both the routed line AND the header in the order
    /// they actually landed — `for_test_live_scrollback` cannot tell the two
    /// orderings apart, because whether the header commits eagerly or is
    /// deferred to the spinner's own settle line, the FINAL committed sequence
    /// is byte-identical; only the moment a mid-spinner write from elsewhere
    /// lands relative to it differs.
    #[test]
    fn commit_header_lands_before_a_mid_spinner_write_from_a_derived_printer() {
        use crate::output::OwnerLabel;

        let (p, buf) = Printer::for_test_with_live_bars();
        let quiet = p.at_verbosity(Verbosity::Quiet);
        {
            let sources_sec = p.section("Sources");
            let owner = sources_sec.section_owner(&OwnerLabel::new("source", "missing-team"));
            owner.commit_header();
            let sp = owner.spinner("Syncing");
            // The library call's own Quiet printer still lands a Fail line
            // while `sp` is live — the shared MultiProgress routes it through
            // rather than garbling the spinner's paint.
            quiet.status_simple(Role::Fail, "boom");
            sp.finish_ok("synced");
        }
        p.flush();

        let out = crate::test_helpers::captured_text(&buf);
        let header_at = out
            .find("source:missing-team")
            .unwrap_or_else(|| panic!("owner header missing: {out:?}"));
        let fail_at = out
            .find("boom")
            .unwrap_or_else(|| panic!("mid-spinner Fail line missing: {out:?}"));
        assert!(
            header_at < fail_at,
            "commit_header must land the owner header before a write that \
             reaches the shared live region while the group's first child \
             (the spinner) is still open: {out:?}"
        );
    }

    /// `Printer::section_caveats` paints its "Caveats" heading `theme.accent`
    /// + bold — the phase-name slot, because the heading is a phase-class
    /// title meant to draw the eye. Every other section (plain or owner)
    /// paints `theme.header`, so a style regression that quietly routed this
    /// heading back through the ordinary path would still pass a plain-string
    /// assertion; only comparing the raw styled run against both candidates
    /// catches it.
    #[test]
    #[serial_test::serial]
    fn section_caveats_heading_is_accent_bold_not_header() {
        use crate::output::Theme;

        let theme = Theme::from_preset("dracula");
        let colored = theme.clone().with_colors(true);
        let expected_accent_bold = colored
            .accent
            .clone()
            .bold()
            .apply_to("Caveats")
            .to_string();
        let header_styled = colored.header.apply_to("Caveats").to_string();
        assert_ne!(
            expected_accent_bold, header_styled,
            "the fixture theme must actually distinguish the two slots, or this test proves nothing"
        );

        let (p, buf) = Printer::for_test_with_theme_colored(theme, Verbosity::Normal);
        {
            let s = p.section_caveats();
            let owner = s.section_owner(&crate::output::OwnerLabel::new("cfgd", "env"));
            owner.status_simple(Role::Warn, "run `source ~/.cfgd.env` — or open a new shell");
        }
        p.flush();
        // raw-capture-ok: asserting the heading's exact styled run reaches the renderer unrestyled — captured_text would strip the ANSI this test exists to check
        let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            raw.contains(&expected_accent_bold),
            "Caveats heading must be theme.accent + bold: {raw:?}"
        );
        assert!(
            !raw.contains(&header_styled),
            "Caveats heading must not fall back to theme.header: {raw:?}"
        );
    }

    /// The collapsing variant keeps the "an owner group is never empty"
    /// invariant for a streaming caller: a group that said nothing leaves no
    /// heading and no `(none)` placeholder behind, while one that spoke reads
    /// exactly like `section_owner`.
    #[test]
    fn section_owner_or_collapse_leaves_no_trace_when_empty() {
        use crate::output::OwnerLabel;

        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let phase = p.section("Phase: Files");
            drop(phase.section_owner_or_collapse(&OwnerLabel::new("profile", "tiny")));
            phase.status_simple(Role::Ok, "No file drift");
        }
        p.flush();
        let plain = crate::test_helpers::captured_text(&buf);
        assert!(
            !plain.contains("profile:tiny"),
            "a silent owner group must not head itself: {plain:?}"
        );
        assert!(
            !plain.contains("(none)"),
            "a silent owner group must not leave a placeholder: {plain:?}"
        );

        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        {
            let phase = p.section("Phase: Files");
            let group = phase.section_owner_or_collapse(&OwnerLabel::new("profile", "tiny"));
            group.status_simple(Role::Info, "~/.gitconfig (new file)");
        }
        p.flush();
        let plain = crate::test_helpers::captured_text(&buf);
        assert!(
            plain.contains("\n  profile:tiny\n"),
            "a speaking owner group heads itself under its phase: {plain:?}"
        );
    }
}

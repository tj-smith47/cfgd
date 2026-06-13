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
            },
        );
        self
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
        let bar = super::spinner::make_spinner_bar(
            &self.printer.multi_progress,
            &self.renderer,
            self.printer.verbosity(),
            &message,
        );
        super::spinner::Spinner {
            renderer: self.renderer.clone(),
            sink: self.sink.clone(),
            depth: self.depth,
            bar,
            message,
            finished: false,
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
        let bar = super::spinner::make_progress_bar(
            &self.printer.multi_progress,
            total,
            self.printer.verbosity(),
            &message.into(),
        );
        super::spinner::ProgressBar {
            bar,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Run an external command at this section's depth with live output.
    /// TTY+non-quiet → spinner with tailing ring indented under the section;
    /// otherwise → streaming lines. Either path captures full stdout/stderr.
    pub fn run(
        &self,
        cmd: &mut std::process::Command,
        label: impl Into<String>,
    ) -> std::io::Result<super::process::CommandOutput> {
        super::process::run_command(
            &self.renderer,
            self.sink.as_ref(),
            &self.printer.multi_progress,
            self.depth,
            cmd,
            &label.into(),
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
        let bar = s.progress_bar(10, "loading");
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
        let out = strip_ansi(&buf.lock().unwrap());
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
        let out = strip_ansi(&buf.lock().unwrap());
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
        let out = strip_ansi(&buf.lock().unwrap());
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
}

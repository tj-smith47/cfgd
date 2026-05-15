//! `SectionGuard` is the only path to indented output. Its lifetime is tied
//! to `&Printer`, and Drop closes the section.
//!
//! R1 skeleton: the status-builder / spinner / progress / run / sub-status
//! methods land in T16+ (StatusBuilder, Spinner, ProgressBar, Run). T15
//! lands the subset documented in the task body: bullet/kv/kv_block/
//! hint/note/table/empty_state/status_simple/nested sections + Drop.
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

    /// Status with no extra fields. For chained detail/duration/target, see
    /// the StatusBuilder added in T16.
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

    /// Open a child section. Returns a guard that borrows `&self` so the parent
    /// is locked until the child drops (rule §6.1).
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

    #[must_use]
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

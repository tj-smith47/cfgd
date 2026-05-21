//! `StatusBuilder` is the chainable builder for a single Status line.
//!
//! Commits on Drop. **Style rule (NOT compile-enforced):** never put `?`
//! inside a `.detail(some_op()?)` chain — early return drops the builder
//! with partial fields and emits a half-built Status before the error
//! propagates. Build the inputs first, then construct the builder.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::Role;
use super::component::StatusLabel;
use super::renderer::{Renderer, StatusFields, Writer, finalize_subject};

/// Builder for one Status line. Commits on Drop.
///
/// **Gotcha:** never use `?` to compute a field inside the chain
/// (e.g., `.detail(some_op()?)`). If `some_op()` returns Err, the
/// half-built builder drops, committing a partial Status, then `?`
/// propagates. Build the inputs first, then construct the builder.
pub struct StatusBuilder<'p> {
    pub(crate) renderer: Arc<Renderer>,
    pub(crate) sink: Arc<dyn Writer>,
    pub(crate) depth: usize,
    pub(crate) role: Role,
    pub(crate) subject: String,
    pub(crate) detail: Option<String>,
    pub(crate) duration: Option<Duration>,
    pub(crate) target: Option<PathBuf>,
    pub(crate) label: Option<StatusLabel>,
    /// Lifetime parameter binding to either Printer or SectionGuard.
    pub(crate) _phantom: std::marker::PhantomData<&'p ()>,
}

impl<'p> StatusBuilder<'p> {
    /// Crate-private constructor used by both `Printer::status` and
    /// `SectionGuard::status` to avoid duplicating the field list.
    pub(crate) fn new(
        renderer: Arc<Renderer>,
        sink: Arc<dyn Writer>,
        depth: usize,
        role: Role,
        subject: impl Into<String>,
    ) -> Self {
        Self {
            renderer,
            sink,
            depth,
            role,
            subject: subject.into(),
            detail: None,
            duration: None,
            target: None,
            label: None,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn detail(mut self, text: impl Into<String>) -> Self {
        self.detail = Some(text.into());
        self
    }

    pub fn detail_opt(mut self, text: Option<&str>) -> Self {
        self.detail = text.map(|s| s.to_string());
        self
    }

    pub fn duration(mut self, d: Duration) -> Self {
        self.duration = Some(d);
        self
    }

    pub fn target(mut self, path: &Path) -> Self {
        self.target = Some(path.to_path_buf());
        self
    }

    /// Append a styled label (e.g. `[source-name]`) at the end of the subject.
    /// Auto-prefixes a single space so callers pass just the label content
    /// (`"[source-name]"`, not `" [source-name]"`).
    ///
    /// The label always renders at end-of-subject — the API cannot embed
    /// styled segments mid-subject, which would break the outer role color
    /// via the inner SGR reset.
    pub fn label(mut self, role: Role, text: impl Into<String>) -> Self {
        self.label = Some(StatusLabel {
            role,
            text: text.into(),
        });
        self
    }
}

impl Drop for StatusBuilder<'_> {
    fn drop(&mut self) {
        // Sanitize caller-supplied subject ANSI BEFORE composing the
        // renderer-owned label SGR (foreign `\x1b[0m` in a captured error
        // would otherwise prematurely close the role styling at the inner
        // reset). The label SGR is appended after sanitation so it survives.
        self.subject = finalize_subject(&self.renderer.theme, &self.subject, self.label.as_ref());
        let detail = self.detail.as_deref();
        let target = self.target.as_deref();
        self.renderer.render_status(
            self.sink.as_ref(),
            self.depth,
            &StatusFields {
                role: self.role,
                subject: &self.subject,
                detail,
                duration: self.duration,
                target,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::renderer::{Renderer, StringSink};
    use super::super::{Theme, Verbosity};
    use super::*;
    use crate::output::strip_ansi;
    use serial_test::serial;

    fn build() -> (Arc<Renderer>, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        (
            Arc::new(Renderer::new(Theme::default(), Verbosity::Normal)),
            buf,
        )
    }

    fn sink_for(buf: &Arc<Mutex<String>>) -> Arc<dyn Writer> {
        Arc::new(StringSink(buf.clone()))
    }

    #[test]
    fn unbound_builder_commits_immediately_on_drop() {
        let (r, buf) = build();
        let sink = sink_for(&buf);
        StatusBuilder::new(r, sink, 0, Role::Ok, "done"); // drops here
        let s = strip_ansi(&buf.lock().unwrap());
        assert!(s.contains("✓ done"), "got: {s:?}");
    }

    #[test]
    fn chained_detail_and_duration_render() {
        let (r, buf) = build();
        let sink = sink_for(&buf);
        let b = StatusBuilder::new(r, sink, 0, Role::Fail, "/tmp/foo")
            .detail("permission denied")
            .duration(std::time::Duration::from_millis(2500));
        drop(b);
        let s = strip_ansi(&buf.lock().unwrap());
        assert!(s.contains("✗ /tmp/foo — permission denied"), "got: {s:?}");
        assert!(s.contains("(2.5s)"), "got: {s:?}");
    }

    /// API-contract test for `StatusBuilder::label`. The label is appended at
    /// the END of the subject (auto-prefixed by a space), so the inner SGR
    /// reset closing the label's color cannot be followed by any further
    /// outer-role-styled text. Visible composition: "<glyph> <subject> <label>".
    #[test]
    #[serial]
    fn label_appends_at_end_of_subject() {
        let _restore_no_color = std::env::var("NO_COLOR").ok();
        unsafe {
            std::env::remove_var("NO_COLOR");
        }
        let was_enabled = console::colors_enabled();
        console::set_colors_enabled(true);

        let (r, buf) = build();
        let sink = sink_for(&buf);
        let b = StatusBuilder::new(r, sink, 0, Role::Warn, "subject text")
            .label(Role::Secondary, "[meta]");
        drop(b);
        let raw = buf.lock().unwrap().clone();
        let s = strip_ansi(&raw);
        assert!(
            s.contains("⚠ subject text [meta]"),
            "visible composition wrong; got: {s:?}"
        );

        // Contract: the inner reset (\x1b[0m) introduced by the label's styled
        // segment must NOT be followed by another role-styled run before the
        // end of the line. Specifically, after the last \x1b[0m on the status
        // line, only whitespace or line-terminator may follow on the subject
        // portion (the renderer may append its own trailing SGR sequences, but
        // they must close the line, not re-open a colored run for outer text).
        let line = raw.lines().find(|l| l.contains("subject text")).unwrap();
        let last_reset = line.rfind("\x1b[0m").expect("label adds a reset");
        let tail = &line[last_reset + "\x1b[0m".len()..];
        // Tail can only be: empty, whitespace, or further SGR resets — never
        // a styled run with role color codes for trailing visible content.
        // Strip ANSI from the tail; what remains must be visible whitespace
        // only (no payload chars). The label is the last visible payload.
        let tail_visible = strip_ansi(tail);
        assert!(
            tail_visible.trim().is_empty(),
            "no visible content may follow the label's inner reset; tail_visible={tail_visible:?}, line={line:?}"
        );

        console::set_colors_enabled(was_enabled);
        unsafe {
            if let Some(v) = _restore_no_color {
                std::env::set_var("NO_COLOR", v);
            }
        }
    }

    /// Foreign ANSI carried in a caller-supplied subject (e.g. a captured
    /// error formatted via `format!("sync failed for {url}: {e}")`) must be
    /// stripped at the renderer boundary, so a stray `\x1b[0m` mid-subject
    /// cannot prematurely terminate the role styling and foreign color
    /// escapes cannot paint trailing characters.
    #[cfg(feature = "test-helpers")]
    #[test]
    #[serial]
    fn subject_strips_foreign_ansi_before_role_styling() {
        use crate::output::Printer;

        let (p, cap) = Printer::for_test_doc();
        p.status(Role::Fail, "subject \x1b[31mforeign red\x1b[0m text")
            .detail("plain detail");
        p.flush();
        let raw = cap.human();
        assert!(
            !raw.contains("\x1b[31m"),
            "foreign red SGR must be stripped from subject; raw={raw:?}"
        );
        let visible = strip_ansi(&raw);
        assert!(
            visible.contains("subject foreign red text"),
            "got: {visible:?}"
        );
    }

    /// Mirror of the streaming-path test for the buffered `Doc` path through
    /// `render_doc::render_component` (Status arm). Both call sites compose
    /// the subject via the shared `finalize_subject` helper so the byte
    /// shape must match.
    #[cfg(feature = "test-helpers")]
    #[test]
    #[serial]
    fn doc_subject_strips_foreign_ansi_before_role_styling() {
        use crate::output::{Doc, Printer};

        let (p, cap) = Printer::for_test_doc();
        let doc = Doc::new().status(Role::Fail, "subject with \x1b[31mfoo\x1b[0m");
        p.emit(doc);
        p.flush();
        let raw = cap.human();
        assert!(
            !raw.contains("\x1b[31m"),
            "foreign red SGR must be stripped from Doc subject; raw={raw:?}"
        );
        let visible = strip_ansi(&raw);
        assert!(visible.contains("subject with foo"), "got: {visible:?}");
    }
}

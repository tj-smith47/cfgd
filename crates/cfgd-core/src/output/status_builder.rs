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
use super::renderer::{Renderer, StatusFields, Writer};

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
    pub(crate) label: Option<(Role, String)>,
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
        self.label = Some((role, text.into()));
        self
    }
}

impl Drop for StatusBuilder<'_> {
    fn drop(&mut self) {
        if let Some((label_role, label_text)) = &self.label {
            let (_, style) = super::renderer::role_glyph(&self.renderer.theme, *label_role);
            let styled = style.apply_to(label_text).to_string();
            self.subject.push(' ');
            self.subject.push_str(&styled);
        }
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
    use crate::output::tests::strip_ansi;

    fn build(role: Role) -> (Arc<Renderer>, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let _ = role; // role used by caller
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
        let (r, buf) = build(Role::Ok);
        let sink = sink_for(&buf);
        StatusBuilder {
            renderer: r,
            sink,
            depth: 0,
            role: Role::Ok,
            subject: "done".into(),
            detail: None,
            duration: None,
            target: None,
            label: None,
            _phantom: std::marker::PhantomData,
        }; // drops here
        let s = strip_ansi(&buf.lock().unwrap());
        assert!(s.contains("✓ done"), "got: {s:?}");
    }

    #[test]
    fn chained_detail_and_duration_render() {
        let (r, buf) = build(Role::Fail);
        let sink = sink_for(&buf);
        let b = StatusBuilder {
            renderer: r,
            sink,
            depth: 0,
            role: Role::Fail,
            subject: "/tmp/foo".into(),
            detail: None,
            duration: None,
            target: None,
            label: None,
            _phantom: std::marker::PhantomData,
        }
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
    fn label_appends_at_end_of_subject() {
        let _restore_no_color = std::env::var("NO_COLOR").ok();
        // SAFETY: single-threaded test guarded by serial_test? No — but the
        // test reads its own buffer; collateral damage to other tests would
        // only suppress styling, which doesn't affect strip_ansi assertions.
        unsafe {
            std::env::remove_var("NO_COLOR");
        }
        let was_enabled = console::colors_enabled();
        console::set_colors_enabled(true);

        let (r, buf) = build(Role::Warn);
        let sink = sink_for(&buf);
        let b = StatusBuilder {
            renderer: r,
            sink,
            depth: 0,
            role: Role::Warn,
            subject: "subject text".into(),
            detail: None,
            duration: None,
            target: None,
            label: None,
            _phantom: std::marker::PhantomData,
        }
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
}

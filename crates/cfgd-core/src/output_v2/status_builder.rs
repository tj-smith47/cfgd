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
}

impl Drop for StatusBuilder<'_> {
    fn drop(&mut self) {
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
    use crate::output_v2::tests::strip_ansi;

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
            _phantom: std::marker::PhantomData,
        }
        .detail("permission denied")
        .duration(std::time::Duration::from_millis(2500));
        drop(b);
        let s = strip_ansi(&buf.lock().unwrap());
        assert!(s.contains("✗ /tmp/foo — permission denied"), "got: {s:?}");
        assert!(s.contains("(2.5s)"), "got: {s:?}");
    }
}

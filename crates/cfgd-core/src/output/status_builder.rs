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
use super::theme::ThemedStyle;

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
    pub(crate) marker: Option<StatusLabel>,
    pub(crate) subject_style: Option<ThemedStyle>,
    pub(crate) detail_style: Option<ThemedStyle>,
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
            marker: None,
            subject_style: None,
            detail_style: None,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Paint the SUBJECT with `style`. Crate-private: the theme is `output/`'s
    /// to own, so the only caller is `SectionGuard::action_status`.
    pub(crate) fn with_subject_style(mut self, style: Option<ThemedStyle>) -> Self {
        self.subject_style = style;
        self
    }

    pub fn detail(mut self, text: impl Into<String>) -> Self {
        self.detail = Some(text.into());
        self
    }

    pub fn detail_opt(mut self, text: Option<&str>) -> Self {
        self.detail = text.map(|s| s.to_string());
        self
    }

    /// A detail that is trailing METADATA: rendered `theme.muted`. The plain
    /// `detail` / `detail_opt` pair stays the right form for an error and for
    /// tool output, which the reader has to act on rather than skim past.
    pub fn detail_muted(mut self, text: impl Into<String>) -> Self {
        self.detail = Some(text.into());
        self.detail_style = Some(self.renderer.theme.muted.clone());
        self
    }

    pub fn detail_muted_opt(mut self, text: Option<&str>) -> Self {
        self.detail = text.map(|s| s.to_string());
        self.detail_style = self
            .detail
            .is_some()
            .then(|| self.renderer.theme.muted.clone());
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
    /// A leading styled marker naming the hook a script body belongs to
    /// (`postApply`), rendered as `postApply: <body>`. The colon is the
    /// caller's; the role is not — it is always `Role::Accent`, because which
    /// slot a marker paints in is a theme mapping rather than a per-call-site
    /// choice.
    pub fn marker(mut self, text: impl Into<String>) -> Self {
        self.marker = Some(StatusLabel {
            role: Role::Accent,
            text: text.into(),
        });
        self
    }

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
        self.subject = finalize_subject(
            &self.renderer.theme,
            &self.subject,
            self.marker.as_ref(),
            self.label.as_ref(),
        );
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
                subject_style: self.subject_style.clone(),
                detail_style: self.detail_style.clone(),
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

    /// `build`'s styled sibling, for the tests whose subject IS the escapes a
    /// role emits. A renderer's theme carries its own colour decision, so a
    /// test that asserts on SGR placement asks for one here rather than hoping
    /// the terminal the suite was invoked from supplied it.
    fn build_colored() -> (Arc<Renderer>, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        (
            Arc::new(Renderer::new(
                Theme::default().with_colors(true),
                Verbosity::Normal,
            )),
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
        let s = crate::test_helpers::captured_text(&buf);
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
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("✗ /tmp/foo — permission denied"), "got: {s:?}");
        assert!(s.contains("(2.5s)"), "got: {s:?}");
    }

    /// API-contract test for `StatusBuilder::label`. The label is appended at
    /// the END of the subject (auto-prefixed by a space), so the inner SGR
    /// reset closing the label's color cannot be followed by any further
    /// outer-role-styled text. Visible composition: "<glyph> <subject> <label>".
    #[test]
    fn label_appends_at_end_of_subject() {
        let (r, buf) = build_colored();
        let sink = sink_for(&buf);
        let b = StatusBuilder::new(r, sink, 0, Role::Warn, "subject text")
            .label(Role::Secondary, "[meta]");
        drop(b);
        // raw-capture-ok: the reset-boundary contract below is checked against the raw SGR bytes — captured_text would strip the escapes this test exists to check
        let raw = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
    }

    /// Foreign ANSI carried in a caller-supplied subject (e.g. a captured
    /// error formatted via `format!("sync failed for {url}: {e}")`) must be
    /// stripped at the renderer boundary, so a stray `\x1b[0m` mid-subject
    /// cannot prematurely terminate the role styling and foreign color
    /// escapes cannot paint trailing characters.
    #[cfg(feature = "test-helpers")]
    #[test]
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

    /// `detail_style` paints the detail slot and nothing else. `None` — every
    /// existing call site — must emit a detail carrying no SGR of its own, or
    /// the seam would silently restyle output it was added beside.
    /// Serial because dracula's slots carry an RGB triple, so every render
    /// below asks `supports_truecolor()` — which reads `COLORTERM` /
    /// `NO_COLOR` — and the expected detail is rendered separately from the
    /// two captures it is compared against.
    #[test]
    #[serial]
    fn detail_style_paints_only_the_detail_slot() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        let muted_detail = theme.muted.apply_to("unchanged").to_string();

        let render = |muted: bool| {
            let buf = Arc::new(Mutex::new(String::new()));
            let r = Arc::new(Renderer::new(
                Theme::from_preset("dracula").with_colors(true),
                Verbosity::Normal,
            ));
            let b = StatusBuilder::new(r, sink_for(&buf), 0, Role::Skipped, "nvim");
            drop(if muted {
                b.detail_muted("unchanged")
            } else {
                b.detail("unchanged")
            });
            // raw-capture-ok: the split-half byte-identity check below compares raw SGR runs — captured_text would strip the escapes this test exists to check
            buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
        };

        let plain = render(false);
        let muted = render(true);

        // Same visible line either way.
        assert_eq!(strip_ansi(&plain), strip_ansi(&muted));
        // The subject half is byte-identical: the seam reaches the detail only.
        let split = |s: &str| {
            s.split_once(" — ")
                .map(|(head, tail)| (head.to_string(), tail.to_string()))
                .expect("detail separator missing")
        };
        let (plain_head, plain_tail) = split(&plain);
        let (muted_head, muted_tail) = split(&muted);
        assert_eq!(plain_head, muted_head, "the subject slot was restyled");
        assert_eq!(muted_tail.trim_end(), muted_detail);
        assert!(
            !plain_tail.contains('\x1b'),
            "an unstyled detail must carry no SGR: {plain_tail:?}"
        );
    }
}

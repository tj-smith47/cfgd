use std::path::Path;
use std::time::Duration;

use super::{Renderer, Writer, role_glyph};
use crate::output::{Role, Verbosity};

/// Inputs to a single Status line. Builders convert to this for rendering.
pub struct StatusFields<'a> {
    pub role: Role,
    pub subject: &'a str,
    pub detail: Option<&'a str>,
    pub duration: Option<Duration>,
    pub target: Option<&'a Path>,
}

impl Renderer {
    /// Top-level status dispatcher. Routes to the topmost open section's
    /// pending-statuses buffer when one exists (so subjects can be
    /// right-padded to a common column at section close — spec §13.3/§13.4);
    /// otherwise writes immediately.
    pub fn render_status(&self, w: &dyn Writer, depth: usize, f: &StatusFields<'_>) {
        // Status(Fail) is shown even at Quiet — see spec §12.
        if self.verbosity == Verbosity::Quiet && f.role != Role::Fail {
            return;
        }
        // Buffer when a section is open AND this status's depth is inside
        // (not equal to) the section's header_depth. The depth==header_depth
        // case happens for re-routed top-level emits via `enforce_top_level_emit`;
        // those should render immediately so the warning shape stays inline.
        let buffered = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut did_buffer = false;
            if let Some(top) = s.section_stack.last_mut()
                && depth > top.header_depth
            {
                // Inside the section's child region — buffer.
                top.pending_statuses.push(super::section::BufferedStatus {
                    role: f.role,
                    subject: f.subject.to_string(),
                    detail: f.detail.map(|d| d.to_string()),
                    duration: f.duration,
                    target: f.target.map(|p| p.to_path_buf()),
                    depth,
                });
                did_buffer = true;
            }
            did_buffer
        };
        if buffered {
            // Header emission must still happen so the section's header
            // appears before any of its children. This is idempotent — only
            // the first call writes anything.
            self.flush_pending_section_headers(w);
            return;
        }
        self.render_status_immediate(w, depth, f);
        self.mark_top_level_blank_if_at_root();
    }

    /// Actually emit a Status line, without buffering. Used by the immediate
    /// path AND by `flush_pending_statuses` when a section closes.
    pub(crate) fn render_status_immediate(
        &self,
        w: &dyn Writer,
        depth: usize,
        f: &StatusFields<'_>,
    ) {
        if self.verbosity == Verbosity::Quiet && f.role != Role::Fail {
            return;
        }
        self.flush_pending_section_headers(w);

        let (icon_opt, style) = role_glyph(&self.theme, f.role);
        let mut line = String::new();
        if let Some(icon) = icon_opt {
            line.push_str(&style.apply_to(icon).to_string());
            line.push(' ');
        }
        line.push_str(&style.apply_to(f.subject).to_string());

        // Field order per spec §13.2: subject — detail (target). Detail comes
        // first (with em-dash glue), then target in parens. Duration trails
        // last as its own (Ns) parens block.
        if let Some(detail) = f.detail {
            line.push_str(" — ");
            line.push_str(detail);
        }
        if let Some(target) = f.target {
            let dim = self
                .theme
                .muted
                .apply_to(format!(" ({})", target.display()));
            line.push_str(&dim.to_string());
        }
        if let Some(d) = f.duration {
            let secs = d.as_secs_f64();
            let dim = self.theme.muted.apply_to(format!(" ({:.1}s)", secs));
            line.push_str(&dim.to_string());
        }
        self.write_line(w, depth, &line);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::StringSink;
    use super::*;
    use crate::output::Theme;
    use crate::output::tests::strip_ansi;

    fn capture() -> (Renderer, StringSink, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        (r, sink, buf)
    }

    #[test]
    fn ok_status_renders_check_glyph() {
        let (r, sink, buf) = capture();
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Ok,
                subject: "done",
                detail: None,
                duration: None,
                target: None,
            },
        );
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("✓ done"), "got: {out:?}");
    }

    #[test]
    fn info_role_has_no_icon() {
        let (r, sink, buf) = capture();
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Info,
                subject: "note",
                detail: None,
                duration: None,
                target: None,
            },
        );
        let out = strip_ansi(&buf.lock().unwrap());
        assert_eq!(out.trim_end(), "note");
    }

    #[test]
    fn detail_appended_with_em_dash() {
        let (r, sink, buf) = capture();
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Fail,
                subject: "/tmp/foo",
                detail: Some("permission denied"),
                duration: None,
                target: None,
            },
        );
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(
            out.contains("✗ /tmp/foo — permission denied"),
            "got: {out:?}"
        );
    }

    #[test]
    fn duration_trailed_in_parens() {
        let (r, sink, buf) = capture();
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Ok,
                subject: "done",
                detail: None,
                duration: Some(std::time::Duration::from_millis(1234)),
                target: None,
            },
        );
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("(1.2s)"), "got: {out:?}");
    }

    #[test]
    fn fail_shown_even_at_quiet() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Fail,
                subject: "boom",
                detail: None,
                duration: None,
                target: None,
            },
        );
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(
            out.contains("boom"),
            "Fail must render at Quiet; got: {out:?}"
        );
    }

    #[test]
    fn ok_suppressed_at_quiet() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Ok,
                subject: "done",
                detail: None,
                duration: None,
                target: None,
            },
        );
        assert!(buf.lock().unwrap().is_empty());
    }
}

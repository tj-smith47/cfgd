use std::path::Path;
use std::time::Duration;

use super::{Renderer, Writer, role_glyph};
use crate::output_v2::{Role, Verbosity};

/// Inputs to a single Status line. Builders convert to this for rendering.
pub struct StatusFields<'a> {
    pub role: Role,
    pub subject: &'a str,
    pub detail: Option<&'a str>,
    pub duration: Option<Duration>,
    pub target: Option<&'a Path>,
}

impl Renderer {
    pub fn render_status(&self, w: &dyn Writer, depth: usize, f: &StatusFields<'_>) {
        // Status(Fail) is shown even at Quiet — see spec §12.
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

        if let Some(target) = f.target {
            let dim = self
                .theme
                .muted
                .apply_to(format!(" ({})", target.display()));
            line.push_str(&dim.to_string());
        }
        if let Some(detail) = f.detail {
            line.push_str(" — ");
            line.push_str(detail);
        }
        if let Some(d) = f.duration {
            let secs = d.as_secs_f64();
            let dim = self.theme.muted.apply_to(format!(" ({:.1}s)", secs));
            line.push_str(&dim.to_string());
        }
        self.write_line(w, depth, &line);
        self.mark_top_level_blank_if_at_root();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::StringSink;
    use super::*;
    use crate::output_v2::Theme;
    use crate::output_v2::tests::strip_ansi;

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

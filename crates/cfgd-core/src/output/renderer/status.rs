use std::path::Path;
use std::time::Duration;

use super::{Renderer, Writer, role_glyph};
use crate::PathDisplayExt;
use crate::output::theme::ThemedStyle;
use crate::output::{Role, Verbosity, strip_ansi};

/// Inputs to a single Status line. Builders convert to this for rendering.
pub struct StatusFields<'a> {
    pub role: Role,
    pub subject: &'a str,
    pub detail: Option<&'a str>,
    pub duration: Option<Duration>,
    pub target: Option<&'a Path>,
    /// Style for the SUBJECT only; the glyph always keeps the role's style.
    /// `None` = the subject is painted with the role style.
    pub subject_style: Option<ThemedStyle>,
    /// Style for the DETAIL only. `None` = the detail is written unstyled, in
    /// the terminal's default foreground.
    pub detail_style: Option<ThemedStyle>,
}

/// Where a status emission lands: into the innermost section's close-time
/// buffer, straight out against a pre-computed column, or straight out at the
/// caller's depth.
enum StatusRoute {
    Buffered,
    Live(usize),
    Immediate,
}

/// True when a status carries content after its subject, which is the only
/// case either alignment path pads for. THE rule: `StatusFields` and
/// `BufferedStatus` both answer through this, so the live column and the
/// buffered close can never pad different sets of lines.
pub(crate) fn has_trailing(
    detail: Option<&str>,
    duration: Option<Duration>,
    target: Option<&Path>,
) -> bool {
    detail.is_some() || duration.is_some() || target.is_some()
}

impl StatusFields<'_> {
    pub(crate) fn has_trailing(&self) -> bool {
        has_trailing(self.detail, self.duration, self.target)
    }
}

/// Right-pad `subject` to `width`, or `None` when this status is one the
/// buffered path would have left alone. Shared by both alignment paths so a
/// live column pads exactly the lines a section close would have.
pub(crate) fn pad_subject(subject: &str, width: usize, has_trailing: bool) -> Option<String> {
    if !has_trailing || width == 0 {
        return None;
    }
    let cur = console::measure_text_width(subject);
    (cur < width).then(|| format!("{}{}", subject, " ".repeat(width - cur)))
}

impl Renderer {
    /// Top-level status dispatcher. Routes to the topmost open section's
    /// pending-statuses buffer when one exists (so subjects can be
    /// right-padded to a common column at section close); otherwise writes
    /// immediately.
    pub fn render_status(&self, w: &dyn Writer, depth: usize, f: &StatusFields<'_>) {
        // Status(Fail) is shown even at Quiet.
        if self.verbosity == Verbosity::Quiet && f.role != Role::Fail {
            return;
        }
        // Buffer when a section is open AND this status's depth is inside
        // (not equal to) the section's header_depth. The depth==header_depth
        // case happens for re-routed top-level emits via
        // `enforce_structural_top_level`; those should render immediately so
        // the warning shape stays inline.
        let route = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            match s.section_stack.last_mut() {
                Some(top) if depth > top.header_depth => match top.live_column {
                    // A live frame renders now and pads against a column that
                    // was computed before the run, since there is no close to
                    // buffer until.
                    Some(width) => StatusRoute::Live(width),
                    None => {
                        top.pending_statuses.push(super::section::BufferedStatus {
                            role: f.role,
                            subject: f.subject.to_string(),
                            detail: f.detail.map(|d| d.to_string()),
                            duration: f.duration,
                            target: f.target.map(|p| p.to_path_buf()),
                            depth,
                            subject_style: f.subject_style.clone(),
                            detail_style: f.detail_style.clone(),
                        });
                        StatusRoute::Buffered
                    }
                },
                _ => StatusRoute::Immediate,
            }
        };
        match route {
            StatusRoute::Buffered => {
                // Header emission must still happen so the section's header
                // appears before any of its children. This is idempotent —
                // only the first call writes anything.
                self.flush_pending_section_headers(w);
            }
            StatusRoute::Live(width) => {
                let padded = pad_subject(f.subject, width, f.has_trailing());
                match padded {
                    Some(subject) => self.render_status_immediate(
                        w,
                        depth,
                        &StatusFields {
                            role: f.role,
                            subject: &subject,
                            detail: f.detail,
                            duration: f.duration,
                            target: f.target,
                            subject_style: f.subject_style.clone(),
                            detail_style: f.detail_style.clone(),
                        },
                    ),
                    None => self.render_status_immediate(w, depth, f),
                }
            }
            StatusRoute::Immediate => {
                // The group bookkeeping runs inside the SAME acquisition as
                // the lines: a concurrent emission landing between
                // `open_top_group` and the block would take this status's
                // blank-line decision with it.
                let (line, detail_tail) = self.compose_status(f);
                self.emit_with(w, |e| {
                    e.open_top_group(super::TopGroup::Status);
                    e.flush_section_headers();
                    e.push_line(depth, &line);
                    for tail in &detail_tail {
                        e.push_line(depth + 1, tail);
                    }
                    e.mark_top_level_group(super::TopGroup::Status);
                });
            }
        }
    }

    /// Emit a Status line with no group bookkeeping: the live-column route,
    /// and `flush_pending_statuses` when a section closes. Both are inside a
    /// section, where `open_top_group` / `mark_top_level_group` are no-ops.
    pub(crate) fn render_status_immediate(
        &self,
        w: &dyn Writer,
        depth: usize,
        f: &StatusFields<'_>,
    ) {
        if self.verbosity == Verbosity::Quiet && f.role != Role::Fail {
            return;
        }
        let (line, detail_tail) = self.compose_status(f);
        self.emit_with(w, |e| {
            e.flush_section_headers();
            e.push_line(depth, &line);
            // Continuation lines indent one level past the subject so they
            // read as belonging to this status rather than as new siblings.
            for tail in &detail_tail {
                e.push_line(depth + 1, tail);
            }
        });
    }

    /// Build a status line and its continuation tails. Reads the theme only,
    /// so both emission routes compose the same bytes and neither needs the
    /// state lock to do it.
    fn compose_status(&self, f: &StatusFields<'_>) -> (String, Vec<String>) {
        let (icon_opt, style) = role_glyph(&self.theme, f.role);
        let mut line = String::new();
        if let Some(icon) = icon_opt {
            line.push_str(&style.apply_to(icon).to_string());
            line.push(' ');
        }
        // The glyph keeps the role's style whatever the subject takes.
        let subject_style = f.subject_style.as_ref().unwrap_or(&style);
        line.push_str(&subject_style.apply_to(f.subject).to_string());

        // Detail may carry multi-line external tool stderr (e.g. a failed
        // `cargo install` dumps a whole error chain). Pre-split it like
        // render_note: the first physical line glues to the subject with the
        // em-dash; any further lines render as indented continuation lines at
        // the same depth. write_line forbids embedded newlines, so passing an
        // unsplit multi-line detail would panic in debug builds.
        let mut detail_tail: Vec<String> = Vec::new();
        if let Some(detail) = f.detail {
            // Sanitize at the renderer boundary: detail may carry embedded ANSI
            // escapes. A stray `\x1b[0m` would prematurely terminate the role
            // styling above; foreign color escapes would paint subsequent
            // terminal output until the next reset.
            let clean = strip_ansi(detail);
            let mut lines = clean.lines();
            line.push_str(" — ");
            // Continuation lines take the same style, so a wrapped detail
            // cannot change colour halfway down.
            let paint = |text: &str| match &f.detail_style {
                Some(style) => style.apply_to(text).to_string(),
                None => text.to_string(),
            };
            if let Some(first) = lines.next() {
                line.push_str(&paint(first));
            }
            detail_tail.extend(lines.map(paint));
        }
        if let Some(target) = f.target {
            let dim = self.theme.muted.apply_to(format!(" ({})", target.posix()));
            line.push_str(&dim.to_string());
        }
        if let Some(d) = f.duration {
            let secs = d.as_secs_f64();
            let dim = self.theme.muted.apply_to(format!(" ({:.1}s)", secs));
            line.push_str(&dim.to_string());
        }
        (line, detail_tail)
    }

    /// Emit a Warn-styled diagnostic line that is shown regardless of verbosity
    /// — including under structured output, where the Printer is forced to
    /// `Verbosity::Quiet` and `render_status` would drop every non-`Fail` role.
    /// Reserved for intentional always-visible diagnostics (deprecation
    /// notices) that belong on stderr but must never reach the stdout data
    /// channel. `depth` comes from the caller's `enforce_structural_top_level(0)`
    /// (0 in normal use); subject must not contain `\n`.
    pub fn render_deprecation(&self, w: &dyn Writer, depth: usize, subject: &str) {
        let (icon_opt, style) = role_glyph(&self.theme, Role::Warn);
        let mut line = String::new();
        if let Some(icon) = icon_opt {
            line.push_str(&style.apply_to(icon).to_string());
            line.push(' ');
        }
        line.push_str(&style.apply_to(strip_ansi(subject)).to_string());
        self.write_line(w, depth, &line);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::StringSink;
    use super::*;
    use crate::output::Theme;
    use crate::output::strip_ansi;

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
                subject_style: None,
                detail_style: None,
            },
        );
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(out.contains("✓ done"), "got: {out:?}");
    }

    #[test]
    fn info_role_uses_its_theme_icon() {
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
                subject_style: None,
                detail_style: None,
            },
        );
        let out = strip_ansi(&buf.lock().unwrap());
        assert_eq!(out.trim_end(), "⊙ note");
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
                subject_style: None,
                detail_style: None,
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
                subject_style: None,
                detail_style: None,
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
                subject_style: None,
                detail_style: None,
            },
        );
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(
            out.contains("boom"),
            "Fail must render at Quiet; got: {out:?}"
        );
    }

    #[test]
    fn deprecation_shown_even_at_quiet() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_deprecation(&sink, 0, "--jsonpath is deprecated");
        let out = strip_ansi(&buf.lock().unwrap());
        assert!(
            out.contains("--jsonpath is deprecated"),
            "deprecation must render at Quiet; got: {out:?}"
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
                subject_style: None,
                detail_style: None,
            },
        );
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn multiline_detail_renders_without_panic_and_splits_lines() {
        // A failed subprocess (e.g. `cargo install`) yields a multi-line error
        // chain in `detail`. The renderer must pre-split it instead of handing
        // an embedded newline to write_line (which debug_asserts against it).
        let (r, sink, buf) = capture();
        let detail = "cargo install failed: exit code 101: \
                      \x1b[31merror\x1b[0m: download of windows-sys failed\n\
                      Caused by:\n  curl failed\n  [16] HTTP2 framing layer";
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Fail,
                subject: "[1/1] Failed: package:cargo:install:bat",
                detail: Some(detail),
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        let out = strip_ansi(&buf.lock().unwrap());
        // First physical line glues subject to the first detail line.
        assert!(
            out.lines().next().unwrap().contains(
                "✗ [1/1] Failed: package:cargo:install:bat — cargo install failed: exit code 101: error: download of windows-sys failed"
            ),
            "first line must glue subject + first detail line; got: {out:?}"
        );
        // Continuation lines render as indented siblings, never collapsed.
        assert!(out.contains("Caused by:"), "got: {out:?}");
        assert!(out.contains("curl failed"), "got: {out:?}");
        assert!(out.contains("[16] HTTP2 framing layer"), "got: {out:?}");
        // No embedded newline escaped into a single sink write (the panic case):
        // the body is split into >= 4 physical lines.
        assert!(
            out.lines().filter(|l| !l.trim().is_empty()).count() >= 4,
            "multi-line detail must produce multiple physical lines; got: {out:?}"
        );
    }

    #[test]
    fn detail_strips_ansi_to_prevent_terminal_paint() {
        let (r, sink, buf) = capture();
        let detail = "upstream: \x1b[31mred\x1b[0m text \x1b[1mbold\x1b[0m";
        r.render_status(
            &sink,
            0,
            &StatusFields {
                role: Role::Fail,
                subject: "sync failed",
                detail: Some(detail),
                duration: None,
                target: None,
                subject_style: None,
                detail_style: None,
            },
        );
        let raw = buf.lock().unwrap().clone();
        let visible = strip_ansi(&raw);
        assert!(
            visible.contains("sync failed — upstream: red text bold"),
            "visible composition mismatch; got: {visible:?}"
        );
        // Scope the escape check to the segment the detail was pushed into. The
        // renderer's own styling of the glyph and subject is process-global
        // state — whether it emits colour, bold, or nothing at all depends on
        // terminal detection and on whatever else in the binary last touched
        // the colour flags — so asserting over the whole buffer measures that
        // instead of the sanitization. After the em-dash there is only the
        // detail, and it must carry no escape at all: a surviving `\x1b[0m`
        // would close the subject styling early, and a colour code would paint
        // every later line the terminal prints.
        let detail_segment = raw.rsplit(" — ").next().unwrap_or("");
        assert_eq!(
            detail_segment.trim_end_matches('\n'),
            "upstream: red text bold",
            "detail must reach the buffer fully sanitized; got raw: {raw:?}"
        );
        assert!(
            !detail_segment.contains('\u{1b}'),
            "detail segment must contain no ANSI escapes; got: {detail_segment:?}"
        );
    }
}

use crate::output::component::StatusLabel;
use crate::output::theme::ThemedStyle;
use crate::output::{Role, Theme, strip_ansi};

/// Compose a `subject` with a trailing styled `label`, separated by one ASCII
/// space. The label always lands at end-of-subject so the inner SGR reset
/// closing the label's color cannot be followed by outer-role-styled text —
/// the only safe nesting shape for the streaming renderer. Single source of
/// truth shared by `StatusBuilder::drop` (streaming) and `render_doc`
/// (buffered Doc tree) so the two paths stay byte-identical.
fn compose_subject_with_label(theme: &Theme, subject: &str, label: &StatusLabel) -> String {
    let (_, style) = role_glyph(theme, label.role);
    let styled = style.apply_to(&label.text).to_string();
    format!("{subject} {styled}")
}

/// Compose a `subject` behind a leading styled `marker` (`postApply:`),
/// separated by one ASCII space.
///
/// A prefix rather than the trailing-label shape, because the marker names the
/// hook the body belongs to and is read first. The inner SGR reset that closes
/// it leaves the body in the terminal's own foreground — the marker is styled
/// and the body is not, which is the whole of the mapping.
fn compose_subject_with_marker(theme: &Theme, subject: &str, marker: &StatusLabel) -> String {
    let (_, style) = role_glyph(theme, marker.role);
    let styled = style.apply_to(&marker.text).to_string();
    format!("{styled} {subject}")
}

/// Sanitize a caller-supplied status subject and optionally wrap it in the
/// renderer-owned styled segments: a leading `marker` and a trailing `label`.
/// The subject may carry foreign ANSI from a captured error string
/// (`format!("sync failed for {url}: {e}")`); a stray `\x1b[0m` would
/// prematurely close the role styling at the inner reset, and foreign color
/// escapes would paint trailing characters until the next reset. Strip ANSI
/// from the subject FIRST, then add the legitimate (renderer-controlled)
/// segments so they survive sanitation.
pub(crate) fn finalize_subject(
    theme: &Theme,
    subject: &str,
    marker: Option<&StatusLabel>,
    label: Option<&StatusLabel>,
) -> String {
    let sanitized = strip_ansi(subject);
    let labelled = match label {
        Some(lbl) => compose_subject_with_label(theme, &sanitized, lbl),
        None => sanitized,
    };
    match marker {
        Some(mk) => compose_subject_with_marker(theme, &labelled, mk),
        None => labelled,
    }
}

/// Look up the icon glyph + style for a Role.
pub(crate) fn role_glyph(theme: &Theme, role: Role) -> (Option<&str>, ThemedStyle) {
    match role {
        Role::Ok => (Some(theme.icon_ok.as_str()), theme.success.clone()),
        Role::Warn => (Some(theme.icon_warn.as_str()), theme.warning.clone()),
        Role::Fail => (Some(theme.icon_fail.as_str()), theme.error.clone()),
        Role::Pending => (Some(theme.icon_pending.as_str()), theme.muted.clone()),
        Role::Running => (Some(theme.icon_running.as_str()), theme.running.clone()),
        Role::Skipped => (Some(theme.icon_skipped.as_str()), theme.muted.clone()),
        Role::Info => (Some(theme.icon_info.as_str()), theme.info.clone()),
        // Accent + Secondary intentionally claim no icon — they style the text
        // payload inline, they don't occupy the status-line glyph column that
        // every other role reserves.
        Role::Accent => (None, theme.accent.clone()),
        Role::Secondary => (None, theme.secondary.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ThemedStyle` has no `PartialEq`; its `Debug` carries the color triple
    /// and the attribute set, which is the whole of its rendering payload.
    fn style_repr(style: &ThemedStyle) -> String {
        format!("{style:?}")
    }

    #[test]
    fn info_uses_its_theme_icon_and_style() {
        let t = Theme::default();
        let (icon, style) = role_glyph(&t, Role::Info);
        assert_eq!(icon, Some("⊙"));
        assert_eq!(style_repr(&style), style_repr(&t.info));
        assert_ne!(style_repr(&style), style_repr(&ThemedStyle::plain()));
        // The ASCII preset downgrades it like every other glyph.
        assert_eq!(
            role_glyph(&Theme::from_preset("minimal"), Role::Info).0,
            Some("i")
        );
    }

    /// A role that renders with a style it did not take from the active theme
    /// is invisible to every theme preset and to `ThemeOverrides` — that is how
    /// the npm global-prefix notice shipped as bare unstyled white text while
    /// still passing the "all output routes through Printer" gate. Checked
    /// against each preset so a slot mixup can't hide behind two themes that
    /// happen to color two roles alike.
    #[test]
    fn every_role_renders_with_its_own_theme_slot() {
        for preset in [
            "default",
            "dracula",
            "solarized-dark",
            "solarized-light",
            "minimal",
        ] {
            let t = Theme::from_preset(preset);
            for role in [
                Role::Ok,
                Role::Warn,
                Role::Fail,
                Role::Pending,
                Role::Running,
                Role::Skipped,
                Role::Info,
                Role::Accent,
                Role::Secondary,
            ] {
                // Exhaustive by construction: a new `Role` fails to compile here
                // until its theme slot is named.
                let expected = match role {
                    Role::Ok => &t.success,
                    Role::Warn => &t.warning,
                    Role::Fail => &t.error,
                    Role::Pending | Role::Skipped => &t.muted,
                    Role::Running => &t.running,
                    Role::Info => &t.info,
                    Role::Accent => &t.accent,
                    Role::Secondary => &t.secondary,
                };
                let (_, style) = role_glyph(&t, role);
                assert_eq!(
                    style_repr(&style),
                    style_repr(expected),
                    "{role:?} does not use its {preset} theme slot"
                );
            }
        }
    }

    #[test]
    fn ok_uses_check_glyph() {
        let t = Theme::default();
        let (icon, _) = role_glyph(&t, Role::Ok);
        assert_eq!(icon, Some("✓"));
    }

    #[test]
    fn accent_and_secondary_have_no_icon() {
        let t = Theme::default();
        assert!(role_glyph(&t, Role::Accent).0.is_none());
        assert!(role_glyph(&t, Role::Secondary).0.is_none());
    }
}

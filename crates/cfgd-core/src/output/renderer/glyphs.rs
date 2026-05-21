use crate::output::component::StatusLabel;
use crate::output::theme::ThemedStyle;
use crate::output::{Role, Theme, strip_ansi};

/// Compose a `subject` with a trailing styled `label`, separated by one ASCII
/// space. The label always lands at end-of-subject so the inner SGR reset
/// closing the label's color cannot be followed by outer-role-styled text —
/// the only safe nesting shape for the streaming renderer. Single source of
/// truth shared by `StatusBuilder::drop` (streaming) and `render_doc`
/// (buffered Doc tree) so the two paths stay byte-identical.
pub(crate) fn compose_subject_with_label(
    theme: &Theme,
    subject: &str,
    label: &StatusLabel,
) -> String {
    let (_, style) = role_glyph(theme, label.role);
    let styled = style.apply_to(&label.text).to_string();
    format!("{subject} {styled}")
}

/// Sanitize a caller-supplied status subject and optionally append a styled
/// label. The subject may carry foreign ANSI from a captured error string
/// (`format!("sync failed for {url}: {e}")`); a stray `\x1b[0m` would
/// prematurely close the role styling at the inner reset, and foreign color
/// escapes would paint trailing characters until the next reset. Strip ANSI
/// from the subject FIRST, then append the legitimate (renderer-controlled)
/// label SGR so it survives sanitation.
pub(crate) fn finalize_subject(
    theme: &Theme,
    subject: &str,
    label: Option<&StatusLabel>,
) -> String {
    let sanitized = strip_ansi(subject);
    match label {
        Some(lbl) => compose_subject_with_label(theme, &sanitized, lbl),
        None => sanitized,
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
        Role::Info => (None, ThemedStyle::plain()),
        // Accent + Secondary intentionally claim no icon — they accent the
        // text payload, they don't reserve a status-line glyph column.
        Role::Accent => (None, theme.accent.clone()),
        Role::Secondary => (None, theme.secondary.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_has_no_icon() {
        let t = Theme::default();
        let (icon, _) = role_glyph(&t, Role::Info);
        assert!(icon.is_none());
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

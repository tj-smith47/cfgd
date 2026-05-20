use crate::output::theme::ThemedStyle;
use crate::output::{Role, Theme};

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

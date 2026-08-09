//! `OwnerLabel` — the one renderer of the tri-colour owner token.
//!
//! An owner token names who a group of work belongs to: `module:nvim`,
//! `profile:work`, `cfgd:managers`. It is three theme slots in one string, so
//! it lives here rather than being composed at a call site: a caller that
//! hand-rolled it would reach for `console` or a literal colour and leave the
//! theme unable to restyle it.

use super::{Role, Theme};

/// A `<kind>:<name>` owner token. `plain` is the uncoloured form every
/// structured, quiet and colour-disabled path renders.
pub struct OwnerLabel {
    kind: String,
    name: String,
}

impl OwnerLabel {
    pub fn new(kind: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
        }
    }

    /// `module:nvim` — no styling, and the string every non-colour path uses.
    pub fn plain(&self) -> String {
        format!("{}:{}", self.kind, self.name)
    }

    /// The three-slot styled form: the kind word `Role::Secondary`, the colon
    /// `Role::Warn`, the name `Role::Ok`. Falls back to [`Self::plain`] when
    /// colours are off, so nothing but the token's own text reaches a
    /// redirected stream.
    pub(crate) fn styled(&self, theme: &Theme) -> String {
        if !console::colors_enabled() {
            return self.plain();
        }
        let paint = |role: Role, text: &str| {
            let (_, style) = super::renderer::role_glyph(theme, role);
            style.apply_to(text).to_string()
        };
        format!(
            "{}{}{}",
            paint(Role::Secondary, &self.kind),
            paint(Role::Warn, ":"),
            paint(Role::Ok, &self.name)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::strip_ansi;
    use crate::output::test_support::ColorsEnabledGuard;

    #[test]
    fn plain_is_kind_colon_name() {
        assert_eq!(OwnerLabel::new("module", "nvim").plain(), "module:nvim");
    }

    #[test]
    #[serial_test::serial]
    fn styled_paints_three_slots_and_strips_back_to_plain() {
        let _colors = ColorsEnabledGuard::set(true);
        let theme = Theme::from_preset("dracula");
        let label = OwnerLabel::new("profile", "work");
        let styled = label.styled(&theme);
        assert_eq!(strip_ansi(&styled), "profile:work");
        // Three distinct slots, so three distinct colour runs.
        let kind = theme.secondary.apply_to("profile").to_string();
        let colon = theme.warning.apply_to(":").to_string();
        let name = theme.success.apply_to("work").to_string();
        assert_eq!(styled, format!("{kind}{colon}{name}"));
    }

    #[test]
    #[serial_test::serial]
    fn colour_disabled_falls_back_to_plain() {
        let _colors = ColorsEnabledGuard::set(false);
        let label = OwnerLabel::new("cfgd", "managers");
        assert_eq!(
            label.styled(&Theme::from_preset("dracula")),
            "cfgd:managers"
        );
    }
}

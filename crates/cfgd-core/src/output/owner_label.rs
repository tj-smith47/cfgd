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
    /// `Role::Warn`, the name `Role::Ok`.
    ///
    /// Each slot goes through `ThemedStyle`, which is what decides what a
    /// colour-disabled stream gets: bare text for a colour-only slot, and
    /// attrs-only SGR for a slot whose differentiator is an attribute
    /// (`minimal`'s underlined secondary). Short-circuiting to
    /// [`Self::plain`] here would strip that attribute from the owner token
    /// alone, while every other themed element on the same screen kept it.
    pub(crate) fn styled(&self, theme: &Theme) -> String {
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

    #[test]
    fn plain_is_kind_colon_name() {
        assert_eq!(OwnerLabel::new("module", "nvim").plain(), "module:nvim");
    }

    /// Serial because `supports_truecolor()` reads `COLORTERM` / `NO_COLOR`,
    /// and the composed render is compared against three slot renders taken
    /// afterwards — a concurrent env mutation between the two would split the
    /// comparison.
    #[test]
    #[serial_test::serial]
    fn styled_paints_three_slots_and_strips_back_to_plain() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        let label = OwnerLabel::new("profile", "work");
        let styled = label.styled(&theme);
        assert_eq!(strip_ansi(&styled), "profile:work");
        // Three distinct slots, so three distinct colour runs.
        let kind = theme.secondary.apply_to("profile").to_string();
        let colon = theme.warning.apply_to(":").to_string();
        let name = theme.success.apply_to("work").to_string();
        assert_eq!(styled, format!("{kind}{colon}{name}"));
    }

    /// A preset whose three token slots are colour-only has nothing left to
    /// emit once colour is off, so the token is its own text and no more.
    #[test]
    fn colour_disabled_drops_colour_only_slots() {
        let label = OwnerLabel::new("cfgd", "managers");
        assert_eq!(
            label.styled(&Theme::from_preset("dracula")),
            "cfgd:managers"
        );
    }

    /// `minimal` carries the secondary distinction in an attribute rather
    /// than a colour, and NO_COLOR governs colour only. The owner token keeps
    /// that attribute exactly as every other themed element does.
    #[test]
    fn colour_disabled_keeps_attribute_only_slots() {
        let theme = Theme::from_preset("minimal");
        let styled = OwnerLabel::new("module", "nvim").styled(&theme);
        assert_eq!(strip_ansi(&styled), "module:nvim");
        assert_eq!(
            styled,
            format!("{}:nvim", theme.secondary.apply_to("module")),
            "the underlined secondary must survive NO_COLOR"
        );
        assert!(
            styled.contains('\u{1b}'),
            "attribute SGR was stripped: {styled:?}"
        );
    }
}

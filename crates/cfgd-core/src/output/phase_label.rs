//! `PhaseLabel` — the one renderer of the `Phase: <name>` heading.
//!
//! A phase heading is three theme slots, not one: the `Phase` label takes the
//! heading slot every other section title uses, the colon takes the same
//! separator slot the owner token's colon does, and the name takes the accent
//! slot — attention without alarm, and a colour of its own so the phase's
//! identity reads apart from the label naming what it is. It lives here for
//! the same reason [`super::OwnerLabel`] does — a call site composing it would
//! reach for `console` or a literal colour and leave the theme unable to
//! restyle it.

use super::{Role, Theme};

/// A `Phase: <name>` heading. `plain` is the uncoloured form every structured,
/// quiet and colour-disabled path renders.
pub struct PhaseLabel {
    name: String,
}

impl PhaseLabel {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// `Phase: Packages` — no styling, and the string every non-colour path
    /// uses.
    pub fn plain(&self) -> String {
        format!("Phase: {}", self.name)
    }

    /// The three-slot styled form: `Phase` in the heading slot, the colon in
    /// the separator slot (`Role::Warn`, the slot the owner token's colon
    /// takes), and ` <name>` in the accent slot.
    pub(crate) fn styled(&self, theme: &Theme) -> String {
        let label = theme.header.apply_to("Phase");
        let (_, separator) = super::renderer::role_glyph(theme, Role::Warn);
        let (_, accent) = super::renderer::role_glyph(theme, Role::Accent);
        format!(
            "{label}{}{}",
            separator.apply_to(":"),
            accent.apply_to(format!(" {}", self.name))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::strip_ansi;

    #[test]
    fn plain_is_the_label_colon_name() {
        assert_eq!(PhaseLabel::new("Packages").plain(), "Phase: Packages");
    }

    /// Serial because `supports_truecolor()` reads `COLORTERM` / `NO_COLOR`,
    /// and the composed render is compared against slot renders taken
    /// afterwards — a concurrent env mutation between the two would split the
    /// comparison.
    #[test]
    #[serial_test::serial]
    fn the_heading_is_three_slots_label_separator_name() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        let styled = PhaseLabel::new("Managers").styled(&theme);
        assert_eq!(strip_ansi(&styled), "Phase: Managers");
        let label = theme.header.apply_to("Phase").to_string();
        let colon = theme.warning.apply_to(":").to_string();
        let name = theme.accent.apply_to(" Managers").to_string();
        assert_eq!(styled, format!("{label}{colon}{name}"));
        // Not vacuous: the three slots really are pairwise different under
        // dracula, so a heading painted in fewer slots would fail the
        // equality above rather than pass it by two sides rendering alike.
        assert_ne!(label, theme.warning.apply_to("Phase").to_string());
        assert_ne!(label, theme.accent.apply_to("Phase").to_string());
        assert_ne!(colon, theme.accent.apply_to(":").to_string());
    }

    /// Colour off drops the colour-only slots to bare text, while the heading
    /// slot's bold survives: bold is an attribute, not a colour, and
    /// attributes are load-bearing under `--no-color` (per no-color.org, the
    /// same rule every other heading renders by).
    #[test]
    fn colour_disabled_keeps_only_the_heading_attrs() {
        let theme = Theme::from_preset("dracula").with_colors(false);
        let styled = PhaseLabel::new("Files").styled(&theme);
        assert_eq!(strip_ansi(&styled), "Phase: Files");
        let label = theme.header.apply_to("Phase").to_string();
        assert_eq!(styled, format!("{label}: Files"));
    }

    /// Correlates the `:` and ` <name>` halves directly against
    /// `role_glyph(theme, Role::Warn)` / `role_glyph(theme, Role::Accent)` —
    /// the production lookups `styled` itself calls — rather than re-deriving
    /// the slot styles by hand as
    /// `the_heading_is_three_slots_label_separator_name` does. A different
    /// preset (solarized-dark, not dracula) so this is not the same render
    /// re-asserted under a second name.
    #[test]
    #[serial_test::serial]
    fn colon_and_name_take_their_role_slot_colours() {
        let theme = Theme::from_preset("solarized-dark").with_colors(true);
        let styled = PhaseLabel::new("Prerequisites").styled(&theme);

        let (_, separator) = super::super::renderer::role_glyph(&theme, Role::Warn);
        let (_, accent) = super::super::renderer::role_glyph(&theme, Role::Accent);
        let expected_tail = format!(
            "{}{}",
            separator.apply_to(":"),
            accent.apply_to(" Prerequisites")
        );

        assert!(
            styled.ends_with(&expected_tail),
            "the `: <name>` half must be styled with exactly the separator \
             and accent roles' glyph styles: styled={styled:?} \
             expected_tail={expected_tail:?}"
        );
        // Not vacuous: both slots really do carry a style under this preset,
        // so a heading painted in the plain foreground instead would fail the
        // `ends_with` rather than pass it by both sides being bare.
        assert_ne!(
            expected_tail, ": Prerequisites",
            "the separator and accent roles must actually carry styles under this preset"
        );
    }
}

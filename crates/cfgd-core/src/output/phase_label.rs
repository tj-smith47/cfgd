//! `PhaseLabel` — the one renderer of the `Phase: <name>` heading.
//!
//! A phase heading is two theme slots, not one: the `Phase` label sits in the
//! tree's plain foreground and the `: <name>` half takes the same separator
//! slot the owner token's colon does, so the eye follows one colour down from
//! the heading to the group beneath it. It lives here for the same reason
//! [`super::OwnerLabel`] does — a call site composing it would reach for
//! `console` or a literal colour and leave the theme unable to restyle it.

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

    /// The two-slot styled form: `Phase` in `theme.primary`, `: <name>` in the
    /// separator slot (`Role::Warn`, the slot the owner token's colon takes).
    ///
    /// A preset with no palette foreground of its own leaves `primary` unset,
    /// and the label is then written unstyled — the terminal's own foreground
    /// is exactly what "plain" means there.
    pub(crate) fn styled(&self, theme: &Theme) -> String {
        let label = match &theme.primary {
            Some(style) => style.apply_to("Phase").to_string(),
            None => "Phase".to_string(),
        };
        let (_, separator) = super::renderer::role_glyph(theme, Role::Warn);
        format!("{label}{}", separator.apply_to(format!(": {}", self.name)))
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
    fn the_name_takes_the_separator_slot_and_the_label_does_not() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        let styled = PhaseLabel::new("Managers").styled(&theme);
        assert_eq!(strip_ansi(&styled), "Phase: Managers");
        let label = theme
            .primary
            .as_ref()
            .expect("dracula has a foreground slot")
            .apply_to("Phase")
            .to_string();
        let rest = theme.warning.apply_to(": Managers").to_string();
        assert_eq!(styled, format!("{label}{rest}"));
        // Not vacuous: the two slots really are different colours under
        // dracula, so a heading painted in one slot would fail the equality
        // above rather than pass it by both sides being bare text.
        assert_ne!(label, theme.warning.apply_to("Phase").to_string());
    }

    /// A preset whose slots are colour-only has nothing left to emit once
    /// colour is off, so the heading is its own text and no more.
    #[test]
    fn colour_disabled_drops_colour_only_slots() {
        assert_eq!(
            PhaseLabel::new("Files").styled(&Theme::from_preset("dracula").with_colors(false)),
            "Phase: Files"
        );
    }

    /// Correlates the label's `: <name>` half directly against
    /// `role_glyph(theme, Role::Warn)` — the production lookup `styled`
    /// itself calls — rather than re-deriving the separator style by hand as
    /// `the_name_takes_the_separator_slot_and_the_label_does_not` does. A
    /// different preset (solarized-dark, not dracula) so this is not the same
    /// render re-asserted under a second name.
    #[test]
    #[serial_test::serial]
    fn phase_name_takes_the_separator_colour() {
        let theme = Theme::from_preset("solarized-dark").with_colors(true);
        let styled = PhaseLabel::new("Prerequisites").styled(&theme);

        let (_, separator) = super::super::renderer::role_glyph(&theme, Role::Warn);
        let expected_name_half = separator.apply_to(": Prerequisites").to_string();

        assert!(
            styled.ends_with(&expected_name_half),
            "the `: <name>` half must be styled with exactly the separator \
             role's glyph style: styled={styled:?} expected_tail={expected_name_half:?}"
        );
        // Not vacuous: solarized-dark's warning slot really does carry a
        // colour, so a heading painted in the plain foreground instead would
        // fail this `ends_with` rather than pass it by both sides being bare.
        assert_ne!(
            expected_name_half, ": Prerequisites",
            "the separator role must actually carry a style under this preset"
        );
    }
}

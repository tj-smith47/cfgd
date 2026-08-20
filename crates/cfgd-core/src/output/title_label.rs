//! `TitleLabel` — [`super::PhaseLabel`] generalized from a fixed "Phase"
//! label to any caller-supplied one.
//!
//! A heading whose text is genuinely two parts — a fixed label and a
//! caller-supplied value, joined by a hand-formatted colon
//! (`format!("Status: {name}")`) — is three theme slots, not one: the label
//! takes the same heading slot every other section title uses, the colon
//! takes the separator slot the owner token's colon does, and the value
//! takes the accent slot, so the identity the heading names reads apart from
//! the fixed word describing what it is. A heading with no value part is not
//! a `TitleLabel` — it stays a plain [`super::Printer::heading`].

use super::{Role, Theme};

/// A `Label: value` heading. `plain` is the uncoloured form every structured,
/// quiet and colour-disabled path renders.
pub struct TitleLabel {
    label: String,
    value: String,
}

impl TitleLabel {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }

    /// `Status: dev-tools` — no styling, and the string every non-colour
    /// path uses (including a `-o json` reader's `heading` field).
    pub fn plain(&self) -> String {
        format!("{}: {}", self.label, self.value)
    }

    /// The three-slot styled form: the label in the heading slot, the colon
    /// in the separator slot (`Role::Warn`, the slot the owner token's colon
    /// takes), and ` <value>` in the accent slot.
    pub(crate) fn styled(&self, theme: &Theme) -> String {
        // BOTH text slots are folded before their coats go on — the value
        // names something the caller supplied (a profile, a module), and the
        // label is a caller's string too even though every production caller
        // passes a literal today. [`Self::plain`] is deliberately NOT folded:
        // that form is what a `-o json` reader's `heading` field carries, and
        // a payload stays byte-exact.
        let label = theme.header.apply_to(super::cursor_safe(&self.label));
        let (_, separator) = super::renderer::role_glyph(theme, Role::Warn);
        let (_, accent) = super::renderer::role_glyph(theme, Role::Accent);
        format!(
            "{label}{}{}",
            separator.apply_to(":"),
            accent.apply_to(format!(" {}", super::cursor_safe(&self.value)))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::strip_ansi;

    #[test]
    fn plain_is_the_label_colon_value() {
        assert_eq!(
            TitleLabel::new("Status", "dev-tools").plain(),
            "Status: dev-tools"
        );
    }

    /// Serial because `supports_truecolor()` reads `COLORTERM` / `NO_COLOR`,
    /// and the composed render is compared against slot renders taken
    /// afterwards — a concurrent env mutation between the two would split the
    /// comparison.
    #[test]
    #[serial_test::serial]
    fn the_heading_is_three_slots_label_separator_value() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        let styled = TitleLabel::new("Profile", "work").styled(&theme);
        assert_eq!(strip_ansi(&styled), "Profile: work");
        let label = theme.header.apply_to("Profile").to_string();
        let colon = theme.warning.apply_to(":").to_string();
        let value = theme.accent.apply_to(" work").to_string();
        assert_eq!(styled, format!("{label}{colon}{value}"));
        // Not vacuous: the three slots really are pairwise different under
        // dracula, so a heading painted in fewer slots would fail the
        // equality above rather than pass it by two sides rendering alike.
        assert_ne!(label, theme.warning.apply_to("Profile").to_string());
        assert_ne!(label, theme.accent.apply_to("Profile").to_string());
        assert_ne!(colon, theme.accent.apply_to(":").to_string());
    }

    /// Colour off drops the colour-only slots to bare text, while the label
    /// slot's bold survives: bold is an attribute, not a colour, and
    /// attributes are load-bearing under `--no-color` (per no-color.org, the
    /// same rule every other heading renders by).
    #[test]
    fn colour_disabled_keeps_only_the_label_attrs() {
        let theme = Theme::from_preset("dracula").with_colors(false);
        let styled = TitleLabel::new("Module", "vim-config").styled(&theme);
        assert_eq!(strip_ansi(&styled), "Module: vim-config");
        let label = theme.header.apply_to("Module").to_string();
        assert_eq!(styled, format!("{label}: vim-config"));
    }

    /// Correlates the `:` and ` <value>` halves directly against
    /// `role_glyph(theme, Role::Warn)` / `role_glyph(theme, Role::Accent)` —
    /// the production lookups `styled` itself calls — rather than
    /// re-deriving the slot styles by hand as
    /// `the_heading_is_three_slots_label_separator_value` does. A different
    /// preset (solarized-dark, not dracula) so this is not the same render
    /// re-asserted under a second name.
    #[test]
    #[serial_test::serial]
    fn colon_and_value_take_their_role_slot_colours() {
        let theme = Theme::from_preset("solarized-dark").with_colors(true);
        let styled = TitleLabel::new("Snapshots", "nightly").styled(&theme);

        let (_, separator) = super::super::renderer::role_glyph(&theme, Role::Warn);
        let (_, accent) = super::super::renderer::role_glyph(&theme, Role::Accent);
        let expected_tail = format!("{}{}", separator.apply_to(":"), accent.apply_to(" nightly"));

        assert!(
            styled.ends_with(&expected_tail),
            "the `: <value>` half must be styled with exactly the separator \
             and accent roles' glyph styles: styled={styled:?} \
             expected_tail={expected_tail:?}"
        );
        // Not vacuous: both slots really do carry a style under this preset,
        // so a heading painted in the plain foreground instead would fail
        // the `ends_with` rather than pass it by both sides being bare.
        assert_ne!(
            expected_tail, ": nightly",
            "the separator and accent roles must actually carry styles under this preset"
        );
    }
}

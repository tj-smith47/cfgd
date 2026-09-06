//! `AccentHeading` — a phase-class heading that names ONE thing, with no
//! `Label:` prefix.
//!
//! [`super::PhaseLabel`] and [`super::TitleLabel`] both compose a fixed
//! label, a colon, and a value. Some headings are phase-class — painted to
//! draw the eye the way a phase heading does, reading as part of the run's
//! phase structure — without actually being a phase, and without a second
//! half for `TitleLabel`'s `Label: value` shape. `Caveats` is the case this
//! exists for: routing it through `PhaseLabel` would render `Phase:
//! Caveats`, which is wrong (Caveats is not a phase), and it names nothing
//! else for a `TitleLabel` to pair it with. This is the composer for that
//! shape — a single string, styled through the same `Role::Accent` slot
//! `PhaseLabel`'s value half takes, so it reads as part of the phase
//! structure without a hand-built styled string living in a `Printer` call
//! site.

use super::{Role, Theme};

pub(super) struct AccentHeading(String);

impl AccentHeading {
    pub(super) fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The uncoloured form every non-colour path renders.
    pub(super) fn plain(&self) -> &str {
        &self.0
    }

    /// Styled through `Role::Accent`, no `.bold()` — bold never pairs with
    /// colour on a colour-bearing slot (every named colour preset), and
    /// `minimal`'s accent already carries the distinction as italic rather
    /// than as an attribute this heading would have to add on top.
    pub(super) fn styled(&self, theme: &Theme) -> String {
        let (_, accent) = super::renderer::role_glyph(theme, Role::Accent);
        accent.apply_to(&self.0).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::strip_ansi;

    #[test]
    fn plain_is_the_bare_text() {
        assert_eq!(AccentHeading::new("Caveats").plain(), "Caveats");
    }

    #[test]
    fn styled_carries_no_bold_attribute() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        let styled = AccentHeading::new("Caveats").styled(&theme);
        assert_eq!(strip_ansi(&styled), "Caveats");
        let (_, accent) = super::super::renderer::role_glyph(&theme, Role::Accent);
        assert_eq!(styled, accent.apply_to("Caveats").to_string());
    }

    #[test]
    fn colour_disabled_renders_bare_text() {
        let theme = Theme::from_preset("dracula").with_colors(false);
        assert_eq!(AccentHeading::new("Caveats").styled(&theme), "Caveats");
    }
}

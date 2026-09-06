//! `PhaseLabel` — the `Phase: <name>` heading, expressed as
//! [`super::TitleLabel`] fixed to the `"Phase"` label.
//!
//! A phase heading and any other `Label: value` heading are the same three
//! theme slots (label / colon / value); `PhaseLabel` is not a second
//! implementation of that composition, only a named constructor for it, so
//! a future change to the separator or value slot moves both families at
//! once instead of drifting apart because one was edited and the other
//! forgotten.

use super::{Theme, TitleLabel};

/// A `Phase: <name>` heading. `plain` is the uncoloured form every structured,
/// quiet and colour-disabled path renders.
pub struct PhaseLabel(TitleLabel);

impl PhaseLabel {
    pub fn new(name: impl Into<String>) -> Self {
        Self(TitleLabel::new("Phase", name))
    }

    /// `Phase: Packages` — no styling, and the string every non-colour path
    /// uses.
    pub fn plain(&self) -> String {
        self.0.plain()
    }

    /// The three-slot styled form: `Phase` in the heading slot, the colon in
    /// the separator slot, and ` <name>` in the accent slot.
    pub(crate) fn styled(&self, theme: &Theme) -> String {
        self.0.styled(theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The composition itself (three slots, colour handling, colour-off
    /// attribute survival) is proven once by `TitleLabel`'s own tests; this
    /// pins only that `PhaseLabel` is genuinely that composer fixed to the
    /// `"Phase"` label, not a second copy of it.
    #[test]
    fn phase_label_is_title_label_fixed_to_phase() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        assert_eq!(
            PhaseLabel::new("Packages").plain(),
            TitleLabel::new("Phase", "Packages").plain(),
        );
        assert_eq!(
            PhaseLabel::new("Packages").styled(&theme),
            TitleLabel::new("Phase", "Packages").styled(&theme),
        );
    }

    #[test]
    fn plain_is_the_label_colon_name() {
        assert_eq!(PhaseLabel::new("Packages").plain(), "Phase: Packages");
    }
}

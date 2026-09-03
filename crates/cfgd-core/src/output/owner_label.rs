//! The subjects a status row is PAINTED from rather than coated with.
//!
//! A row's subject normally takes the row's single role. Two do not, because
//! their parts carry different roles: an owner token (`module:nvim` — kind,
//! colon, name) and a state transition (`(Compliant → Violation)` — each
//! status word in the role its own vocabulary paired it with). Both live here
//! rather than at a call site: a caller composing one would reach for
//! `console` or a literal colour and leave the theme unable to restyle it,
//! and the renderer's `cursor_safe` fold would eat the coat anyway.

use super::{Role, Theme};

/// A subject the renderer paints from typed parts. `plain` is what `-o json`,
/// a quiet render and every width computation read.
#[derive(Clone, Debug)]
pub enum PaintedSubject {
    Owner(OwnerLabel),
    Transition(StatusTransition),
}

impl PaintedSubject {
    pub(crate) fn plain(&self) -> String {
        match self {
            Self::Owner(o) => o.plain(),
            Self::Transition(t) => t.plain(),
        }
    }

    pub(crate) fn styled(&self, theme: &Theme) -> String {
        match self {
            Self::Owner(o) => o.styled(theme),
            Self::Transition(t) => t.styled(theme),
        }
    }
}

/// `<subject> (<old> → <new>)` — a recorded state CHANGE, each status word in
/// the role its own vocabulary paired it with.
///
/// The row's role is the NEW state's, so leaving the pair to the row's coat
/// paints the old word in the new state's colour — a `Compliant` rendered in
/// Fail red. Both words are Title-Cased status words, and a Title-Cased
/// status word renders in its own role everywhere.
#[derive(Clone, Debug)]
pub struct StatusTransition {
    subject: String,
    old: (String, Role),
    new: (String, Role),
    arrow: String,
}

impl StatusTransition {
    /// `arrow` is the caller's own [`Theme::arrow`], so the plain form and the
    /// styled one cannot spell the relationship with two different glyphs on a
    /// theme that overrides it.
    pub fn new(
        subject: impl Into<String>,
        old: (&str, Role),
        new: (&str, Role),
        arrow: impl Into<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            old: (old.0.to_string(), old.1),
            new: (new.0.to_string(), new.1),
            arrow: arrow.into(),
        }
    }

    /// The uncoloured form, for the paths that render a component without
    /// painting it.
    fn plain(&self) -> String {
        format!(
            "{} ({} {} {})",
            self.subject, self.old.0, self.arrow, self.new.0
        )
    }

    fn styled(&self, theme: &Theme) -> String {
        let paint = |role: Role, text: &str| {
            let (_, style) = super::renderer::role_glyph(theme, role);
            style.apply_to(super::cursor_safe(text)).to_string()
        };
        // The subject is a check KEY from a recorded snapshot, so it is folded
        // like the owner token's slots; the frame stays the theme's own.
        format!(
            "{} ({} {} {})",
            super::cursor_safe(&self.subject),
            paint(self.old.1, &self.old.0),
            self.arrow,
            paint(self.new.1, &self.new.0)
        )
    }
}

/// A `<kind>:<name>` owner token. `plain` is the uncoloured form every
/// structured, quiet and colour-disabled path renders.
#[derive(Clone, Debug)]
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
        // A `kind` and a `name` can both come from a module document a remote
        // source shipped, so each slot's text is folded before its own coat
        // goes on — [`Self::plain`] is deliberately NOT folded, because that
        // form is what the structured payload carries and a payload stays
        // byte-exact.
        let paint = |role: Role, text: &str| {
            let (_, style) = super::renderer::role_glyph(theme, role);
            style.apply_to(super::cursor_safe(text)).to_string()
        };
        format!(
            "{}{}{}",
            paint(Role::Secondary, &self.kind),
            paint(Role::Warn, ":"),
            paint(Role::Ok, &self.name)
        )
    }

    /// `<prefix> <kind>:<name>` (`Add module:vim-config`) — the fixed verb in
    /// the heading slot, then this token's own three slots. `plain` mirrors
    /// `format!("{prefix} {}", self.plain())`.
    pub(crate) fn styled_with_prefix(&self, theme: &Theme, prefix: &str) -> String {
        // The prefix is folded like the token's own two slots, for the same
        // reason and on the same terms: every production caller passes a
        // literal or an id it formatted itself, and the slot must not be the
        // one way in for a caller that does not.
        format!(
            "{} {}",
            theme.header.apply_to(super::cursor_safe(prefix)),
            self.styled(theme)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::strip_ansi;

    /// Both forms spell the relationship with the arrow the CALLER passed —
    /// the plain one cannot fall back to a default theme's glyph while the
    /// styled one renders an override.
    #[test]
    fn a_transitions_two_forms_spell_one_arrow() {
        let t = StatusTransition::new(
            "file:conf",
            ("Compliant", Role::Ok),
            ("Warning", Role::Warn),
            "=>",
        );
        assert_eq!(t.plain(), "file:conf (Compliant => Warning)");
        assert!(strip_ansi(&t.styled(&Theme::default())).contains("(Compliant => Warning)"));
    }

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
    ///
    /// The colour-ON half is what makes the colour-OFF half mean anything: an
    /// unstamped theme spends no colour either, so asserting only the OFF case
    /// would pass with the stamp ignored entirely.
    #[test]
    fn colour_disabled_drops_colour_only_slots() {
        let label = OwnerLabel::new("cfgd", "managers");
        assert_eq!(
            label.styled(&Theme::from_preset("dracula").with_colors(false)),
            "cfgd:managers"
        );
        let on = label.styled(&Theme::from_preset("dracula").with_colors(true));
        assert!(
            on.contains("\u{1b}["),
            "dracula's token slots spend no colour even with it on, so the \
             assertion above proves nothing: {on:?}"
        );
        assert_eq!(strip_ansi(&on), "cfgd:managers");
    }

    /// `minimal` carries the secondary distinction in an attribute rather
    /// than a colour, and NO_COLOR governs colour only. The owner token keeps
    /// that attribute exactly as every other themed element does.
    #[test]
    fn colour_disabled_keeps_attribute_only_slots() {
        let theme = Theme::from_preset("minimal").with_colors(false);
        let styled = OwnerLabel::new("module", "nvim").styled(&theme);
        assert_eq!(strip_ansi(&styled), "module:nvim");
        // Spelled out rather than re-derived from the same theme: comparing
        // against `theme.secondary.apply_to(…)` asserts only that the token
        // used the slot it was built from, which holds however the slot renders.
        assert_eq!(
            styled, "\u{1b}[4mmodule\u{1b}[0m:nvim",
            "the underlined secondary must survive colour being off"
        );
    }

    /// `styled_with_prefix` is the fixed verb in the heading slot, a space,
    /// then the owner token's own three slots — not a fourth slot of its own.
    #[test]
    #[serial_test::serial]
    fn styled_with_prefix_is_header_prefix_plus_styled_token() {
        let theme = Theme::from_preset("dracula").with_colors(true);
        let label = OwnerLabel::new("source", "acme");
        let styled = label.styled_with_prefix(&theme, "Add");
        assert_eq!(strip_ansi(&styled), "Add source:acme");
        assert_eq!(
            styled,
            format!("{} {}", theme.header.apply_to("Add"), label.styled(&theme))
        );
    }
}

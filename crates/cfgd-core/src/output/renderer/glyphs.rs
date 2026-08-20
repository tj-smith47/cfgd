use crate::output::component::StatusLabel;
use crate::output::theme::ThemedStyle;
use crate::output::{Role, Theme, cursor_safe};

/// Compose a `subject` with a trailing styled `label`, separated by one ASCII
/// space. The label always lands at end-of-subject so the inner SGR reset
/// closing the label's color cannot be followed by outer-role-styled text —
/// the only safe nesting shape for the streaming renderer. Single source of
/// truth shared by `StatusBuilder::drop` (streaming) and `render_doc`
/// (buffered Doc tree) so the two paths stay byte-identical.
fn compose_subject_with_label(theme: &Theme, subject: &str, label: &StatusLabel) -> String {
    let (_, style) = role_glyph(theme, label.role);
    let styled = style.apply_to(&label.text).to_string();
    format!("{subject} {styled}")
}

/// Compose a `subject` with a trailing `": " qualifier` (`curl: missing`) —
/// three slots: the subject keeps whatever role-slot styling the status line
/// already carries (untouched here), the colon is always `Role::Warn`, and
/// the qualifier text is always `theme.muted`, matching `TitleLabel`'s
/// separator/value split rather than taking a per-call role the way `label`
/// does. Landed at end-of-subject, same reasoning as
/// `compose_subject_with_label`: the inner SGR reset closing the qualifier's
/// color cannot be followed by outer-role-styled text.
fn compose_subject_with_qualifier(theme: &Theme, subject: &str, qualifier: &str) -> String {
    let (_, colon_style) = role_glyph(theme, Role::Warn);
    // `qualifier` is as untrusted as `subject` — both can carry a captured
    // error string — so it gets the same sanitation before it is wrapped in
    // the renderer-owned muted span, or a foreign `\x1b[0m` inside it closes
    // the span early and everything the renderer writes after paints in
    // whatever colour the foreign escape last set.
    let sanitized_qualifier = cursor_safe(qualifier);
    let muted = theme.muted.apply_to(format!(" {sanitized_qualifier}"));
    format!("{subject}{}{muted}", colon_style.apply_to(":"))
}

/// Compose a `subject` behind a leading styled `marker` (`postApply:`),
/// separated by one ASCII space.
///
/// A prefix rather than the trailing-label shape, because the marker names the
/// hook the body belongs to and is read first. The inner SGR reset that closes
/// it leaves the body in the terminal's own foreground — the marker is styled
/// and the body is not, which is the whole of the mapping.
fn compose_subject_with_marker(theme: &Theme, subject: &str, marker: &StatusLabel) -> String {
    let (_, style) = role_glyph(theme, marker.role);
    let styled = style.apply_to(&marker.text).to_string();
    format!("{styled} {subject}")
}

/// Sanitize a caller-supplied status subject and optionally wrap it in the
/// renderer-owned styled segments: a leading `marker` and a trailing `label`.
/// The subject may carry foreign ANSI from a captured error string
/// (`format!("sync failed for {url}: {e}")`); a stray `\x1b[0m` would
/// prematurely close the role styling at the inner reset, and foreign color
/// escapes would paint trailing characters until the next reset — and a bare
/// `\r` no escape sequence introduced would repaint the line the subject is
/// written on. [`cursor_safe`] closes both. Fold the subject FIRST, then add
/// the legitimate (renderer-controlled) segments so they survive sanitation.
pub(crate) fn finalize_subject(
    theme: &Theme,
    subject: &str,
    marker: Option<&StatusLabel>,
    qualifier: Option<&str>,
    label: Option<&StatusLabel>,
) -> String {
    let sanitized = cursor_safe(subject);
    let qualified = match qualifier {
        Some(q) => compose_subject_with_qualifier(theme, &sanitized, q),
        None => sanitized,
    };
    let labelled = match label {
        Some(lbl) => compose_subject_with_label(theme, &qualified, lbl),
        None => qualified,
    };
    match marker {
        Some(mk) => compose_subject_with_marker(theme, &labelled, mk),
        None => labelled,
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
        Role::Info => (Some(theme.icon_info.as_str()), theme.info.clone()),
        // Accent + Secondary intentionally claim no icon — they style the text
        // payload inline, they don't occupy the status-line glyph column that
        // every other role reserves.
        Role::Accent => (None, theme.accent.clone()),
        Role::Secondary => (None, theme.secondary.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ThemedStyle` has no `PartialEq`; its `Debug` carries the color triple
    /// and the attribute set, which is the whole of its rendering payload.
    fn style_repr(style: &ThemedStyle) -> String {
        format!("{style:?}")
    }

    #[test]
    fn qualifier_composes_subject_colon_space_qualifier() {
        let t = Theme::default();
        let composed = finalize_subject(&t, "curl", None, Some("missing"), None);
        assert_eq!(crate::output::strip_ansi(&composed), "curl: missing");
    }

    /// The colon is always `Role::Warn` and the qualifier text always
    /// `theme.muted`, whatever role the status LINE itself carries — the
    /// "role slot / warning colon / muted qualifier" split composes fixed
    /// styling for the trailing two slots, not a per-call choice the way
    /// `label`'s role parameter is.
    #[test]
    fn qualifier_colon_is_warn_and_text_is_muted() {
        let t = Theme::default().with_colors(true);
        let composed = finalize_subject(&t, "curl", None, Some("missing"), None);
        let colon = role_glyph(&t, Role::Warn).1.apply_to(":").to_string();
        let text = t.muted.apply_to(" missing").to_string();
        assert!(
            composed.contains(&colon),
            "colon not styled Warn; got: {composed:?}"
        );
        assert!(
            composed.contains(&text),
            "qualifier text not styled muted; got: {composed:?}"
        );
    }

    /// A qualifier built from a captured error string can carry foreign SGR
    /// (a colour-emitting child process's own escapes). It must be stripped
    /// exactly like `subject` is — a raw `\x1b[0m` surviving inside the
    /// muted span would close it early and leave everything the renderer
    /// writes afterward painted in whatever colour the foreign escape last
    /// set.
    #[test]
    fn qualifier_carrying_foreign_ansi_is_stripped_before_composing() {
        let t = Theme::default().with_colors(true);
        let composed = finalize_subject(&t, "curl", None, Some("\x1b[31mmissing\x1b[0m"), None);
        // The foreign red-fg code and reset must be gone — only the
        // renderer's own muted styling may remain, proven by round-tripping
        // through `strip_ansi` back to the plain text.
        assert!(
            !composed.contains("\x1b[31m"),
            "foreign colour escape survived inside the composed qualifier: {composed:?}"
        );
        assert_eq!(crate::output::strip_ansi(&composed), "curl: missing");
    }

    /// Qualifier lands ahead of `label` — both are trailing, at-end-of-subject
    /// segments, and the qualifier reads as part of what the subject IS about
    /// while the label is a separate trailing annotation (an owner token).
    #[test]
    fn qualifier_lands_before_label() {
        let t = Theme::default();
        let label = StatusLabel {
            role: Role::Secondary,
            text: "[team-config]".into(),
        };
        let composed = finalize_subject(&t, "curl", None, Some("missing"), Some(&label));
        assert_eq!(
            crate::output::strip_ansi(&composed),
            "curl: missing [team-config]"
        );
    }

    #[test]
    fn info_uses_its_theme_icon_and_style() {
        let t = Theme::default();
        let (icon, style) = role_glyph(&t, Role::Info);
        assert_eq!(icon, Some("⊙"));
        assert_eq!(style_repr(&style), style_repr(&t.info));
        assert_ne!(style_repr(&style), style_repr(&ThemedStyle::plain()));
        // The ASCII preset downgrades it like every other glyph.
        assert_eq!(
            role_glyph(&Theme::from_preset("minimal"), Role::Info).0,
            Some("i")
        );
    }

    /// A role that renders with a style it did not take from the active theme
    /// is invisible to every theme preset and to `ThemeOverrides` — that is how
    /// the npm global-prefix notice shipped as bare unstyled white text while
    /// still passing the "all output routes through Printer" gate. Checked
    /// against each preset so a slot mixup can't hide behind two themes that
    /// happen to color two roles alike.
    #[test]
    fn every_role_renders_with_its_own_theme_slot() {
        for preset in [
            "default",
            "dracula",
            "solarized-dark",
            "solarized-light",
            "minimal",
        ] {
            let t = Theme::from_preset(preset);
            for role in [
                Role::Ok,
                Role::Warn,
                Role::Fail,
                Role::Pending,
                Role::Running,
                Role::Skipped,
                Role::Info,
                Role::Accent,
                Role::Secondary,
            ] {
                // Exhaustive by construction: a new `Role` fails to compile here
                // until its theme slot is named.
                let expected = match role {
                    Role::Ok => &t.success,
                    Role::Warn => &t.warning,
                    Role::Fail => &t.error,
                    Role::Pending | Role::Skipped => &t.muted,
                    Role::Running => &t.running,
                    Role::Info => &t.info,
                    Role::Accent => &t.accent,
                    Role::Secondary => &t.secondary,
                };
                let (_, style) = role_glyph(&t, role);
                assert_eq!(
                    style_repr(&style),
                    style_repr(expected),
                    "{role:?} does not use its {preset} theme slot"
                );
            }
        }
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

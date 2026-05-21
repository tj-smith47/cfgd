//! Raw-bytes assertions on accent/secondary palette per preset.
//!
//! `golden_themed!` strips ANSI before snapshot compare, so a hex-value
//! regression to (say) `dracula.accent` would not fail any existing test.
//! These tests inspect raw rendered bytes for the expected truecolor SGR
//! codes per preset to lock in palette values.

use crate::output::test_support::ColorsEnabledGuard;
use crate::output::{Doc, Printer, Role, Theme, Verbosity};
use crate::test_helpers::EnvVarGuard;
use serial_test::serial;

fn render_with_theme(name: &str, doc: Doc) -> String {
    let (p, buf) = Printer::for_test_with_theme(Theme::from_preset(name), Verbosity::Normal);
    p.emit(doc);
    p.flush();
    // Mutex is local to this test — poisoning would only occur if a prior
    // borrower panicked while holding the guard, which the synchronous
    // emit/flush above cannot trigger. Recover the inner value either way
    // so the audit gate's no-unwrap rule stays clean.
    match buf.lock() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[test]
#[serial]
fn accent_emits_truecolor_sgr_per_preset() {
    // Each preset's accent hex value (cf. theme.rs preset bodies).
    // Truecolor SGR: \x1b[38;2;R;G;Bm
    let cases = [
        ("default", (0xd7, 0x87, 0x00)),        // #d78700 italic
        ("dracula", (0xff, 0xb8, 0x6c)),        // #ffb86c
        ("solarized-dark", (0xcb, 0x4b, 0x16)), // #cb4b16
        ("solarized-light", (0xcb, 0x4b, 0x16)), // #cb4b16
                                                // minimal has no hex — uses italic only. Verified separately.
    ];
    let _no_color = EnvVarGuard::unset("NO_COLOR");
    let _term = EnvVarGuard::set("COLORTERM", "truecolor");
    let _guard = ColorsEnabledGuard::set(true);
    for (preset, (r, g, b)) in cases {
        let doc = Doc::new().status(Role::Accent, "marker");
        let raw = render_with_theme(preset, doc);
        // Expect the truecolor SGR with these exact rgb values. The default
        // preset additionally carries italic (3) — check `\x1b[3;38;2;...m`
        // OR `\x1b[38;2;...m`. Use a contains check.
        let needle_plain = format!("\x1b[38;2;{r};{g};{b}m");
        let needle_italic = format!("\x1b[3;38;2;{r};{g};{b}m");
        assert!(
            raw.contains(&needle_plain) || raw.contains(&needle_italic),
            "preset {preset}: missing accent SGR for rgb({r},{g},{b}); raw={raw:?}",
        );
    }
}

#[test]
#[serial]
fn secondary_emits_truecolor_sgr_per_preset() {
    let cases = [
        ("default", (0xaf, 0x5f, 0xd7)),         // #af5fd7
        ("dracula", (0xff, 0x79, 0xc6)),         // #ff79c6
        ("solarized-dark", (0xd3, 0x36, 0x82)),  // #d33682
        ("solarized-light", (0xd3, 0x36, 0x82)), // #d33682
    ];
    let _no_color = EnvVarGuard::unset("NO_COLOR");
    let _term = EnvVarGuard::set("COLORTERM", "truecolor");
    let _guard = ColorsEnabledGuard::set(true);
    for (preset, (r, g, b)) in cases {
        let doc = Doc::new().status(Role::Secondary, "marker");
        let raw = render_with_theme(preset, doc);
        let needle = format!("\x1b[38;2;{r};{g};{b}m");
        assert!(
            raw.contains(&needle),
            "preset {preset}: missing secondary SGR for rgb({r},{g},{b}); raw={raw:?}",
        );
    }
}

#[test]
#[serial]
fn minimal_accent_emits_italic_attr_no_color() {
    let _no_color = EnvVarGuard::unset("NO_COLOR");
    let _term = EnvVarGuard::set("COLORTERM", "truecolor");
    let _guard = ColorsEnabledGuard::set(true);
    let doc = Doc::new().status(Role::Accent, "marker");
    let raw = render_with_theme("minimal", doc);
    // minimal accent = plain.italic, no hex. Expect \x1b[3m (italic only).
    assert!(
        raw.contains("\x1b[3m"),
        "minimal accent must emit italic SGR; raw={raw:?}"
    );
    // And no truecolor escape.
    assert!(
        !raw.contains("\x1b[38;2;"),
        "minimal accent must not emit truecolor SGR; raw={raw:?}"
    );
}

#[test]
#[serial]
fn minimal_secondary_emits_underline_attr_no_color() {
    let _no_color = EnvVarGuard::unset("NO_COLOR");
    let _term = EnvVarGuard::set("COLORTERM", "truecolor");
    let _guard = ColorsEnabledGuard::set(true);
    let doc = Doc::new().status(Role::Secondary, "marker");
    let raw = render_with_theme("minimal", doc);
    // minimal secondary = plain.underlined, no hex. Expect \x1b[4m.
    assert!(
        raw.contains("\x1b[4m"),
        "minimal secondary must emit underline SGR; raw={raw:?}"
    );
    assert!(
        !raw.contains("\x1b[38;2;"),
        "minimal secondary must not emit truecolor SGR; raw={raw:?}"
    );
}

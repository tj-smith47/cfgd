//! Raw-bytes assertions on accent/secondary palette per preset.
//!
//! `golden_themed!` strips ANSI before snapshot compare, so a hex-value
//! regression to (say) `dracula.accent` would not fail any existing test.
//! These tests inspect raw rendered bytes for the expected truecolor SGR
//! codes per preset to lock in palette values.

use crate::output::{Doc, Printer, Role, Theme, Verbosity};
use crate::test_helpers::EnvVarGuard;
use serial_test::serial;

fn render_with_theme(name: &str, doc: Doc) -> String {
    let (p, buf) =
        Printer::for_test_with_theme_colored(Theme::from_preset(name), Verbosity::Normal);
    p.emit(doc);
    p.flush();
    // Mutex is local to this test — poisoning would only occur if a prior
    // borrower panicked while holding the guard, which the synchronous
    // emit/flush above cannot trigger. Recover the inner value either way
    // so the audit gate's no-unwrap rule stays clean.
    // The raw truecolor SGR bytes are the assertion subject here, so this read
    // stays raw: captured_text would strip the ANSI this test exists to check.
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
        ("default", (0xd7, 0x87, 0x00)),          // #d78700 italic
        ("dracula", (0xff, 0xb8, 0x6c)),          // #ffb86c
        ("solarized-dark", (0xcb, 0x4b, 0x16)),   // #cb4b16
        ("solarized-light", (0xcb, 0x4b, 0x16)),  // #cb4b16
        ("nord", (0xd0, 0x87, 0x70)),             // #d08770
        ("monokai", (0xfd, 0x97, 0x1f)),          // #fd971f
        ("adventure-time", (0xe7, 0x74, 0x1e)),   // #e7741e
        ("catppuccin-mocha", (0xfa, 0xb3, 0x87)), // #fab387
        ("gruvbox-dark", (0xfe, 0x80, 0x19)),     // #fe8019
        ("tokyo-night", (0xff, 0x9e, 0x64)),      // #ff9e64
        ("one-dark", (0xd1, 0x9a, 0x66)),         // #d19a66
                                                  // minimal has no hex — uses italic only. Verified separately.
    ];
    let _no_color = EnvVarGuard::unset("NO_COLOR");
    let _term = EnvVarGuard::set("COLORTERM", "truecolor");
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
        ("default", (0xaf, 0x5f, 0xd7)),          // #af5fd7
        ("dracula", (0xff, 0x79, 0xc6)),          // #ff79c6
        ("solarized-dark", (0xd3, 0x36, 0x82)),   // #d33682
        ("solarized-light", (0xd3, 0x36, 0x82)),  // #d33682
        ("nord", (0xb4, 0x8e, 0xad)),             // #b48ead
        ("monokai", (0xf9, 0x26, 0x72)),          // #f92672
        ("adventure-time", (0x66, 0x59, 0x93)),   // #665993
        ("catppuccin-mocha", (0xf5, 0xc2, 0xe7)), // #f5c2e7
        ("gruvbox-dark", (0xd3, 0x86, 0x9b)),     // #d3869b
        ("tokyo-night", (0xbb, 0x9a, 0xf7)),      // #bb9af7
        ("one-dark", (0xc6, 0x78, 0xdd)),         // #c678dd
    ];
    let _no_color = EnvVarGuard::unset("NO_COLOR");
    let _term = EnvVarGuard::set("COLORTERM", "truecolor");
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
fn minimal_accent_spends_an_italic_where_it_spends_no_colour() {
    let _no_color = EnvVarGuard::unset("NO_COLOR");
    let _term = EnvVarGuard::set("COLORTERM", "truecolor");
    let doc = Doc::new().status(Role::Accent, "marker");
    let raw = render_with_theme("minimal", doc);
    // minimal accent = plain.italic, no hex: the preset spends no COLOUR on
    // the slot, and the printer here has colour ON. Expect \x1b[3m alone.
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
fn minimal_secondary_spends_an_underline_where_it_spends_no_colour() {
    let _no_color = EnvVarGuard::unset("NO_COLOR");
    let _term = EnvVarGuard::set("COLORTERM", "truecolor");
    let doc = Doc::new().status(Role::Secondary, "marker");
    let raw = render_with_theme("minimal", doc);
    // minimal secondary = plain.underlined, no hex; colour is ON here, and the
    // slot has none of its own to spend. Expect \x1b[4m.
    assert!(
        raw.contains("\x1b[4m"),
        "minimal secondary must emit underline SGR; raw={raw:?}"
    );
    assert!(
        !raw.contains("\x1b[38;2;"),
        "minimal secondary must not emit truecolor SGR; raw={raw:?}"
    );
}

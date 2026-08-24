//! Theme presets: one representative Doc rendered against every theme preset.
//! 12 cases — one per `from_preset` name.
//!
//! Goldens are ANSI-stripped, so themes that differ only by color produce
//! identical output. The `minimal` preset additionally swaps glyphs
//! (✓ → +, ⚠ → !, ✗ → x, ◐ → .) so it diverges from the others.

use crate::golden_themed;
use crate::output::{Doc, Role};

fn representative_doc() -> Doc {
    Doc::new()
        .heading("Status")
        .kv_block([("Profile", "dev"), ("Modules", "12")])
        .section("Drift", |s| {
            s.status(Role::Warn, "shell-config")
                .status(Role::Warn, "git-config")
        })
        .section("Highlights", |s| {
            s.status(Role::Accent, "new release available")
                .status(Role::Secondary, "from registry: stable")
        })
}

golden_themed!(themes, default_preset, "default", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, dracula_preset, "dracula", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, solarized_dark_preset, "solarized-dark", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, solarized_light_preset, "solarized-light", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, nord_preset, "nord", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, monokai_preset, "monokai", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, adventure_time_preset, "adventure-time", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, catppuccin_mocha_preset, "catppuccin-mocha", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, gruvbox_dark_preset, "gruvbox-dark", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, tokyo_night_preset, "tokyo-night", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, one_dark_preset, "one-dark", |p| {
    p.emit(representative_doc());
});

golden_themed!(themes, minimal_preset, "minimal", |p| {
    p.emit(representative_doc());
});

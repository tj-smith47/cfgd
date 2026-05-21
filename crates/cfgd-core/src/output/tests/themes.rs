//! Theme presets: one representative Doc rendered against every theme preset.
//! 5 cases — default, dracula, solarized-dark, solarized-light, minimal.
//!
//! Goldens are ANSI-stripped, so themes that differ only by color produce
//! identical output. The `minimal` preset additionally swaps glyphs
//! (✓ → +, ⚠ → !, ✗ → x, ◐ → .) so it diverges from the others.

use crate::golden_themed;
use crate::output::{Doc, Role};

golden_themed!(themes, default_preset, "default", |p| {
    let doc = Doc::new()
        .heading("Status")
        .kv_block([("Profile", "dev"), ("Modules", "12")])
        .section("Drift", |s| {
            s.status(Role::Warn, "shell-config")
                .status(Role::Warn, "git-config")
        })
        .section("Highlights", |s| {
            s.status(Role::Accent, "new release available")
                .status(Role::Secondary, "from registry: stable")
        });
    p.emit(doc);
});

golden_themed!(themes, dracula_preset, "dracula", |p| {
    let doc = Doc::new()
        .heading("Status")
        .kv_block([("Profile", "dev"), ("Modules", "12")])
        .section("Drift", |s| {
            s.status(Role::Warn, "shell-config")
                .status(Role::Warn, "git-config")
        })
        .section("Highlights", |s| {
            s.status(Role::Accent, "new release available")
                .status(Role::Secondary, "from registry: stable")
        });
    p.emit(doc);
});

golden_themed!(themes, solarized_dark_preset, "solarized-dark", |p| {
    let doc = Doc::new()
        .heading("Status")
        .kv_block([("Profile", "dev"), ("Modules", "12")])
        .section("Drift", |s| {
            s.status(Role::Warn, "shell-config")
                .status(Role::Warn, "git-config")
        })
        .section("Highlights", |s| {
            s.status(Role::Accent, "new release available")
                .status(Role::Secondary, "from registry: stable")
        });
    p.emit(doc);
});

golden_themed!(themes, solarized_light_preset, "solarized-light", |p| {
    let doc = Doc::new()
        .heading("Status")
        .kv_block([("Profile", "dev"), ("Modules", "12")])
        .section("Drift", |s| {
            s.status(Role::Warn, "shell-config")
                .status(Role::Warn, "git-config")
        })
        .section("Highlights", |s| {
            s.status(Role::Accent, "new release available")
                .status(Role::Secondary, "from registry: stable")
        });
    p.emit(doc);
});

golden_themed!(themes, minimal_preset, "minimal", |p| {
    let doc = Doc::new()
        .heading("Status")
        .kv_block([("Profile", "dev"), ("Modules", "12")])
        .section("Drift", |s| {
            s.status(Role::Warn, "shell-config")
                .status(Role::Warn, "git-config")
        })
        .section("Highlights", |s| {
            s.status(Role::Accent, "new release available")
                .status(Role::Secondary, "from registry: stable")
        });
    p.emit(doc);
});

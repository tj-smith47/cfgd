//! `--color never` withholds EVERY escape, not merely the colour ones.
//!
//! `ColorChoice::resolve` is the one styling decision a printer holds, and
//! `ThemedStyle::apply_to`'s `Display` is the one place a styled span becomes
//! bytes. A slot whose theme differentiator is an attribute rather than a
//! colour (`default`'s italic accent and type hint, `minimal`'s italic accent
//! and underlined secondary) is the shape that used to reach a colourless
//! stream as bare SGR — `cfgd --color never explain profile` wrote
//! `\x1b[3m<[]ShellAlias>\x1b[0m` into a pipe. `docs/cli-reference.md`
//! promises otherwise, so the gate answers for attributes too.

use std::sync::{Arc, Mutex};

use crate::output::live_row::RowStatus;
use crate::output::printer::ColorChoice;
use crate::output::renderer::Elapsed;
use crate::output::{
    CommandPair, Doc, KvPair, OutputFormat, OwnerLabel, Printer, Role, Theme, Verbosity,
};
use crate::test_helpers::EnvVarGuard;

fn raw(buf: &Arc<Mutex<String>>) -> String {
    // raw-capture-ok: the claim IS that no escape was written; captured_text strips
    buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Every renderer slot whose theme entry can carry an ATTRIBUTE, in one
/// document: a typed command row, a typed heading, an accent and a secondary
/// row (`minimal` spends only attributes on both), a withheld row's muted
/// detail, an annotated kv note, a linked kv value, an owner-token subject and
/// a prose paragraph.
fn every_attribute_carrying_slot() -> Doc {
    Doc::new()
        .heading_title_typed("Field", "aliases <[]ShellAlias>", "<[]ShellAlias>")
        .paragraph("Shell aliases this profile sets.")
        .command_list([CommandPair::typed(
            "aliases   <[]ShellAlias>",
            "<[]ShellAlias>",
            "Shell aliases this profile sets.",
        )])
        .kv_rows([
            KvPair::annotated("Source", "remote", "locked"),
            KvPair::linked("Docs", "configuration.md", "https://cfgd.io/configuration"),
        ])
        .status(Role::Accent, "attention without alarm")
        .status(Role::Secondary, "structural pivot")
        .status_with(Role::Skipped, "brew install jq", |f| {
            f.detail("no session manager")
                .duration(std::time::Duration::from_millis(120))
        })
        .status_owner_with(Role::Ok, OwnerLabel::new("module", "nvim"), |f| {
            f.verdict("Synced").detail("24 packages, 6 files")
        })
}

/// The buffered document, every preset, colour resolved the way production
/// resolves it for `--color never`.
#[test]
fn no_escape_reaches_a_stream_the_printer_decided_against() {
    let colors = ColorChoice::Never.resolve(&OutputFormat::Table);
    assert!(!colors, "ColorChoice::Never must resolve to no colour");
    for name in Theme::PRESET_NAMES {
        let theme = Theme::from_preset(name);
        let (p, buf) = Printer::for_test_with_theme(theme, Verbosity::Normal);
        p.emit(every_attribute_carrying_slot());
        p.flush();
        let out = raw(&buf);
        assert!(
            !out.contains('\u{1b}'),
            "preset {name} wrote an escape onto a colourless stream: {out:?}"
        );
        // Not vacuous: the same document really is painted with colour on.
        let (on, on_buf) =
            Printer::for_test_with_theme_colored(Theme::from_preset(name), Verbosity::Normal);
        on.emit(every_attribute_carrying_slot());
        on.flush();
        assert!(
            raw(&on_buf).contains('\u{1b}'),
            "preset {name} paints nothing even with colour on, so the claim above is vacuous"
        );
    }
}

/// `NO_COLOR` reaches the same decision through `ColorChoice::Auto`, so it
/// reaches the same stream. Asserted on the preset whose accent, secondary and
/// type slots are attributes alone — the one that could still leak.
#[test]
#[serial_test::serial]
fn no_color_reaches_the_same_stream_as_color_never() {
    let _term = EnvVarGuard::set("COLORTERM", "truecolor");
    let _no_color = EnvVarGuard::set("NO_COLOR", "1");
    let colors = ColorChoice::Auto.resolve(&OutputFormat::Table);
    assert!(!colors, "NO_COLOR must resolve Auto to no colour");
    let (p, buf) = Printer::for_test_with_theme(Theme::from_preset("minimal"), Verbosity::Normal);
    p.emit(every_attribute_carrying_slot());
    p.flush();
    let out = raw(&buf);
    assert!(
        !out.contains('\u{1b}'),
        "NO_COLOR left an escape on the stream: {out:?}"
    );
}

/// The live painter is the one surface no buffered document reaches: a row's
/// two slots are emphasised inside `LiveRow::set_action_status`, and a
/// withheld row's muted subject is the attribute-only span there.
#[test]
fn a_live_row_paints_no_escape_onto_a_colourless_stream() {
    for name in Theme::PRESET_NAMES {
        let (p, buf) =
            Printer::for_test_with_live_bars_themed(Theme::from_preset(name).with_colors(false));
        {
            let row = p.live_row_at(1);
            row.set_action_status(
                &RowStatus {
                    role: Role::Skipped,
                    subject: "brew-cask install firefox",
                    detail: Some("waiting on brew"),
                    detail_muted: true,
                    duration: Some(Elapsed::row(std::time::Duration::from_millis(120))),
                },
                30,
            );
        }
        let out = raw(&buf);
        assert!(
            !out.contains('\u{1b}'),
            "preset {name}'s live row wrote an escape onto a colourless stream: {out:?}"
        );
        assert!(
            out.contains("brew-cask install firefox"),
            "preset {name}'s live row painted nothing at all, so the claim above is vacuous: {out:?}"
        );
    }
}

/// Every escape `output/` writes comes from the gate, or says why not.
///
/// The gate is `ThemedStyle::apply_to`'s `Display` (`theme.rs`), the one place
/// a styled span becomes bytes. A second writer of an SGR or OSC sequence is a
/// second answer to the colour question, which is how an attribute-carrying
/// slot outlived `--color never` in the first place; a `console::Style`
/// applied outside `theme.rs` is the same bypass reached through the crate the
/// gate wraps.
#[test]
fn every_styled_span_reaches_bytes_through_the_one_gate() {
    fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    // The escape a WRITER emits: the introducer plus its CSI/OSC bracket. A
    // reader comparing a bare `\u{1b}` char (the strippers, the wrapper's
    // width scan) is not writing one and is deliberately not matched.
    const WRITES: &[&str] = &["\\x1b[", "\\x1b]", "\\u{1b}[", "\\u{1b}]"];
    const HATCH: &str = "// style-gate-ok:";

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/output");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    files.sort();
    assert!(
        files.len() > 20,
        "the walk found almost no sources under {}, so it proves nothing",
        root.display()
    );

    let mut hatched = 0usize;
    let mut offenders = Vec::new();
    for path in &files {
        // The gate itself, and the tests that assert about what it writes.
        if path.ends_with("theme.rs") || path.components().any(|c| c.as_os_str() == "tests") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let production = crate::test_helpers::production_slice(&src);
        let lines: Vec<&str> = production.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            let writes_escape = WRITES.iter().any(|needle| line.contains(needle));
            let applies_console_style =
                line.contains("console::Style") || line.contains("Style::new()");
            if !writes_escape && !applies_console_style {
                continue;
            }
            // The hatch is read off the contiguous comment block above the
            // line, not off the one line before it: a reason worth writing
            // rarely fits on one, and a two-line justification is not a
            // missing one.
            let mut hatched_here = line.contains(HATCH);
            let mut back = i;
            while !hatched_here && back > 0 {
                back -= 1;
                let prev = lines[back].trim_start();
                if prev.contains(HATCH) {
                    hatched_here = true;
                } else if !prev.starts_with("//") && !prev.starts_with('#') {
                    break;
                }
            }
            if hatched_here {
                hatched += 1;
            } else {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, code));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "an escape reached a stream without passing the colour gate:\n{}",
        offenders.join("\n")
    );
    // A floor, so a walk that stopped matching anything cannot pass silently:
    // the cursor's show/hide sequences, the OSC 8 composer and the emulated
    // terminal's mode change are the standing hatches.
    assert!(
        hatched >= 3,
        "the walk matched {hatched} hatched sites; it has stopped seeing the escapes it guards"
    );
}

//! The OSC 8 hyperlink slot: what the terminal is asked, what the theme
//! stamps, and what the kv renderer emits for a linked value.

use serial_test::serial;

use crate::output::{
    HYPERLINK_ENV_VARS, HYPERLINK_MIN_VTE_VERSION, HYPERLINK_TERM_PROGRAMS,
    HYPERLINK_TERMINAL_VARS, KvPair, Printer, Theme, Verbosity, terminal_supports_hyperlinks,
};
use crate::test_helpers::{EnvVarGuard, captured_text};

/// Clear every variable the detection reads, so a positive case is the one
/// variable the test set and the negative case is a terminal that named itself
/// not at all — including under a suite invoked from a hyperlink-capable
/// terminal of the developer's own.
fn cleared() -> Vec<EnvVarGuard> {
    HYPERLINK_ENV_VARS
        .iter()
        .map(|v| EnvVarGuard::unset(v))
        .collect()
}

/// The two tables and the clearing list are one population: a terminal added
/// to either detection list but not to `HYPERLINK_ENV_VARS` would leave the
/// negative case below asserting against the developer's own terminal.
#[test]
fn the_cleared_list_covers_every_variable_the_detection_reads() {
    assert!(
        HYPERLINK_ENV_VARS.contains(&"TERM_PROGRAM") && HYPERLINK_ENV_VARS.contains(&"VTE_VERSION"),
        "the two named variables are read directly and must be clearable"
    );
    for var in HYPERLINK_TERMINAL_VARS {
        assert!(
            HYPERLINK_ENV_VARS.contains(var),
            "{var} is detected but not clearable"
        );
    }
}

#[test]
#[serial]
fn a_term_program_that_names_a_hyperlink_terminal_is_detected() {
    let _cleared = cleared();
    for program in HYPERLINK_TERM_PROGRAMS {
        let _set = EnvVarGuard::set("TERM_PROGRAM", program);
        assert!(
            terminal_supports_hyperlinks(),
            "TERM_PROGRAM={program} names an OSC 8 terminal"
        );
    }
}

#[test]
#[serial]
fn a_terminal_naming_itself_by_its_own_variable_is_detected() {
    let _cleared = cleared();
    for var in HYPERLINK_TERMINAL_VARS {
        let _set = EnvVarGuard::set(var, "1");
        assert!(
            terminal_supports_hyperlinks(),
            "{var} names an OSC 8 terminal"
        );
    }
}

#[test]
#[serial]
fn vte_renders_hyperlinks_from_its_first_supporting_release() {
    let _cleared = cleared();
    {
        let _at = EnvVarGuard::set("VTE_VERSION", &HYPERLINK_MIN_VTE_VERSION.to_string());
        assert!(terminal_supports_hyperlinks(), "VTE 0.50 renders OSC 8");
    }
    let _below = EnvVarGuard::set("VTE_VERSION", &(HYPERLINK_MIN_VTE_VERSION - 1).to_string());
    assert!(
        !terminal_supports_hyperlinks(),
        "the release below the first supporting one does not"
    );
}

#[test]
#[serial]
fn a_terminal_that_names_itself_not_at_all_gets_no_hyperlink() {
    let _cleared = cleared();
    assert!(
        !terminal_supports_hyperlinks(),
        "an unidentified terminal reads the plain URL instead"
    );
}

/// A hyperlink is an escape sequence, so the colour decision governs it: a
/// printer that may not emit colour may not emit one either.
#[test]
fn a_colourless_theme_cannot_be_stamped_with_hyperlinks() {
    assert!(
        !Theme::default()
            .with_colors(false)
            .with_hyperlinks(true)
            .hyperlinks(),
        "colour off withholds the escape"
    );
    assert!(
        Theme::default()
            .with_colors(true)
            .with_hyperlinks(true)
            .hyperlinks()
    );
    // And in the other order: a stamped theme that later loses colour loses
    // the escape with it, so no call order can leave the two disagreeing.
    assert!(
        !Theme::default()
            .with_colors(true)
            .with_hyperlinks(true)
            .with_colors(false)
            .hyperlinks(),
        "colour withdrawn withdraws the escape"
    );
}

fn linked_row(theme: Theme) -> String {
    let (printer, buf) = Printer::for_test_with_theme_colored(theme, Verbosity::Normal);
    printer.kv_rows([KvPair::linked(
        "Docs",
        "docs/spec/module.md#fields",
        "https://example.test/docs/spec/module.md#fields",
    )]);
    printer.flush();
    // raw-capture-ok: the claim IS the OSC 8 escape — captured_text strips it
    buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[test]
fn a_linked_value_under_a_hyperlink_theme_wraps_its_text_in_osc_8() {
    let out = linked_row(Theme::default().with_colors(true).with_hyperlinks(true));
    assert!(
        out.contains(
            "\x1b]8;;https://example.test/docs/spec/module.md#fields\x1b\\\
             docs/spec/module.md#fields\x1b]8;;\x1b\\"
        ),
        "the row opens the URL behind the short path, got: {out:?}"
    );
}

/// Everywhere else the URL IS the value: a repo-relative path is something no
/// terminal auto-links and no reader can paste into a browser.
#[test]
fn a_linked_value_without_hyperlinks_prints_the_url_itself() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    printer.kv_rows([KvPair::linked(
        "Docs",
        "docs/spec/module.md#fields",
        "https://example.test/docs/spec/module.md#fields",
    )]);
    printer.flush();
    let out = captured_text(&buf);
    assert!(
        out.contains("Docs  https://example.test/docs/spec/module.md#fields"),
        "the value is the URL, got: {out:?}"
    );
    assert!(
        !out.contains('\x1b'),
        "a capture emits no escape, got: {out:?}"
    );
}

/// A physical-row break inside an OSC 8 escape would put its URL bytes on the
/// screen and leave the link half-open. The width accounting skips an OSC
/// string whole, exactly as it skips a CSI.
#[test]
fn wrapping_a_linked_value_never_splits_its_escape() {
    let url = "https://example.test/docs/spec/module.md#fields";
    let text = "docs/spec/module.md#a-very-long-anchor-name";
    let body = crate::output::osc8_hyperlink(url, text);
    let rows = crate::output::renderer::wrap::wrap_segment(&body, "Docs  ", "      ", Some(30));
    assert!(rows.len() > 1, "the row is narrow enough to wrap: {rows:?}");
    let opener = format!("\x1b]8;;{url}\x1b\\");
    assert!(
        rows[0].contains(&opener),
        "the opening escape stays whole on one row, got: {rows:?}"
    );
    for row in &rows {
        let visible = crate::output::strip_ansi(row);
        assert!(
            !visible.contains("8;;") && !visible.contains("example.test"),
            "no byte of the escape reaches the screen, got: {visible:?}"
        );
    }
}

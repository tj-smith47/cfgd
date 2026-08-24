//! The type span inside a `command_list` key ([`crate::output::CommandPair::typed`])
//! is the RENDERER's coat, not the caller's: the key is folded through
//! `cursor_safe` first and painted after, so a schema type reads as its own
//! column while the row stays byte-identical to a plain one with colour off.
use crate::output::{CommandPair, Doc, Printer, Theme, Verbosity};

fn dracula() -> Theme {
    Theme::from_preset("dracula").with_colors(true)
}

fn raw(buf: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
    // raw-capture-ok: the subject IS the accent SGR, which captured_text strips.
    buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[test]
#[serial_test::serial]
fn a_typed_command_row_paints_its_type_span_in_the_accent_slot() {
    let theme = dracula();
    let (p, buf) = Printer::for_test_with_theme_colored(theme.clone(), Verbosity::Normal);
    p.emit(Doc::new().command_list([CommandPair::typed(
        "files  <[]ModuleFileEntry>",
        "<[]ModuleFileEntry>",
        "Files this module deploys.",
    )]));
    p.flush();
    let out = raw(&buf);
    let expected = format!(
        "{}{}",
        theme.secondary.apply_to("files  "),
        theme.accent.apply_to("<[]ModuleFileEntry>")
    );
    assert!(
        out.contains(&expected),
        "the type span did not take the accent slot: {out:?}"
    );
}

#[test]
#[serial_test::serial]
fn a_type_span_the_key_does_not_carry_paints_nothing_extra() {
    let theme = dracula();
    let (p, buf) = Printer::for_test_with_theme_colored(theme.clone(), Verbosity::Normal);
    p.emit(Doc::new().command_list([CommandPair::typed("files", "<Missing>", "desc")]));
    p.flush();
    let out = raw(&buf);
    assert!(
        !out.contains(&theme.accent.apply_to("<Missing>").to_string()),
        "a span absent from the key must paint nothing: {out:?}"
    );
}

/// Colour off, a typed row is byte-identical to the untyped row it would
/// otherwise have been — which is what keeps every explain golden a golden.
#[test]
fn without_colour_a_typed_row_is_byte_identical_to_a_plain_one() {
    let render = |doc: Doc| {
        let (p, buf) = Printer::for_test();
        p.emit(doc);
        p.flush();
        crate::test_helpers::captured_text(&buf)
    };
    assert_eq!(
        render(Doc::new().command_list([CommandPair::typed(
            "files  <[]ModuleFileEntry>",
            "<[]ModuleFileEntry>",
            "Files this module deploys.",
        )])),
        render(
            Doc::new()
                .command_list([("files  <[]ModuleFileEntry>", "Files this module deploys.",)])
        ),
    );
}

//! The type span inside a `command_list` key ([`crate::output::CommandPair::typed`])
//! is the RENDERER's coat, not the caller's: the key is folded through
//! `cursor_safe` first and painted after, so a schema type reads as its own
//! column while the row's TEXT stays byte-identical to a plain one. With
//! colour off the type slot still emits its own attributes, which is the
//! product-wide NO_COLOR policy rather than a leak.
use crate::output::{CommandPair, Doc, Printer, Theme, Verbosity};

fn dracula() -> Theme {
    Theme::from_preset("dracula").with_colors(true)
}

fn raw(buf: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
    // raw-capture-ok: the subject IS the type slot's SGR, which captured_text strips.
    buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[test]
#[serial_test::serial]
fn a_typed_command_row_paints_its_type_span_in_the_type_hint_slot() {
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
        theme.type_hint.apply_to("<[]ModuleFileEntry>")
    );
    assert!(
        out.contains(&expected),
        "the type span did not take the type_hint slot: {out:?}"
    );
    // Not vacuous: dracula's type slot and its accent really are different
    // colours, so a span still painted accent would fail here rather than
    // pass by the two slots rendering alike.
    assert!(
        !out.contains(&theme.accent.apply_to("<[]ModuleFileEntry>").to_string()),
        "a schema type must no longer take the accent slot: {out:?}"
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
        !out.contains(&theme.type_hint.apply_to("<Missing>").to_string()),
        "a span absent from the key must paint nothing: {out:?}"
    );
}

/// The typed row and the plain row it replaced render the same TEXT — which is
/// what keeps every explain golden a golden, since a golden is captured
/// stripped.
#[test]
fn stripped_a_typed_row_is_byte_identical_to_a_plain_one() {
    let render = |doc: Doc| {
        // Not `for_test()`: it captures at Quiet, where a command list renders
        // nothing at all and every comparison between two rows holds vacuously.
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.emit(doc);
        p.flush();
        crate::test_helpers::captured_text(&buf)
    };
    assert_eq!(render(typed_doc()), render(plain_doc()));
}

/// Colour OFF is not the same claim as stripped. `theme.type_hint` carries
/// `.italic()` under the default theme, and an attribute-carrying slot still
/// emits its attrs-only SGR with colour off — NO_COLOR governs colour alone.
/// So the raw colours-off row differs from the plain one by exactly that:
/// an italic run around the type span, and no colour sequence anywhere.
#[test]
fn without_colour_a_typed_row_adds_only_an_attrs_only_run_around_the_span() {
    let render = |doc: Doc| {
        // Not `for_test()`: it captures at Quiet, where a command list renders
        // nothing at all and every comparison between two rows holds vacuously.
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.emit(doc);
        p.flush();
        raw(&buf)
    };
    let typed = render(typed_doc());
    let plain = render(plain_doc());
    assert!(
        typed.contains("\x1b[3m<[]ModuleFileEntry>\x1b[0m"),
        "the type slot's italic run is missing: {typed:?}"
    );
    // Every escape in the row is one of the two an attrs-only run is made of;
    // a colour would arrive as a `38;5`/`38;2` parameter run inside one of them.
    for seq in typed.split('\x1b').skip(1) {
        assert!(
            seq.starts_with("[3m") || seq.starts_with("[0m"),
            "a colour sequence survived NO_COLOR: {typed:?}"
        );
    }
    assert!(
        !plain.contains('\x1b'),
        "the plain row emits no escapes at all: {plain:?}"
    );
}

/// A key whose field name repeats the span's own text tints the LAST run — the
/// position every producer composes the type at. Matching the first occurrence
/// paints the field name and leaves the real type bare.
#[test]
#[serial_test::serial]
fn a_repeated_span_paints_the_key_tail_not_the_first_match() {
    let theme = dracula();
    let (p, buf) = Printer::for_test_with_theme_colored(theme.clone(), Verbosity::Normal);
    p.emit(Doc::new().command_list([CommandPair::typed(
        "<string>  <string>",
        "<string>",
        "A field whose own name reads like its type.",
    )]));
    p.flush();
    let out = raw(&buf);
    let expected = format!(
        "{}{}",
        theme.secondary.apply_to("<string>  "),
        theme.type_hint.apply_to("<string>")
    );
    assert!(
        out.contains(&expected),
        "the tail occurrence was not the painted one: {out:?}"
    );
}

fn typed_doc() -> Doc {
    Doc::new().command_list([CommandPair::typed(
        "files  <[]ModuleFileEntry>",
        "<[]ModuleFileEntry>",
        "Files this module deploys.",
    )])
}

fn plain_doc() -> Doc {
    Doc::new().command_list([("files  <[]ModuleFileEntry>", "Files this module deploys.")])
}

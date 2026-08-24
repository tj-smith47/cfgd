//! The role-tinted kv value ([`crate::output::KvPair::role_valued`]) reaches
//! the same rendered bytes down BOTH surfaces — the buffered `Doc` tree and
//! the streaming `SectionGuard` — because a status screen built one way and a
//! wide screen built the other must not colour the same fact differently.
use crate::output::{Doc, KvPair, Printer, Role, Theme, Verbosity};

fn dracula() -> Theme {
    Theme::from_preset("dracula").with_colors(true)
}

/// The value column as the renderer paints it: the key in `theme.secondary`,
/// the two-space gap unpainted, the value in `Role::Warn`'s own slot.
fn expected_warn_row(theme: &Theme) -> String {
    format!(
        "{}  {}",
        theme.secondary.apply_to("Status"),
        theme.warning.apply_to("Drifted")
    )
}

fn raw(buf: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
    // raw-capture-ok: the subject IS the role's SGR bytes, which captured_text strips.
    buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[test]
#[serial_test::serial]
fn a_doc_kv_row_carries_its_value_role_to_the_renderer() {
    let theme = dracula();
    let (p, buf) = Printer::for_test_with_theme_colored(theme.clone(), Verbosity::Normal);
    p.emit(Doc::new().kv_rows([KvPair::role_valued("Status", "Drifted", Role::Warn)]));
    p.flush();
    let out = raw(&buf);
    assert!(
        out.contains(&expected_warn_row(&theme)),
        "the Doc path lost the value role: {out:?}"
    );
}

#[test]
#[serial_test::serial]
fn a_streaming_kv_row_paints_the_same_bytes_as_the_doc_path() {
    let theme = dracula();
    let (p, buf) = Printer::for_test_with_theme_colored(theme.clone(), Verbosity::Normal);
    {
        let section = p.section("State");
        section.kv_rows([KvPair::role_valued("Status", "Drifted", Role::Warn)]);
    }
    p.flush();
    let out = raw(&buf);
    assert!(
        out.contains(&expected_warn_row(&theme)),
        "the streaming path lost the value role: {out:?}"
    );
}

/// Colour off, the tinted row is byte-identical to the plain row it would
/// otherwise have been. That is what keeps every existing golden still a
/// golden when a row gains a tint.
#[test]
fn without_colour_a_tinted_row_is_byte_identical_to_a_plain_one() {
    let render = |doc: Doc| {
        // Not `for_test()`: it captures at Quiet, where a kv block renders
        // nothing at all and the comparison below holds vacuously.
        let (p, buf) = Printer::for_test_at(Verbosity::Normal);
        p.emit(doc);
        p.flush();
        crate::test_helpers::captured_text(&buf)
    };
    assert_eq!(
        render(Doc::new().kv_rows([KvPair::role_valued("Status", "Drifted", Role::Warn)])),
        render(Doc::new().kv("Status", "Drifted")),
    );
}

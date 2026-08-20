//! Every renderer slot that carries text cfgd did not author folds it through
//! [`crate::output::cursor_safe`] — proven by what the terminal is LEFT
//! HOLDING, not by what was written to it.
//!
//! The poison is the pair a hostile value actually needs: a bare `\r` returns
//! the cursor to column 0, and `ESC [ 2 K` erases the row it lands on. Executed,
//! they leave the operator reading the attacker's replacement text on the line
//! that was supposed to describe the thing being approved; folded, the row still
//! carries its own description and the `\r` stands as visible `\x0d`.
//!
//! [`Printer::for_test_live_terminal`] is the only capture that can state this.
//! The scrollback constructor draws to a hidden target and the recording
//! constructor keeps every byte ever written — both would hold the description
//! whether or not the escapes were executed, so the assertion would pass
//! vacuously. The emulated screen EXECUTES cursor moves and line clears, so a
//! description still on it is a description nothing erased.
use crate::output::{Printer, Role, Verbosity};

/// `<text>` followed by the two-character return-and-erase, then the text a
/// hostile value would repaint the row with.
fn poisoned(text: &str) -> String {
    format!("{text}\r\u{1b}[2Krepainted")
}

/// The row survived intact: its own words are still on screen, the carriage
/// return is visible text rather than a cursor move, and no escape sequence
/// reached the screen in either executed or literal form.
fn assert_row_survived(held: &str, description: &str) {
    assert!(
        held.contains(description),
        "the row describing {description:?} was repainted away; screen holds: {held:?}"
    );
    assert!(
        held.contains("\\x0d"),
        "the carriage return was executed instead of shown; screen holds: {held:?}"
    );
    assert!(
        !held.contains("[2K"),
        "an erase sequence reached the screen; screen holds: {held:?}"
    );
}

#[test]
fn a_status_subject_cannot_repaint_the_line_it_is_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    printer.status_simple(Role::Ok, poisoned("server status: ok"));
    printer.flush();
    assert_row_survived(&screen.contents(), "server status: ok");
}

#[test]
fn a_status_qualifier_cannot_repaint_the_line_it_is_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    printer
        .status(Role::Warn, "curl")
        .qualifier(poisoned("missing"));
    printer.flush();
    assert_row_survived(&screen.contents(), "missing");
}

#[test]
fn a_status_detail_cannot_repaint_the_line_it_is_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    printer
        .status(Role::Fail, "install")
        .detail(poisoned("exit code 1"));
    printer.flush();
    assert_row_survived(&screen.contents(), "exit code 1");
}

#[test]
fn a_kv_key_and_value_cannot_repaint_the_rows_they_are_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    printer.kv("Server status", poisoned("ok"));
    printer.kv(poisoned("Device"), "d-1");
    printer.flush();
    let held = screen.contents();
    assert_row_survived(&held, "Server status");
    assert_row_survived(&held, "d-1");
}

/// The announce arm this covers fires only when there is NO bar to hold the
/// label — a redirected run — so the emulated screen cannot reach it and the
/// scrollback capture is the right one: what it holds IS the log file the
/// operator opens afterwards, escapes and all.
#[test]
fn an_output_window_label_cannot_repaint_the_log_it_announces_into() {
    let (printer, buf) = Printer::for_test_live_scrollback();
    let window = printer.output_window(poisoned("running hook"));
    window.finish_ok("done");
    printer.flush();
    let held = crate::test_helpers::captured_text(&buf);
    assert_row_survived(&held, "running hook");
}

/// The `\n` exemption, which the fold exists around: a subject really is
/// allowed to be two sentences (a brew caveat is), and the renderer lays the
/// second one out as an indented continuation. Escaping it would print a
/// literal `\x0a` down the middle of the line instead.
#[test]
fn a_multi_line_subject_still_renders_as_an_indented_continuation() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    printer.status_simple(Role::Info, "first sentence\nsecond sentence");
    printer.flush();
    let held = crate::test_helpers::captured_text(&buf);
    assert!(
        !held.contains("\\x0a"),
        "the newline was escaped instead of laid out: {held:?}"
    );
    let lines: Vec<&str> = held.lines().collect();
    assert_eq!(lines.len(), 2, "expected two physical lines: {held:?}");
    assert!(lines[0].contains("first sentence"), "got: {:?}", lines[0]);
    assert!(
        lines[1].starts_with("  ") && lines[1].trim() == "second sentence",
        "continuation is not indented: {:?}",
        lines[1]
    );
}

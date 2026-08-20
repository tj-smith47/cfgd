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
/// return stands after them as visible text rather than as a cursor move, and
/// the text the poison meant to repaint with follows it IMMEDIATELY — nothing
/// of the erase sequence in between, in executed or in literal form.
///
/// The three claims are read off ONE row rather than off the whole screen, and
/// in order, because each of the three ways the fold can be wrong has to fail a
/// different one of them: dropped entirely, or reduced to
/// [`crate::escape_control_chars`], leaves the description erased and the row
/// unfindable; reduced to [`crate::output::strip_ansi`], it leaves the `\r`
/// executed and no `\x0d` to split on; and an escape that reached the screen
/// as literal text lands between the split point and `repainted`.
fn assert_row_survived(held: &str, description: &str) {
    let row = held
        .lines()
        .find(|l| l.contains(description))
        .unwrap_or_else(|| {
            panic!("the row describing {description:?} was repainted away; screen holds: {held:?}")
        });
    let (before, after) = row.split_once("\\x0d").unwrap_or_else(|| {
        panic!("the carriage return was executed instead of shown; row: {row:?}")
    });
    assert!(
        before.contains(description),
        "the row's own words no longer stand ahead of the return; row: {row:?}"
    );
    assert!(
        after.trim_start().starts_with("repainted"),
        "an erase sequence reached the screen between the shown return and the \
         text it meant to repaint with; row: {row:?}"
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
    // The KEY's own word, never the value beside it: `d-1` is written after
    // the erase lands, so it stands on the row whether the key was folded or
    // not and would pin only the value fold a second time.
    assert_row_survived(&held, "Device");
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

/// `alert` and `deprecation` are the one role that survives `Verbosity::Quiet`,
/// so on a `-o json` run this line can be the only thing on stderr — with no
/// neighbouring output to make a repaint look wrong.
#[test]
fn an_advisory_cannot_repaint_the_line_it_is_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    printer.alert(poisoned("source acme is unreachable"));
    printer.flush();
    assert_row_survived(&screen.contents(), "source acme is unreachable");
}

#[test]
fn a_bullet_a_hint_and_a_code_block_cannot_repaint_the_lines_they_are_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    {
        let section = printer.section("Review");
        section.bullet(poisoned("run: install.sh"));
        section.hint(poisoned("cfgd module show nvim"));
        section.code_block([poisoned("packages:")]);
    }
    printer.flush();
    let held = screen.contents();
    assert_row_survived(&held, "run: install.sh");
    assert_row_survived(&held, "cfgd module show nvim");
    assert_row_survived(&held, "packages:");
}

/// A note renders only at `Verbose`, so it needs the verbosity-taking
/// constructor rather than the default `Normal` one — at `Normal` the screen
/// is empty and every assertion below would fail on the row lookup rather than
/// pass vacuously.
#[test]
fn a_note_cannot_repaint_the_line_it_is_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal_at(Verbosity::Verbose, 24, 120);
    printer.note(poisoned("the module declares no packages"));
    printer.flush();
    assert_row_survived(&screen.contents(), "the module declares no packages");
}

/// A table pads its columns in terminal columns, so the fold has to happen
/// BEFORE the widths are taken. Both a header and a cell are poisoned, and the
/// row below them still aligns to the column the folded text really occupies —
/// a width measured over the unfolded string would put `aligned` somewhere
/// else.
#[test]
fn a_table_header_and_cell_cannot_repaint_the_rows_they_are_written_on() {
    use crate::output::renderer::Table;
    let (printer, screen) = Printer::for_test_live_terminal(24, 200);
    printer.table(
        Table::new([poisoned("Name"), "Command".to_string()])
            .row([poisoned("nvim"), "aligned".to_string()])
            .row(["zsh".to_string(), "second".to_string()]),
    );
    printer.flush();
    let held = screen.contents();
    assert_row_survived(&held, "Name");
    assert_row_survived(&held, "nvim");
    let column_of = |needle: &str| {
        let row = held
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("row {needle:?} missing; screen holds: {held:?}"));
        row.find(needle)
            .map(|i| crate::output::measure_width(&row[..i]))
            .unwrap_or_else(|| panic!("needle {needle:?} missing in {row:?}"))
    };
    assert_eq!(
        column_of("aligned"),
        column_of("second"),
        "the second column must be padded against the folded width; screen holds: {held:?}"
    );
}

#[test]
fn a_command_list_description_cannot_repaint_the_line_it_is_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    printer.command_list([("cfgd apply", poisoned("converge this machine"))]);
    printer.flush();
    assert_row_survived(&screen.contents(), "converge this machine");
}

#[test]
fn a_heading_and_a_section_name_cannot_repaint_the_lines_they_are_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    printer.heading(poisoned("Module nvim"));
    {
        let section = printer.section(poisoned("Files"));
        section.status_simple(Role::Ok, "init.lua");
    }
    printer.flush();
    let held = screen.contents();
    assert_row_survived(&held, "Module nvim");
    assert_row_survived(&held, "Files");
}

/// A child process's own output reaches this slot, and the window in front of
/// it settles a `\r`-rewritten progress line rather than escaping it — so the
/// renderer's fold is what a caller reaching the slot directly gets.
#[test]
fn a_streamed_child_line_cannot_repaint_the_line_it_is_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    printer.stream_line_at(0, &poisoned("Fetching neovim"));
    printer.flush();
    assert_row_survived(&screen.contents(), "Fetching neovim");
}

/// A `\r` that opens a CRLF is a line break, not a cursor move: a detail
/// captured on Windows lays out as the same indented continuation a `\n` gets,
/// with no `\x0d` at the end of each line. The lone-`\r` half of the rule is
/// pinned by every test above, which is the same slot with the return standing
/// on its own.
#[test]
fn a_crlf_detail_lays_out_as_a_continuation_rather_than_a_visible_return() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    printer
        .status(Role::Fail, "install")
        .detail("exit code 1\r\nsee the log");
    printer.flush();
    let held = crate::test_helpers::captured_text(&buf);
    assert!(
        !held.contains("\\x0d"),
        "the CRLF's return was escaped instead of read as part of the line \
         break: {held:?}"
    );
    let lines: Vec<&str> = held.lines().collect();
    assert_eq!(lines.len(), 2, "expected two physical lines: {held:?}");
    assert!(lines[0].contains("exit code 1"), "got: {:?}", lines[0]);
    assert!(
        lines[1].trim() == "see the log",
        "continuation is not the second line: {:?}",
        lines[1]
    );
}

/// The fold's own contract for the two shapes a carriage return arrives in,
/// stated on the function rather than through a slot: a CRLF is one line
/// break, a lone `\r` is a cursor move and stays visible. Spelled as exact
/// equalities so a fold that collapsed BOTH — restoring the old render at the
/// cost of the guarantee — fails here rather than passing a `contains` check.
#[test]
fn a_crlf_is_a_line_break_while_a_lone_return_stays_visible() {
    use crate::output::cursor_safe;
    assert_eq!(cursor_safe("one\r\ntwo"), "one\ntwo");
    assert_eq!(cursor_safe("one\rtwo"), "one\\x0dtwo");
    assert_eq!(cursor_safe("trailing\r"), "trailing\\x0d");
    // The escape sequence goes first, so a `\r` left holding a `\n` across a
    // consumed `ESC [ K` is still read as the line break it opens.
    assert_eq!(cursor_safe("one\r\u{1b}[K\ntwo"), "one\ntwo");
}

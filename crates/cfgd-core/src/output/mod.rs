//! Typed-component output system — the sole interface for terminal output
//! across cfgd. See `.claude/specs/2026-05-14-output-system-redesign-design.md`
//! for the design.
//!
//! # Three sanitation policies for text cfgd did not author
//!
//! A module's caveat, a remote's manifest string, a tool's captured stderr: any
//! of them can carry bytes that repaint the very line describing them. Which
//! policy a surface takes is decided by what the surface is FOR, and the three
//! are not interchangeable.
//!
//! **FOLD** ([`cursor_safe`]) is the default and covers every renderer slot
//! carrying caller text: the status subject, qualifier, detail, label, marker
//! and target path; the always-visible advisory; both halves of a kv row plus
//! its annotation; both columns of a `command_list`; the bullet, hint, note,
//! paragraph, code-block line and streamed child line; every table header and
//! cell; a heading and a section's name and empty-state placeholder; and every
//! live-region label, which funnels through `spinner.rs`'s composer (that one
//! folds and then PAINTS, so the fold cannot eat its own coat). Two slots sit
//! outside the renderer and fold at their own write: `structured.rs`'s human
//! stderr diagnostic under the selector formats, and
//! `tracing_writer.rs`'s sink, which folds the whole formatted event on both
//! destinations and so covers every `warn!`/`error!` site by construction — a
//! subscriber taking that writer passes `.with_ansi(false)`, since the fold
//! strips ANSI and telling an `ESC [ 0 m` from an `ESC [ 2 K` would mean a
//! second policy with its own parser.
//!
//! **ESCAPE** ([`crate::escape_control_chars`]) is what a PRE-APPROVAL surface
//! takes instead, because the fold STRIPS an ANSI sequence and a screen the
//! operator is approving from has to SHOW it — a value carrying `\x1b[2K` is
//! approved and then written to disk with those bytes in it. That covers the
//! module review summary, the permission-change bullets, every prompt message
//! and `prompt_select`'s option list (`inquire` is a terminal writer no
//! renderer fold reaches, which is also why that call takes its answer back by
//! INDEX: the drawn list is escaped while the returned option stays byte-exact,
//! and matching drawn text against the raw list would miss precisely the option
//! that carried a control character). The two RAW CONTENT renderers in
//! `raw.rs` escape per line INSIDE the renderer for the same reason, so no call
//! site handing them module content sanitizes by hand.
//!
//! **STRIP** is the third, and `prompt_text`'s `default` is its only slot: a
//! default is pre-filled into the editable buffer and handed back AS the
//! answer, so escaping it would write `\x0d` into the value while leaving it
//! raw lets a proposal repaint the line offering it. Dropping the character is
//! the only resolution under which what is DRAWN and what is RETURNED are the
//! same string.
//!
//! The policies compose rather than fight: after escaping there is no ESC byte
//! left to strip and no control character left to escape, so a later fold is
//! the identity on it.
//!
//! Never folded, and each a decision: every `plain()` form and every persisted
//! or `-o json` string (a payload stays byte-exact), and `raw.rs`'s `data_line`
//! (the machine channel). Five terminal writers take no policy at all —
//! inherited CHILD STDIO (the child owns the terminal and cfgd is not in the
//! byte path; a PIPED child is sanitized at `window.rs`), clap's own error and
//! help writer (it echoes the argv the user just typed), `cfgd-csi`'s JSON log
//! line (its serializer is its sanitizer, and folding it would cost the
//! consumer a parseable payload), the Windows service's file and Event Log
//! layers (a log file, not a terminal), and the default panic hook (cfgd
//! installs none, and no production panic interpolates untrusted text — one
//! that ever quotes a module- or remote-supplied value escapes at the
//! interpolation, nothing downstream of `std`'s hook being able to fold it).

pub mod role;
pub use role::Role;

pub mod verbosity;
pub use verbosity::{OutputFormat, Verbosity};

pub mod theme;
pub use theme::Theme;

pub mod component;
pub use component::{
    CommandPair, Component, ConfigHeader, HeaderModule, KvPair, config_header_rows,
    modules_header_row, modules_header_row_for,
};

pub mod renderer;

mod cursor;
pub use cursor::claim_termination_signals;

pub mod printer;

/// The `TERM_PROGRAM` values of terminals that render OSC 8 hyperlinks.
const HYPERLINK_TERM_PROGRAMS: &[&str] = &["iTerm.app", "WezTerm", "vscode", "ghostty", "Hyper"];

/// Terminals that identify themselves by a variable of their own rather than
/// by `TERM_PROGRAM`; presence alone is the identification.
const HYPERLINK_TERMINAL_VARS: &[&str] = &[
    "WT_SESSION",
    "KITTY_WINDOW_ID",
    "ALACRITTY_WINDOW_ID",
    "ALACRITTY_SOCKET",
    "KONSOLE_VERSION",
];

/// The minimum VTE release that renders OSC 8, in VTE's own `MMmmpp` encoding
/// (0.50.0). GNOME Terminal, Tilix and every other VTE embedder reports it.
const HYPERLINK_MIN_VTE_VERSION: u32 = 5000;

/// The variables [`terminal_supports_hyperlinks`] reads BY NAME, as opposed to
/// through [`HYPERLINK_TERMINAL_VARS`]. Together the two are every variable the
/// predicate touches, which is what a test clears to assert the negative;
/// `every_variable_the_detection_reads_is_one_a_test_can_clear` walks the
/// predicate's own source, so a new direct read fails until it is listed here.
#[cfg(test)]
const HYPERLINK_DIRECT_VARS: &[&str] = &["TERM_PROGRAM", "VTE_VERSION", "TMUX", "STY"];

/// Whether the terminal this process writes to renders OSC 8 hyperlinks.
///
/// DETECTED, never a flag: a hyperlink is a per-terminal capability, not a
/// presentation knob a reader chooses. The answer is the terminal's own
/// identification — `TERM_PROGRAM` for the emitters that set it, and the
/// per-terminal variables for those that do not. A terminal not on the list
/// prints the plain URL a linked value falls back to, so an unknown terminal
/// costs a reader nothing but a longer line. Colour is a separate gate, judged
/// by the caller ([`Theme::with_hyperlinks`]): a printer that may not emit
/// colour may not emit this escape either.
///
/// A terminal MULTIPLEXER (`TMUX`, or `STY` for `screen`) answers no whatever
/// the terminal underneath says. Every variable above is inherited by a pane,
/// so an old tmux under iTerm2 identifies as iTerm2 while passing none of the
/// escape through — and an escape a multiplexer swallows leaves the reader
/// with neither a link nor a URL, the one outcome the plain-URL fallback
/// exists to prevent. tmux forwards OSC 8 from 3.4 on, but it publishes no
/// version anywhere this predicate can read, so the conservative answer is the
/// only one available: a multiplexed session gets the URL as text.
pub fn terminal_supports_hyperlinks() -> bool {
    // A multiplexer's panes inherit the outer terminal's identification, so
    // this is asked FIRST — the vars below would otherwise answer for a
    // terminal whose escapes never reach the screen.
    if std::env::var_os("TMUX").is_some() || std::env::var_os("STY").is_some() {
        return false;
    }
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if HYPERLINK_TERM_PROGRAMS.contains(&program.as_str()) {
        return true;
    }
    if HYPERLINK_TERMINAL_VARS
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|value| !value.is_empty()))
    {
        return true;
    }
    // VTE-based terminals (GNOME Terminal, Tilix, …) render OSC 8 from 0.50.
    std::env::var("VTE_VERSION")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|v| v >= HYPERLINK_MIN_VTE_VERSION)
}

/// The OSC 8 hyperlink wrapping `text` so a click opens `url`; the text stays
/// the terminal's visible bytes. The ONE spelling of the escape, read by the
/// kv renderer's linked slot.
pub(crate) fn osc8_hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}
pub use printer::{ColorChoice, DocCapture, Printer, PromptAnswer};

pub mod owner_label;
pub use owner_label::OwnerLabel;

pub mod phase_label;
pub use phase_label::PhaseLabel;

pub mod title_label;
pub use title_label::TitleLabel;

mod accent_heading;
use accent_heading::AccentHeading;

pub mod section_guard;
pub use section_guard::SectionGuard;

pub mod status_builder;
pub use status_builder::StatusBuilder;

pub mod spinner;
pub use spinner::{ProgressBar, Spinner};

pub mod window;
pub use window::OutputWindow;

// Every item is `pub(crate)`: a row is a live-region primitive the reconciler
// draws its phase tree with, never a published surface.
pub(crate) mod live_row;

pub mod lane;
pub use lane::LaneOutput;

pub mod process;
pub use process::CommandOutput;

pub mod prompts;

pub mod raw;

pub mod tracing_writer;
pub use tracing_writer::{LiveTracingWriter, LocalTimeOfDay};

pub mod doc;
pub use doc::{Doc, SectionBuilder, StatusFields};

/// Strip ANSI CSI escape sequences (`ESC [ ... m`) from a string.
///
/// Used as a sanitization boundary for any text that originates outside the
/// renderer (e.g. captured stderr from an external tool, error `Display`
/// output, user-supplied detail strings) before it lands in a styled line.
/// A stray foreign `\x1b[0m` mid-detail would otherwise prematurely terminate
/// the role styling of the subject; foreign color escapes would paint
/// subsequent terminal output until the next reset.
///
/// Walks `char`s (escape sequences are all ASCII, so this is safe across
/// multi-byte UTF-8 glyphs like `✓ ✗ — →`). Recognizes the three shapes a
/// child process actually emits:
///
/// - **CSI** — `ESC [`, parameter and intermediate bytes, then a final byte in
///   `0x40..=0x7E`. Ending a CSI on `m` alone is not a partial implementation
///   but a swallowing one: `ESC [ 2 J` (clear screen) and `ESC [ H` (home)
///   carry no `m`, so an SGR-only stripper consumes the whole remainder of the
///   line as if it were part of the escape. `nvim --headless` emits both.
/// - **OSC** — `ESC ]`, a payload, then `BEL` or the two-char ST (`ESC \`).
/// - **Two-byte escapes** — `ESC 7`, `ESC =`, and the charset selectors
///   (`ESC ( B`), which carry one further byte.
///
/// An unterminated escape is swallowed to end-of-string, which is the safer
/// outcome at a sanitization boundary — a malicious unterminated escape
/// shouldn't paint anything.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for inner in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&inner) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(inner) = chars.next() {
                    if inner == '\u{07}' {
                        break;
                    }
                    if inner == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some('(') | Some(')') | Some('*') | Some('+') => {
                chars.next();
            }
            _ => {}
        }
    }
    out
}

/// Fold caller-supplied text into a form that cannot move the terminal cursor.
///
/// The ONE sanitation every renderer slot that carries text cfgd did not author
/// applies — a status subject, its qualifier, its detail, an advisory, a kv key
/// or value, a bullet, a hint, a note, a code-block line, a table cell, a
/// heading and a section name. The guarantee is narrow and total: what comes back occupies exactly the
/// columns it displays, on the line the renderer put it on. Nothing in it can
/// reposition the cursor, erase what is already on screen, or repaint the
/// description of the very thing an operator is being asked to approve.
///
/// [`strip_ansi`] alone does not give that. It consumes sequences introduced by
/// `ESC`, so a lone `\r`, a `\x08`, or a C1 `U+009B` walks straight through it
/// and still repaints the line it is written on. [`crate::escape_control_chars`]
/// alone does not give it either: it escapes `\n`, which the status slot lays
/// out as an indented continuation line, so a two-sentence brew caveat would
/// render a literal `\x0a` down the middle.
///
/// So: strip the escape sequences, then escape every control character that
/// survives EXCEPT `\n`. A tab is escaped with the rest — column alignment is
/// computed in terminal columns and a tab jumps to a stop that count cannot
/// predict, which mis-pads every field after it.
///
/// A `\r` that immediately precedes a `\n` is part of that line break rather
/// than a cursor move, so a CRLF collapses to the exempt `\n` instead of
/// rendering a visible `\x0d` at the end of every line of a Windows-captured
/// message. A LONE `\r` is still escaped — that one is precisely the repaint
/// this fold exists to stop.
///
/// Renderer-owned styling is applied AFTER this fold, never before, or the
/// fold eats the renderer's own SGR (see `finalize_subject`).
pub fn cursor_safe(s: &str) -> String {
    crate::escape_control_chars_except_newline(&strip_ansi(s).replace("\r\n", "\n"))
}

/// The rendered column width of `text`, in terminal columns.
///
/// The ONE width measurement available outside the renderer, so a caller that
/// pre-computes an alignment column measures exactly what the renderer pads
/// against: ANSI escapes count as zero columns and a multi-byte glyph (`✓`,
/// `—`) counts as the columns a terminal gives it, neither of which
/// `str::len()` answers.
pub fn measure_width(text: &str) -> usize {
    console::measure_text_width(text)
}

/// Collapse a multi-line error message into a single subject-safe line.
///
/// `Renderer::write_line` debug-asserts on bodies containing `\n`, so any
/// captured error (`io::Error`, `CfgdError`, command stderr) that gets
/// pumped into a `Printer::status[_simple]` subject or detail must be
/// flattened first. The first non-empty line becomes the head; subsequent
/// non-empty lines are joined with ` — ` so trailing systemctl/launchd
/// context (e.g. `"See system logs and 'systemctl status …' for details."`)
/// stays visible on a single physical row. Bounded through `bounded_lines`,
/// so a slot that must be one row cannot become forty screens of a child's
/// progress output.
pub fn collapse_to_subject_line(err: impl std::fmt::Display) -> String {
    bounded_lines(&err.to_string()).join(" — ")
}

/// The DETAIL-slot twin of [`collapse_to_subject_line`], for a message that may
/// carry a child process's captured output.
///
/// Same bound, different glue: the lines are kept as lines, because
/// `renderer::compose_status` already lays a `\n`-carrying detail out as the
/// subject's first line plus indented continuations. Joining them with ` — `
/// instead — which `collapse_to_subject_line` must, its destination being one
/// physical row — spends the row's own subject/detail separator on text that is
/// not a subject boundary, and an error chain comes out as
/// `Caused by: — failed to download …`.
///
/// Reach for this wherever a captured message reaches a detail slot and nothing
/// downstream needs one physical line. The STORED copies are untouched:
/// `journal_fail`, `ActionResult.error` and the `-o json` payload keep the full
/// text, exactly as `compliance::system_checks_from_diffs` keeps its persisted
/// detail out of `drift_detail`.
pub fn captured_output_detail(err: impl std::fmt::Display) -> String {
    bounded_lines(&err.to_string()).join("\n")
}

/// The ONE bound on a captured message reaching a rendered slot: its
/// non-empty lines, trimmed, capped at [`window::VISIBLE_LINES`] — the same
/// count the live output window kept under the spinner while the command ran.
///
/// A child's stderr is unbounded and mostly progress: `cargo install` prefixes
/// its diagnosis with forty `Downloaded <crate>` lines, so an uncapped fold put
/// twenty-one physical lines in one action row and pushed the run's own header
/// off the screen. The head is kept because it names what failed; the elision
/// is taken out of the MIDDLE and never off the tail, because cargo, npm, pip
/// and brew all put the progress first and the diagnosis last. Never a byte
/// cap, which cuts mid-sentence.
fn bounded_lines(s: &str) -> Vec<String> {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.len() <= window::VISIBLE_LINES {
        return lines.into_iter().map(str::to_string).collect();
    }
    // head + marker + tail == VISIBLE_LINES, so the bound is what a reader counts.
    let tail = window::VISIBLE_LINES - 2;
    let elided = lines.len() - 1 - tail;
    let mut out = vec![lines[0].to_string(), format!("… {elided} more lines")];
    out.extend(lines[lines.len() - tail..].iter().map(|l| l.to_string()));
    out
}

/// The ONE spelling of a planned-vs-actual mismatch: `want: <expected>,
/// have: <actual>`. [`status_builder::StatusBuilder::drift`] and
/// `doc::StatusFields::drift` both compose their detail slot through this —
/// two DISPLAY producers that would otherwise drift apart (two different
/// spellings were observed: `want {}, have {}` with no colon, and this
/// canonical form baked straight into a status subject instead of the
/// detail slot). Reach for this only where the string is RENDERED to a
/// human, never where it is stored: `crate::compliance::system_checks_from_diffs`
/// composes its persisted/hashed `ComplianceCheck.detail` with a plain
/// `format!("expected {}, actual {}", …)` on purpose, because that string is
/// serialized into the `-o json` payload and into `compliance_snapshots`'
/// content-hash digest — a display-only spelling change there breaks a
/// consumer parsing `.detail` and re-hashes every stored snapshot on
/// upgrade for no real drift. `cli::compliance` composes this function at
/// the point it prints a check for a human, not at the point the check is
/// built.
pub fn drift_detail(expected: impl std::fmt::Display, actual: impl std::fmt::Display) -> String {
    format!("want: {expected}, have: {actual}")
}

/// The word a drift row reads for a stored `resource_type`.
///
/// The stored types are a state-matching vocabulary and never move
/// (`drift_events.resource_type` is half of the UPSERT key); two of them are
/// re-worded here. `env-var` is internal jargon rather than a word a reader of
/// the report would use, so the display says `env` and the store keeps
/// `env-var`. `env` is the managed env FILE, and letting it inherit that same
/// word puts `env: EDITOR` and `env: /home/u/.cfgd.env` side by side in one
/// report — two different kinds under one label — so the file row reads
/// `env file`. Every other kind is already the word, and passes through.
#[must_use]
pub fn drift_kind_label(resource_type: &str) -> &str {
    match resource_type {
        "env-var" => "env",
        "env" => "env file",
        other => other,
    }
}

/// Whether a kind belongs to the shell surface — the managed env file, its rc
/// source line, and the declared env vars and aliases inside it.
///
/// One predicate rather than four literals at each site, because three
/// surfaces (`diff`, `verify`, `status`) each have to answer the same question
/// twice: how to name the row, and whether the file-freshness row beside it is
/// redundant.
#[must_use]
pub fn is_shell_drift_kind(resource_type: &str) -> bool {
    matches!(resource_type, "env" | "env-rc" | "env-var" | "alias")
}

/// The subject a drift/verify item row reads.
///
/// A shell row names its kind with a colon (`env: EDITOR`, `alias: gs`) so the
/// kind reads as a label on the item rather than as the first word of a
/// sentence; every other kind keeps the `<kind> <id>` shape the id itself
/// completes (`package ripgrep`, `file ~/.zshrc`). One composer so the three
/// surfaces cannot spell one row two ways.
#[must_use]
pub fn drift_item_subject(resource_type: &str, resource_id: &str) -> String {
    let label = drift_kind_label(resource_type);
    if is_shell_drift_kind(resource_type) {
        format!("{label}: {resource_id}")
    } else {
        format!("{label} {resource_id}")
    }
}

/// The DISPLAY wording of one drift row's `want`/`have` operands.
///
/// The ONE derivation of how each resource kind words the two halves of
/// [`drift_detail`], and what every display producer folds a stored or freshly
/// computed pair through before rendering it. It exists because the detectors
/// each invented their own absence word for the same fact — a missing package
/// was `missing` from the reconciler's verify pass and `not installed` from
/// the live scan, so one host answered two spellings depending on which
/// command asked. The [`crate::Absence`] enum is the vocabulary; this is where
/// each kind is mapped onto it.
///
/// DISPLAY only. The stored `drift_events` pair, the `-o json` `shape` field
/// and every compliance-snapshot string keep the producer's own literals —
/// those are matched and hashed, and re-wording one re-hashes every stored
/// snapshot for no real drift.
#[must_use]
pub fn drift_operands(resource_type: &str, expected: &str, actual: &str) -> (String, String) {
    (
        drift_operand(resource_type, expected),
        drift_operand(resource_type, actual),
    )
}

/// One side of [`drift_operands`].
fn drift_operand(resource_type: &str, operand: &str) -> String {
    let absent = match resource_type {
        // A manager or package could exist on this host and the manager says
        // it does not — `NotInstalled`, whichever detector phrased it.
        "package" | "manager" => crate::Absence::NotInstalled,
        // Everything else the user DECLARED and the machine does not hold.
        _ => crate::Absence::Missing,
    };
    match operand.trim() {
        // A system value the host has no setting for at all renders as an
        // empty operand — `have: ` states nothing where the whole point of
        // the row is what the machine holds instead.
        "" => absent.as_str().to_string(),
        "missing" | "not installed" | "missing or changed" => absent.as_str().to_string(),
        other => other.to_string(),
    }
}

/// The terse cause a drift row ends with when the report has no room for both
/// operands (`cfgd status <module>`'s per-surface rows and its `-o wide`
/// inventories).
///
/// The verbose pair states two content hashes a reader cannot act on; this
/// says what KIND of divergence was found and leaves the bytes to `cfgd diff`.
/// It lives beside [`drift_operands`] because the two answer one question at
/// two lengths and must agree about the absence word they reach for. Anything
/// outside the known shapes keeps the producer's own phrasing rather than
/// being flattened into a word that would describe it wrongly.
#[must_use]
pub fn drift_terse_cause(resource_type: &str, expected: &str, actual: &str) -> String {
    let actual = drift_operand(resource_type, actual);
    if actual == crate::Absence::Missing.as_str() || actual == crate::Absence::NotInstalled.as_str()
    {
        return actual;
    }
    if actual.starts_with("content differs") {
        return "content differs".to_string();
    }
    if crate::parse_loose_version(expected).is_some()
        && crate::parse_loose_version(&actual).is_some()
    {
        return "version mismatch".to_string();
    }
    actual
}

/// Whether the managed env FILE's own freshness row is redundant in a report
/// that also carries `kinds`.
///
/// `want: current, have: stale` names no value and explains nothing a reader
/// can act on; when the per-item rows beneath it already say WHICH declared
/// env var or alias the file is missing, the freshness row is the same finding
/// stated a second time and less usefully. It survives only when it stands
/// alone — a file that is stale for a reason no item row names (a hand-added
/// line, a reordering) still has to be reported by something.
#[must_use]
pub fn env_file_row_is_redundant<'a>(kinds: impl IntoIterator<Item = &'a str>) -> bool {
    kinds.into_iter().any(|k| matches!(k, "env-var" | "alias"))
}

/// Rendered width cap for [`condense_script_label`], in `char`s.
///
/// Eighty columns is the terminal width a status subject can assume without
/// wrapping on a standard, unresized terminal; `render_status_immediate`
/// still appends a role glyph and an optional `(Ns)` duration suffix after
/// the subject, so the cap leaves that trailing room rather than filling the
/// full width with script text alone.
pub const SCRIPT_LABEL_MAX_CHARS: usize = 80;

/// The fewest `char`s a budget may cut a script label to: below this a row
/// names no command at all, so a narrow terminal cuts the marker's room
/// rather than the body's.
pub const SCRIPT_LABEL_MIN_CHARS: usize = 16;

/// Condense a `ScriptEntry::run_str()` body into a single-line, width-bounded
/// label for status subjects and error messages.
///
/// An inline multi-line `run:` script handed straight to a status subject
/// (spinner label, `status_simple`, `finish_ok`/`finish_fail`) trips
/// `Renderer::write_line`'s `!body.contains('\n')` debug_assert; a release
/// build instead prints the whole body down the terminal as if it were one
/// status line. Takes the first non-empty, trimmed line and appends an
/// ellipsis marker when either that line was truncated to fit
/// `SCRIPT_LABEL_MAX_CHARS`, or further non-empty content follows it.
///
/// `str::lines()` only recognizes `\n` and `\r\n` as terminators, so a lone
/// `\r` (a classic-Mac line ending, or any other stray carriage return) would
/// otherwise ride along inside the "first line" untouched; it is scrubbed
/// explicitly so the result can never carry a `\r` forward.
pub fn condense_script_label(body: &str) -> String {
    condense_script_label_within(body, SCRIPT_LABEL_MAX_CHARS)
}

/// [`condense_script_label`] cut at `max_chars` instead of the fixed cap, for
/// a subject that knows its report's budget: a script row is the one action
/// shape whose operand is prose, and a cap that cannot see the terminal left
/// it the widest row of every report on any screen narrower than the cap.
pub fn condense_script_label_within(body: &str, max_chars: usize) -> String {
    let max_chars = max_chars.max(SCRIPT_LABEL_MIN_CHARS);
    let mut lines = body.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first_raw) = lines.next() else {
        return String::new();
    };
    let first: String = first_raw.chars().filter(|&c| c != '\r').collect();
    let more_lines = lines.next().is_some();

    if first.chars().count() <= max_chars {
        if more_lines {
            format!("{first} …")
        } else {
            first
        }
    } else {
        // The one column cut a script label meets, and it retreats to a token
        // like every other cut of text cfgd authored: `clamp_at_token` counts
        // its own marker inside `max`, so the budget is widened by the column
        // this function's marker takes. `markdown-preview.nvim/a…` is what a
        // raw take made of a path whose `/` sat one column back.
        renderer::wrap::clamp_at_token(&first, max_chars + 1)
    }
}

/// Build a stable-shaped error Doc for `bail!`-on-emit-then-fail sites.
/// Carries an `error` category key so structured consumers (`-o json`) can
/// route a failure without parsing the human message; `name` joins it only
/// when the caller actually has one — a resource-less failure (a plain
/// `?`-propagated error with no CLI handler attached) has no name to report,
/// and an empty string in the payload would read as "the name is empty"
/// rather than "there is no name". Any extra fields in `extras` (object
/// literal expected) are merged into the payload alongside them.
pub fn error_doc(
    name: &str,
    error_kind: &str,
    message: impl Into<String>,
    extras: serde_json::Value,
) -> Doc {
    let mut payload = serde_json::json!({ "error": error_kind });
    if !name.is_empty()
        && let serde_json::Value::Object(payload_map) = &mut payload
    {
        payload_map.insert("name".to_string(), serde_json::Value::String(name.into()));
    }
    if let serde_json::Value::Object(extra_map) = extras
        && let serde_json::Value::Object(payload_map) = &mut payload
    {
        for (k, v) in extra_map {
            payload_map.insert(k, v);
        }
    }
    let mut doc = Doc::new().status(Role::Fail, message).with_data(payload);
    doc.is_error = true;
    doc
}

pub mod render_doc;

pub mod structured;
pub use structured::validate_jsonpath_expr;

#[cfg(feature = "test-helpers")]
pub mod test_capture;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod error_doc_tests {
    use super::error_doc;

    #[test]
    fn a_real_name_is_present_in_the_payload() {
        let doc = error_doc("mymod", "already_exists", "boom", serde_json::json!({}));
        let json = doc.data_or_self_json();
        assert_eq!(json["error"], "already_exists");
        assert_eq!(json["name"], "mymod");
    }

    #[test]
    fn an_empty_name_is_omitted_rather_than_serialized_as_an_empty_string() {
        let doc = error_doc("", "internal", "boom", serde_json::json!({}));
        let json = doc.data_or_self_json();
        assert_eq!(json["error"], "internal");
        assert!(
            json.get("name").is_none(),
            "an empty name must not appear in the payload at all: {json:?}"
        );
        assert_eq!(
            json.as_object().map(serde_json::Map::len),
            Some(1),
            "the payload must carry only the error key: {json:?}"
        );
    }

    #[test]
    fn extras_still_merge_alongside_an_omitted_name() {
        let doc = error_doc(
            "",
            "clone_failed",
            "boom",
            serde_json::json!({ "url": "https://example.com/acme.git" }),
        );
        let json = doc.data_or_self_json();
        assert_eq!(json["error"], "clone_failed");
        assert_eq!(json["url"], "https://example.com/acme.git");
        assert!(json.get("name").is_none());
    }
}

#[cfg(test)]
mod drift_detail_tests {
    use super::drift_detail;

    #[test]
    fn composes_the_canonical_want_have_spelling() {
        assert_eq!(drift_detail("1.2.3", "1.2.0"), "want: 1.2.3, have: 1.2.0");
    }

    /// `String`, `&str` and `&String` all reach it unchanged (`impl
    /// Display` rather than `impl Into<String>`) — every producer call
    /// site holds a different one of the three.
    #[test]
    fn accepts_string_and_str_and_ref_string_uniformly() {
        let owned = String::from("present");
        assert_eq!(
            drift_detail(owned.clone(), "absent"),
            drift_detail(owned.as_str(), "absent")
        );
        assert_eq!(
            drift_detail(&owned, "absent"),
            drift_detail(owned, "absent")
        );
    }

    #[test]
    fn a_question_mark_placeholder_composes_like_any_other_value() {
        // status.rs's drift event renders `?` when a value was never
        // recorded (`Option::as_deref().unwrap_or("?")`) — the composer
        // does not special-case it.
        assert_eq!(drift_detail("?", "present"), "want: ?, have: present");
    }
}

#[cfg(test)]
mod drift_vocabulary_tests {
    use super::{
        drift_item_subject, drift_kind_label, drift_operands, drift_terse_cause,
        env_file_row_is_redundant,
    };

    #[test]
    fn the_stored_env_var_type_reads_as_env_and_every_other_kind_passes_through() {
        assert_eq!(drift_kind_label("env-var"), "env");
        for kind in ["alias", "env-rc", "package", "file", "system"] {
            assert_eq!(drift_kind_label(kind), kind);
        }
    }

    /// The item rows and the FILE row are two different kinds; one label for
    /// both is what put `env: EDITOR` and `env: /home/u/.cfgd.env` in one list.
    #[test]
    fn the_managed_env_file_row_is_labelled_apart_from_the_item_rows() {
        assert_eq!(drift_kind_label("env"), "env file");
        assert_ne!(drift_kind_label("env"), drift_kind_label("env-var"));
        assert_eq!(
            drift_item_subject("env", "/home/u/.cfgd.env"),
            "env file: /home/u/.cfgd.env"
        );
    }

    #[test]
    fn a_shell_row_labels_its_kind_with_a_colon_and_the_rest_do_not() {
        assert_eq!(drift_item_subject("env-var", "EDITOR"), "env: EDITOR");
        assert_eq!(drift_item_subject("alias", "gs"), "alias: gs");
        assert_eq!(drift_item_subject("package", "ripgrep"), "package ripgrep");
        assert_eq!(
            drift_item_subject("system", "sysctl.vm.swappiness"),
            "system sysctl.vm.swappiness"
        );
    }

    /// The point of the derivation: two detectors word one missing package
    /// two ways, and both rows read the same after the fold.
    #[test]
    fn one_absent_package_reads_the_same_whichever_detector_worded_it() {
        assert_eq!(
            drift_operands("package", "installed", "missing"),
            drift_operands("package", "installed", "not installed")
        );
        assert_eq!(
            drift_operands("package", "installed", "missing").1,
            "not installed"
        );
    }

    #[test]
    fn a_declared_item_the_machine_does_not_hold_reads_as_missing() {
        assert_eq!(
            drift_operands("env-var", "export EDITOR=\"vim\"", "missing or changed").1,
            "missing"
        );
        // An unset system value arrives as an empty operand; `have: ` states
        // nothing where the whole row is about what the machine holds.
        assert_eq!(drift_operands("system", "/opt/bin", "").1, "missing");
    }

    #[test]
    fn an_unrecognized_phrase_keeps_the_producers_own_wording() {
        assert_eq!(
            drift_operands(
                "file",
                "content matches source",
                "content differs from source"
            ),
            (
                "content matches source".to_string(),
                "content differs from source".to_string()
            )
        );
    }

    /// The three shapes a cause is condensed into, and the pass-through for a
    /// producer whose phrasing matches none of them.
    #[test]
    fn a_terse_cause_names_the_kind_of_divergence() {
        assert_eq!(
            drift_terse_cause(
                "file",
                "content matches source",
                "content differs from source"
            ),
            "content differs"
        );
        assert_eq!(
            drift_terse_cause("package", "installed", "missing"),
            "not installed"
        );
        assert_eq!(
            drift_terse_cause("package", "14.1.0", "13.0.0"),
            "version mismatch"
        );
        assert_eq!(
            drift_terse_cause("file", "present", "unreadable: permission denied"),
            "unreadable: permission denied"
        );
    }

    #[test]
    fn the_env_file_freshness_row_survives_only_when_it_stands_alone() {
        assert!(!env_file_row_is_redundant(["env", "env-rc"]));
        assert!(env_file_row_is_redundant(["env", "alias"]));
        assert!(env_file_row_is_redundant(["env-var", "env"]));
    }
}

#[cfg(test)]
mod collapse_tests {
    use super::window::VISIBLE_LINES;
    use super::{captured_output_detail, collapse_to_subject_line};

    /// 40 `Downloaded <crate>` lines followed by the one sentence that says
    /// why: what `cargo install` really writes to stderr.
    fn cargo_style_stderr() -> String {
        let mut lines = vec!["Updating crates.io index".to_string()];
        lines.extend((0..40).map(|i| format!("Downloaded crate-{i} v1.0.0")));
        lines.push("error: feature `edition2024` is required".to_string());
        lines.join("\n")
    }

    #[test]
    fn a_captured_dump_is_bounded_and_elided_from_the_middle() {
        for rendered in [
            captured_output_detail(cargo_style_stderr()),
            collapse_to_subject_line(cargo_style_stderr()),
        ] {
            let parts: Vec<&str> = if rendered.contains('\n') {
                rendered.lines().collect()
            } else {
                rendered.split(" — ").collect()
            };
            assert_eq!(
                parts.len(),
                VISIBLE_LINES,
                "a captured dump must be bounded at the window's own ceiling: {rendered:?}"
            );
            assert_eq!(
                parts[0], "Updating crates.io index",
                "the head names what failed and is kept"
            );
            assert_eq!(
                parts[VISIBLE_LINES - 1],
                "error: feature `edition2024` is required",
                "the diagnosis is last on every manager and must survive"
            );
            assert!(
                parts[1].starts_with('…') && parts[1].contains("more lines"),
                "the elision is marked, not silent: {rendered:?}"
            );
            assert!(
                !rendered.contains("Downloaded crate-0 "),
                "progress noise from the head must be elided: {rendered:?}"
            );
        }
    }

    #[test]
    fn a_detail_fold_keeps_its_lines_as_lines() {
        let input = "failed to compile\nCaused by:\nfeature is required";
        assert_eq!(
            captured_output_detail(input),
            input,
            "the renderer lays continuation lines out itself; the ` — ` glue is \
             the row's subject/detail separator and means something else"
        );
        assert_eq!(
            collapse_to_subject_line(input),
            "failed to compile — Caused by: — feature is required",
            "the subject fold still owes its caller one physical row"
        );
    }

    #[test]
    fn single_line_passes_through_trimmed() {
        assert_eq!(collapse_to_subject_line("simple error"), "simple error");
        assert_eq!(
            collapse_to_subject_line("  padded  "),
            "padded",
            "outer whitespace must be trimmed"
        );
    }

    #[test]
    fn multi_line_joined_with_em_dash() {
        let input = "Transport endpoint is not connected\n\
                     See system logs and 'systemctl status kubelet.service' for details.";
        assert_eq!(
            collapse_to_subject_line(input),
            "Transport endpoint is not connected — \
             See system logs and 'systemctl status kubelet.service' for details."
        );
    }

    #[test]
    fn leading_and_trailing_blank_lines_skipped() {
        let input = "\n\n   \nfirst real line\nsecond real line\n   \n\n";
        assert_eq!(
            collapse_to_subject_line(input),
            "first real line — second real line"
        );
    }

    #[test]
    fn interior_blank_lines_skipped_and_inner_lines_trimmed() {
        let input = "  head  \n\n   \n\t  body  \t\n";
        assert_eq!(collapse_to_subject_line(input), "head — body");
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(collapse_to_subject_line(""), "");
        assert_eq!(collapse_to_subject_line("   "), "");
        assert_eq!(collapse_to_subject_line("\n\n\n"), "");
    }

    #[test]
    fn display_impl_consumed_not_just_strings() {
        let err = std::io::Error::other("first line\nsecond line");
        assert_eq!(
            collapse_to_subject_line(&err),
            "first line — second line",
            "any Display value (e.g. io::Error) must work"
        );
    }
}

#[cfg(test)]
mod condense_script_label_tests {
    use super::condense_script_label;

    /// Every column cut of text cfgd itself AUTHORED retreats to a token: the
    /// script label (`condense_script_label_within`), a live row's repaint and
    /// a spinner label (`clamp_at_token`). A raw `chars().take(n)` cut the
    /// hero's `postApply` row at `markdown-preview.nvim/a…`, one column past
    /// the `/` the retreat would have stopped on, and read as a directory
    /// named `a`. The behavioural half drives both cutters over a body whose
    /// cut lands one character past a space, a comma and a `/`; the source
    /// half walks `output/` and `reconciler/format.rs` for a column-cut idiom
    /// (`chars().take(`, `wrap::clamp(`, `clamp_line(`, `truncate_str(`)
    /// outside the cutters' own definitions, hatched with
    /// `// plain-clamp-ok: <why>` where the text is FOREIGN (a captured
    /// child's line) or the width is a table column's. An operand list is
    /// never cut at all — a subject names every operand and wraps — so it
    /// reaches none of them.
    fn on_boundary_text(body: &str, kept: &str) -> bool {
        body.starts_with(kept)
            && (kept.ends_with([' ', ',', '/']) || body[kept.len()..].starts_with([' ', ',', '/']))
    }

    #[test]
    fn every_column_cut_of_cfgd_authored_text_retreats_to_a_token() {
        use super::super::output::renderer::wrap::clamp_at_token;
        use super::condense_script_label_within;

        // Each body's boundary sits exactly one column before the cut, the
        // shape a raw take gets wrong: the kept text is the whole token run
        // up to the boundary, and the marker stands where the boundary was.
        let cases = [
            (
                "nvim --headless \"+Lazy! load go.nvim\" \"+GoInstallBinaries\" +qa && echo done",
                " +qa",
            ),
            (
                "brew install neovim, ripgrep, fd, bat, eza, cargo, zoxide, node",
                ", zoxide",
            ),
            (
                "mp=\"$HOME/.local/share/nvim/lazy/markdown-preview.nvim/app\" && cd \"$mp\"",
                "/app",
            ),
        ];
        for (body, needle) in cases {
            let at = body.find(needle).unwrap();
            let boundary = needle.chars().next().unwrap();
            let max = body[..at].chars().count() + 3;
            let raw: String = body.chars().take(max).collect();
            assert!(
                !on_boundary_text(body, &raw),
                "the case is not one a raw take gets right"
            );
            let label = condense_script_label_within(body, max);
            // The marker stands ON a boundary: the kept text ends with one,
            // or the next character of the body is one.
            let on_boundary =
                |cut: &str| on_boundary_text(body, cut.strip_suffix('…').unwrap_or(cut));
            assert!(
                on_boundary(&label),
                "the script label cut past the `{boundary}` a column back: {label:?}"
            );
            assert!(label.chars().count() <= max + 1);
            let live = clamp_at_token(body, max + 1);
            assert!(
                on_boundary(&live),
                "the live-row cut past the `{boundary}` a column back: {live:?}"
            );
        }

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        let mut pending = vec![root.join("output")];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && path.file_name().is_none_or(|n| n != "tests.rs")
                {
                    files.push(path);
                }
            }
        }
        files.push(root.join("reconciler/format.rs"));
        let idioms = [
            "chars().take(",
            "wrap::clamp(",
            "clamp_line(",
            "truncate_str(",
        ];
        let mut seen = 0usize;
        let mut offenders = Vec::new();
        for path in files {
            if path.ends_with("renderer/wrap.rs") {
                continue;
            }
            let body = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = body.lines().collect();
            let mut in_tests = false;
            for (n, line) in lines.iter().enumerate() {
                let code = line.trim_start();
                // The file's own test module opens at column 0; an indented
                // `#[cfg(test)]` gates one item inside production code.
                if line.starts_with("#[cfg(test)]") {
                    in_tests = true;
                }
                if in_tests || code.starts_with("//") || code.starts_with("use ") {
                    continue;
                }
                if !idioms.iter().any(|i| code.contains(i)) {
                    continue;
                }
                seen += 1;
                let hatched = [line, lines.get(n.wrapping_sub(1)).copied().unwrap_or("")]
                    .iter()
                    .any(|l| l.contains("// plain-clamp-ok:"));
                if !hatched {
                    offenders.push(format!("{}:{}: {}", path.display(), n + 1, code.trim()));
                }
            }
        }
        assert!(
            seen >= 2,
            "the walk no longer reaches the hatched cut sites"
        );
        assert!(
            offenders.is_empty(),
            "a column cut of cfgd-authored text retreats to a token through \
             `wrap::clamp_at_token`; a cut of foreign bytes or a table cell \
             carries `// plain-clamp-ok: <why>`:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn multi_line_keeps_only_first_line_plus_ellipsis() {
        let script = "echo start\napt-get update\napt-get install -y neovim\necho done";
        assert_eq!(condense_script_label(script), "echo start …");
    }

    #[test]
    fn leading_blank_lines_skipped() {
        let script = "\n\n   \necho hello\necho world";
        assert_eq!(condense_script_label(script), "echo hello …");
    }

    #[test]
    fn single_line_no_trailing_ellipsis() {
        assert_eq!(condense_script_label("echo hello"), "echo hello");
    }

    #[test]
    fn whitespace_only_input_returns_empty() {
        assert_eq!(condense_script_label("   \n\t\n   "), "");
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(condense_script_label(""), "");
    }

    #[test]
    fn long_single_line_truncated_at_cap() {
        let long = "x".repeat(200);
        let label = condense_script_label(&long);
        // +1 for the appended `…`; a retreat may give columns back.
        assert!(label.chars().count() <= super::SCRIPT_LABEL_MAX_CHARS + 1);
        assert!(label.ends_with('…'));
        assert!(!label.contains('\n') && !label.contains('\r'));
    }

    #[test]
    fn multibyte_utf8_truncated_without_panic() {
        // Every char is 3 bytes in UTF-8 (☃ U+2603); a byte-index truncation
        // at SCRIPT_LABEL_MAX_CHARS would land mid-character and panic.
        let long = "☃".repeat(200);
        let label = condense_script_label(&long);
        assert!(label.chars().count() <= super::SCRIPT_LABEL_MAX_CHARS + 1);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn lone_carriage_return_never_survives() {
        // A bare `\r` (no following `\n`) is not a line terminator to
        // `str::lines()`, so it stays embedded in the "first line" unless
        // scrubbed explicitly.
        let script = "echo hi\rthere\nnext line";
        let label = condense_script_label(script);
        assert!(!label.contains('\r'));
        assert!(!label.contains('\n'));
    }

    #[test]
    fn never_contains_newline_or_carriage_return() {
        for input in ["a\nb\nc", "\r\n\r\n", "line\r\n", "\r", "a", ""] {
            let label = condense_script_label(input);
            assert!(
                !label.contains('\n') && !label.contains('\r'),
                "condense_script_label({input:?}) = {label:?} carried a line terminator"
            );
        }
    }
}

#[cfg(test)]
mod strip_ansi_tests {
    use super::strip_ansi;

    #[test]
    fn plain_text_passes_through_unchanged() {
        assert_eq!(strip_ansi("hello world"), "hello world");
        assert_eq!(strip_ansi(""), "");
        assert_eq!(strip_ansi("✓ ✗ — →"), "✓ ✗ — →");
    }

    #[test]
    fn red_sgr_pair_stripped() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn compound_sgr_stripped() {
        assert_eq!(strip_ansi("\x1b[1;31mbold-red\x1b[0m"), "bold-red");
    }

    #[test]
    fn prefix_and_suffix_preserved_around_stripped_sgr() {
        assert_eq!(
            strip_ansi("prefix\x1b[31mcolored\x1b[0msuffix"),
            "prefixcoloredsuffix"
        );
    }

    #[test]
    fn incomplete_escape_swallowed_to_eos() {
        assert_eq!(strip_ansi("safe\x1b[31"), "safe");
        assert_eq!(strip_ansi("\x1b[31"), "");
    }

    #[test]
    fn two_byte_escape_consumed_leaving_no_raw_escape_byte() {
        // A raw `\x1b` surviving sanitization is the thing this function
        // exists to prevent, so a two-byte escape is consumed, not passed on.
        assert_eq!(strip_ansi("a\x1bXb"), "ab");
        assert_eq!(strip_ansi("a\x1b7b"), "ab");
    }

    #[test]
    fn charset_selector_consumes_its_trailing_byte() {
        assert_eq!(strip_ansi("\x1b(Bplain"), "plain");
    }

    #[test]
    fn non_sgr_csi_does_not_swallow_the_rest_of_the_line() {
        // `nvim --headless` emits clear-screen and cursor-home; neither ends
        // in `m`, and an SGR-only stripper ate everything that followed.
        assert_eq!(strip_ansi("\x1b[2J\x1b[Hcleared"), "cleared");
        assert_eq!(strip_ansi("\x1b[1;1Hhome"), "home");
        assert_eq!(strip_ansi("before\x1b[Kafter"), "beforeafter");
    }

    #[test]
    fn osc_title_sequence_stripped_at_bel_and_at_st() {
        assert_eq!(strip_ansi("\x1b]0;window title\x07kept"), "kept");
        assert_eq!(strip_ansi("\x1b]0;window title\x1b\\kept"), "kept");
    }
}

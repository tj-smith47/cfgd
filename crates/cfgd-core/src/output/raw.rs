//! Raw renderers — diff, syntax_highlight, data_line.
//!
//! `diff` and `syntax_highlight` render CONTENT cfgd did not author — a
//! module's own file, a model's generated manifest — on screens an operator
//! reads before approving what they describe. They take the ESCAPE policy
//! rather than the renderer fold: every content line goes through
//! [`crate::escape_control_chars`] BEFORE any styling is applied, so a lone
//! `\r` or an `ESC [ 2 K` inside the content stands as visible `\x0d` /
//! `\x1b[2K` instead of repainting the rows above it. Escaping rather than
//! folding is what the pre-approval rule asks for: `strip_ansi` would DELETE
//! the escape, and a screen somebody approves from has to show the bytes that
//! are about to be written to disk. A tab escapes with everything else — the
//! policy is one pass with no exemptions, and a source line that renders
//! `\x09` where it held a tab still says what it holds.
//!
//! `data_line` is the machine channel and stays byte-exact, per the fold
//! catalog's data-channel exemption.
//!
//! `diff` and `syntax_highlight` are exempt from the WRAP invariant every
//! other emission gets: their content is multi-line and word-wrapping a diff
//! row or a highlighted source line mid-token would no longer be the content
//! it was built from. They are NOT exempt from indentation — each nests
//! under whatever depth its caller's section opened, via
//! `Renderer::emit_raw_block`, the same as any other emission. `data_line`
//! stays at depth 0 unconditionally: it is the machine channel (`cfgd
//! config get`-shaped output consumed by other programs), not a rendered
//! line that could nest under a human-facing section.

use similar::{ChangeTag, TextDiff};
use syntect::easy::HighlightLines;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

use crate::escape_control_chars;

use super::component::{ScriptStep, ScriptsForm};
use super::renderer::{Renderer, Writer};
use super::{Verbosity, cursor_safe};

/// The syntax a declared script body is highlighted as. Every lifecycle hook
/// runs through a shell, so one language covers the whole population.
const SCRIPT_BODY_LANG: &str = "bash";

// style-gate-ok: syntect writes its own foreground runs, which the gate never
// wrote and so cannot close; this is the reset that closes them, appended only
// under `render_syntax_highlight`'s colour check.
const SYNTECT_RESET: &str = "\x1b[0m";

impl Renderer {
    /// Render a unified diff using `theme.diff_*` styles. Lines starting with
    /// `+` are themed diff_add, `-` themed diff_remove, others diff_context.
    /// Nests at `depth`, like any other emission.
    ///
    /// Each row is escaped before the theme paints it, per this module's
    /// ESCAPE policy — the content is a module's file, not cfgd's own text.
    pub fn render_diff(&self, w: &dyn Writer, depth: usize, old: &str, new: &str) {
        let diff = TextDiff::from_lines(old, new);
        let mut lines = Vec::new();
        for change in diff.iter_all_changes() {
            let (sign, style) = match change.tag() {
                ChangeTag::Insert => ("+", &self.theme.diff_add),
                ChangeTag::Delete => ("-", &self.theme.diff_remove),
                ChangeTag::Equal => (" ", &self.theme.diff_context),
            };
            // A CRLF is one line break, not a cursor move — escaping its
            // return would put a visible `\x0d` at the end of every row of a
            // Windows-authored file. A LONE return is not a line break, even
            // though the line splitter treats it as one, so it keeps its
            // escape and stands on the screen as the cursor move it is.
            let value = change.value();
            let body = match value.strip_suffix("\r\n") {
                Some(head) => head,
                None => value.strip_suffix('\n').unwrap_or(value),
            };
            let body = escape_control_chars(body);
            lines.push(style.apply_to(format!("{sign}{body}")).to_string());
        }
        // One block per render, so a diff is never split across two of
        // indicatif's clear/redraw cycles.
        self.emit_raw_block(w, depth, &lines);
    }

    /// Render syntax-highlighted code. Caller passes the `lang` hint (e.g.,
    /// "yaml", "rust", "json"); falls back to plain text on unknown. Nests
    /// at `depth`, like any other emission.
    ///
    /// Every line is escaped before syntect sees it, per this module's ESCAPE
    /// policy — `cfgd generate` shows a model-authored manifest through here
    /// with an Accept/Reject prompt seven lines below it, and highlighting an
    /// unescaped line hands its control bytes straight to the terminal
    /// between syntect's own SGR runs. `str::lines` already drops the return
    /// of a CRLF, so only a LONE return is left to escape.
    ///
    /// The palette is the printer's own theme (`Theme::syntect_theme`), so a
    /// `--theme dracula` run highlights in Dracula, the palette the rest of the
    /// screen is drawn in.
    pub fn render_syntax_highlight(
        &self,
        w: &dyn Writer,
        depth: usize,
        code: &str,
        lang: &str,
        syntax_set: &SyntaxSet,
    ) {
        // One block per render, so a highlighted body is never split across
        // two of indicatif's clear/redraw cycles.
        self.emit_raw_block(w, depth, &self.highlight_lines(code, lang, syntax_set));
    }

    /// The rows [`Self::render_syntax_highlight`] emits, without emitting them
    /// — for a caller composing them into a larger block whose other rows it
    /// also owns (a script step's marker line above its body).
    pub(crate) fn highlight_lines(
        &self,
        code: &str,
        lang: &str,
        syntax_set: &SyntaxSet,
    ) -> Vec<String> {
        let unstyled = || {
            code.lines()
                .map(escape_control_chars)
                .collect::<Vec<String>>()
        };
        // syntect writes its truecolor escapes itself, without passing through
        // `ThemedStyle::apply_to`, so a colour decision enforced only at style
        // lookup does not reach it and `cfgd diff --no-color` / `NO_COLOR=1`
        // still wrote escapes into the reader's pipe.
        if !self.theme.colors() {
            return unstyled();
        }
        let syntax = syntax_set
            .find_syntax_by_token(lang)
            .or_else(|| syntax_set.find_syntax_by_extension(lang))
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
        // The preset renders a body plain (`minimal`), or its asset did not
        // parse; either way the lines still have to be shown.
        let Some(theme) = self.theme.syntect_theme() else {
            return unstyled();
        };
        let mut h = HighlightLines::new(syntax, theme);
        let mut lines = Vec::new();
        for line in code.lines() {
            let line = escape_control_chars(line);
            // Every line is fed with its newline: these syntaxes are the
            // `_newlines` set, whose pop patterns match at the line ending, so a
            // line handed over without one closes no context. The newline is
            // layout the emitter owns, so it comes back off before the row is
            // kept.
            let fed = format!("{line}\n");
            // An empty source line highlights to a bare reset, which the
            // emitter then indents: a row carrying nothing but whitespace and
            // an escape. The blank line a body declares stays blank, and the
            // highlighter still sees it, because a blank line is what closes a
            // context in some grammars (a Markdown paragraph).
            if line.is_empty() {
                let _ = h.highlight_line(&fed, syntax_set);
                lines.push(String::new());
                continue;
            }
            lines.push(match h.highlight_line(&fed, syntax_set) {
                // A line that did not close its last foreground run leaves it in
                // force over whatever the command prints next.
                Ok(mut ranges) => {
                    // The fed newline comes off the last range rather than off
                    // the assembled bytes: syntect gives it a range of its own,
                    // and trimming the string would leave that range's colour
                    // escape standing with no text under it.
                    if let Some((_, text)) = ranges.last_mut() {
                        *text = text.strip_suffix('\n').unwrap_or(text);
                    }
                    ranges.retain(|(_, text)| !text.is_empty());
                    format!(
                        "{}{SYNTECT_RESET}",
                        as_24_bit_terminal_escaped(&ranges, false)
                    )
                }
                // The highlighter gave up on this one line. Its TEXT is what an
                // operator approves from, so the line stands unstyled; empty
                // ranges would have printed a reset and dropped the content.
                Err(_) => line,
            });
        }
        // Returned rather than emitted: highlighting is expensive and touches
        // no render state, so the lock is taken only around the emission.
        lines
    }

    /// Render the script steps one lifecycle hook declares.
    ///
    /// The whole hook is one emission, which is what lets the renderer own the
    /// blank line between steps: a composer that wrote the separator itself
    /// would be painting layout, and a per-step emission could not tell a
    /// first step from a later one.
    ///
    /// Under [`ScriptsForm::Full`] each step is its muted marker line and then
    /// its body, highlighted in the printer's own preset. Under
    /// [`ScriptsForm::Condensed`] each step is one plain row: the body is a
    /// lossy one-line label its composer already cut, and a coat on it would
    /// read as a verdict no check gave it.
    pub fn render_script_steps(
        &self,
        w: &dyn Writer,
        depth: usize,
        steps: &[ScriptStep],
        form: ScriptsForm,
        syntax_set: &SyntaxSet,
    ) {
        if self.verbosity == Verbosity::Quiet || steps.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            if form == ScriptsForm::Full && index > 0 {
                lines.push(String::new());
            }
            if let Some(marker) = &step.marker {
                lines.push(self.theme.muted.apply_to(cursor_safe(marker)).to_string());
            }
            match form {
                ScriptsForm::Full => {
                    lines.extend(self.highlight_lines(&step.body, SCRIPT_BODY_LANG, syntax_set));
                }
                // Escaped, not folded: this is the body that will run, and a
                // reader has to see the bytes it holds.
                ScriptsForm::Condensed => lines.push(escape_control_chars(&step.body)),
            }
        }
        self.emit_raw_block(w, depth, &lines);
    }
}

impl super::Printer {
    /// Diff renderer. Goes to stderr. Nests at whatever depth the caller's
    /// section opened, the same as every other Printer emission.
    ///
    /// The renderer escapes each row, so a caller passing a module's own file
    /// content does not sanitize it first.
    pub fn diff(&self, old: &str, new: &str) {
        let depth = self.renderer.inherit_depth();
        self.renderer
            .render_diff(self.sink_stderr.as_ref(), depth, old, new);
    }

    /// Syntax-highlighted code. Goes to stderr. Nests at whatever depth the
    /// caller's section opened, the same as every other Printer emission.
    ///
    /// The renderer escapes each line, so a caller passing model- or
    /// registry-supplied text does not sanitize it first.
    pub fn syntax_highlight(&self, code: &str, lang: &str) {
        let depth = self.renderer.inherit_depth();
        self.renderer.render_syntax_highlight(
            self.sink_stderr.as_ref(),
            depth,
            code,
            lang,
            &self.syntax_set,
        );
    }

    /// Raw stdout line, no decoration, no indent. For `config get`-shaped
    /// callers whose output is consumed by other programs.
    pub fn data_line(&self, text: &str) {
        // The sink appends the line's newline; a payload that already ends
        // with one (a serialized YAML document) would otherwise close the
        // command's output on a blank line.
        self.sink_stdout
            .write_line(text.trim_end_matches(['\n', '\r']));
    }

    /// One line of live child-process output, dim and rendered at `depth`.
    ///
    /// This is the un-windowed fallback, not a general-purpose surface: it
    /// appends and never reclaims the line. Reach for
    /// [`super::Printer::output_window_at`] instead — it owns the decision
    /// between a bounded repainting tail and this, and callers that pick
    /// streaming by hand are how a step's whole output ends up in the
    /// scrollback on a terminal that could have collapsed it.
    ///
    /// The caller sanitizes each line first (child ANSI would otherwise
    /// execute against the real terminal).
    pub(crate) fn stream_line_at(&self, depth: usize, text: &str) {
        self.renderer
            .render_stream_line(self.sink_stderr.as_ref(), depth, text);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::renderer::StringSink;
    use super::super::{Theme, Verbosity};
    use super::*;
    use crate::output::strip_ansi;

    #[test]
    fn diff_marks_changed_lines() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        r.render_diff(&sink, 0, "a\nb\nc\n", "a\nB\nc\n");
        let out = crate::test_helpers::captured_text(&buf);
        assert!(out.contains("-b"), "got: {out:?}");
        assert!(out.contains("+B"), "got: {out:?}");
    }

    /// A raw block indents to its owning depth like any other emission,
    /// rather than landing at column 0 under a depth-2 parent.
    #[test]
    fn diff_indents_to_the_given_depth() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        r.render_diff(&sink, 2, "a\nb\nc\n", "a\nB\nc\n");
        let out = crate::test_helpers::captured_text(&buf);
        let removed = out
            .lines()
            .find(|l| l.contains("-b"))
            .unwrap_or_else(|| panic!("removed line missing: {out:?}"));
        assert!(
            removed.starts_with("    -b"),
            "depth-2 raw line must carry a four-space indent: {removed:?}"
        );
    }

    /// `emit_raw_block` flushes a deferred section header before the
    /// block's own lines, the same as every other emission — a diff opened
    /// mid-section must not skip past the header that names it.
    #[test]
    fn diff_flushes_a_pending_section_header_first() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        r.render_section_open("module:nvim", true);
        r.render_diff(&sink, 1, "a\n", "b\n");
        let out = crate::test_helpers::captured_text(&buf);
        let header_at = out
            .find("module:nvim")
            .unwrap_or_else(|| panic!("section header missing: {out:?}"));
        let diff_at = out
            .find("-a")
            .unwrap_or_else(|| panic!("removed line missing: {out:?}"));
        assert!(
            header_at < diff_at,
            "the section header must render before the diff it opened: {out:?}"
        );
    }

    /// `emit_raw_block` drains any buffered `kv` pairs before writing the
    /// block's own lines, the same as `flush_section_headers` does for a
    /// pending header — a diff opened right after a `kv()` call must not
    /// render above the buffered pair it followed.
    #[test]
    fn diff_drains_a_pending_kv_buffer_first() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        r.render_kv("Path", "/etc/cfgd.yaml");
        r.render_diff(&sink, 0, "a\n", "b\n");
        let out = crate::test_helpers::captured_text(&buf);
        let kv_at = out
            .find("/etc/cfgd.yaml")
            .unwrap_or_else(|| panic!("buffered kv missing: {out:?}"));
        let diff_at = out
            .find("-a")
            .unwrap_or_else(|| panic!("removed line missing: {out:?}"));
        assert!(
            kv_at < diff_at,
            "a kv buffered before the diff must render first: {out:?}"
        );
    }

    #[test]
    fn syntax_highlight_renders_lines() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let ss = SyntaxSet::load_defaults_newlines();
        r.render_syntax_highlight(&sink, 0, "let x = 1;\nlet y = 2;\n", "rs", &ss);
        let out = crate::test_helpers::captured_text(&buf);
        let stripped = strip_ansi(&out);
        assert!(
            stripped.contains("let x"),
            "stripped output missing 'let x': {stripped:?}"
        );
        assert!(
            stripped.contains("let y"),
            "stripped output missing 'let y': {stripped:?}"
        );
    }

    /// An empty source line has nothing to highlight, and syntect answers it
    /// with a bare reset: indented by the emitter, that is a row of whitespace
    /// and an escape, which a golden reads as trailing whitespace.
    #[test]
    fn a_blank_line_inside_a_highlighted_body_renders_blank() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default().with_colors(true), Verbosity::Normal);
        let ss = SyntaxSet::load_defaults_newlines();
        r.render_syntax_highlight(&sink, 1, "let x = 1;\n\nlet y = 2;\n", "rs", &ss);
        // raw-capture-ok: the claim is that the blank row carries no escape at all
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let blank = out
            .lines()
            .find(|l| strip_ansi(l).trim().is_empty())
            .unwrap_or_else(|| panic!("the declared blank line renders as a row: {out:?}"));
        assert_eq!(blank, "", "the blank row carries no indent and no escape");
        // The newline each line is fed with is the emitter's to place, so no
        // highlighted row carries one of its own.
        for row in r.highlight_lines("let x = 1;\n\nlet y = 2;\n", "rs", &ss) {
            assert!(
                !row.contains('\n'),
                "a highlighted row kept the newline it was fed: {row:?}"
            );
        }
    }

    /// syntect carries its own theme and emits truecolor escapes without ever
    /// consulting `Theme`, so `cfgd diff --no-color` wrote escapes into the
    /// reader's pipe while every other line on the same screen was unstyled.
    #[test]
    fn syntax_highlight_spends_no_colour_when_the_printer_has_none() {
        let ss = SyntaxSet::load_defaults_newlines();

        let render = |colors: bool| {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let r = Renderer::new(Theme::default().with_colors(colors), Verbosity::Normal);
            r.render_syntax_highlight(&sink, 0, "let x = 1;\nlet y = 2;\n", "rs", &ss);
            // raw-capture-ok: asserting on the presence/absence of raw ANSI escapes themselves — captured_text would strip them
            buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
        };

        let off = render(false);
        assert!(
            !off.contains('\u{1b}'),
            "a colourless printer highlighted with escapes: {off:?}"
        );
        assert!(
            off.contains("let x"),
            "the code itself was dropped: {off:?}"
        );

        let on = render(true);
        assert!(
            on.contains('\u{1b}'),
            "a colour printer emitted no escapes, so the assertion above \
             proves nothing: {on:?}"
        );
    }

    /// The colour-ON arm hands each line to syntect, which writes the text
    /// between its own SGR runs untouched — so the escape has to land BEFORE
    /// highlighting, not after it. The emulated-screen tests in
    /// `output/tests/cursor_safe_slots.rs` run on a colourless test printer
    /// and so reach only the plain arm; this is the other one.
    #[test]
    fn syntax_highlight_escapes_hostile_content_before_it_is_highlighted() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default().with_colors(true), Verbosity::Normal);
        let ss = SyntaxSet::load_defaults_newlines();
        r.render_syntax_highlight(
            &sink,
            0,
            "packages: [ripgrep]\r\u{1b}[2Krepainted\n",
            "yaml",
            &ss,
        );
        // raw-capture-ok: the claim is about which escapes survive, and captured_text strips exactly what this test looks for
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            out.contains("\\x0d") && out.contains("\\x1b[2K"),
            "the hostile bytes must be SHOWN on a screen an operator approves \
             from: {out:?}"
        );
        assert!(
            !out.contains("\u{1b}[2K") && !out.contains('\r'),
            "a live erase or return reached the sink: {out:?}"
        );
        assert!(
            out.contains('\u{1b}'),
            "syntect emitted no styling, so the assertion above proves \
             nothing about the highlighted arm: {out:?}"
        );
    }

    /// The approved Dracula bytes for a script body, to the escape.
    ///
    /// The palette is the preset's, not a syntect default, and each line closes
    /// on a reset: without one the last foreground run of a body stays in force
    /// over whatever the command prints next. The expected line is the third
    /// line of the approved render, carrying the state the two lines above it
    /// left the highlighter in.
    #[test]
    fn a_script_body_under_dracula_renders_the_approved_bytes() {
        let script = "set -eu\n\
                      if ! command -v rg >/dev/null; then\n  \
                        echo \"ripgrep missing\" >&2\n  \
                        exit 1\n\
                      fi\n\
                      rg --version | head -1\n";
        let expected = "\u{1b}[38;2;248;248;242m  \u{1b}[38;2;139;233;253mecho\
                        \u{1b}[38;2;248;248;242m \u{1b}[38;2;241;250;140m\"\
                        \u{1b}[38;2;241;250;140mripgrep missing\
                        \u{1b}[38;2;241;250;140m\"\u{1b}[38;2;248;248;242m \
                        \u{1b}[38;2;255;121;198m>&\u{1b}[38;2;189;147;249m2\u{1b}[0m";

        let dracula =
            Theme::preset("dracula").unwrap_or_else(|| panic!("the dracula preset must exist"));
        let (printer, buf) =
            super::super::Printer::for_test_with_theme_colored(dracula, Verbosity::Normal);
        printer.syntax_highlight(script, "bash");
        // raw-capture-ok: the claim IS the escapes, which captured_text strips
        let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let echoed = out
            .lines()
            .nth(2)
            .unwrap_or_else(|| panic!("the body rendered fewer than three lines: {out:?}"));
        assert_eq!(echoed, expected);
    }

    #[test]
    /// A payload that already ends with a newline (a serialized document,
    /// a decrypted file) closes on exactly one, never on a blank line.
    fn data_line_closes_a_newline_terminated_payload_on_one_newline() {
        use super::super::Verbosity;
        use super::super::printer::Printer;

        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let (mut p, _shared_buf) = Printer::for_test_at(Verbosity::Normal);
        p.sink_stdout = Arc::new(StringSink(stdout_buf.clone()));
        p.data_line("kind: Pod\nspec: {}\n");
        p.flush();
        let stdout = crate::test_helpers::captured_text(&stdout_buf);
        assert_eq!(stdout, "kind: Pod\nspec: {}\n");
    }

    #[test]
    fn data_line_writes_to_stdout_raw() {
        use super::super::Verbosity;
        use super::super::printer::Printer;

        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        // `for_test_at` pins live_region/interactive_stdin/colors rather than
        // probing the real terminal `Printer::new` would; the two sinks it
        // hands back share one buffer, so this test — which asserts stdout and
        // stderr stay separate — swaps in its own pair after construction.
        let (mut p, _shared_buf) = Printer::for_test_at(Verbosity::Normal);
        p.sink_stdout = Arc::new(StringSink(stdout_buf.clone()));
        p.sink_stderr = Arc::new(StringSink(stderr_buf.clone()));

        p.data_line("raw payload");
        p.flush();

        let stdout = crate::test_helpers::captured_text(&stdout_buf);
        let stderr = crate::test_helpers::captured_text(&stderr_buf);
        // data_line is RAW: exact text on stdout, no decoration, no indent.
        assert!(stdout.contains("raw payload"), "stdout got: {stdout:?}");
        // And NOT routed through the section/indent system to stderr.
        assert!(
            !stderr.contains("raw payload"),
            "leaked to stderr: {stderr:?}"
        );
    }
}

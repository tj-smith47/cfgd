//! Raw renderers — diff, syntax_highlight, data_line.
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
use syntect::highlighting::Style as SynStyle;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

use super::renderer::{Renderer, Writer};

impl Renderer {
    /// Render a unified diff using `theme.diff_*` styles. Lines starting with
    /// `+` are themed diff_add, `-` themed diff_remove, others diff_context.
    /// Nests at `depth`, like any other emission.
    pub fn render_diff(&self, w: &dyn Writer, depth: usize, old: &str, new: &str) {
        let diff = TextDiff::from_lines(old, new);
        let mut lines = Vec::new();
        for change in diff.iter_all_changes() {
            let (sign, style) = match change.tag() {
                ChangeTag::Insert => ("+", &self.theme.diff_add),
                ChangeTag::Delete => ("-", &self.theme.diff_remove),
                ChangeTag::Equal => (" ", &self.theme.diff_context),
            };
            let body = format!("{sign}{change}");
            let body = body.trim_end_matches('\n');
            lines.push(style.apply_to(body).to_string());
        }
        // One block per render, so a diff is never split across two of
        // indicatif's clear/redraw cycles.
        self.emit_raw_block(w, depth, &lines);
    }

    /// Render syntax-highlighted code. Caller passes the `lang` hint (e.g.,
    /// "yaml", "rust", "json"); falls back to plain text on unknown. Nests
    /// at `depth`, like any other emission.
    pub fn render_syntax_highlight(
        &self,
        w: &dyn Writer,
        depth: usize,
        code: &str,
        lang: &str,
        syntax_set: &SyntaxSet,
        theme_set: &syntect::highlighting::ThemeSet,
    ) {
        // syntect emits truecolor escapes of its own, from its own theme —
        // nothing about this path passes through `Theme`, so a colour decision
        // enforced only at style lookup does not reach it, and `cfgd diff
        // --no-color` / `NO_COLOR=1` still wrote escapes into the reader's
        // pipe. Same fallback as the missing-theme arm below.
        if !self.theme.colors() {
            let plain: Vec<String> = code.lines().map(str::to_string).collect();
            self.emit_raw_block(w, depth, &plain);
            return;
        }
        let syntax = syntax_set
            .find_syntax_by_token(lang)
            .or_else(|| syntax_set.find_syntax_by_extension(lang))
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
        let Some(theme) = theme_set
            .themes
            .get("base16-ocean.dark")
            .or_else(|| theme_set.themes.values().next())
        else {
            // No syntect themes available; emit unstyled lines.
            let plain: Vec<String> = code.lines().map(str::to_string).collect();
            self.emit_raw_block(w, depth, &plain);
            return;
        };
        let mut h = HighlightLines::new(syntax, theme);
        let mut lines = Vec::new();
        for line in code.lines() {
            let ranges: Vec<(SynStyle, &str)> =
                h.highlight_line(line, syntax_set).unwrap_or_default();
            lines.push(as_24_bit_terminal_escaped(&ranges, false));
        }
        // Built outside the guard: highlighting is expensive and touches no
        // render state, so the lock is taken only around the emission.
        self.emit_raw_block(w, depth, &lines);
    }
}

impl super::Printer {
    /// Diff renderer. Goes to stderr. Nests at whatever depth the caller's
    /// section opened, the same as every other Printer emission.
    pub fn diff(&self, old: &str, new: &str) {
        let depth = self.renderer.inherit_depth();
        self.renderer
            .render_diff(self.sink_stderr.as_ref(), depth, old, new);
    }

    /// Syntax-highlighted code. Goes to stderr. Nests at whatever depth the
    /// caller's section opened, the same as every other Printer emission.
    pub fn syntax_highlight(&self, code: &str, lang: &str) {
        let depth = self.renderer.inherit_depth();
        self.renderer.render_syntax_highlight(
            self.sink_stderr.as_ref(),
            depth,
            code,
            lang,
            &self.syntax_set,
            &self.theme_set,
        );
    }

    /// Raw stdout line, no decoration, no indent. For `config get`-shaped
    /// callers whose output is consumed by other programs.
    pub fn data_line(&self, text: &str) {
        self.sink_stdout.write_line(text);
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

    /// R11: a raw block indents to its owning depth like any other
    /// emission, rather than landing at column 0 under a depth-2 parent.
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

    /// R11: `emit_raw_block` flushes a deferred section header before the
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

    #[test]
    fn syntax_highlight_renders_lines() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = syntect::highlighting::ThemeSet::load_defaults();
        r.render_syntax_highlight(&sink, 0, "let x = 1;\nlet y = 2;\n", "rs", &ss, &ts);
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

    /// syntect carries its own theme and emits truecolor escapes without ever
    /// consulting `Theme`, so `cfgd diff --no-color` wrote escapes into the
    /// reader's pipe while every other line on the same screen was unstyled.
    #[test]
    fn syntax_highlight_spends_no_colour_when_the_printer_has_none() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = syntect::highlighting::ThemeSet::load_defaults();

        let render = |colors: bool| {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            let r = Renderer::new(Theme::default().with_colors(colors), Verbosity::Normal);
            r.render_syntax_highlight(&sink, 0, "let x = 1;\nlet y = 2;\n", "rs", &ss, &ts);
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
